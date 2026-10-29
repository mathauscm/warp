use warpui::{AppContext, ModelHandle, View, ViewContext, ViewHandle};

use super::view::PaneView;
use super::{
    DetachType, PaneConfiguration, PaneContent, PaneGroup, PaneId, ShareableLink,
    ShareableLinkError,
};
use crate::app_state::LeafContents;
use crate::native_chat::session::ClaudeChatSession;
use crate::native_chat::{ClaudeChatPaneSnapshot, ClaudeChatView, ClaudeChatViewEvent};

pub struct ClaudeChatPane {
    view: ViewHandle<PaneView<ClaudeChatView>>,
    pane_configuration: ModelHandle<PaneConfiguration>,
    /// Kept apart from the view so the pane group can read the chat's folder while the view is
    /// mid-update (its actions can make the workspace refresh working directories).
    session: ModelHandle<ClaudeChatSession>,
}

impl ClaudeChatPane {
    pub fn from_view(backing_view: ViewHandle<ClaudeChatView>, ctx: &mut AppContext) -> Self {
        let pane_configuration = backing_view.as_ref(ctx).pane_configuration();
        let session = backing_view.as_ref(ctx).session().clone();

        let view = ctx.add_typed_action_view(backing_view.window_id(ctx), |ctx| {
            let pane_id = PaneId::from_claude_chat_pane_ctx(ctx);
            PaneView::new(pane_id, backing_view, (), pane_configuration.clone(), ctx)
        });

        Self {
            view,
            pane_configuration,
            session,
        }
    }

    pub fn new<V: View>(snapshot: ClaudeChatPaneSnapshot, ctx: &mut ViewContext<V>) -> Self {
        let view = ctx.add_typed_action_view(|ctx| ClaudeChatView::new(snapshot, ctx));
        Self::from_view(view, ctx)
    }

    pub fn backing_view(&self, ctx: &AppContext) -> ViewHandle<ClaudeChatView> {
        self.view.as_ref(ctx).child(ctx)
    }

    pub fn session(&self) -> &ModelHandle<ClaudeChatSession> {
        &self.session
    }
}

impl PaneContent for ClaudeChatPane {
    fn id(&self) -> PaneId {
        PaneId::from_claude_chat_pane_view(&self.view)
    }

    fn attach(
        &self,
        _group: &PaneGroup,
        focus_handle: crate::pane_group::focus_state::PaneFocusHandle,
        ctx: &mut ViewContext<PaneGroup>,
    ) {
        self.view
            .update(ctx, |view, ctx| view.set_focus_handle(focus_handle, ctx));

        let backing_view = self.backing_view(ctx);
        let pane_id = self.id();
        backing_view.update(ctx, |view, ctx| view.set_attached(true, ctx));

        ctx.subscribe_to_view(&backing_view, move |pane_group, _, event, ctx| {
            let ClaudeChatViewEvent::Pane(pane_event) = event;
            pane_group.handle_pane_event(pane_id, pane_event, ctx)
        });
        ctx.subscribe_to_view(&self.view, move |group, _, event, ctx| {
            group.handle_pane_view_event(pane_id, event, ctx);
        });
    }

    fn detach(
        &self,
        _group: &PaneGroup,
        detach_type: DetachType,
        ctx: &mut ViewContext<PaneGroup>,
    ) {
        // Always unsubscribe from views.
        let backing_view = self.backing_view(ctx);
        ctx.unsubscribe_to_view(&backing_view);
        ctx.unsubscribe_to_view(&self.view);
        match detach_type {
            DetachType::Closed | DetachType::HiddenForClose => {
                backing_view.update(ctx, |view, ctx| view.set_attached(false, ctx));
            }
            DetachType::Moved => {}
        }
    }

    fn snapshot(&self, app: &AppContext) -> LeafContents {
        LeafContents::ClaudeChat(self.backing_view(app).as_ref(app).snapshot(app))
    }

    fn has_application_focus(&self, ctx: &mut ViewContext<PaneGroup>) -> bool {
        self.view.is_self_or_child_focused(ctx)
    }

    fn focus(&self, ctx: &mut ViewContext<PaneGroup>) {
        self.backing_view(ctx)
            .update(ctx, |view, ctx| view.focus(ctx));
    }

    fn shareable_link(
        &self,
        _ctx: &mut ViewContext<PaneGroup>,
    ) -> Result<ShareableLink, ShareableLinkError> {
        Ok(ShareableLink::Base)
    }

    fn pane_configuration(&self) -> ModelHandle<PaneConfiguration> {
        self.pane_configuration.clone()
    }

    fn is_pane_being_dragged(&self, ctx: &AppContext) -> bool {
        self.view.as_ref(ctx).is_being_dragged()
    }
}
