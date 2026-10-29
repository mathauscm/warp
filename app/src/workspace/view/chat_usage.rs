//! Tells the top bar's plan usage readout which native chat is in the focused pane.

use warpui::{AppContext, ViewContext, ViewHandle};

use super::Workspace;
use crate::native_chat::ClaudeChatView;
use crate::pane_group::ClaudeChatPane;

impl Workspace {
    pub(super) fn sync_chat_usage_bar(&mut self, ctx: &mut ViewContext<Self>) {
        let chat = self.focused_chat_view(ctx);
        self.chat_usage_bar
            .update(ctx, |bar, ctx| bar.set_focused_chat(chat.as_ref(), ctx));
    }

    fn focused_chat_view(&self, app: &AppContext) -> Option<ViewHandle<ClaudeChatView>> {
        let pane_group = self.active_tab_pane_group().as_ref(app);
        let pane =
            pane_group.downcast_pane_by_id::<ClaudeChatPane>(pane_group.focused_pane_id(app))?;
        Some(pane.backing_view(app))
    }
}
