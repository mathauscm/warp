//! The conversation: user bubbles, Claude's answers, the work it did and its notices. Rows come
//! from `claude_chat::presentation` and a viewported list lays out only the ones on screen, so
//! long conversations stay fast.

mod details;
mod markdown;
mod render;
mod tools;
mod viewport;

use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use base64::Engine as _;
use claude_chat::artifacts::{ArtifactMeta, artifact_id_from_tool_result, is_artifact_tool};
use claude_chat::presentation::{Row, build_rows};
use claude_chat::reducer::{ConversationChange, ConversationStatus};
use claude_chat::timeline::TimelineItem;
use pathfinder_color::ColorU;
use pathfinder_geometry::vector::{Vector2F, vec2f};
use serde_json::Value;
use syntax_tree::{ColorMap, highlight_snippet};
use warpui::assets::asset_cache::{AssetCache, AssetSource};
use warpui::r#async::Timer;
use warpui::clipboard::ClipboardContent;
use warpui::elements::new_scrollable::{
    NewScrollableElement, ScrollableAppearance, SingleAxisConfig,
};
use warpui::elements::shimmering_text::{
    ShimmerConfig, ShimmeringTextElement, ShimmeringTextStateHandle,
};
use warpui::elements::{
    Align, Border, ChildAnchor, ClippedScrollStateHandle, ConstrainedBox, Container, CornerRadius,
    CrossAxisAlignment, DispatchEventResult, DropShadow, Empty, EventHandler, Fill, Flex,
    Hoverable, Image, ListState, MainAxisSize, MouseStateHandle, NewScrollable, OffsetPositioning,
    ParentAnchor, ParentElement, ParentOffsetBounds, Radius, SavePosition, ScrollStateHandle,
    ScrollbarWidth, SelectableArea, SelectionHandle, Stack, Text,
};
use warpui::fonts::{FamilyId, Properties, Weight};
use warpui::image_cache::{CacheOption, ImageType};
use warpui::platform::Cursor;
use warpui::{
    AppContext, Element, Entity, ModelHandle, SingletonEntity, TypedActionView, View, ViewContext,
    WeakViewHandle,
};

use self::markdown::Block;
use self::render::{RowContext, Style, icon};
use self::viewport::{Viewport, ViewportState};
use crate::native_chat::design::{ChatDesign, ChatDesignEvent, Palette};
use crate::native_chat::session::{ClaudeChatSession, ClaudeChatSessionEvent};

/// SF Mono under the names macOS knows it by.
const INLINE_CODE_FONTS: [&str; 3] = [".AppleSystemUIFontMonospaced", ".SF NS Mono", "SF Mono"];
const COPIED_FEEDBACK: Duration = Duration::from_millis(1500);
const APP_ICON: &str = "bundled/png/picolas-icon.png";
const EMPTY_STATE_ICON_SIZE: f32 = 96.;
const ICON_FOLDER: &str = "bundled/svg/picolas-folder.svg";
const ICON_ARROW_DOWN: &str = "bundled/svg/picolas-arrow-down.svg";
const ICON_MENTION: &str = "bundled/svg/picolas-mention.svg";
/// Space between the last selected line and the "Mencionar" pill.
const MENTION_OFFSET: f32 = 6.;
const MENTION_HEIGHT: f32 = 26.;
/// Space between the "↓" button and the bottom of the transcript.
const SCROLL_BUTTON_MARGIN: f32 = 14.;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptEvent {
    /// Text the user selected and wants to mention in the next message.
    Mention(String),
    OpenArtifact(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum TranscriptAction {
    /// Folds or unfolds something; `row` is the top-level row to re-measure and `expanded`
    /// whether it was showing.
    Toggle {
        key: String,
        row: usize,
        expanded: bool,
    },
    Copy {
        key: String,
        text: String,
    },
    OpenUrl(String),
    OpenArtifact(String),
    /// Opens a local file in its default app.
    OpenFile(String),
    ScrollToBottom,
    SelectionChanged(Option<String>),
    MouseDown,
    /// The left button went up at this window position.
    MouseUp(Vector2F),
    Mention,
    CopySelection,
}

/// A code block's syntax colors, by character range.
type Highlights = Rc<Vec<(Range<usize>, ColorU)>>;

/// Per-row state handles and parsed Markdown, created as rows are first drawn.
#[derive(Default)]
struct RenderCache {
    mouse: HashMap<String, MouseStateHandle>,
    scroll: HashMap<String, ClippedScrollStateHandle>,
    shimmer: HashMap<String, ShimmeringTextStateHandle>,
    blocks: HashMap<String, (u64, Rc<Vec<Block>>)>,
    highlights: HashMap<String, (u64, Highlights)>,
}

pub struct TranscriptView {
    session: ModelHandle<ClaudeChatSession>,
    list_state: ListState<()>,
    viewport: Rc<ViewportState>,
    scroll_state: ScrollStateHandle,
    rows: Vec<Row>,
    row_keys: Vec<String>,
    /// Folds the user opened or closed, overriding each one's default.
    expanded: HashMap<String, bool>,
    cache: RefCell<RenderCache>,
    /// The artifact each artifact tool call made, by tool call id.
    artifacts: HashMap<String, ArtifactMeta>,
    /// Images sent as data, loaded into the asset cache under these ids.
    image_assets: HashSet<String>,
    selection_handle: SelectionHandle,
    selection: Option<String>,
    /// Where the "Mencionar" pill goes, relative to the transcript.
    mention_at: Option<Vector2F>,
    is_mouse_down: bool,
    /// Where the last drag ended, relative to the transcript, for placing the pill once the
    /// selection is known.
    last_mouse_up: Option<Vector2F>,
    copied: Option<String>,
    is_ticking: bool,
    position_id: String,
    /// The font of inline code in prose: SF Mono, whose glyphs sit closer to the prose's size
    /// than most terminal fonts, falling back to the code font.
    inline_code_font: Option<FamilyId>,
    /// `PICOLAS_TRANSCRIPT_EXPAND` unfolds everything in debug builds, for screenshots.
    expand_all: bool,
}

impl TranscriptView {
    pub fn new(session: ModelHandle<ClaudeChatSession>, ctx: &mut ViewContext<Self>) -> Self {
        ctx.subscribe_to_model(&session, |me, _, event, ctx| {
            me.on_session_event(event, ctx)
        });
        ctx.subscribe_to_model(&ChatDesign::handle(ctx), |me, _, event, ctx| match event {
            ChatDesignEvent::Changed => {
                me.viewport.remeasure();
                ctx.notify();
            }
        });
        let handle: WeakViewHandle<Self> = ctx.handle();
        let list_state = ListState::new(move |index, _, app| match handle.upgrade(app) {
            Some(view) => view.as_ref(app).render_row_at(index, app),
            None => Empty::new().finish(),
        });
        let mut view = Self {
            session,
            list_state,
            viewport: Rc::default(),
            scroll_state: ScrollStateHandle::default(),
            rows: Vec::new(),
            row_keys: Vec::new(),
            expanded: HashMap::new(),
            cache: RefCell::default(),
            artifacts: HashMap::new(),
            image_assets: HashSet::new(),
            selection_handle: SelectionHandle::default(),
            selection: None,
            mention_at: None,
            is_mouse_down: false,
            last_mouse_up: None,
            copied: None,
            is_ticking: false,
            inline_code_font: warpui::fonts::Cache::handle(ctx).update(ctx, |cache, _| {
                INLINE_CODE_FONTS
                    .iter()
                    .find_map(|name| cache.get_or_load_system_font(name).ok())
            }),
            expand_all: cfg!(debug_assertions)
                && std::env::var_os("PICOLAS_TRANSCRIPT_EXPAND").is_some(),
            position_id: format!("native_chat_transcript_{}", ctx.view_id()),
        };
        view.reload(ctx);
        view
    }

    fn on_session_event(&mut self, event: &ClaudeChatSessionEvent, ctx: &mut ViewContext<Self>) {
        match event {
            ClaudeChatSessionEvent::Changed(changes) => self.on_conversation_changed(changes, ctx),
            ClaudeChatSessionEvent::HistoryLoaded => self.reload(ctx),
            ClaudeChatSessionEvent::ProcessStateChanged => {
                let from = self.last_turn_start(ctx);
                self.refresh_rows(Some(from), ctx);
            }
            ClaudeChatSessionEvent::TitleChanged
            | ClaudeChatSessionEvent::SessionIdAssigned
            | ClaudeChatSessionEvent::FileSuggestions { .. }
            | ClaudeChatSessionEvent::QueuedMessageCancelled { .. }
            | ClaudeChatSessionEvent::Error(_) => {}
        }
    }

    /// Rebuilds every row, as when the saved conversation finished loading.
    fn reload(&mut self, ctx: &mut ViewContext<Self>) {
        let count = self.session.as_ref(ctx).conversation().items().len();
        for index in 0..count {
            self.track_item(index, ctx);
        }
        for index in (0..self.rows.len()).rev() {
            self.list_state.remove(index);
        }
        self.rows.clear();
        self.row_keys.clear();
        // `PICOLAS_TRANSCRIPT_TOP` opens debug builds at the start of the conversation, for
        // screenshots of its beginning.
        let open_at_top =
            cfg!(debug_assertions) && std::env::var_os("PICOLAS_TRANSCRIPT_TOP").is_some();
        self.viewport.stick_to_bottom.set(!open_at_top);
        self.refresh_rows(None, ctx);
    }

    fn on_conversation_changed(
        &mut self,
        changes: &[ConversationChange],
        ctx: &mut ViewContext<Self>,
    ) {
        let mut dirty_from: Option<usize> = None;
        let mut changed = false;
        for change in changes {
            match change {
                ConversationChange::ItemInserted { index, .. }
                | ConversationChange::ItemUpdated { index, .. } => {
                    self.track_item(*index, ctx);
                    dirty_from = Some(dirty_from.map_or(*index, |from| from.min(*index)));
                }
                ConversationChange::ItemRemoved { index, .. } => {
                    dirty_from = Some(dirty_from.map_or(*index, |from| from.min(*index)));
                }
                ConversationChange::Status | ConversationChange::PendingRequests => changed = true,
                ConversationChange::Session
                | ConversationChange::Commands
                | ConversationChange::Models
                | ConversationChange::Account
                | ConversationChange::McpServers
                | ConversationChange::ContextUsage
                | ConversationChange::RateLimits
                | ConversationChange::Cost
                | ConversationChange::Todos
                | ConversationChange::Tasks
                | ConversationChange::RemoteControl => {}
            }
        }
        if changed {
            let from = self.last_turn_start(ctx);
            dirty_from = Some(dirty_from.map_or(from, |dirty| dirty.min(from)));
        }
        if dirty_from.is_some() {
            self.refresh_rows(dirty_from, ctx);
        }
    }

    /// Notes what an item needs besides its row: the artifact an artifact tool made, and the
    /// images a message carries as data.
    fn track_item(&mut self, index: usize, ctx: &mut ViewContext<Self>) {
        let session = self.session.as_ref(ctx);
        let Some(item) = session.conversation().items().get(index) else {
            return;
        };
        match item {
            TimelineItem::Tool(tool) if is_artifact_tool(&tool.name) => {
                let Some(result) = &tool.result else {
                    return;
                };
                let result_value = result
                    .structured
                    .clone()
                    .unwrap_or_else(|| Value::String(result.text.clone()));
                if let Some(meta) =
                    artifact_id_from_tool_result(&tool.name, &tool.input, Some(&result_value))
                        .and_then(|id| session.artifacts().get(&id))
                {
                    self.artifacts.insert(tool.id.clone(), meta);
                }
            }
            TimelineItem::User(message) => {
                let mut images = Vec::new();
                for (position, image) in message.images.iter().enumerate() {
                    let id = image_asset_id(&message.id, position);
                    if image.path.is_some() || self.image_assets.contains(&id) {
                        continue;
                    }
                    let Some(bytes) = image.data.as_deref().and_then(|data| {
                        base64::engine::general_purpose::STANDARD.decode(data).ok()
                    }) else {
                        continue;
                    };
                    images.push((id, bytes));
                }
                for (id, bytes) in images {
                    AssetCache::handle(ctx).update(ctx, |cache, ctx| {
                        cache.insert_raw_asset_bytes::<ImageType>(id.clone(), &bytes, ctx);
                    });
                    self.image_assets.insert(id);
                }
            }
            TimelineItem::Tool(_)
            | TimelineItem::Assistant(_)
            | TimelineItem::Thinking(_)
            | TimelineItem::Notice(_)
            | TimelineItem::TurnEnd(_) => {}
        }
    }

    /// The index of the last turn's user message, from where a status change can regroup rows.
    fn last_turn_start(&self, app: &AppContext) -> usize {
        let items = self.session.as_ref(app).conversation().items();
        items
            .iter()
            .rposition(|item| matches!(item, TimelineItem::User(_)))
            .unwrap_or(0)
    }

    fn is_working(&self, app: &AppContext) -> bool {
        match self.session.as_ref(app).conversation().status() {
            ConversationStatus::Working { .. } | ConversationStatus::AwaitingUser => true,
            ConversationStatus::Starting
            | ConversationStatus::Idle
            | ConversationStatus::Exited => false,
        }
    }

    /// Regroups the rows and re-measures those from the first one that changed; `dirty_from` is
    /// the first item whose content changed.
    fn refresh_rows(&mut self, dirty_from: Option<usize>, ctx: &mut ViewContext<Self>) {
        let is_working = self.is_working(ctx);
        let items = self.session.as_ref(ctx).conversation().items();
        let rows = build_rows(items, is_working, SystemTime::now());
        let keys: Vec<String> = rows.iter().map(|row| row_key(row, items)).collect();
        let dirty_row = dirty_from.map(|item| {
            rows.iter()
                .position(|row| row_last_item(row) >= item)
                .unwrap_or(rows.len().saturating_sub(1))
        });
        let start = first_changed_row(&self.row_keys, &keys, dirty_row);
        for index in (start..self.rows.len()).rev() {
            self.list_state.remove(index);
        }
        for _ in start..rows.len() {
            self.list_state.add_item();
        }
        // The previous last row loses its bottom padding when rows are added after it.
        if start == self.rows.len() && start > 0 && rows.len() > start {
            self.list_state.invalidate_height_for_index(start - 1);
        }
        self.viewport.row_count.set(rows.len());
        self.rows = rows;
        self.row_keys = keys;
        if is_working {
            self.schedule_tick(ctx);
        }
        ctx.notify();
    }

    /// Refreshes live timers ("Trabalhando há 12s") once a second while Claude works.
    fn schedule_tick(&mut self, ctx: &mut ViewContext<Self>) {
        if self.is_ticking {
            return;
        }
        self.is_ticking = true;
        ctx.spawn(Timer::after(render::LIVE_TICK), |me, _, ctx| {
            me.is_ticking = false;
            if me.is_working(ctx) {
                me.refresh_rows(None, ctx);
            }
        });
    }

    fn render_row_at(&self, index: usize, app: &AppContext) -> Box<dyn Element> {
        let Some(row) = self.rows.get(index) else {
            return Empty::new().finish();
        };
        let session = self.session.as_ref(app);
        let style = Style::current(app);
        let cx = RowContext {
            view: self,
            items: session.conversation().items(),
            cwd: session.cwd(),
            style: &style,
            row: index,
            now: SystemTime::now(),
        };
        let sizes = &style.sizes;
        let spacing_after = match row {
            Row::UserBubble { .. } => 2.,
            Row::AssistantProse { .. } => sizes.message_spacing - 6.,
            Row::WorkGroup(_) => 10.,
            Row::Thinking { .. }
            | Row::Tool { .. }
            | Row::ToolBatch(_)
            | Row::Notice { .. }
            | Row::TurnError { .. } => 8.,
        };
        let is_last = index + 1 == self.rows.len();
        Container::new(render::render_row(&cx, row, false))
            .with_padding_top(if index == 0 {
                sizes.transcript_padding
            } else {
                0.
            })
            .with_padding_bottom(if is_last {
                sizes.transcript_padding + spacing_after
            } else {
                spacing_after
            })
            .finish()
    }

    // --- State the row renderers read -----------------------------------------------------

    fn mouse(&self, key: &str) -> MouseStateHandle {
        self.cache
            .borrow_mut()
            .mouse
            .entry(key.to_owned())
            .or_default()
            .clone()
    }

    fn scroll_handle(&self, key: &str) -> ClippedScrollStateHandle {
        self.cache
            .borrow_mut()
            .scroll
            .entry(key.to_owned())
            .or_default()
            .clone()
    }

    fn shimmer(&self, key: &str) -> ShimmeringTextStateHandle {
        self.cache
            .borrow_mut()
            .shimmer
            .entry(key.to_owned())
            .or_default()
            .clone()
    }

    /// `markdown` split into blocks, parsed again only when it changed.
    fn blocks(&self, key: &str, markdown: &str) -> Rc<Vec<Block>> {
        let mut hasher = DefaultHasher::new();
        markdown.hash(&mut hasher);
        let hash = hasher.finish();
        let mut cache = self.cache.borrow_mut();
        if let Some((cached_hash, blocks)) = cache.blocks.get(key)
            && *cached_hash == hash
        {
            return blocks.clone();
        }
        let blocks = Rc::new(markdown::split_blocks(markdown));
        cache.blocks.insert(key.to_owned(), (hash, blocks.clone()));
        blocks
    }

    /// Syntax colors for a code block, computed again only when its code or the palette changed.
    fn highlights(&self, key: &str, language: &str, code: &str, palette: &Palette) -> Highlights {
        let mut hasher = DefaultHasher::new();
        (
            language,
            code,
            palette.syntax_keyword.0,
            palette.syntax_comment.0,
        )
            .hash(&mut hasher);
        let hash = hasher.finish();
        if let Some((cached_hash, highlights)) = self.cache.borrow().highlights.get(key)
            && *cached_hash == hash
        {
            return highlights.clone();
        }
        let color_map = ColorMap {
            keyword_color: palette.syntax_keyword.0,
            function_color: palette.syntax_function.0,
            string_color: palette.syntax_string.0,
            type_color: palette.syntax_type.0,
            number_color: palette.syntax_number.0,
            comment_color: palette.syntax_comment.0,
            property_color: palette.syntax_property.0,
            tag_color: palette.syntax_tag.0,
            by_capture: None,
        };
        let highlights = Rc::new(highlight_snippet(language, code, color_map));
        self.cache
            .borrow_mut()
            .highlights
            .insert(key.to_owned(), (hash, highlights.clone()));
        highlights
    }

    fn is_expanded(&self, key: &str, default: bool) -> bool {
        self.expanded
            .get(key)
            .copied()
            .unwrap_or(default || self.expand_all)
    }

    fn is_copied(&self, key: &str) -> bool {
        self.copied.as_deref() == Some(key)
    }

    fn artifact(&self, tool_id: &str) -> Option<&ArtifactMeta> {
        self.artifacts.get(tool_id)
    }

    fn image_asset_id(&self, message_id: &str, position: usize) -> Option<String> {
        let id = image_asset_id(message_id, position);
        self.image_assets.contains(&id).then_some(id)
    }

    // --- Actions ---------------------------------------------------------------------------

    fn copy(&mut self, key: String, text: String, ctx: &mut ViewContext<Self>) {
        ctx.clipboard().write(ClipboardContent::plain_text(text));
        self.copied = Some(key.clone());
        ctx.notify();
        ctx.spawn(Timer::after(COPIED_FEEDBACK), move |me, _, ctx| {
            if me.copied.as_deref() == Some(key.as_str()) {
                me.copied = None;
                ctx.notify();
            }
        });
    }

    fn set_mention_at(&mut self, at: Option<Vector2F>, ctx: &mut ViewContext<Self>) {
        if self.mention_at != at {
            self.mention_at = at;
            ctx.notify();
        }
    }

    fn mention_selection(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(text) = self.selection.take() else {
            return;
        };
        self.selection_handle.clear();
        self.mention_at = None;
        ctx.emit(TranscriptEvent::Mention(text));
        ctx.notify();
    }

    // --- Whole-view states -----------------------------------------------------------------

    fn render_loading(style: &Style) -> Box<dyn Element> {
        Align::new(
            ShimmeringTextElement::new(
                "Carregando conversa…",
                style.ui,
                style.sizes.small_font_size + 0.5,
                style.palette.text_muted.0,
                style.palette.shimmer.0,
                ShimmerConfig::default(),
                ShimmeringTextStateHandle::default(),
            )
            .finish(),
        )
        .finish()
    }

    fn render_empty_state(style: &Style, folder: &str) -> Box<dyn Element> {
        let palette = &style.palette;
        let sizes = &style.sizes;
        Align::new(
            Flex::column()
                .with_main_axis_size(MainAxisSize::Min)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(
                    ConstrainedBox::new(
                        Image::new(AssetSource::Bundled { path: APP_ICON }, CacheOption::BySize)
                            .contain()
                            .finish(),
                    )
                    .with_width(EMPTY_STATE_ICON_SIZE)
                    .with_height(EMPTY_STATE_ICON_SIZE)
                    .finish(),
                )
                .with_child(
                    Container::new(
                        Text::new_inline(
                            "Como posso ajudar?",
                            style.ui,
                            sizes.title_font_size * 1.6,
                        )
                        .with_color(palette.text.0)
                        .with_style(Properties::default().weight(Weight::Medium))
                        .with_selectable(false)
                        .finish(),
                    )
                    .with_padding_top(14.)
                    .finish(),
                )
                .with_child(
                    Container::new(
                        Flex::row()
                            .with_main_axis_size(MainAxisSize::Min)
                            .with_cross_axis_alignment(CrossAxisAlignment::Center)
                            .with_spacing(6.)
                            .with_child(icon(
                                ICON_FOLDER,
                                palette.text_faint.0,
                                sizes.small_icon_size,
                            ))
                            .with_child(
                                Text::new_inline(
                                    folder.to_owned(),
                                    style.ui,
                                    sizes.small_font_size,
                                )
                                .with_color(palette.text_muted.0)
                                .with_selectable(false)
                                .finish(),
                            )
                            .finish(),
                    )
                    .with_padding_top(8.)
                    .finish(),
                )
                .finish(),
        )
        .finish()
    }

    fn render_scroll_button(&self, style: &Style) -> Box<dyn Element> {
        let palette = style.palette.clone();
        let size = style.sizes.scroll_button_size;
        let icon_size = style.sizes.icon_size;
        Hoverable::new(self.mouse("scroll_to_bottom"), move |state| {
            let background = if state.is_hovered() {
                palette.surface_hover.0
            } else {
                palette.surface.0
            };
            ConstrainedBox::new(
                Container::new(
                    Align::new(icon(ICON_ARROW_DOWN, palette.text.0, icon_size)).finish(),
                )
                .with_background_color(background)
                .with_border(Border::all(1.).with_border_color(palette.border_strong.0))
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(size / 2.)))
                .with_drop_shadow(DropShadow::new_with_standard_offset_and_spread(
                    palette.shadow.0,
                ))
                .finish(),
            )
            .with_width(size)
            .with_height(size)
            .finish()
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(|ctx, _, _| ctx.dispatch_typed_action(TranscriptAction::ScrollToBottom))
        .finish()
    }

    fn render_mention_pill(&self, style: &Style) -> Box<dyn Element> {
        let palette = style.palette.clone();
        let ui = style.ui;
        let font_size = style.sizes.small_font_size;
        let icon_size = style.sizes.small_icon_size;
        Hoverable::new(self.mouse("mention"), move |state| {
            let background = if state.is_hovered() {
                palette.surface_hover.0
            } else {
                palette.surface.0
            };
            let content = Flex::row()
                .with_main_axis_size(MainAxisSize::Min)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(6.)
                .with_child(icon(ICON_MENTION, palette.text_muted.0, icon_size))
                .with_child(
                    Text::new_inline("Mencionar", ui, font_size)
                        .with_color(palette.text.0)
                        .with_style(Properties::default().weight(Weight::Medium))
                        .with_selectable(false)
                        .finish(),
                )
                .with_child(
                    Text::new_inline("⌘L", ui, font_size)
                        .with_color(palette.text_faint.0)
                        .with_selectable(false)
                        .finish(),
                )
                .finish();
            Container::new(
                ConstrainedBox::new(content)
                    .with_height(MENTION_HEIGHT)
                    .finish(),
            )
            .with_horizontal_padding(10.)
            .with_background_color(background)
            .with_border(Border::all(1.).with_border_color(palette.border_strong.0))
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(MENTION_HEIGHT / 2.)))
            .with_drop_shadow(DropShadow::new_with_standard_offset_and_spread(
                palette.shadow.0,
            ))
            .finish()
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(|ctx, _, _| ctx.dispatch_typed_action(TranscriptAction::Mention))
        .finish()
    }
}

fn image_asset_id(message_id: &str, position: usize) -> String {
    format!("native_chat_image:{message_id}:{position}")
}

/// A stable identity for a row, so a row that keeps its key keeps its measured height.
fn row_key(row: &Row, items: &[TimelineItem]) -> String {
    let id = |index: usize| items.get(index).map_or("", TimelineItem::id);
    match row {
        Row::UserBubble { index } => format!("user:{}", id(*index)),
        Row::AssistantProse { index } => format!("assistant:{}", id(*index)),
        Row::Thinking { index } => format!("thinking:{}", id(*index)),
        Row::Tool { index } => format!("tool:{}", id(*index)),
        Row::ToolBatch(batch) => format!("batch:{}", batch.id),
        Row::Notice { index } => format!("notice:{}", id(*index)),
        Row::WorkGroup(group) => group.id.clone(),
        Row::TurnError { index } => format!("error:{}", id(*index)),
    }
}

/// The last item a row shows, to tell which rows an item change touches.
fn row_last_item(row: &Row) -> usize {
    match row {
        Row::UserBubble { index }
        | Row::AssistantProse { index }
        | Row::Thinking { index }
        | Row::Tool { index }
        | Row::Notice { index }
        | Row::TurnError { index } => *index,
        Row::ToolBatch(batch) => batch.items.iter().copied().max().unwrap_or_default(),
        Row::WorkGroup(group) => group
            .rows
            .iter()
            .map(row_last_item)
            .chain(group.items.iter().copied())
            .max()
            .unwrap_or_default(),
    }
}

/// The first row to lay out again: where the keys stop matching, or the first row whose content
/// changed, whichever comes first.
fn first_changed_row(old_keys: &[String], new_keys: &[String], dirty_row: Option<usize>) -> usize {
    let first_key_change = old_keys
        .iter()
        .zip(new_keys)
        .position(|(old, new)| old != new)
        .unwrap_or(old_keys.len().min(new_keys.len()));
    first_key_change
        .min(dirty_row.unwrap_or(usize::MAX))
        .min(old_keys.len())
        .min(new_keys.len())
}

impl Entity for TranscriptView {
    type Event = TranscriptEvent;
}

impl View for TranscriptView {
    fn ui_name() -> &'static str {
        "NativeChatTranscript"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let style = Style::current(app);
        let session = self.session.as_ref(app);
        if !session.is_history_loaded() {
            return Self::render_loading(&style);
        }
        if self.rows.is_empty() {
            let folder = session
                .cwd()
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| session.cwd().display().to_string());
            return Self::render_empty_state(&style, &folder);
        }

        let scroller = NewScrollable::vertical(
            SingleAxisConfig::Manual {
                handle: self.scroll_state.clone(),
                child: Viewport::new(self.list_state.clone(), self.viewport.clone())
                    .finish_scrollable(),
            },
            Fill::None,
            Fill::None,
            Fill::None,
        )
        // No scrollbar at all, so rows get the column's full width.
        .with_vertical_scrollbar(ScrollableAppearance::new(ScrollbarWidth::None, true))
        .finish();
        let selectable = SelectableArea::new(
            self.selection_handle.clone(),
            |args, ctx, _| {
                ctx.dispatch_typed_action(TranscriptAction::SelectionChanged(args.selection))
            },
            scroller,
        )
        .finish();
        let events = EventHandler::new(selectable)
            .with_always_handle()
            .on_left_mouse_down(|ctx, _, _| {
                ctx.dispatch_typed_action(TranscriptAction::MouseDown);
                DispatchEventResult::PropagateToParent
            })
            .on_left_mouse_up(|ctx, _, position| {
                ctx.dispatch_typed_action(TranscriptAction::MouseUp(position));
                DispatchEventResult::PropagateToParent
            })
            .on_keydown(|ctx, _, keystroke| {
                if !keystroke.cmd || keystroke.ctrl || keystroke.alt {
                    return DispatchEventResult::PropagateToParent;
                }
                match keystroke.key.as_str() {
                    "l" => {
                        ctx.dispatch_typed_action(TranscriptAction::Mention);
                        DispatchEventResult::StopPropagation
                    }
                    "c" => {
                        ctx.dispatch_typed_action(TranscriptAction::CopySelection);
                        DispatchEventResult::StopPropagation
                    }
                    _ => DispatchEventResult::PropagateToParent,
                }
            })
            .finish();

        let mut stack = Stack::new().with_child(events);
        if !self.viewport.stick_to_bottom.get() && self.viewport.has_more_below() {
            stack.add_positioned_overlay_child(
                self.render_scroll_button(&style),
                OffsetPositioning::offset_from_parent(
                    vec2f(0., -SCROLL_BUTTON_MARGIN),
                    ParentOffsetBounds::ParentByPosition,
                    ParentAnchor::BottomMiddle,
                    ChildAnchor::BottomMiddle,
                ),
            );
        }
        if let Some(at) = self.mention_at.filter(|_| self.selection.is_some()) {
            stack.add_positioned_overlay_child(
                self.render_mention_pill(&style),
                OffsetPositioning::offset_from_parent(
                    // The drag ends somewhere inside the last selected line; the pill goes under it.
                    at + vec2f(
                        0.,
                        style.sizes.font_size * style.sizes.line_height_ratio / 2. + MENTION_OFFSET,
                    ),
                    ParentOffsetBounds::ParentByPosition,
                    ParentAnchor::TopLeft,
                    ChildAnchor::TopRight,
                ),
            );
        }
        SavePosition::new(stack.finish(), &self.position_id).finish()
    }
}

impl TypedActionView for TranscriptView {
    type Action = TranscriptAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            TranscriptAction::Toggle { key, row, expanded } => {
                self.expanded.insert(key.clone(), !expanded);
                if *row < self.rows.len() {
                    self.list_state.invalidate_height_for_index(*row);
                }
                ctx.notify();
            }
            TranscriptAction::Copy { key, text } => self.copy(key.clone(), text.clone(), ctx),
            TranscriptAction::OpenUrl(url) => ctx.open_url(url),
            TranscriptAction::OpenFile(path) => ctx.open_file_path(Path::new(path)),
            TranscriptAction::OpenArtifact(id) => {
                ctx.emit(TranscriptEvent::OpenArtifact(id.clone()));
            }
            TranscriptAction::ScrollToBottom => {
                self.viewport.stick_to_bottom.set(true);
                ctx.notify();
            }
            TranscriptAction::SelectionChanged(selection) => {
                self.selection = selection
                    .clone()
                    .filter(|selection| !selection.trim().is_empty());
                match (
                    self.selection.is_some(),
                    self.is_mouse_down,
                    self.last_mouse_up,
                ) {
                    (false, _, _) => self.set_mention_at(None, ctx),
                    // The selection settles after the mouse-up that ended the drag.
                    (true, false, Some(at)) => {
                        ctx.focus_self();
                        self.set_mention_at(Some(at), ctx);
                    }
                    (true, _, _) => {}
                }
            }
            TranscriptAction::MouseDown => {
                self.is_mouse_down = true;
                self.last_mouse_up = None;
                self.set_mention_at(None, ctx);
            }
            TranscriptAction::MouseUp(position) => {
                self.is_mouse_down = false;
                let Some(bounds) = ctx.element_position_by_id(&self.position_id) else {
                    return;
                };
                let at = *position - bounds.origin();
                self.last_mouse_up = Some(at);
                if self.selection.is_some() {
                    ctx.focus_self();
                    self.set_mention_at(Some(at), ctx);
                }
            }
            TranscriptAction::Mention => self.mention_selection(ctx),
            TranscriptAction::CopySelection => {
                if let Some(selection) = self.selection.clone() {
                    ctx.clipboard()
                        .write(ClipboardContent::plain_text(selection));
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "transcript_tests.rs"]
mod tests;
