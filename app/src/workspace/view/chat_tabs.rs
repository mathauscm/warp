//! Where native chats open, and the chats that close on their own.
//!
//! A conversation opens in the most recently used tab holding a lone chat that can show another
//! conversation without losing anything (see [`ChatActivity::can_be_replaced`]), so navigating
//! threads doesn't pile up tabs. A chat that's working, or has unsent text, keeps its tab and the
//! conversation opens in a new one. Chats in a split stay where they are. A chat that never sent
//! anything closes once its tab is left.

use std::collections::HashMap;
use std::sync::Arc;

use warpui::{AppContext, EntityId, SingletonEntity, ViewContext, ViewHandle};

use super::Workspace;
use crate::app_state::{ClaudeChatPaneSnapshot, LeafContents, LeafSnapshot, PaneNodeSnapshot};
use crate::native_chat::registry::ChatActivity;
use crate::native_chat::{ChatWorkspace, ClaudeChatView};
use crate::pane_group::{ClaudeChatPane, PaneGroup, PaneId, PanesLayout};
use crate::workspace::WorkspaceRegistry;
use crate::workspace::util::PaneViewLocator;

impl Workspace {
    /// Opens a native Claude chat on `snapshot`: focuses the pane already showing its
    /// conversation, or shows it in a chat tab to reuse, or in a new tab. With `group`, the tab
    /// moves to the tab group of that name.
    pub(super) fn open_claude_chat(
        &mut self,
        snapshot: ClaudeChatPaneSnapshot,
        group: Option<&str>,
        ctx: &mut ViewContext<Self>,
    ) {
        if let Some(locator) = snapshot
            .session_id
            .as_deref()
            .and_then(|session_id| self.find_claude_chat_pane(session_id, ctx))
        {
            self.focus_pane(locator, ctx);
        } else if !self.show_in_reused_chat_tab(snapshot.clone(), group.is_none(), ctx) {
            let panes_layout =
                PanesLayout::Snapshot(Box::new(PaneNodeSnapshot::Leaf(LeafSnapshot {
                    is_focused: true,
                    custom_vertical_tabs_title: None,
                    contents: LeafContents::ClaudeChat(snapshot),
                })));
            self.add_tab_with_pane_layout(panes_layout, Arc::new(HashMap::new()), None, ctx);
        }
        if let Some(name) = group {
            self.move_active_tab_to_named_group(name, ctx);
        }
    }

    /// Swaps the chat of the chat tab to reuse for one on `snapshot`, and activates that tab.
    /// Returns false when there's no tab to reuse. With `leave_group_on_move`, a tab whose chat
    /// moves to another folder leaves its tab group, which was named after the old folder.
    fn show_in_reused_chat_tab(
        &mut self,
        snapshot: ClaudeChatPaneSnapshot,
        leave_group_on_move: bool,
        ctx: &mut ViewContext<Self>,
    ) -> bool {
        let chat_tabs = self.chat_tabs(ctx);
        let activities = chat_tabs
            .iter()
            .map(|(id, (_, view))| (*id, view.as_ref(ctx).activity(ctx)))
            .collect();
        let Some(pane_group_id) = chat_tab_to_reuse(&self.tab_mru_order, &activities) else {
            return false;
        };
        let Some((pane_id, view)) = chat_tabs.get(&pane_group_id).cloned() else {
            return false;
        };
        let Some(tab_index) = self.tab_index_of(pane_group_id) else {
            return false;
        };
        let session = view.as_ref(ctx).session().as_ref(ctx);
        let workspace_id = |workspace: Option<&ChatWorkspace>| workspace.map(|w| w.id.clone());
        let moves_folder = session.cwd() != snapshot.cwd
            || workspace_id(session.workspace()) != workspace_id(snapshot.workspace.as_ref());
        let pane_group = self.tabs[tab_index].pane_group.clone();
        let pane = ClaudeChatPane::new(snapshot, ctx);
        let replaced = pane_group.update(ctx, |group, ctx| {
            group.replace_pane(pane_id, pane, false, ctx)
        });
        if !replaced {
            return false;
        }
        self.activate_tab_internal(tab_index, ctx);
        if leave_group_on_move && moves_folder {
            self.remove_tab_from_group(tab_index, ctx);
        }
        ctx.dispatch_global_action("workspace:save_app", ());
        ctx.notify();
        true
    }

    /// The pane showing the native chat of `session_id`, in any tab of this window.
    pub(super) fn find_claude_chat_pane(
        &self,
        session_id: &str,
        ctx: &AppContext,
    ) -> Option<PaneViewLocator> {
        self.tabs.iter().find_map(|tab| {
            let pane_group = tab.pane_group.as_ref(ctx);
            pane_group.pane_ids().find_map(|pane_id| {
                let view = chat_view(pane_group, pane_id, ctx)?;
                (view.as_ref(ctx).session().as_ref(ctx).session_id() == session_id).then_some(
                    PaneViewLocator {
                        pane_group_id: tab.pane_group.id(),
                        pane_id,
                    },
                )
            })
        })
    }

    /// The tabs that hold a lone native chat, by pane group.
    fn chat_tabs(
        &self,
        app: &AppContext,
    ) -> HashMap<EntityId, (PaneId, ViewHandle<ClaudeChatView>)> {
        self.tabs
            .iter()
            .filter_map(|tab| {
                let chat = sole_chat(tab.pane_group.as_ref(app), app)?;
                Some((tab.pane_group.id(), chat))
            })
            .collect()
    }

    fn tab_index_of(&self, pane_group_id: EntityId) -> Option<usize> {
        self.tabs
            .iter()
            .position(|tab| tab.pane_group.id() == pane_group_id)
    }

    /// Closes the unused chats left behind once the active tab changes. Deferred, since tabs
    /// change while they're being added and removed.
    pub(super) fn schedule_unused_chat_cleanup(&mut self, ctx: &mut ViewContext<Self>) {
        ctx.spawn(async {}, |me, (), ctx| me.close_unused_chats(ctx));
    }

    /// Closes the tabs outside the active one that show nothing but never-used chats.
    fn close_unused_chats(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(active) = self
            .tabs
            .get(self.active_tab_index)
            .map(|tab| tab.pane_group.id())
        else {
            return;
        };
        let closing = unused_chat_tabs_to_close(active, &self.panes_on_screen(ctx));
        if closing.is_empty() {
            return;
        }
        for pane_group_id in closing {
            if let Some(index) = self.tab_index_of(pane_group_id) {
                self.remove_tab(index, false, true, ctx);
            }
        }
        ctx.notify();
    }

    /// Every pane on screen in this window by its tab, with what it holds when it's a native chat.
    fn panes_on_screen(&self, app: &AppContext) -> Vec<(EntityId, Option<ChatActivity>)> {
        self.tabs
            .iter()
            .flat_map(|tab| {
                let pane_group = tab.pane_group.as_ref(app);
                pane_group
                    .visible_pane_ids()
                    .into_iter()
                    .map(move |pane_id| {
                        let activity = chat_view(pane_group, pane_id, app)
                            .map(|view| view.as_ref(app).activity(app));
                        (tab.pane_group.id(), activity)
                    })
            })
            .collect()
    }

    /// Closes the native chat showing `session_id`, in whichever window holds it, ending its
    /// Claude process.
    pub(super) fn close_claude_chat(&mut self, session_id: &str, ctx: &mut ViewContext<Self>) {
        if self.close_local_claude_chat(session_id, ctx) {
            return;
        }
        let current_window = ctx.window_id();
        for (window_id, workspace) in WorkspaceRegistry::as_ref(ctx).all_workspaces(ctx) {
            if window_id != current_window
                && workspace.update(ctx, |workspace, ctx| {
                    workspace.close_local_claude_chat(session_id, ctx)
                })
            {
                return;
            }
        }
    }

    /// Closes the native chat showing `session_id` in this window, if any. Its tab closes with it
    /// unless it's the window's last, which shows a new chat in the same folder instead.
    fn close_local_claude_chat(&mut self, session_id: &str, ctx: &mut ViewContext<Self>) -> bool {
        let Some(locator) = self.find_claude_chat_pane(session_id, ctx) else {
            return false;
        };
        let Some(tab_index) = self.tab_index_of(locator.pane_group_id) else {
            return false;
        };
        let pane_group = self.tabs[tab_index].pane_group.clone();
        let Some(view) = chat_view(pane_group.as_ref(ctx), locator.pane_id, ctx) else {
            return false;
        };
        let is_sole_pane = pane_group.as_ref(ctx).visible_pane_ids() == [locator.pane_id];
        let is_last_tab = is_sole_pane && self.tabs.len() == 1;
        if is_last_tab && view.as_ref(ctx).activity(ctx) == ChatActivity::Unused {
            return true;
        }
        let session = view.as_ref(ctx).session().clone();
        session.as_ref(ctx).kill();
        if is_last_tab {
            let mut snapshot = ClaudeChatPaneSnapshot::new(session.as_ref(ctx).cwd().to_path_buf());
            snapshot.workspace = session.as_ref(ctx).workspace().cloned();
            let pane = ClaudeChatPane::new(snapshot, ctx);
            pane_group.update(ctx, |group, ctx| {
                group.replace_pane(locator.pane_id, pane, false, ctx)
            });
        } else if is_sole_pane {
            self.remove_tab(tab_index, false, true, ctx);
        } else {
            pane_group.update(ctx, |group, ctx| group.close_pane(locator.pane_id, ctx));
        }
        ctx.dispatch_global_action("workspace:save_app", ());
        ctx.notify();
        true
    }
}

fn chat_view(
    pane_group: &PaneGroup,
    pane_id: PaneId,
    app: &AppContext,
) -> Option<ViewHandle<ClaudeChatView>> {
    let pane = pane_group.downcast_pane_by_id::<ClaudeChatPane>(pane_id)?;
    Some(pane.backing_view(app))
}

/// The native chat a tab shows, when it's the only pane on screen.
fn sole_chat(
    pane_group: &PaneGroup,
    app: &AppContext,
) -> Option<(PaneId, ViewHandle<ClaudeChatView>)> {
    let visible = pane_group.visible_pane_ids();
    let [pane_id] = visible[..] else {
        return None;
    };
    Some((pane_id, chat_view(pane_group, pane_id, app)?))
}

/// The chat tab a conversation opens in: the most recently used one that can show another
/// conversation.
fn chat_tab_to_reuse(
    mru_order: &[EntityId],
    chat_tabs: &HashMap<EntityId, ChatActivity>,
) -> Option<EntityId> {
    mru_order
        .iter()
        .find(|id| {
            chat_tabs
                .get(id)
                .is_some_and(|activity| activity.can_be_replaced())
        })
        .copied()
}

/// The tabs to close once `active_tab` is shown: the others that show nothing but never-used
/// chats. `panes` holds every pane on screen by its tab, with what it holds when it's a native
/// chat. A chat sharing its tab with anything else stays, since closing a pane of a tab in the
/// background moves that tab's focus around.
fn unused_chat_tabs_to_close(
    active_tab: EntityId,
    panes: &[(EntityId, Option<ChatActivity>)],
) -> Vec<EntityId> {
    let mut tabs: Vec<EntityId> = Vec::new();
    for (tab, _) in panes {
        if !tabs.contains(tab) {
            tabs.push(*tab);
        }
    }
    tabs.retain(|tab| {
        *tab != active_tab
            && panes
                .iter()
                .filter(|(pane_tab, _)| pane_tab == tab)
                .all(|(_, activity)| *activity == Some(ChatActivity::Unused))
    });
    tabs
}

#[cfg(test)]
#[path = "chat_tabs_tests.rs"]
mod tests;
