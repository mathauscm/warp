//! The chat pane: the conversation, the composer and the side panel's cards, which float in the
//! top-right corner when the pane has room for them beside the conversation and open as a popover
//! otherwise. The pane header only shows when the tab is split.

mod composer;
mod side_panel;
mod transcript;

use std::time::Duration;

use instant::Instant;
use pathfinder_geometry::rect::RectF;
use pathfinder_geometry::vector::vec2f;
use warpui::elements::{
    Align, ChildView, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, Dismiss,
    Element, Expanded, Flex, Hoverable, Icon as SvgIcon, MainAxisAlignment, MainAxisSize,
    MouseStateHandle, ParentElement, Radius, SavePosition, Shrinkable, SizeConstraintCondition,
    SizeConstraintSwitch, Stack,
};
use warpui::platform::Cursor;
use warpui::text_layout::ClipConfig;
use warpui::ui_components::components::UiComponent;
use warpui::{
    AppContext, Entity, ModelHandle, SingletonEntity, TypedActionView, View, ViewContext,
    ViewHandle,
};

use self::composer::{ComposerEvent, ComposerView};
use self::side_panel::SidePanelView;
use self::transcript::{TranscriptEvent, TranscriptView};
use super::ClaudeChatPaneSnapshot;
use super::design::{ChatDesign, ChatDesignEvent, Palette, Sizes};
use super::registry::NativeChatRegistry;
use super::session::{ClaudeChatSession, ClaudeChatSessionEvent};
use crate::appearance::Appearance;
use crate::pane_group::focus_state::PaneFocusHandle;
use crate::pane_group::pane::view::{self, HeaderContent, StandardHeader, StandardHeaderOptions};
use crate::pane_group::{BackingView, PaneConfiguration, PaneEvent, PaneHeaderAction};
use crate::ui_components::blended_colors;
use crate::ui_components::buttons::icon_button_with_color;
use crate::ui_components::icons::Icon;
use crate::workspace::WorkspaceAction;
use claude_chat::reducer::ConversationChange;

const NEW_CHAT_TITLE: &str = "Novo chat";
const PANEL_TOGGLE_ICON: &str = "bundled/svg/info.svg";
const PANEL_TOGGLE_TOOLTIP: &str = "Ambiente, contexto e conectores";
/// Space between the cards and the pane's edge, and between the cards and the conversation.
const CARD_GAP: f32 = 16.;
/// The corner layer's own inset; the cards add `side_panel_padding` around themselves.
const CORNER_INSET: f32 = 4.;
const TOGGLE_PADDING: f32 = 6.;
const TOGGLE_RADIUS: f32 = 8.;
/// Keeps the top of the conversation clear of the corner's panel toggle when there's no header.
const HEADERLESS_TOP_PADDING: f32 = 28.;
/// A click outside the popover dismisses it; the header button's click that follows it must not
/// reopen it.
const DISMISS_DEBOUNCE: Duration = Duration::from_millis(400);

/// A card of the side panel, for opening the panel right where a command points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidePanelSection {
    Environment,
    Context,
    Usage,
    Connectors,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudeChatViewEvent {
    Pane(PaneEvent),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudeChatViewAction {
    /// Shows or hides the floating cards, or opens them as a popover when there's no room.
    ToggleSidePanel,
    DismissSidePanelPopover,
}

pub struct ClaudeChatView {
    session: ModelHandle<ClaudeChatSession>,
    transcript: ViewHandle<TranscriptView>,
    composer: ViewHandle<ComposerView>,
    side_panel: ViewHandle<SidePanelView>,
    /// Whether the cards float beside the conversation when the pane has room for them.
    show_docked_cards: bool,
    is_popover_open: bool,
    popover_dismissed_at: Option<Instant>,
    side_panel_button: MouseStateHandle,
    corner_toggle: MouseStateHandle,
    /// Measures the pane, to know whether the cards fit beside the conversation.
    root_position_id: String,
    pane_configuration: ModelHandle<PaneConfiguration>,
    focus_handle: Option<PaneFocusHandle>,
}

impl ClaudeChatView {
    pub fn new(snapshot: ClaudeChatPaneSnapshot, ctx: &mut ViewContext<Self>) -> Self {
        let pane_configuration = ctx.add_model(|_| PaneConfiguration::new(NEW_CHAT_TITLE));
        let session = ctx.add_model(|ctx| {
            ClaudeChatSession::new(snapshot.cwd.clone(), snapshot.session_id.clone(), ctx)
        });
        let transcript = ctx.add_typed_action_view(|ctx| TranscriptView::new(session.clone(), ctx));
        let send_draft = cfg!(debug_assertions) && std::env::var_os("PICOLAS_CHAT_SEND").is_some();
        let composer_draft = snapshot.draft.clone().filter(|_| !send_draft);
        let composer = ctx.add_typed_action_view(|ctx| {
            ComposerView::new(
                session.clone(),
                composer_draft,
                snapshot.artifact_kind.clone(),
                ctx,
            )
        });
        let side_panel = ctx.add_typed_action_view(|ctx| SidePanelView::new(session.clone(), ctx));

        ctx.subscribe_to_model(&ChatDesign::handle(ctx), |_, _, event, ctx| match event {
            ChatDesignEvent::Changed => ctx.notify(),
        });
        ctx.subscribe_to_model(&session, |me, _, event, ctx| {
            me.on_session_event(event, ctx)
        });
        register_session(&session, ctx);
        ctx.subscribe_to_view(&transcript, |me, _, event, ctx| {
            me.on_transcript_event(event, ctx)
        });
        ctx.subscribe_to_view(&composer, |me, _, event, ctx| {
            me.on_composer_event(event, ctx)
        });
        ctx.subscribe_to_view(&side_panel, |_, _, event, _| match *event {});

        if cfg!(debug_assertions)
            && let Ok(model) = std::env::var("PICOLAS_CHAT_MODEL")
        {
            session.update(ctx, |session, ctx| session.set_model(Some(model), ctx));
        }
        if cfg!(debug_assertions)
            && let Some(width) = std::env::var("PICOLAS_WINDOW_WIDTH")
                .ok()
                .and_then(|width| width.parse::<f32>().ok())
        {
            let window_id = ctx.window_id();
            ctx.windows()
                .set_window_bounds(window_id, RectF::new(vec2f(40., 40.), vec2f(width, 860.)));
        }
        if send_draft && let Some(draft) = snapshot.draft.clone() {
            session.update(ctx, |session, ctx| {
                session.send_message(draft, Vec::new(), ctx)
            });
        }

        Self {
            session,
            transcript,
            composer,
            side_panel,
            show_docked_cards: true,
            is_popover_open: cfg!(debug_assertions)
                && std::env::var_os("PICOLAS_CHAT_PANEL").is_some(),
            popover_dismissed_at: None,
            side_panel_button: Default::default(),
            corner_toggle: Default::default(),
            root_position_id: format!("native_chat_root:{}", ctx.view_id()),
            pane_configuration,
            focus_handle: None,
        }
    }

    /// What restores this chat: its conversation once it has one, and its folder.
    pub fn snapshot(&self, app: &AppContext) -> ClaudeChatPaneSnapshot {
        let session = self.session.as_ref(app);
        ClaudeChatPaneSnapshot {
            session_id: session
                .has_transcript()
                .then(|| session.session_id().to_owned()),
            cwd: session.cwd().to_path_buf(),
            draft: None,
            artifact_kind: None,
        }
    }

    pub fn session(&self) -> &ModelHandle<ClaudeChatSession> {
        &self.session
    }

    pub fn pane_configuration(&self) -> ModelHandle<PaneConfiguration> {
        self.pane_configuration.clone()
    }

    pub fn focus(&mut self, ctx: &mut ViewContext<Self>) {
        self.composer
            .update(ctx, |composer, ctx| composer.focus(ctx));
    }

    fn on_session_event(&mut self, event: &ClaudeChatSessionEvent, ctx: &mut ViewContext<Self>) {
        match event {
            ClaudeChatSessionEvent::TitleChanged => {
                let title = self
                    .session
                    .as_ref(ctx)
                    .title()
                    .unwrap_or(NEW_CHAT_TITLE)
                    .to_owned();
                self.pane_configuration.update(ctx, |configuration, ctx| {
                    configuration.set_title(title, ctx)
                });
                NativeChatRegistry::handle(ctx)
                    .update(ctx, |registry, ctx| registry.notify_changed(ctx));
                ctx.notify();
            }
            ClaudeChatSessionEvent::SessionIdAssigned => {
                register_session(&self.session, ctx);
                ctx.dispatch_global_action("workspace:save_app", ());
                ctx.notify();
            }
            ClaudeChatSessionEvent::Changed(changes)
                if changes.contains(&ConversationChange::Status) =>
            {
                NativeChatRegistry::handle(ctx)
                    .update(ctx, |registry, ctx| registry.notify_changed(ctx));
            }
            ClaudeChatSessionEvent::Changed(_)
            | ClaudeChatSessionEvent::ProcessStateChanged
            | ClaudeChatSessionEvent::HistoryLoaded
            | ClaudeChatSessionEvent::FileSuggestions { .. }
            | ClaudeChatSessionEvent::Error(_) => {}
        }
    }

    fn on_transcript_event(&mut self, event: &TranscriptEvent, ctx: &mut ViewContext<Self>) {
        match event {
            TranscriptEvent::Mention(text) => {
                let text = text.clone();
                self.composer
                    .update(ctx, |composer, ctx| composer.add_mention(text, ctx));
            }
            TranscriptEvent::OpenArtifact(artifact_id) => {
                ctx.dispatch_typed_action(&WorkspaceAction::OpenArtifacts {
                    artifact_id: Some(artifact_id.clone()),
                });
            }
        }
    }

    fn on_composer_event(&mut self, event: &ComposerEvent, ctx: &mut ViewContext<Self>) {
        match event {
            ComposerEvent::OpenSidePanel(section) => {
                self.open_side_panel(ctx);
                let section = *section;
                self.side_panel
                    .update(ctx, |side_panel, ctx| side_panel.reveal(section, ctx));
            }
            ComposerEvent::NewChat => {
                let cwd = self.session.as_ref(ctx).cwd().to_path_buf();
                ctx.dispatch_typed_action(&WorkspaceAction::OpenClaudeChat {
                    cwd,
                    session_id: None,
                    draft: None,
                    artifact_kind: None,
                    group: None,
                });
            }
            ComposerEvent::Rename(title) => {
                let title = title.clone();
                self.session
                    .update(ctx, |session, ctx| session.rename(title, ctx));
            }
        }
    }

    fn is_in_split_pane(&self, app: &AppContext) -> bool {
        self.focus_handle
            .as_ref()
            .is_some_and(|focus_handle| focus_handle.is_in_split_pane(app))
    }

    /// Whether the cards fit beside the conversation, going by the last frame's layout.
    fn has_room_for_cards(&self, ctx: &ViewContext<Self>) -> bool {
        let pane_width = ctx
            .element_position_by_id(&self.root_position_id)
            .map_or(0., |rect| rect.width());
        has_room_for_cards(
            self.is_in_split_pane(ctx),
            pane_width,
            ChatDesign::as_ref(ctx).sizes(),
        )
    }

    fn toggle_side_panel(&mut self, ctx: &mut ViewContext<Self>) {
        let is_opening = if self.has_room_for_cards(ctx) {
            self.is_popover_open = false;
            self.show_docked_cards = !self.show_docked_cards;
            self.show_docked_cards
        } else {
            let was_just_dismissed = self
                .popover_dismissed_at
                .is_some_and(|dismissed_at| dismissed_at.elapsed() < DISMISS_DEBOUNCE);
            if !self.is_popover_open && was_just_dismissed {
                return;
            }
            self.is_popover_open = !self.is_popover_open;
            self.is_popover_open
        };
        if is_opening {
            self.side_panel
                .update(ctx, |side_panel, ctx| side_panel.refresh(ctx));
        }
        ctx.notify();
    }

    /// Shows the cards wherever they fit.
    fn open_side_panel(&mut self, ctx: &mut ViewContext<Self>) {
        if self.has_room_for_cards(ctx) {
            self.show_docked_cards = true;
        } else {
            self.is_popover_open = true;
        }
        self.side_panel
            .update(ctx, |side_panel, ctx| side_panel.refresh(ctx));
        ctx.notify();
    }
}

impl ClaudeChatView {
    fn render_header_buttons(&self, app: &AppContext) -> Box<dyn Element> {
        header_button(
            Icon::Info,
            PANEL_TOGGLE_TOOLTIP,
            self.is_popover_open,
            self.side_panel_button.clone(),
            ClaudeChatViewAction::ToggleSidePanel,
            app,
        )
    }

    /// The top-right corner over the conversation: the panel toggle when the pane has no header,
    /// and the cards when `show_cards`. A popover closes when clicked outside.
    fn render_corner(
        &self,
        show_toggle: bool,
        show_cards: bool,
        is_popover: bool,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let design = ChatDesign::as_ref(app);
        let palette = design.palette(app);
        let sizes = design.sizes();
        let mut column = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::End);
        if show_toggle {
            column.add_child(
                Container::new(render_panel_toggle(
                    show_cards,
                    self.corner_toggle.clone(),
                    palette,
                    sizes,
                    app,
                ))
                .with_padding_top(sizes.side_panel_padding - CORNER_INSET)
                .with_padding_right(sizes.side_panel_padding)
                .finish(),
            );
        }
        if show_cards {
            column
                .add_child(Shrinkable::new(1., ChildView::new(&self.side_panel).finish()).finish());
        }
        let mut corner = Container::new(column.finish())
            .with_padding_top(CORNER_INSET)
            .with_padding_right(CORNER_INSET)
            .finish();
        if is_popover && show_cards {
            corner = Dismiss::new(corner)
                .on_dismiss(|ctx, _| {
                    ctx.dispatch_typed_action(ClaudeChatViewAction::DismissSidePanelPopover);
                })
                .finish();
        }
        Align::new(corner).top_right().finish()
    }
}

/// The narrowest pane that fits the cards beside the centered conversation.
fn min_width_for_docked_cards(sizes: &Sizes) -> f32 {
    sizes.column_max_width + 2. * (sizes.side_panel_width + 2. * CARD_GAP)
}

/// Whether the cards float beside the conversation; otherwise they only open as a popover.
fn has_room_for_cards(is_in_split_pane: bool, pane_width: f32, sizes: &Sizes) -> bool {
    !is_in_split_pane && pane_width >= min_width_for_docked_cards(sizes)
}

/// The small button in the corner that shows or hides the cards when the pane has no header.
fn render_panel_toggle(
    is_active: bool,
    mouse_state: MouseStateHandle,
    palette: &Palette,
    sizes: &Sizes,
    app: &AppContext,
) -> Box<dyn Element> {
    let ui_builder = Appearance::as_ref(app).ui_builder().clone();
    let (text, text_muted, surface_hover, surface) = (
        palette.text.0,
        palette.text_muted.0,
        palette.surface_hover.0,
        palette.surface.0,
    );
    let icon_size = sizes.icon_size;
    Hoverable::new(mouse_state, move |state| {
        let color = if state.is_hovered() || is_active {
            text
        } else {
            text_muted
        };
        let mut button = Container::new(
            ConstrainedBox::new(SvgIcon::new(PANEL_TOGGLE_ICON, color).finish())
                .with_width(icon_size)
                .with_height(icon_size)
                .finish(),
        )
        .with_uniform_padding(TOGGLE_PADDING)
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(TOGGLE_RADIUS)));
        if state.is_hovered() {
            button = button.with_background_color(surface_hover);
        } else if is_active {
            button = button.with_background_color(surface);
        }
        let button = button.finish();
        if !state.is_hovered() {
            return button;
        }
        let tooltip = ui_builder
            .tool_tip(PANEL_TOGGLE_TOOLTIP.to_owned())
            .build()
            .finish();
        let mut stack = Stack::new().with_child(button);
        stack.add_positioned_overlay_child(
            tooltip,
            warpui::elements::OffsetPositioning::offset_from_parent(
                vec2f(0., 6.),
                warpui::elements::ParentOffsetBounds::WindowByPosition,
                warpui::elements::ParentAnchor::BottomRight,
                warpui::elements::ChildAnchor::TopRight,
            ),
        );
        stack.finish()
    })
    .with_cursor(Cursor::PointingHand)
    .on_click(|ctx, _, _| ctx.dispatch_typed_action(ClaudeChatViewAction::ToggleSidePanel))
    .finish()
}

fn header_button(
    icon: Icon,
    tooltip: &'static str,
    active: bool,
    mouse_state: MouseStateHandle,
    action: ClaudeChatViewAction,
    app: &AppContext,
) -> Box<dyn Element> {
    let appearance = Appearance::as_ref(app);
    let theme = appearance.theme();
    let ui_builder = appearance.ui_builder().clone();
    icon_button_with_color(
        appearance,
        icon,
        active,
        mouse_state,
        blended_colors::text_sub(theme, theme.background()).into(),
    )
    .with_tooltip(move || ui_builder.tool_tip(tooltip.to_owned()).build().finish())
    .build()
    .on_click(move |ctx, _, _| {
        ctx.dispatch_typed_action::<PaneHeaderAction<ClaudeChatViewAction, ClaudeChatViewAction>>(
            PaneHeaderAction::CustomAction(action.clone()),
        );
    })
    .finish()
}

fn register_session(
    session: &ModelHandle<ClaudeChatSession>,
    ctx: &mut ViewContext<ClaudeChatView>,
) {
    let session_id = session.as_ref(ctx).session_id().to_owned();
    NativeChatRegistry::handle(ctx).update(ctx, |registry, ctx| {
        registry.register(&session_id, session, ctx)
    });
}

impl Entity for ClaudeChatView {
    type Event = ClaudeChatViewEvent;
}

impl View for ClaudeChatView {
    fn ui_name() -> &'static str {
        "ClaudeChatView"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let design = ChatDesign::as_ref(app);
        let palette = design.palette(app);
        let sizes = design.sizes();
        let is_in_split_pane = self.is_in_split_pane(app);

        let column = ConstrainedBox::new(
            Flex::column()
                .with_child(Expanded::new(1., ChildView::new(&self.transcript).finish()).finish())
                .with_child(
                    Container::new(ChildView::new(&self.composer).finish())
                        .with_padding_bottom(sizes.composer_bottom_margin)
                        .finish(),
                )
                .finish(),
        )
        .with_max_width(sizes.column_max_width + 2. * sizes.column_padding)
        .finish();

        let chat = Container::new(
            Flex::row()
                .with_main_axis_alignment(MainAxisAlignment::Center)
                .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
                .with_main_axis_size(MainAxisSize::Max)
                .with_child(
                    Expanded::new(
                        1.,
                        Container::new(column)
                            .with_horizontal_padding(sizes.column_padding)
                            .with_padding_top(if is_in_split_pane {
                                0.
                            } else {
                                HEADERLESS_TOP_PADDING
                            })
                            .finish(),
                    )
                    .finish(),
                )
                .finish(),
        )
        .with_background_color(palette.page.0)
        .finish();

        let corner = if is_in_split_pane {
            self.render_corner(false, self.is_popover_open, true, app)
        } else {
            SizeConstraintSwitch::new(
                self.render_corner(true, self.show_docked_cards, false, app),
                [(
                    SizeConstraintCondition::WidthLessThan(min_width_for_docked_cards(sizes)),
                    self.render_corner(true, self.is_popover_open, true, app),
                )],
            )
            .finish()
        };
        Stack::new()
            .with_child(SavePosition::new(chat, &self.root_position_id).finish())
            .with_child(corner)
            .finish()
    }
}

impl TypedActionView for ClaudeChatView {
    type Action = ClaudeChatViewAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            ClaudeChatViewAction::ToggleSidePanel => self.toggle_side_panel(ctx),
            ClaudeChatViewAction::DismissSidePanelPopover => {
                self.is_popover_open = false;
                self.popover_dismissed_at = Some(Instant::now());
                ctx.notify();
            }
        }
    }
}

impl BackingView for ClaudeChatView {
    type PaneHeaderOverflowMenuAction = ClaudeChatViewAction;
    type CustomAction = ClaudeChatViewAction;
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
        ctx.emit(ClaudeChatViewEvent::Pane(PaneEvent::Close));
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
            left_of_overflow: Some(self.render_header_buttons(app)),
            options: StandardHeaderOptions {
                always_show_icons: true,
                ..StandardHeaderOptions::default()
            },
        })
    }

    fn should_render_header(&self, app: &AppContext) -> bool {
        self.is_in_split_pane(app)
    }

    fn set_focus_handle(&mut self, focus_handle: PaneFocusHandle, ctx: &mut ViewContext<Self>) {
        ctx.subscribe_to_model(focus_handle.focus_state_handle(), |_, _, _, ctx| {
            ctx.notify()
        });
        self.focus_handle = Some(focus_handle);
        ctx.notify();
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
