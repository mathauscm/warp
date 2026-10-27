//! The "Artefatos" pane: a gallery of the HTML artifacts Claude created, and an interactive view of
//! the one that's open (a native web view laid over the pane, see [`WebViewSlot`]).

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use chrono::{Local, Utc};
use claude_chat::artifacts::{ArtifactKind, ArtifactMeta, ArtifactStore, kind_label_pt};
use instant::Instant;
use notify_debouncer_full::notify::{RecursiveMode, WatchFilter};
use pathfinder_color::ColorU;
use warpui::assets::asset_cache::AssetSource;
use warpui::r#async::Timer;
use warpui::elements::{
    Align, Border, ChildAnchor, Clipped, ClippedScrollStateHandle, ClippedScrollable,
    ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, Dismiss, DropShadow, Element,
    Expanded, Fill, Flex, Hoverable, Icon as SvgIcon, Image, MainAxisAlignment, MainAxisSize,
    MouseStateHandle, OffsetPositioning, ParentElement, PositionedElementAnchor,
    PositionedElementOffsetBounds, Radius, Rect, SavePosition, ScrollbarWidth, Shrinkable, Stack,
    Text,
};
use warpui::fonts::{Properties, Weight};
use warpui::geometry::vector::vec2f;
use warpui::image_cache::CacheOption;
use warpui::platform::Cursor;
use warpui::text_layout::ClipConfig;
use warpui::{
    AppContext, Entity, FocusContext, ModelHandle, SingletonEntity, TypedActionView, View,
    ViewContext,
};
use watcher::{BulkFilesystemWatcher, BulkFilesystemWatcherEvent};

use super::artifacts::grid::{Grid, MaxWidthCentered};
use super::artifacts::helpers::{
    CREATABLE_KINDS, available_filters, card_caption_pt, file_url, kind_plural_pt, matches_filter,
    needs_thumbnail,
};
use super::artifacts::web_view_slot::{WebViewHost, WebViewSlot};
use super::design::{self, ChatDesign, ChatDesignEvent, Palette, Sizes};
use super::session::artifacts_root;
use crate::pane_group::focus_state::PaneFocusHandle;
use crate::pane_group::pane::view::{self, HeaderContent, StandardHeader, StandardHeaderOptions};
use crate::pane_group::{BackingView, PaneConfiguration, PaneEvent};
use crate::workspace::WorkspaceAction;

const TITLE: &str = "Artefatos";
const WATCH_DEBOUNCE: Duration = Duration::from_millis(250);
/// Time given to a page to load and settle before its thumbnail is captured.
const SNAPSHOT_DELAY: Duration = Duration::from_millis(1500);
const MENU_WIDTH: f32 = 210.;

const ICON_BACK: &str = "bundled/svg/picolas-arrow-left.svg";
const ICON_RELOAD: &str = "bundled/svg/picolas-reload.svg";
const ICON_EXTERNAL: &str = "bundled/svg/picolas-external.svg";
const ICON_FOLDER: &str = "bundled/svg/picolas-folder.svg";
const ICON_DOTS: &str = "bundled/svg/picolas-dots.svg";
const ICON_TRASH: &str = "bundled/svg/picolas-trash.svg";
const ICON_OPEN: &str = "bundled/svg/picolas-expand.svg";
const PATTERN_DOTS: &str = "bundled/svg/picolas-pattern-dots.svg";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactsViewEvent {
    Pane(PaneEvent),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactsViewAction {
    SetFilter(Option<ArtifactKind>),
    Open(String),
    Back,
    Reload,
    OpenInBrowser(String),
    RevealInFinder(String),
    ToggleMenu(String),
    CloseMenu,
    AskDelete(String),
    CancelDelete,
    ConfirmDelete(String),
}

struct Card {
    meta: ArtifactMeta,
    /// `None` when the artifact has no thumbnail yet.
    thumbnail: Option<AssetSource>,
}

/// The artifact shown instead of the gallery.
struct Viewer {
    id: String,
    /// `None` when the artifact no longer exists.
    meta: Option<ArtifactMeta>,
    /// When to capture the page as the artifact's thumbnail, if it needs one.
    snapshot_due: Option<Instant>,
    /// When the shown page was written, to reload only when it really changes.
    loaded_html_at: Option<SystemTime>,
}

pub struct ArtifactsView {
    store: ArtifactStore,
    cards: Vec<Card>,
    filter: Option<ArtifactKind>,
    viewer: Option<Viewer>,
    menu_open_for: Option<String>,
    pending_delete: Option<String>,
    mouse_states: RefCell<HashMap<String, MouseStateHandle>>,
    scroll_state: ClippedScrollStateHandle,
    web_view: Rc<WebViewHost>,
    slot_position_id: String,
    /// Wakes the view after each drawn frame while the viewer is open; see [`Self::watch_next_frame`].
    frame_tx: async_channel::Sender<()>,
    is_watching_frame: bool,
    _watcher: ModelHandle<BulkFilesystemWatcher>,
    pane_configuration: ModelHandle<PaneConfiguration>,
    focus_handle: Option<PaneFocusHandle>,
}

impl ArtifactsView {
    pub fn new(open_artifact: Option<String>, ctx: &mut ViewContext<Self>) -> Self {
        let pane_configuration = ctx.add_model(|_| PaneConfiguration::new(TITLE));
        ctx.subscribe_to_model(&ChatDesign::handle(ctx), |_, _, event, ctx| match event {
            ChatDesignEvent::Changed => ctx.notify(),
        });

        let store = ArtifactStore::new(artifacts_root());
        if let Err(err) = std::fs::create_dir_all(store.root()) {
            log::warn!("Can't create the artifacts folder: {err}");
        }
        let watcher = ctx.add_model(|ctx| BulkFilesystemWatcher::new(WATCH_DEBOUNCE, ctx));
        let registration = watcher.update(ctx, |watcher, _| {
            watcher.register_path(
                store.root(),
                WatchFilter::accept_all(),
                RecursiveMode::Recursive,
            )
        });
        ctx.background_executor()
            .spawn(async move {
                if let Err(err) = registration.await {
                    log::warn!("Can't watch the artifacts folder: {err:#}");
                }
            })
            .detach();
        ctx.subscribe_to_model(&watcher, |me, _, event, ctx| {
            me.on_files_changed(event, ctx)
        });

        let (frame_tx, frame_rx) = async_channel::unbounded();
        ctx.spawn_stream_local(frame_rx, |me, (), ctx| me.on_frame_drawn(ctx), |_, _| {});

        let mut view = Self {
            store,
            cards: Vec::new(),
            filter: None,
            viewer: None,
            menu_open_for: None,
            pending_delete: None,
            mouse_states: Default::default(),
            scroll_state: Default::default(),
            web_view: Default::default(),
            slot_position_id: format!("artifact-web-view:{}", ctx.view_id()),
            frame_tx,
            is_watching_frame: false,
            _watcher: watcher,
            pane_configuration,
            focus_handle: None,
        };
        view.reload_cards();
        if let Some(id) = open_artifact {
            view.open(id, ctx);
        }
        view
    }

    pub fn pane_configuration(&self) -> ModelHandle<PaneConfiguration> {
        self.pane_configuration.clone()
    }

    pub fn open_artifact(&self) -> Option<&str> {
        self.viewer.as_ref().map(|viewer| viewer.id.as_str())
    }

    pub fn focus(&mut self, ctx: &mut ViewContext<Self>) {
        ctx.focus_self();
    }

    fn mouse_state(&self, key: impl Into<String>) -> MouseStateHandle {
        self.mouse_states
            .borrow_mut()
            .entry(key.into())
            .or_default()
            .clone()
    }

    fn reload_cards(&mut self) {
        self.cards = self
            .store
            .list()
            .into_iter()
            .map(|meta| {
                let thumbnail = self
                    .store
                    .thumbnail_path(&meta.id)
                    .ok()
                    .filter(|path| path.is_file())
                    .map(|path| {
                        AssetSource::LocalFile {
                            path: path.to_string_lossy().into_owned(),
                            content_version: None,
                        }
                        .with_local_file_content_version()
                    });
                Card { meta, thumbnail }
            })
            .collect();
        if let Some(filter) = self.filter
            && !self.cards.iter().any(|card| card.meta.kind == filter)
        {
            self.filter = None;
        }
    }

    fn set_title(&self, title: String, ctx: &mut ViewContext<Self>) {
        self.pane_configuration.update(ctx, |configuration, ctx| {
            configuration.set_title(title, ctx)
        });
    }

    pub fn open(&mut self, id: String, ctx: &mut ViewContext<Self>) {
        let meta = self.store.get(&id);
        self.set_title(
            meta.as_ref()
                .map_or_else(|| TITLE.to_owned(), |meta| meta.title.clone()),
            ctx,
        );
        self.menu_open_for = None;
        self.pending_delete = None;
        self.web_view.set(None);
        self.viewer = Some(Viewer {
            id,
            meta,
            snapshot_due: None,
            loaded_html_at: None,
        });
        self.ensure_web_view(ctx);
        self.watch_next_frame(ctx);
        ctx.notify();
    }

    fn back(&mut self, ctx: &mut ViewContext<Self>) {
        self.web_view.with(|web_view| web_view.release_focus());
        self.web_view.set(None);
        self.viewer = None;
        self.set_title(TITLE.to_owned(), ctx);
        self.reload_cards();
        ctx.focus_self();
        ctx.notify();
    }

    /// Creates the web view for the open artifact once the window can host it.
    fn ensure_web_view(&mut self, ctx: &mut ViewContext<Self>) {
        if self.web_view.has_web_view() {
            return;
        }
        let Some(viewer) = &self.viewer else {
            return;
        };
        if viewer.meta.is_none() {
            return;
        }
        let (Ok(html_path), Some(web_view)) =
            (self.store.html_path(&viewer.id), create_web_view(ctx))
        else {
            return;
        };
        if let Some(dir) = html_path.parent() {
            web_view.load_file(&html_path, dir);
        }
        log::debug!("Created the web view for artifact {}", viewer.id);
        let loaded_html_at = modified_at(&html_path);
        self.web_view.set(Some(web_view));
        if let Some(viewer) = &mut self.viewer {
            viewer.loaded_html_at = loaded_html_at;
        }
        self.schedule_snapshot(ctx);
    }

    fn schedule_snapshot(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(viewer) = &mut self.viewer else {
            return;
        };
        viewer.snapshot_due = Some(Instant::now() + SNAPSHOT_DELAY);
        ctx.spawn(Timer::after(SNAPSHOT_DELAY), |me, _, _| {
            me.capture_thumbnail_if_due()
        });
    }

    /// Saves the page as the artifact's thumbnail when the old one is missing or stale. Waits
    /// while the web view is hidden, since a hidden view captures blank.
    fn capture_thumbnail_if_due(&mut self) {
        let Some(viewer) = &mut self.viewer else {
            return;
        };
        if viewer
            .snapshot_due
            .is_none_or(|due| Instant::now() < due || !self.web_view.is_visible())
        {
            return;
        }
        viewer.snapshot_due = None;
        let (Ok(thumbnail), Ok(html)) = (
            self.store.thumbnail_path(&viewer.id),
            self.store.html_path(&viewer.id),
        ) else {
            return;
        };
        if !needs_thumbnail(modified_at(&thumbnail), modified_at(&html)) {
            return;
        }
        self.web_view.with(|web_view| {
            web_view.snapshot_png(&thumbnail, |result| {
                if let Err(err) = result {
                    log::warn!("Can't save the artifact thumbnail: {err:#}");
                }
            })
        });
    }

    /// Hooks the next drawn frame of the window, to show the web view only while its placeholder
    /// is on screen and to keep checking for a thumbnail to capture.
    fn watch_next_frame(&mut self, ctx: &mut ViewContext<Self>) {
        if self.is_watching_frame {
            return;
        }
        self.is_watching_frame = true;
        let frame_tx = self.frame_tx.clone();
        ctx.on_next_frame_drawn(move || {
            let _ = frame_tx.try_send(());
        });
    }

    fn on_frame_drawn(&mut self, ctx: &mut ViewContext<Self>) {
        self.is_watching_frame = false;
        if self.viewer.is_none() {
            return;
        }
        if !self.web_view.has_web_view() {
            self.ensure_web_view(ctx);
            if self.web_view.has_web_view() {
                ctx.notify();
            }
        }
        let in_scene = ctx
            .element_position_by_id_at_last_frame(ctx.window_id(), &self.slot_position_id)
            .is_some();
        self.web_view.set_in_scene(in_scene);
        self.capture_thumbnail_if_due();
        self.watch_next_frame(ctx);
    }

    fn on_files_changed(
        &mut self,
        event: &BulkFilesystemWatcherEvent,
        ctx: &mut ViewContext<Self>,
    ) {
        let touched: Vec<&PathBuf> = event
            .added
            .iter()
            .chain(&event.modified)
            .chain(&event.deleted)
            .chain(event.moved.iter().flat_map(|(to, from)| [to, from]))
            .collect();
        if touched.is_empty() {
            return;
        }
        self.reload_cards();

        let open_html = self
            .viewer
            .as_ref()
            .and_then(|viewer| self.store.html_path(&viewer.id).ok());
        if let Some(open_html) = open_html
            && touched.iter().any(|path| **path == open_html)
            && let Some(viewer) = &mut self.viewer
            && modified_at(&open_html) != viewer.loaded_html_at
        {
            log::debug!("Reloading artifact {} after its page changed", viewer.id);
            viewer.loaded_html_at = modified_at(&open_html);
            viewer.meta = self.store.get(&viewer.id);
            let title = viewer.meta.as_ref().map(|meta| meta.title.clone());
            if let Some(title) = title {
                self.set_title(title, ctx);
            }
            self.web_view.with(|web_view| web_view.reload());
            self.schedule_snapshot(ctx);
        }
        ctx.notify();
    }

    fn delete(&mut self, id: &str, ctx: &mut ViewContext<Self>) {
        if let Err(err) = self.store.delete(id) {
            log::warn!("Can't delete artifact {id}: {err:#}");
        }
        self.pending_delete = None;
        self.reload_cards();
        ctx.notify();
    }

    fn html_path(&self, id: &str) -> Option<PathBuf> {
        self.store.html_path(id).ok().filter(|path| path.is_file())
    }
}

#[cfg(target_os = "macos")]
fn create_web_view(ctx: &mut ViewContext<ArtifactsView>) -> Option<picolas_webview::WebView> {
    let parent = ctx
        .windows()
        .platform_window(ctx.window_id())
        .and_then(|window| {
            window
                .as_any()
                .downcast_ref::<warpui::platform::mac::Window>()?
                .content_view()
        })?;
    picolas_webview::WebView::new(&parent)
        .inspect_err(|err| log::warn!("Can't create the artifact web view: {err:#}"))
        .ok()
}

#[cfg(not(target_os = "macos"))]
fn create_web_view(_ctx: &mut ViewContext<ArtifactsView>) -> Option<picolas_webview::WebView> {
    None
}

fn modified_at(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
}

impl Drop for ArtifactsView {
    fn drop(&mut self) {
        self.web_view.set(None);
    }
}

impl Entity for ArtifactsView {
    type Event = ArtifactsViewEvent;
}

/// What every render helper needs.
#[derive(Clone, Copy)]
struct Look<'a> {
    palette: &'a Palette,
    sizes: &'a Sizes,
    fonts: design::ChatFonts,
}

impl View for ArtifactsView {
    fn ui_name() -> &'static str {
        "ArtifactsView"
    }

    fn on_focus(&mut self, _focus_ctx: &FocusContext, ctx: &mut ViewContext<Self>) {
        if self.viewer.is_none() {
            self.reload_cards();
            ctx.notify();
        }
    }

    fn on_window_transferred(
        &mut self,
        _source_window_id: warpui::WindowId,
        _target_window_id: warpui::WindowId,
        ctx: &mut ViewContext<Self>,
    ) {
        // The web view belongs to the old window's view hierarchy; a new one is made for the new
        // window after its next frame.
        self.web_view.set(None);
        self.is_watching_frame = false;
        if self.viewer.is_some() {
            self.watch_next_frame(ctx);
        }
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let design = ChatDesign::as_ref(app);
        let look = Look {
            palette: design.palette(app),
            sizes: design.sizes(),
            fonts: design::fonts(app),
        };
        let content = match &self.viewer {
            Some(viewer) => self.render_viewer(viewer, look, app),
            None => self.render_gallery(look),
        };
        Container::new(content)
            .with_background_color(look.palette.page.0)
            .finish()
    }
}

// --- Gallery ---

impl ArtifactsView {
    fn render_gallery(&self, look: Look<'_>) -> Box<dyn Element> {
        let Look { palette, sizes, .. } = look;
        let mut column = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(self.render_gallery_title(look))
            .with_child(
                Container::new(section_label("Crie algo novo", look))
                    .with_margin_top(28.)
                    .with_margin_bottom(12.)
                    .finish(),
            )
            .with_child(
                Grid::new(
                    CREATABLE_KINDS
                        .into_iter()
                        .map(|kind| self.render_new_tile(kind, look))
                        .collect(),
                    sizes.gallery_tile_size,
                    sizes.gallery_gap,
                )
                .with_max_columns(CREATABLE_KINDS.len())
                .finish(),
            );

        if self.cards.is_empty() {
            column.add_child(
                Container::new(render_empty_state(look))
                    .with_margin_top(40.)
                    .finish(),
            );
        } else {
            column.add_child(
                Container::new(self.render_filters(look))
                    .with_margin_top(40.)
                    .with_margin_bottom(18.)
                    .finish(),
            );
            let now = Utc::now();
            let offset = *Local::now().offset();
            let cards = self
                .cards
                .iter()
                .filter(|card| matches_filter(&card.meta, self.filter))
                .map(|card| self.render_card(card, card_caption_pt(&card.meta, now, offset), look))
                .collect();
            column
                .add_child(Grid::new(cards, sizes.gallery_card_width, sizes.gallery_gap).finish());
        }

        let body = Container::new(
            MaxWidthCentered::new(column.finish(), sizes.gallery_max_width).finish(),
        )
        .with_horizontal_padding(sizes.gallery_padding)
        .with_padding_top(sizes.gallery_padding)
        .with_padding_bottom(sizes.gallery_padding + 16.)
        .finish();
        let scrollable = ClippedScrollable::vertical(
            self.scroll_state.clone(),
            body,
            ScrollbarWidth::Custom(6.),
            Fill::Solid(palette.border_strong.0),
            Fill::Solid(palette.text_faint.0),
            Fill::None,
        )
        .with_overlayed_scrollbar()
        .finish();

        let mut stack = Stack::new().with_child(scrollable);
        if let Some(id) = &self.menu_open_for {
            stack.add_positioned_overlay_child(
                self.render_card_menu(id, look),
                OffsetPositioning::offset_from_save_position_element(
                    menu_button_position_id(id),
                    vec2f(0., 6.),
                    PositionedElementOffsetBounds::WindowByPosition,
                    PositionedElementAnchor::BottomRight,
                    ChildAnchor::TopRight,
                ),
            );
        }
        stack.finish()
    }

    fn render_gallery_title(&self, look: Look<'_>) -> Box<dyn Element> {
        let Look {
            palette,
            sizes,
            fonts,
        } = look;
        let count = match self.cards.len() {
            0 => String::new(),
            1 => "1 artefato".to_owned(),
            count => format!("{count} artefatos"),
        };
        Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
            .with_cross_axis_alignment(CrossAxisAlignment::End)
            .with_child(
                Text::new_inline(TITLE, fonts.ui, sizes.gallery_title_font_size)
                    .with_style(Properties::default().weight(Weight::Semibold))
                    .with_color(palette.text.0)
                    .finish(),
            )
            .with_child(
                Container::new(
                    Text::new_inline(count, fonts.ui, sizes.small_font_size)
                        .with_color(palette.text_faint.0)
                        .finish(),
                )
                .with_padding_bottom(5.)
                .finish(),
            )
            .finish()
    }

    /// A colored "new" tile that starts a chat with the kind's starter prompt.
    fn render_new_tile(&self, kind: ArtifactKind, look: Look<'_>) -> Box<dyn Element> {
        let Look {
            palette,
            sizes,
            fonts,
        } = look;
        let color = kind_color(kind, palette);
        let text = palette.text.0;
        let muted = palette.text_muted.0;
        let radius = sizes.card_radius;
        let tile_height = sizes.gallery_tile_height;
        let icon_size = sizes.icon_size * 1.75;
        let label_size = sizes.small_font_size + 0.5;
        let badge_size = sizes.tiny_font_size - 1.;
        let badge = (kind == ArtifactKind::Motion).then_some("Beta");
        Hoverable::new(
            self.mouse_state(format!("tile:{}", kind.as_str())),
            move |state| {
                let hovered = state.is_hovered();
                let start = if hovered {
                    mix(color, ColorU::white(), 0.12)
                } else {
                    color
                };
                let end = mix(start, ColorU::black(), 0.28);
                let mut art = Stack::new()
                    .with_child(
                        Rect::new()
                            .with_background_gradient(vec2f(0., 0.), vec2f(1., 1.), start, end)
                            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(radius)))
                            .finish(),
                    )
                    .with_child(
                        SvgIcon::new(PATTERN_DOTS, with_alpha(ColorU::white(), 0x2a)).finish(),
                    )
                    .with_child(
                        Align::new(
                            ConstrainedBox::new(
                                SvgIcon::new(kind_icon(kind), ColorU::white()).finish(),
                            )
                            .with_width(icon_size)
                            .with_height(icon_size)
                            .finish(),
                        )
                        .finish(),
                    );
                if let Some(badge) = badge {
                    art.add_child(
                        Align::new(
                            Container::new(
                                Text::new_inline(badge, fonts.ui, badge_size)
                                    .with_style(Properties::default().weight(Weight::Semibold))
                                    .with_color(ColorU::white())
                                    .finish(),
                            )
                            .with_horizontal_padding(6.)
                            .with_vertical_padding(1.5)
                            .with_background_color(with_alpha(ColorU::black(), 0x40))
                            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(6.)))
                            .with_margin_top(8.)
                            .with_margin_right(8.)
                            .finish(),
                        )
                        .top_right()
                        .finish(),
                    );
                }
                Flex::column()
                    .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
                    .with_child(
                        ConstrainedBox::new(art.finish())
                            .with_height(tile_height)
                            .finish(),
                    )
                    .with_child(
                        Container::new(
                            Text::new_inline(kind_label_pt(kind), fonts.ui, label_size)
                                .with_color(if hovered { text } else { muted })
                                .finish(),
                        )
                        .with_margin_top(8.)
                        .finish(),
                    )
                    .finish()
            },
        )
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(WorkspaceAction::OpenClaudeChat {
                cwd: dirs::home_dir().unwrap_or_default(),
                session_id: None,
                draft: None,
                artifact_kind: Some(kind.as_str().to_owned()),
                group: None,
            });
        })
        .finish()
    }

    fn render_filters(&self, look: Look<'_>) -> Box<dyn Element> {
        let artifacts: Vec<ArtifactMeta> =
            self.cards.iter().map(|card| card.meta.clone()).collect();
        let mut row = Flex::row().with_spacing(6.);
        row.add_child(self.render_filter_chip(None, "Todos", artifacts.len(), look));
        for kind in available_filters(&artifacts) {
            let count = artifacts.iter().filter(|meta| meta.kind == kind).count();
            row.add_child(self.render_filter_chip(Some(kind), kind_plural_pt(kind), count, look));
        }
        row.finish()
    }

    fn render_filter_chip(
        &self,
        kind: Option<ArtifactKind>,
        label: &'static str,
        count: usize,
        look: Look<'_>,
    ) -> Box<dyn Element> {
        let Look {
            palette,
            sizes,
            fonts,
        } = look;
        let selected = self.filter == kind;
        let key = kind.map_or("all", ArtifactKind::as_str);
        let palette = palette.clone();
        let font_size = sizes.small_font_size;
        let height = sizes.chip_height + 2.;
        Hoverable::new(self.mouse_state(format!("filter:{key}")), move |state| {
            let (background, color) = if selected {
                (Some(palette.surface_strong.0), palette.text.0)
            } else if state.is_hovered() {
                (Some(palette.surface_hover.0), palette.text.0)
            } else {
                (None, palette.text_muted.0)
            };
            let content = Flex::row()
                .with_spacing(6.)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(
                    Text::new_inline(label, fonts.ui, font_size)
                        .with_style(Properties::default().weight(Weight::Medium))
                        .with_color(color)
                        .finish(),
                )
                .with_child(
                    Text::new_inline(count.to_string(), fonts.ui, font_size - 1.)
                        .with_color(palette.text_faint.0)
                        .finish(),
                )
                .finish();
            let mut chip = Container::new(
                ConstrainedBox::new(Align::new(content).finish())
                    .with_height(height)
                    .finish(),
            )
            .with_horizontal_padding(12.)
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(height / 2.)));
            if let Some(background) = background {
                chip = chip.with_background_color(background);
            }
            chip.finish()
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(ArtifactsViewAction::SetFilter(kind));
        })
        .finish()
    }

    fn render_card(&self, card: &Card, caption: String, look: Look<'_>) -> Box<dyn Element> {
        let Look { palette, sizes, .. } = look;
        let id = card.meta.id.clone();
        let is_menu_open = self.menu_open_for.as_deref() == Some(id.as_str());
        let is_confirming = self.pending_delete.as_deref() == Some(id.as_str());
        let thumbnail = render_thumbnail(card, look);
        let menu_button = SavePosition::new(
            render_icon_button(
                ICON_DOTS,
                self.mouse_state(format!("card-menu:{id}")),
                ArtifactsViewAction::ToggleMenu(id.clone()),
                is_menu_open,
                look,
            ),
            &menu_button_position_id(&id),
        )
        .finish();
        let footer = if is_confirming {
            self.render_delete_confirmation(&id, look)
        } else {
            render_card_footer(&card.meta, caption, menu_button, look)
        };
        Hoverable::new(self.mouse_state(format!("card:{id}")), |state| {
            let highlighted = state.is_hovered() || is_menu_open;
            let mut container = Container::new(
                Flex::column()
                    .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
                    .with_child(Expanded::new(1., thumbnail).finish())
                    .with_child(footer)
                    .finish(),
            )
            .with_background_color(palette.surface.0)
            .with_border(Border::all(1.).with_border_color(if highlighted {
                palette.border_strong.0
            } else {
                palette.border.0
            }))
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(sizes.card_radius)));
            if highlighted {
                container = container.with_drop_shadow(DropShadow {
                    color: palette.shadow.0,
                    offset: vec2f(0., 4.),
                    blur_radius: sizes.shadow_blur,
                    spread_radius: 0.,
                });
            }
            ConstrainedBox::new(container.finish())
                .with_height(sizes.gallery_card_height)
                .finish()
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(ArtifactsViewAction::Open(id.clone()));
        })
        .with_defer_events_to_children()
        .finish()
    }

    fn render_delete_confirmation(&self, id: &str, look: Look<'_>) -> Box<dyn Element> {
        let Look {
            palette,
            sizes,
            fonts,
        } = look;
        Container::new(
            Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(6.)
                .with_child(
                    Expanded::new(
                        1.,
                        Text::new_inline("Excluir este artefato?", fonts.ui, sizes.small_font_size)
                            .with_style(Properties::default().weight(Weight::Medium))
                            .with_color(palette.text.0)
                            .with_clip(ClipConfig::end())
                            .finish(),
                    )
                    .finish(),
                )
                .with_child(render_text_button(
                    "Cancelar",
                    self.mouse_state(format!("delete-cancel:{id}")),
                    ArtifactsViewAction::CancelDelete,
                    ButtonStyle::Neutral,
                    look,
                ))
                .with_child(render_text_button(
                    "Excluir",
                    self.mouse_state(format!("delete-confirm:{id}")),
                    ArtifactsViewAction::ConfirmDelete(id.to_owned()),
                    ButtonStyle::Danger,
                    look,
                ))
                .finish(),
        )
        .with_horizontal_padding(12.)
        .with_vertical_padding(13.)
        .finish()
    }

    fn render_card_menu(&self, id: &str, look: Look<'_>) -> Box<dyn Element> {
        let Look { palette, sizes, .. } = look;
        let items = [
            (
                "Abrir",
                ICON_OPEN,
                ArtifactsViewAction::Open(id.to_owned()),
                false,
            ),
            (
                "Abrir no navegador",
                ICON_EXTERNAL,
                ArtifactsViewAction::OpenInBrowser(id.to_owned()),
                false,
            ),
            (
                "Mostrar no Finder",
                ICON_FOLDER,
                ArtifactsViewAction::RevealInFinder(id.to_owned()),
                false,
            ),
            (
                "Excluir",
                ICON_TRASH,
                ArtifactsViewAction::AskDelete(id.to_owned()),
                true,
            ),
        ];
        let mut column = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);
        for (index, (label, icon, action, danger)) in items.into_iter().enumerate() {
            if danger {
                column.add_child(
                    Container::new(
                        ConstrainedBox::new(
                            Rect::new().with_background_color(palette.border.0).finish(),
                        )
                        .with_height(1.)
                        .finish(),
                    )
                    .with_vertical_margin(4.)
                    .finish(),
                );
            }
            column.add_child(render_menu_row(
                label,
                icon,
                self.mouse_state(format!("menu-item:{index}")),
                action,
                danger,
                look,
            ));
        }
        let menu = Container::new(
            ConstrainedBox::new(column.finish())
                .with_width(MENU_WIDTH)
                .finish(),
        )
        .with_uniform_padding(5.)
        .with_background_color(palette.surface.0)
        .with_border(Border::all(1.).with_border_color(palette.border.0))
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(
            sizes.chip_radius + 3.,
        )))
        .with_drop_shadow(DropShadow {
            color: palette.shadow.0,
            offset: vec2f(0., 8.),
            blur_radius: sizes.shadow_blur,
            spread_radius: 0.,
        })
        .finish();
        Dismiss::new(menu)
            .prevent_interaction_with_other_elements()
            .on_dismiss(|ctx, _| ctx.dispatch_typed_action(ArtifactsViewAction::CloseMenu))
            .finish()
    }
}

// --- Viewer ---

impl ArtifactsView {
    fn render_viewer(&self, viewer: &Viewer, look: Look<'_>, app: &AppContext) -> Box<dyn Element> {
        let Look {
            palette,
            sizes,
            fonts,
        } = look;
        let body = match &viewer.meta {
            Some(_) => WebViewSlot::new(
                self.web_view.clone(),
                &self.slot_position_id,
                Rect::new().with_background_color(palette.page.0).finish(),
                design::is_dark(app),
                palette.page.0,
            )
            .finish(),
            None => Align::new(
                Text::new_inline("Este artefato não existe mais", fonts.ui, sizes.font_size)
                    .with_color(palette.text_muted.0)
                    .finish(),
            )
            .finish(),
        };
        Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(self.render_viewer_header(viewer, look))
            .with_child(Expanded::new(1., body).finish())
            .finish()
    }

    fn render_viewer_header(&self, viewer: &Viewer, look: Look<'_>) -> Box<dyn Element> {
        let Look {
            palette,
            sizes,
            fonts,
        } = look;
        let back = {
            let palette = palette.clone();
            let font_size = sizes.small_font_size + 0.5;
            let icon_size = sizes.icon_size;
            Hoverable::new(self.mouse_state("back"), move |state| {
                let color = if state.is_hovered() {
                    palette.text.0
                } else {
                    palette.text_muted.0
                };
                let mut button = Container::new(
                    Flex::row()
                        .with_spacing(6.)
                        .with_cross_axis_alignment(CrossAxisAlignment::Center)
                        .with_child(svg(ICON_BACK, color, icon_size))
                        .with_child(
                            Text::new_inline(TITLE, fonts.ui, font_size)
                                .with_style(Properties::default().weight(Weight::Medium))
                                .with_color(color)
                                .finish(),
                        )
                        .finish(),
                )
                .with_horizontal_padding(8.)
                .with_vertical_padding(5.)
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(8.)));
                if state.is_hovered() {
                    button = button.with_background_color(palette.surface_hover.0);
                }
                button.finish()
            })
            .with_cursor(Cursor::PointingHand)
            .on_click(|ctx, _, _| ctx.dispatch_typed_action(ArtifactsViewAction::Back))
            .finish()
        };

        let mut title_row = Flex::row()
            .with_spacing(8.)
            .with_cross_axis_alignment(CrossAxisAlignment::Center);
        if let Some(meta) = &viewer.meta {
            title_row.add_child(render_kind_badge(meta.kind, look));
            title_row.add_child(
                Shrinkable::new(
                    1.,
                    Text::new_inline(meta.title.clone(), fonts.ui, sizes.font_size)
                        .with_style(Properties::default().weight(Weight::Semibold))
                        .with_color(palette.text.0)
                        .with_clip(ClipConfig::end())
                        .finish(),
                )
                .finish(),
            );
            title_row.add_child(
                Container::new(
                    Text::new_inline(format!("v{}", meta.version), fonts.ui, sizes.tiny_font_size)
                        .with_style(Properties::default().weight(Weight::Medium))
                        .with_color(palette.text_muted.0)
                        .finish(),
                )
                .with_horizontal_padding(6.)
                .with_vertical_padding(1.5)
                .with_background_color(palette.surface_strong.0)
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(6.)))
                .finish(),
            );
        }

        let mut row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(10.)
            .with_child(back)
            .with_child(
                ConstrainedBox::new(Rect::new().with_background_color(palette.border.0).finish())
                    .with_width(1.)
                    .with_height(18.)
                    .finish(),
            )
            .with_child(Expanded::new(1., title_row.finish()).finish());
        if viewer.meta.is_some() {
            let id = viewer.id.clone();
            row.add_child(
                Flex::row()
                    .with_spacing(4.)
                    .with_child(render_header_button(
                        "Recarregar",
                        ICON_RELOAD,
                        self.mouse_state("reload"),
                        ArtifactsViewAction::Reload,
                        look,
                    ))
                    .with_child(render_header_button(
                        "Abrir no navegador",
                        ICON_EXTERNAL,
                        self.mouse_state("browser"),
                        ArtifactsViewAction::OpenInBrowser(id.clone()),
                        look,
                    ))
                    .with_child(render_header_button(
                        "Mostrar no Finder",
                        ICON_FOLDER,
                        self.mouse_state("finder"),
                        ArtifactsViewAction::RevealInFinder(id),
                        look,
                    ))
                    .finish(),
            );
        }
        Container::new(
            ConstrainedBox::new(row.finish())
                .with_height(sizes.header_height)
                .finish(),
        )
        .with_horizontal_padding(10.)
        .with_border(Border::bottom(1.).with_border_color(palette.border.0))
        .with_background_color(palette.page.0)
        .finish()
    }
}

fn render_thumbnail(card: &Card, look: Look<'_>) -> Box<dyn Element> {
    let Look { palette, sizes, .. } = look;
    let radius = CornerRadius::with_top(Radius::Pixels(sizes.card_radius - 1.));
    let placeholder = || {
        let color = kind_color(card.meta.kind, palette);
        let icon_size = sizes.icon_size * 2.25;
        Container::new(
            Stack::new()
                .with_child(Rect::new().finish())
                .with_child(SvgIcon::new(PATTERN_DOTS, with_alpha(color, 0x24)).finish())
                .with_child(
                    Align::new(
                        ConstrainedBox::new(
                            SvgIcon::new(kind_icon(card.meta.kind), color).finish(),
                        )
                        .with_width(icon_size)
                        .with_height(icon_size)
                        .finish(),
                    )
                    .finish(),
                )
                .finish(),
        )
        .with_background_color(mix(palette.surface.0, color, 0.1))
        .with_corner_radius(radius)
        .finish()
    };
    let content = match &card.thumbnail {
        Some(source) => Image::new(source.clone(), CacheOption::Original)
            .cover()
            .top_aligned()
            .with_corner_radius(radius)
            .before_load(placeholder())
            .on_load_failure(placeholder())
            .finish(),
        None => placeholder(),
    };
    // A cover-fit image paints past its bounds.
    Container::new(Clipped::new(content).finish())
        .with_background_color(palette.page.0)
        .with_corner_radius(radius)
        .with_border(Border::bottom(1.).with_border_color(palette.border.0))
        .finish()
}

fn render_card_footer(
    meta: &ArtifactMeta,
    caption: String,
    menu_button: Box<dyn Element>,
    look: Look<'_>,
) -> Box<dyn Element> {
    let Look {
        palette,
        sizes,
        fonts,
    } = look;
    let text = Flex::column()
        .with_spacing(2.)
        .with_child(
            Text::new_inline(meta.title.clone(), fonts.ui, sizes.small_font_size + 1.)
                .with_style(Properties::default().weight(Weight::Semibold))
                .with_color(palette.text.0)
                .with_clip(ClipConfig::end())
                .finish(),
        )
        .with_child(
            Text::new_inline(caption, fonts.ui, sizes.tiny_font_size + 0.5)
                .with_color(palette.text_muted.0)
                .with_clip(ClipConfig::end())
                .finish(),
        )
        .finish();
    Container::new(
        Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(10.)
            .with_child(render_kind_badge(meta.kind, look))
            .with_child(Expanded::new(1., text).finish())
            .with_child(menu_button)
            .finish(),
    )
    .with_horizontal_padding(12.)
    .with_vertical_padding(10.)
    .finish()
}

fn render_empty_state(look: Look<'_>) -> Box<dyn Element> {
    let Look {
        palette,
        sizes,
        fonts,
    } = look;
    Container::new(
        Flex::column()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(8.)
            .with_child(
                Container::new(svg(
                    "bundled/svg/picolas-artifact-other.svg",
                    palette.text_faint.0,
                    sizes.icon_size * 1.75,
                ))
                .with_margin_bottom(6.)
                .finish(),
            )
            .with_child(
                Text::new_inline("Nenhum artefato ainda", fonts.ui, sizes.font_size + 1.)
                    .with_style(Properties::default().weight(Weight::Semibold))
                    .with_color(palette.text.0)
                    .finish(),
            )
            .with_child(
                Text::new_inline(
                    "Peça ao Claude para criar um documento, apresentação ou painel",
                    fonts.ui,
                    sizes.small_font_size,
                )
                .with_color(palette.text_muted.0)
                .finish(),
            )
            .finish(),
    )
    .with_vertical_padding(48.)
    .with_horizontal_padding(24.)
    .with_background_color(palette.sidebar.0)
    .with_border(Border::all(1.).with_border_color(palette.border.0))
    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(
        sizes.card_radius + 4.,
    )))
    .finish()
}

fn render_kind_badge(kind: ArtifactKind, look: Look<'_>) -> Box<dyn Element> {
    let size = look.sizes.icon_size + 8.;
    Container::new(
        ConstrainedBox::new(
            Align::new(svg(
                kind_icon(kind),
                ColorU::white(),
                look.sizes.small_icon_size,
            ))
            .finish(),
        )
        .with_width(size)
        .with_height(size)
        .finish(),
    )
    .with_background_color(kind_color(kind, look.palette))
    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(6.)))
    .finish()
}

fn section_label(label: &'static str, look: Look<'_>) -> Box<dyn Element> {
    Text::new_inline(label, look.fonts.ui, look.sizes.small_font_size + 0.5)
        .with_style(Properties::default().weight(Weight::Semibold))
        .with_color(look.palette.text_muted.0)
        .finish()
}

fn render_icon_button(
    icon: &'static str,
    mouse_state: MouseStateHandle,
    action: ArtifactsViewAction,
    active: bool,
    look: Look<'_>,
) -> Box<dyn Element> {
    let palette = look.palette.clone();
    let size = look.sizes.icon_size;
    Hoverable::new(mouse_state, move |state| {
        let hovered = state.is_hovered() || active;
        let color = if hovered {
            palette.text.0
        } else {
            palette.text_faint.0
        };
        let mut button = Container::new(svg(icon, color, size))
            .with_uniform_padding(4.)
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(7.)));
        if hovered {
            button = button.with_background_color(palette.surface_strong.0);
        }
        button.finish()
    })
    .with_cursor(Cursor::PointingHand)
    .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
    .finish()
}

fn render_header_button(
    label: &'static str,
    icon: &'static str,
    mouse_state: MouseStateHandle,
    action: ArtifactsViewAction,
    look: Look<'_>,
) -> Box<dyn Element> {
    let palette = look.palette.clone();
    let fonts = look.fonts;
    let font_size = look.sizes.small_font_size;
    let icon_size = look.sizes.small_icon_size + 1.;
    Hoverable::new(mouse_state, move |state| {
        let hovered = state.is_hovered();
        let color = if hovered {
            palette.text.0
        } else {
            palette.text_muted.0
        };
        let mut button = Container::new(
            Flex::row()
                .with_spacing(6.)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(svg(icon, color, icon_size))
                .with_child(
                    Text::new_inline(label, fonts.ui, font_size)
                        .with_color(color)
                        .finish(),
                )
                .finish(),
        )
        .with_horizontal_padding(9.)
        .with_vertical_padding(5.)
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(8.)));
        if hovered {
            button = button.with_background_color(palette.surface_hover.0);
        }
        button.finish()
    })
    .with_cursor(Cursor::PointingHand)
    .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
    .finish()
}

#[derive(Clone, Copy)]
enum ButtonStyle {
    Neutral,
    Danger,
}

fn render_text_button(
    label: &'static str,
    mouse_state: MouseStateHandle,
    action: ArtifactsViewAction,
    style: ButtonStyle,
    look: Look<'_>,
) -> Box<dyn Element> {
    let palette = look.palette.clone();
    let fonts = look.fonts;
    let font_size = look.sizes.small_font_size;
    Hoverable::new(mouse_state, move |state| {
        let hovered = state.is_hovered();
        let (background, color) = match style {
            ButtonStyle::Neutral => (
                if hovered {
                    palette.surface_hover.0
                } else {
                    palette.surface_strong.0
                },
                palette.text.0,
            ),
            ButtonStyle::Danger => (
                if hovered {
                    mix(palette.error.0, ColorU::black(), 0.12)
                } else {
                    palette.error.0
                },
                palette.on_accent.0,
            ),
        };
        Container::new(
            Text::new_inline(label, fonts.ui, font_size)
                .with_style(Properties::default().weight(Weight::Medium))
                .with_color(color)
                .finish(),
        )
        .with_horizontal_padding(10.)
        .with_vertical_padding(4.)
        .with_background_color(background)
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(7.)))
        .finish()
    })
    .with_cursor(Cursor::PointingHand)
    .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
    .finish()
}

fn render_menu_row(
    label: &'static str,
    icon: &'static str,
    mouse_state: MouseStateHandle,
    action: ArtifactsViewAction,
    danger: bool,
    look: Look<'_>,
) -> Box<dyn Element> {
    let palette = look.palette.clone();
    let fonts = look.fonts;
    let font_size = look.sizes.small_font_size + 0.5;
    let icon_size = look.sizes.small_icon_size + 1.;
    Hoverable::new(mouse_state, move |state| {
        let color = if danger {
            palette.error.0
        } else {
            palette.text.0
        };
        let mut row = Container::new(
            Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_spacing(10.)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(svg(
                    icon,
                    if danger { color } else { palette.text_muted.0 },
                    icon_size,
                ))
                .with_child(
                    Text::new_inline(label, fonts.ui, font_size)
                        .with_color(color)
                        .finish(),
                )
                .finish(),
        )
        .with_horizontal_padding(10.)
        .with_vertical_padding(7.)
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(7.)));
        if state.is_hovered() {
            row = row.with_background_color(palette.surface_hover.0);
        }
        row.finish()
    })
    .with_cursor(Cursor::PointingHand)
    .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
    .finish()
}

fn svg(path: &'static str, color: ColorU, size: f32) -> Box<dyn Element> {
    ConstrainedBox::new(SvgIcon::new(path, color).finish())
        .with_width(size)
        .with_height(size)
        .finish()
}

fn menu_button_position_id(id: &str) -> String {
    format!("artifact-card-menu:{id}")
}

fn kind_icon(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Document => "bundled/svg/picolas-artifact-document.svg",
        ArtifactKind::Presentation => "bundled/svg/picolas-artifact-presentation.svg",
        ArtifactKind::Design => "bundled/svg/picolas-artifact-design.svg",
        ArtifactKind::Dashboard => "bundled/svg/picolas-artifact-dashboard.svg",
        ArtifactKind::Motion => "bundled/svg/picolas-artifact-motion.svg",
        ArtifactKind::Other => "bundled/svg/picolas-artifact-other.svg",
    }
}

fn kind_color(kind: ArtifactKind, palette: &Palette) -> ColorU {
    match kind {
        ArtifactKind::Document => palette.kind_document.0,
        ArtifactKind::Presentation => palette.kind_presentation.0,
        ArtifactKind::Design => palette.kind_design.0,
        ArtifactKind::Dashboard => palette.kind_dashboard.0,
        ArtifactKind::Motion => palette.kind_motion.0,
        ArtifactKind::Other => palette.kind_other.0,
    }
}

/// `a` moved towards `b` by `amount` (0 to 1), keeping `a`'s alpha.
fn mix(a: ColorU, b: ColorU, amount: f32) -> ColorU {
    let channel =
        |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * amount).round() as u8;
    ColorU::new(channel(a.r, b.r), channel(a.g, b.g), channel(a.b, b.b), a.a)
}

fn with_alpha(color: ColorU, alpha: u8) -> ColorU {
    ColorU { a: alpha, ..color }
}

impl TypedActionView for ArtifactsView {
    type Action = ArtifactsViewAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            ArtifactsViewAction::SetFilter(kind) => {
                self.filter = *kind;
                ctx.notify();
            }
            ArtifactsViewAction::Open(id) => self.open(id.clone(), ctx),
            ArtifactsViewAction::Back => self.back(ctx),
            ArtifactsViewAction::Reload => {
                if let Some(viewer) = &mut self.viewer {
                    viewer.meta = self.store.get(&viewer.id);
                }
                self.web_view.with(|web_view| web_view.reload());
                ctx.notify();
            }
            ArtifactsViewAction::OpenInBrowser(id) => {
                self.menu_open_for = None;
                if let Some(path) = self.html_path(id) {
                    ctx.open_url(&file_url(&path));
                }
                ctx.notify();
            }
            ArtifactsViewAction::RevealInFinder(id) => {
                self.menu_open_for = None;
                if let Some(path) = self.html_path(id) {
                    ctx.open_file_path_in_explorer(&path);
                }
                ctx.notify();
            }
            ArtifactsViewAction::ToggleMenu(id) => {
                self.menu_open_for = if self.menu_open_for.as_deref() == Some(id.as_str()) {
                    None
                } else {
                    Some(id.clone())
                };
                self.pending_delete = None;
                ctx.notify();
            }
            ArtifactsViewAction::CloseMenu => {
                self.menu_open_for = None;
                ctx.notify();
            }
            ArtifactsViewAction::AskDelete(id) => {
                self.menu_open_for = None;
                self.pending_delete = Some(id.clone());
                ctx.notify();
            }
            ArtifactsViewAction::CancelDelete => {
                self.pending_delete = None;
                ctx.notify();
            }
            ArtifactsViewAction::ConfirmDelete(id) => self.delete(id, ctx),
        }
    }
}

impl BackingView for ArtifactsView {
    type PaneHeaderOverflowMenuAction = ArtifactsViewAction;
    type CustomAction = ArtifactsViewAction;
    type AssociatedData = ();

    fn handle_pane_header_overflow_menu_action(
        &mut self,
        action: &Self::PaneHeaderOverflowMenuAction,
        ctx: &mut ViewContext<Self>,
    ) {
        self.handle_action(action, ctx);
    }

    fn handle_custom_action(&mut self, action: &Self::CustomAction, ctx: &mut ViewContext<Self>) {
        self.handle_action(action, ctx);
    }

    fn close(&mut self, ctx: &mut ViewContext<Self>) {
        self.web_view.set(None);
        ctx.emit(ArtifactsViewEvent::Pane(PaneEvent::Close));
    }

    fn focus_contents(&mut self, ctx: &mut ViewContext<Self>) {
        self.focus(ctx);
    }

    fn render_header_content(
        &self,
        _ctx: &view::HeaderRenderContext<'_>,
        app: &AppContext,
    ) -> HeaderContent {
        HeaderContent::Standard(StandardHeader {
            title: self.pane_configuration.as_ref(app).title().to_owned(),
            title_secondary: None,
            title_style: None,
            title_clip_config: ClipConfig::end(),
            title_max_width: None,
            left_of_title: None,
            right_of_title: None,
            left_of_overflow: None,
            options: StandardHeaderOptions::default(),
        })
    }

    fn set_focus_handle(&mut self, focus_handle: PaneFocusHandle, _ctx: &mut ViewContext<Self>) {
        self.focus_handle = Some(focus_handle);
    }
}
