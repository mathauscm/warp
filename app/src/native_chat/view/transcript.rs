//! The conversation: user bubbles, Claude's answers, the work it did and its notices. Rows come
//! from `claude_chat::presentation` and a viewported list lays out only the ones on screen, so
//! long conversations stay fast.

mod chips;
mod details;
mod markdown;
mod paths;
mod render;
mod tools;
mod viewport;

use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
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
use ui_components::lightbox::{LightboxImage, LightboxImageSource};
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
    Align, Border, ChildAnchor, ChildView, ClippedScrollStateHandle, ConstrainedBox, Container,
    CornerRadius, CrossAxisAlignment, DispatchEventResult, DropShadow, Empty, EventHandler, Fill,
    Flex, Hoverable, Image, ListState, MainAxisSize, MouseStateHandle, NewScrollable,
    OffsetPositioning, ParentAnchor, ParentElement, ParentOffsetBounds, PositionedElementAnchor,
    PositionedElementOffsetBounds, Radius, SavePosition, ScrollStateHandle, ScrollbarWidth,
    SelectableArea, SelectionDragEnd, SelectionHandle, Stack, Text,
};
use warpui::fonts::{FamilyId, Properties, Weight};
use warpui::image_cache::{CacheOption, ImageType};
use warpui::keymap::FixedBinding;
use warpui::platform::Cursor;
use warpui::units::IntoPixels;
use warpui::{
    AppContext, Element, Entity, ModelHandle, SingletonEntity, TypedActionView, View, ViewContext,
    ViewHandle, WeakViewHandle,
};

use self::markdown::Block;
use self::paths::PathKind;
use self::render::{RowContext, Style, icon};
use self::viewport::{ScrollFollow, Viewport, ViewportState};
use crate::menu::{Event as MenuEvent, Menu, MenuItem, MenuItemFields};
use crate::native_chat::design::{ChatDesign, ChatDesignEvent, Palette};
use crate::native_chat::session::{ClaudeChatSession, ClaudeChatSessionEvent};
use crate::util::bindings::CustomAction;
use crate::util::openable_file_type::is_supported_code_file;
use crate::workspace::WorkspaceAction;

/// SF Mono under the names macOS knows it by.
const INLINE_CODE_FONTS: [&str; 3] = [".AppleSystemUIFontMonospaced", ".SF NS Mono", "SF Mono"];
const COPIED_FEEDBACK: Duration = Duration::from_millis(1500);
const APP_ICON: &str = "bundled/png/picolas-icon.png";
const EMPTY_STATE_ICON_SIZE: f32 = 96.;
const ICON_FOLDER: &str = "bundled/svg/picolas-folder.svg";
const ICON_ARROW_DOWN: &str = "bundled/svg/picolas-arrow-down.svg";
const ICON_MENTION: &str = "bundled/svg/picolas-mention.svg";
/// Space between the line a selection's drag ended on and the "Mencionar" pill.
const MENTION_OFFSET: f32 = 6.;
const MENTION_HEIGHT: f32 = 26.;
const CONTEXT_MENU_WIDTH: f32 = 200.;
/// Space between a path chip and its tooltip.
const PATH_TOOLTIP_OFFSET: f32 = 6.;
/// Space between the "↓" button and the bottom of the transcript.
const SCROLL_BUTTON_MARGIN: f32 = 14.;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptEvent {
    /// Text the user selected and wants to mention in the next message.
    Mention(String),
    OpenArtifact(String),
    /// A text or code file to open in the editor beside the chat.
    OpenFile(PathBuf),
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
    /// Opens a file or folder named in an answer: text in the editor, images in the lightbox, the
    /// rest in its default app.
    OpenPath {
        path: PathBuf,
        kind: PathKind,
    },
    /// The pointer entered or left a path chip; `id` is the chip's saved position.
    HoverPath {
        id: String,
        path: PathBuf,
        hovered: bool,
    },
    /// Shows an image attached to a message in the lightbox, with the message's other images.
    OpenImage {
        message_id: String,
        position: usize,
    },
    ScrollToBottom,
    SelectionChanged {
        text: Option<String>,
        /// Set when a drag that made the selection just ended.
        ended_drag: Option<SelectionDragEnd>,
    },
    Mention,
    CopySelection,
    /// A right-click at this window position.
    OpenContextMenu(Vector2F),
    /// Copies the whole message of a top-level row.
    CopyRow(usize),
}

/// Where a selection's drag ended, for placing the "Mencionar" pill beside it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct MentionAnchor {
    /// The end of the drag relative to the top-left of the transcript's content, so the pill
    /// stays with the selection as the conversation scrolls.
    tail: Vector2F,
    is_reversed: bool,
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
    /// Whether each path named in an answer is a folder, or `None` when nothing is there.
    paths: HashMap<PathBuf, Option<bool>>,
}

/// The path chip under the pointer.
#[derive(Debug, Clone, PartialEq)]
struct HoveredPath {
    position_id: String,
    path: PathBuf,
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
    mention: Option<MentionAnchor>,
    context_menu: ViewHandle<Menu<TranscriptAction>>,
    /// Where the open context menu is, relative to the transcript.
    context_menu_at: Option<Vector2F>,
    hovered_path: Option<HoveredPath>,
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
        register_bindings(ctx);
        ctx.subscribe_to_model(&session, |me, _, event, ctx| {
            me.on_session_event(event, ctx)
        });
        ctx.subscribe_to_model(&ChatDesign::handle(ctx), |me, _, event, ctx| match event {
            ChatDesignEvent::Changed => {
                me.viewport.remeasure();
                ctx.notify();
            }
        });
        let context_menu = ctx.add_typed_action_view(|_| {
            Menu::new()
                .prevent_interaction_with_other_elements()
                .with_drop_shadow()
                .with_width(CONTEXT_MENU_WIDTH)
        });
        ctx.subscribe_to_view(&context_menu, |me, _, event, ctx| match event {
            MenuEvent::Close { .. } => {
                me.context_menu_at = None;
                ctx.notify();
            }
            MenuEvent::ItemSelected | MenuEvent::ItemHovered => {}
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
            mention: None,
            context_menu,
            context_menu_at: None,
            hovered_path: None,
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
                ConversationChange::ItemInserted { index, .. } => {
                    self.track_item(*index, ctx);
                    self.run_dev_hooks(*index, ctx);
                    dirty_from = Some(dirty_from.map_or(*index, |from| from.min(*index)));
                }
                ConversationChange::ItemUpdated { index, .. } => {
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

    /// For screenshots, once the first message sent in this run is in: `PICOLAS_CHAT_OPEN_FILE=<path>`
    /// opens that file beside the chat and `PICOLAS_CHAT_LIGHTBOX=1` shows the message's images in
    /// the lightbox. Debug builds only.
    fn run_dev_hooks(&mut self, index: usize, ctx: &mut ViewContext<Self>) {
        static RAN: AtomicBool = AtomicBool::new(false);
        if !cfg!(debug_assertions) {
            return;
        }
        let Some(TimelineItem::User(message)) =
            self.session.as_ref(ctx).conversation().items().get(index)
        else {
            return;
        };
        let message_id = message.id.clone();
        if RAN.swap(true, Ordering::Relaxed) {
            return;
        }
        if let Ok(path) = std::env::var("PICOLAS_CHAT_OPEN_FILE") {
            ctx.emit(TranscriptEvent::OpenFile(PathBuf::from(path)));
        }
        if std::env::var_os("PICOLAS_CHAT_LIGHTBOX").is_some() {
            self.open_image(&message_id, 0, ctx);
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

    /// `markdown` split into blocks, parsed again only when it changed, with the files and
    /// folders it names under `cwd` as chips.
    fn blocks(&self, key: &str, markdown: &str, cwd: &Path) -> Rc<Vec<Block>> {
        let mut hasher = DefaultHasher::new();
        markdown.hash(&mut hasher);
        let hash = hasher.finish();
        let mut cache = self.cache.borrow_mut();
        if let Some((cached_hash, blocks)) = cache.blocks.get(key)
            && *cached_hash == hash
        {
            return blocks.clone();
        }
        let mut blocks = markdown::split_blocks(markdown);
        let paths = &mut cache.paths;
        paths::add_chips(&mut blocks, cwd, dirs::home_dir().as_deref(), &mut |path| {
            *paths.entry(path.to_path_buf()).or_insert_with(|| {
                std::fs::metadata(path)
                    .ok()
                    .map(|metadata| metadata.is_dir())
            })
        });
        let blocks = Rc::new(blocks);
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

    fn set_mention(&mut self, mention: Option<MentionAnchor>, ctx: &mut ViewContext<Self>) {
        if self.mention != mention {
            self.mention = mention;
            ctx.notify();
        }
    }

    fn mention_selection(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(text) = self.selection.take() else {
            return;
        };
        self.selection_handle.clear();
        self.mention = None;
        ctx.emit(TranscriptEvent::Mention(text));
        ctx.notify();
    }

    /// Opens the right-click menu at `position`, in window coordinates, leaving the selection as
    /// it is.
    fn open_context_menu(&mut self, position: Vector2F, ctx: &mut ViewContext<Self>) {
        let Some(bounds) = ctx.element_position_by_id(&self.position_id) else {
            return;
        };
        let mut items: Vec<MenuItem<TranscriptAction>> = Vec::new();
        if self.selection.is_some() {
            items.push(
                MenuItemFields::new("Copiar")
                    .with_key_shortcut_label(Some("⌘C"))
                    .with_on_select_action(TranscriptAction::CopySelection)
                    .into_item(),
            );
            items.push(
                MenuItemFields::new("Mencionar")
                    .with_key_shortcut_label(Some("⌘L"))
                    .with_on_select_action(TranscriptAction::Mention)
                    .into_item(),
            );
        }
        if let Some(row) = self
            .list_state
            .item_at(position)
            .filter(|row| self.row_copy_text(*row, ctx).is_some())
        {
            items.push(
                MenuItemFields::new("Copiar mensagem")
                    .with_on_select_action(TranscriptAction::CopyRow(row))
                    .into_item(),
            );
        }
        if items.is_empty() {
            return;
        }
        self.context_menu
            .update(ctx, |menu, ctx| menu.set_items(items, ctx));
        self.context_menu_at = Some(position - bounds.origin());
        ctx.notify();
    }

    /// The text of a message row, for copying it whole.
    fn row_copy_text(&self, row: usize, app: &AppContext) -> Option<String> {
        let items = self.session.as_ref(app).conversation().items();
        match self.rows.get(row)? {
            Row::UserBubble { index } => {
                let TimelineItem::User(message) = items.get(*index)? else {
                    return None;
                };
                Some(message.text.clone())
            }
            Row::AssistantProse { index } => {
                let TimelineItem::Assistant(text) = items.get(*index)? else {
                    return None;
                };
                Some(text.markdown.clone())
            }
            Row::Thinking { .. }
            | Row::Tool { .. }
            | Row::ToolBatch(_)
            | Row::Notice { .. }
            | Row::WorkGroup(_)
            | Row::TurnError { .. } => None,
        }
    }

    fn open_path(&mut self, path: &Path, kind: PathKind, ctx: &mut ViewContext<Self>) {
        let opens_in_editor = match kind {
            PathKind::Code => true,
            PathKind::File => is_supported_code_file(path),
            PathKind::Folder
            | PathKind::Image
            | PathKind::Video
            | PathKind::Audio
            | PathKind::Pdf
            | PathKind::Archive => false,
        };
        if opens_in_editor {
            ctx.emit(TranscriptEvent::OpenFile(path.to_path_buf()));
        } else if matches!(kind, PathKind::Image) {
            let image = LightboxImage {
                source: LightboxImageSource::Resolved {
                    asset_source: AssetSource::LocalFile {
                        path: path.to_string_lossy().into_owned(),
                        content_version: None,
                    },
                },
                description: path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned()),
            };
            open_lightbox(vec![image], 0, ctx);
        } else {
            ctx.open_file_path(path);
        }
    }

    fn set_hovered_path(
        &mut self,
        id: &str,
        path: &Path,
        hovered: bool,
        ctx: &mut ViewContext<Self>,
    ) {
        let hovered_path = if hovered {
            Some(HoveredPath {
                position_id: id.to_owned(),
                path: path.to_path_buf(),
            })
        } else if self
            .hovered_path
            .as_ref()
            .is_some_and(|hovered| hovered.position_id != id)
        {
            // Leaving a chip after entering the next one keeps the next one's tooltip.
            return;
        } else {
            None
        };
        if self.hovered_path != hovered_path {
            self.hovered_path = hovered_path;
            ctx.notify();
        }
    }

    /// Shows the images attached to a message in the lightbox, starting at `position`.
    fn open_image(&mut self, message_id: &str, position: usize, ctx: &mut ViewContext<Self>) {
        let session = self.session.as_ref(ctx);
        let Some(message) = session
            .conversation()
            .items()
            .iter()
            .find_map(|item| match item {
                TimelineItem::User(message) if message.id == message_id => Some(message),
                _ => None,
            })
        else {
            return;
        };
        let count = message.images.len();
        let mut images = Vec::new();
        let mut start = 0;
        for (index, image) in message.images.iter().enumerate() {
            let asset_source = match &image.path {
                Some(path) => AssetSource::LocalFile {
                    path: path.clone(),
                    content_version: None,
                },
                None => match self.image_asset_id(&message.id, index) {
                    Some(id) => AssetSource::Raw { id },
                    None => continue,
                },
            };
            if index == position {
                start = images.len();
            }
            let name = image
                .path
                .as_deref()
                .and_then(|path| Path::new(path).file_name())
                .map(|name| name.to_string_lossy().into_owned());
            images.push(LightboxImage {
                source: LightboxImageSource::Resolved { asset_source },
                description: lightbox_description(name.as_deref(), index, count),
            });
        }
        if !images.is_empty() {
            open_lightbox(images, start, ctx);
        }
    }

    /// Copies the selected text, if any.
    pub fn copy_selection(&mut self, ctx: &mut ViewContext<Self>) {
        if let Some(selection) = self.selection.clone() {
            ctx.clipboard()
                .write(ClipboardContent::plain_text(selection));
        }
    }

    fn on_selection_changed(
        &mut self,
        text: Option<String>,
        ended_drag: Option<SelectionDragEnd>,
        ctx: &mut ViewContext<Self>,
    ) {
        self.selection = text
            .map(|text| paths::strip_chip_room(&text))
            .filter(|text| !text.trim().is_empty());
        if self.selection.is_none() {
            self.set_mention(None, ctx);
            return;
        }
        // A mouse-up that didn't end a drag here (a click in the composer, say) keeps the pill and
        // the focus where they are.
        let Some(ended_drag) = ended_drag else {
            return;
        };
        if let Some(bounds) = ctx.element_position_by_id(&self.position_id) {
            let tail = ended_drag.tail - bounds.origin() + vec2f(0., self.viewport.scroll_start());
            self.set_mention(
                Some(MentionAnchor {
                    tail,
                    is_reversed: ended_drag.is_reversed,
                }),
                ctx,
            );
        }
        // So ⌘C and ⌘L reach the transcript's bindings.
        ctx.focus_self();
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

/// Registers the transcript's key bindings the first time one is created. Binding the menu's
/// actions, rather than handling the keys, is what makes ⌘C and ⌘L work: macOS sends them to the
/// Edit menu before the view sees them.
fn register_bindings(app: &mut AppContext) {
    static REGISTERED: AtomicBool = AtomicBool::new(false);
    if REGISTERED.swap(true, Ordering::Relaxed) {
        return;
    }
    use warpui::keymap::macros::*;

    let context = id!(TranscriptView::ui_name()) & !id!("IMEOpen");
    app.register_fixed_bindings([
        FixedBinding::custom(
            CustomAction::Copy,
            TranscriptAction::CopySelection,
            "Copiar",
            context.clone(),
        ),
        FixedBinding::custom(
            CustomAction::FocusInput,
            TranscriptAction::Mention,
            "Mencionar seleção",
            context,
        ),
    ]);
}

/// Where the "Mencionar" pill goes for a selection whose drag ended at `tail`, relative to the
/// transcript, and which of its corners goes there: beside the line the drag ended on, on the side
/// away from the selected text — below it when the drag went forward, above it when it went
/// backward, or along the line when there's no room.
fn mention_placement(
    tail: Vector2F,
    is_reversed: bool,
    line_height: f32,
    visible_height: f32,
) -> (Vector2F, ChildAnchor) {
    let half_line = line_height / 2.;
    if is_reversed {
        let above = tail.y() - half_line - MENTION_OFFSET;
        if above - MENTION_HEIGHT >= 0. {
            (vec2f(tail.x(), above), ChildAnchor::BottomLeft)
        } else {
            (
                vec2f(tail.x() - MENTION_OFFSET, tail.y()),
                ChildAnchor::MiddleRight,
            )
        }
    } else {
        let below = tail.y() + half_line + MENTION_OFFSET;
        if below + MENTION_HEIGHT <= visible_height {
            (vec2f(tail.x(), below), ChildAnchor::TopRight)
        } else {
            (
                vec2f(tail.x() + MENTION_OFFSET, tail.y()),
                ChildAnchor::MiddleLeft,
            )
        }
    }
}

fn image_asset_id(message_id: &str, position: usize) -> String {
    format!("native_chat_image:{message_id}:{position}")
}

/// Shows `images` in the window's lightbox, starting at `index`.
fn open_lightbox(images: Vec<LightboxImage>, index: usize, ctx: &mut ViewContext<TranscriptView>) {
    ctx.dispatch_typed_action_deferred(WorkspaceAction::OpenLightbox {
        images,
        initial_index: index,
    });
}

/// The line under an image in the lightbox: its file name, if it has one, and where it sits among
/// the message's images when there are several.
fn lightbox_description(name: Option<&str>, position: usize, count: usize) -> Option<String> {
    let counter = (count > 1).then(|| format!("{} / {count}", position + 1));
    match (name, counter) {
        (Some(name), Some(counter)) => Some(format!("{name}  ·  {counter}")),
        (Some(name), None) => Some(name.to_owned()),
        (None, counter) => counter,
    }
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
        let list_state = self.list_state.clone();
        let viewport = self.viewport.clone();
        let selectable = SelectableArea::new(
            self.selection_handle.clone(),
            |args, ctx, _| {
                ctx.dispatch_typed_action(TranscriptAction::SelectionChanged {
                    text: args.selection,
                    ended_drag: args.ended_drag,
                })
            },
            scroller,
        )
        .with_autoscroll(move |step, _| {
            if step < 0. {
                viewport.stick_to_bottom.set(false);
            }
            list_state.scroll_by(step.into_pixels());
        })
        .finish();
        // ⌘L also mentions from the composer, whose editor doesn't bind it.
        let events = EventHandler::new(selectable)
            .on_right_mouse_down(|ctx, _, position, _| {
                ctx.dispatch_typed_action(TranscriptAction::OpenContextMenu(position));
                DispatchEventResult::StopPropagation
            })
            .on_keydown(|ctx, _, keystroke| {
                if keystroke.cmd && !keystroke.ctrl && !keystroke.alt && keystroke.key == "l" {
                    ctx.dispatch_typed_action(TranscriptAction::Mention);
                    return DispatchEventResult::StopPropagation;
                }
                DispatchEventResult::PropagateToParent
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
        if let Some(mention) = self.mention.filter(|_| self.selection.is_some()) {
            let scroll_start = self.viewport.scroll_start();
            let visible_height = self.viewport.visible_height();
            let tail = mention.tail - vec2f(0., scroll_start);
            // Hidden while the end of the selection is scrolled out of view.
            if (0.0..=visible_height).contains(&tail.y()) {
                let (offset, child_anchor) = mention_placement(
                    tail,
                    mention.is_reversed,
                    style.sizes.font_size * style.sizes.line_height_ratio,
                    visible_height,
                );
                stack.add_positioned_overlay_child(
                    ScrollFollow::new(
                        self.render_mention_pill(&style),
                        self.viewport.clone(),
                        scroll_start,
                    )
                    .finish(),
                    OffsetPositioning::offset_from_parent(
                        offset,
                        ParentOffsetBounds::ParentByPosition,
                        ParentAnchor::TopLeft,
                        child_anchor,
                    ),
                );
            }
        }
        if let Some(hovered) = &self.hovered_path {
            stack.add_positioned_overlay_child(
                chips::path_tooltip(&style, &hovered.path),
                OffsetPositioning::offset_from_save_position_element(
                    hovered.position_id.clone(),
                    vec2f(0., -PATH_TOOLTIP_OFFSET),
                    PositionedElementOffsetBounds::WindowByPosition,
                    PositionedElementAnchor::TopLeft,
                    ChildAnchor::BottomLeft,
                ),
            );
        }
        if let Some(at) = self.context_menu_at {
            stack.add_positioned_overlay_child(
                ChildView::new(&self.context_menu).finish(),
                OffsetPositioning::offset_from_parent(
                    at,
                    ParentOffsetBounds::WindowByPosition,
                    ParentAnchor::TopLeft,
                    ChildAnchor::TopLeft,
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
            TranscriptAction::OpenPath { path, kind } => self.open_path(path, *kind, ctx),
            TranscriptAction::HoverPath { id, path, hovered } => {
                self.set_hovered_path(id, path, *hovered, ctx);
            }
            TranscriptAction::OpenImage {
                message_id,
                position,
            } => self.open_image(message_id, *position, ctx),
            TranscriptAction::OpenArtifact(id) => {
                ctx.emit(TranscriptEvent::OpenArtifact(id.clone()));
            }
            TranscriptAction::ScrollToBottom => {
                self.viewport.stick_to_bottom.set(true);
                ctx.notify();
            }
            TranscriptAction::SelectionChanged { text, ended_drag } => {
                self.on_selection_changed(text.clone(), *ended_drag, ctx);
            }
            TranscriptAction::Mention => self.mention_selection(ctx),
            TranscriptAction::CopySelection => self.copy_selection(ctx),
            TranscriptAction::OpenContextMenu(position) => self.open_context_menu(*position, ctx),
            TranscriptAction::CopyRow(row) => {
                if let Some(text) = self.row_copy_text(*row, ctx) {
                    ctx.clipboard().write(ClipboardContent::plain_text(text));
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "transcript_tests.rs"]
mod tests;
