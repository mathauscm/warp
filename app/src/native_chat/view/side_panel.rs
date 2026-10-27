//! The floating cards beside the conversation: where Claude runs (folder, Remote Control), how
//! full its context is, the plan's usage and the MCP connectors. Dragging a card reorders them.

mod connectors;
mod context;
mod environment;
mod format;
mod order;
mod usage;
mod widgets;

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use claude_chat::reducer::ConversationChange;
use pathfinder_geometry::rect::RectF;
use warpui::r#async::Timer;
use warpui::clipboard::ClipboardContent;
use warpui::elements::{
    ClippedScrollStateHandle, ClippedScrollable, ConstrainedBox, Container, CrossAxisAlignment,
    DragAxis, Draggable, DraggableState, Element, Fill, Flex, MainAxisSize, MouseStateHandle,
    ParentElement, SavePosition, ScrollTarget, ScrollToPositionMode, ScrollbarWidth,
};
use warpui::platform::FilePickerConfiguration;
use warpui::{
    AppContext, Entity, ModelHandle, SingletonEntity, TypedActionView, View, ViewContext,
};

use self::widgets::Look;
use super::SidePanelSection;
use crate::native_chat::design::{self, ChatDesign, ChatDesignEvent};
use crate::native_chat::session::{ClaudeChatSession, ClaudeChatSessionEvent};

const CARD_SPACING: f32 = 10.;
const SCROLLBAR_WIDTH: f32 = 4.;
/// How long a copy button shows its check mark.
const COPIED_FEEDBACK: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidePanelEvent {}

#[derive(Debug, Clone, PartialEq)]
pub enum SidePanelAction {
    ToggleSection(SidePanelSection),
    ToggleMcpTools,
    ToggleMemoryFiles,
    ToggleAllConnectors,
    PickFolder,
    SetRemoteControl(bool),
    OpenUrl(String),
    Copy(String),
    Compact,
    AuthenticateServer(String),
    ReconnectServer(String),
    SetServerEnabled {
        name: String,
        enabled: bool,
    },
    /// A card is being dragged; `position` is where it is now.
    DragCard {
        section: SidePanelSection,
        position: RectF,
    },
    DropCard,
}

pub struct SidePanelView {
    session: ModelHandle<ClaudeChatSession>,
    expanded: Vec<SidePanelSection>,
    order: Vec<SidePanelSection>,
    drag_states: [DraggableState; 4],
    show_mcp_tools: bool,
    show_memory_files: bool,
    show_all_connectors: bool,
    /// Whether a copy button just copied, to show its check mark.
    copied: bool,
    scroll_state: ClippedScrollStateHandle,
    mouse_states: RefCell<HashMap<String, MouseStateHandle>>,
    /// Makes the cards' position ids unique among chat panes.
    position_prefix: String,
}

impl SidePanelView {
    pub fn new(session: ModelHandle<ClaudeChatSession>, ctx: &mut ViewContext<Self>) -> Self {
        ctx.subscribe_to_model(&ChatDesign::handle(ctx), |_, _, event, ctx| match event {
            ChatDesignEvent::Changed => ctx.notify(),
        });
        ctx.subscribe_to_model(&session, |_, _, event, ctx| {
            if affects_panel(event) {
                ctx.notify();
            }
        });
        Self {
            session,
            expanded: vec![SidePanelSection::Environment, SidePanelSection::Connectors],
            order: order::load(),
            drag_states: Default::default(),
            show_mcp_tools: false,
            show_memory_files: false,
            show_all_connectors: false,
            copied: false,
            scroll_state: ClippedScrollStateHandle::new(),
            mouse_states: Default::default(),
            position_prefix: format!("native_chat_side_panel:{}", ctx.view_id()),
        }
    }

    /// Expands `section` and scrolls to it.
    pub fn reveal(&mut self, section: SidePanelSection, ctx: &mut ViewContext<Self>) {
        if !self.is_expanded(section) {
            self.expanded.push(section);
        }
        self.scroll_state.scroll_to_position(ScrollTarget {
            position_id: self.position_id(section),
            mode: ScrollToPositionMode::TopIntoView,
        });
        self.refresh(ctx);
        ctx.notify();
    }

    /// Asks Claude for fresh context, plan usage and connector numbers; call it when the panel
    /// opens.
    pub fn refresh(&mut self, ctx: &mut ViewContext<Self>) {
        self.session.update(ctx, |session, ctx| {
            session.refresh_context_usage(ctx);
            session.refresh_usage(ctx);
            session.refresh_mcp_status(ctx);
        });
    }

    fn is_expanded(&self, section: SidePanelSection) -> bool {
        self.expanded.contains(&section)
    }

    fn toggle_section(&mut self, section: SidePanelSection, ctx: &mut ViewContext<Self>) {
        if self.is_expanded(section) {
            self.expanded.retain(|expanded| *expanded != section);
        } else {
            self.expanded.push(section);
            self.session.update(ctx, |session, ctx| match section {
                SidePanelSection::Context => session.refresh_context_usage(ctx),
                SidePanelSection::Usage => session.refresh_usage(ctx),
                SidePanelSection::Connectors => session.refresh_mcp_status(ctx),
                SidePanelSection::Environment => {}
            });
        }
        ctx.notify();
    }

    fn position_id(&self, section: SidePanelSection) -> String {
        format!("{}:{section:?}", self.position_prefix)
    }

    fn mouse(&self, key: impl Into<String>) -> MouseStateHandle {
        self.mouse_states
            .borrow_mut()
            .entry(key.into())
            .or_default()
            .clone()
    }

    fn pick_folder(&mut self, ctx: &mut ViewContext<Self>) {
        ctx.open_file_picker(
            move |result, ctx| {
                let Ok(paths) = result else { return };
                let Some(path) = paths.into_iter().next() else {
                    return;
                };
                if let Some(handle) = ctx.handle().upgrade(ctx) {
                    handle.update(ctx, |me, ctx| {
                        me.session.update(ctx, |session, ctx| {
                            session.change_cwd(PathBuf::from(path), ctx);
                        });
                    });
                }
            },
            FilePickerConfiguration::new().folders_only(),
        );
    }

    fn copy(&mut self, text: String, ctx: &mut ViewContext<Self>) {
        ctx.clipboard().write(ClipboardContent::plain_text(text));
        self.copied = true;
        ctx.notify();
        ctx.spawn(Timer::after(COPIED_FEEDBACK), |me, _, ctx| {
            me.copied = false;
            ctx.notify();
        });
    }

    fn drag_state(&self, section: SidePanelSection) -> DraggableState {
        let index = order::DEFAULT_ORDER
            .iter()
            .position(|candidate| *candidate == section)
            .unwrap_or_default();
        self.drag_states[index].clone()
    }

    /// Moves the dragged card to the slot under the cursor; the order is saved on drop.
    fn drag_card(
        &mut self,
        section: SidePanelSection,
        position: RectF,
        ctx: &mut ViewContext<Self>,
    ) {
        let Some(from) = self.order.iter().position(|card| *card == section) else {
            return;
        };
        let cursor_y = position.center().y();
        let to = self.order.iter().position(|card| {
            ctx.element_position_by_id(self.position_id(*card))
                .is_some_and(|rect| rect.min_y() <= cursor_y && cursor_y < rect.max_y())
        });
        if let Some(to) = to
            && to != from
        {
            let card = self.order.remove(from);
            self.order.insert(to, card);
        }
        ctx.notify();
    }

    fn render_card(
        &self,
        section: SidePanelSection,
        look: Look<'_>,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let content = match section {
            SidePanelSection::Environment => self.render_environment(look, app),
            SidePanelSection::Context => self.render_context(look, app),
            SidePanelSection::Usage => self.render_usage(look, app),
            SidePanelSection::Connectors => self.render_connectors(look, app),
        };
        let drag_state = self.drag_state(section);
        let card = widgets::card_frame(content, drag_state.is_dragging(), look);
        let draggable = Draggable::new(drag_state, card)
            .on_drag(move |ctx, _, position, _| {
                ctx.dispatch_typed_action(SidePanelAction::DragCard { section, position });
            })
            .on_drop(|ctx, _, _, _| ctx.dispatch_typed_action(SidePanelAction::DropCard))
            .with_drag_axis(DragAxis::VerticalOnly)
            .finish();
        SavePosition::new(draggable, &self.position_id(section)).finish()
    }
}

/// Whether a session event changes anything the panel shows; streamed text doesn't.
fn affects_panel(event: &ClaudeChatSessionEvent) -> bool {
    match event {
        ClaudeChatSessionEvent::Changed(changes) => {
            changes.is_empty()
                || changes.iter().any(|change| match change {
                    ConversationChange::Status
                    | ConversationChange::Session
                    | ConversationChange::McpServers
                    | ConversationChange::ContextUsage
                    | ConversationChange::RateLimits
                    | ConversationChange::Cost
                    | ConversationChange::RemoteControl => true,
                    ConversationChange::ItemInserted { .. }
                    | ConversationChange::ItemUpdated { .. }
                    | ConversationChange::ItemRemoved { .. }
                    | ConversationChange::PendingRequests
                    | ConversationChange::Commands
                    | ConversationChange::Models
                    | ConversationChange::Account
                    | ConversationChange::Todos
                    | ConversationChange::Tasks => false,
                })
        }
        ClaudeChatSessionEvent::ProcessStateChanged
        | ClaudeChatSessionEvent::HistoryLoaded
        | ClaudeChatSessionEvent::SessionIdAssigned => true,
        ClaudeChatSessionEvent::TitleChanged
        | ClaudeChatSessionEvent::FileSuggestions { .. }
        | ClaudeChatSessionEvent::QueuedMessageCancelled { .. }
        | ClaudeChatSessionEvent::Error(_) => false,
    }
}

impl Entity for SidePanelView {
    type Event = SidePanelEvent;
}

impl View for SidePanelView {
    fn ui_name() -> &'static str {
        "NativeChatSidePanel"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let design = ChatDesign::as_ref(app);
        let palette = design.palette(app);
        let sizes = design.sizes();
        let look = Look {
            palette,
            sizes,
            font: design::fonts(app).ui,
        };

        let column = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(CARD_SPACING)
            .with_children(
                self.order
                    .iter()
                    .map(|section| self.render_card(*section, look, app)),
            )
            .finish();
        // The padding leaves room for the cards' shadows inside the scroll area's clip.
        let scrollable = ClippedScrollable::vertical(
            self.scroll_state.clone(),
            Container::new(column)
                .with_uniform_padding(sizes.side_panel_padding)
                .finish(),
            ScrollbarWidth::Custom(SCROLLBAR_WIDTH),
            Fill::Solid(palette.border_strong.0),
            Fill::Solid(palette.text_faint.0),
            Fill::None,
        )
        .with_overlayed_scrollbar()
        .finish();
        ConstrainedBox::new(scrollable)
            .with_width(sizes.side_panel_width + 2. * sizes.side_panel_padding)
            .finish()
    }
}

impl TypedActionView for SidePanelView {
    type Action = SidePanelAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            SidePanelAction::ToggleSection(section) => self.toggle_section(*section, ctx),
            SidePanelAction::ToggleMcpTools => {
                self.show_mcp_tools = !self.show_mcp_tools;
                ctx.notify();
            }
            SidePanelAction::ToggleMemoryFiles => {
                self.show_memory_files = !self.show_memory_files;
                ctx.notify();
            }
            SidePanelAction::ToggleAllConnectors => {
                self.show_all_connectors = !self.show_all_connectors;
                ctx.notify();
            }
            SidePanelAction::PickFolder => self.pick_folder(ctx),
            SidePanelAction::SetRemoteControl(enabled) => {
                let enabled = *enabled;
                self.session
                    .update(ctx, |session, ctx| session.set_remote_control(enabled, ctx));
            }
            SidePanelAction::OpenUrl(url) => ctx.open_url(url),
            SidePanelAction::Copy(text) => self.copy(text.clone(), ctx),
            SidePanelAction::Compact => {
                self.session
                    .update(ctx, |session, ctx| session.compact(ctx));
            }
            SidePanelAction::AuthenticateServer(name) => {
                let name = name.clone();
                self.session.update(ctx, |session, ctx| {
                    session.authenticate_mcp_server(name, ctx)
                });
            }
            SidePanelAction::ReconnectServer(name) => {
                let name = name.clone();
                self.session
                    .update(ctx, |session, ctx| session.reconnect_mcp_server(name, ctx));
            }
            SidePanelAction::DragCard { section, position } => {
                self.drag_card(*section, *position, ctx);
            }
            SidePanelAction::DropCard => {
                order::save(&self.order);
                ctx.notify();
            }
            SidePanelAction::SetServerEnabled { name, enabled } => {
                let (name, enabled) = (name.clone(), *enabled);
                self.session.update(ctx, |session, ctx| {
                    session.set_mcp_server_enabled(name, enabled, ctx)
                });
            }
        }
    }
}

#[cfg(test)]
#[path = "side_panel_tests.rs"]
mod tests;
