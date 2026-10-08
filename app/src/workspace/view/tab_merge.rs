//! Dropping a dragged tab onto a pane of the active tab merges the tab's panes
//! into the active tab as splits, keeping their sessions running. The same
//! works when dragging a group that holds a single tab, and dropping a Claude
//! thread from the Threads panel opens it in a new split there.

use pathfinder_color::ColorU;
use pathfinder_geometry::rect::RectF;
use pathfinder_geometry::vector::{Vector2F, vec2f};
use warpui::elements::{
    Border, ChildAnchor, ConstrainedBox, Container, Element, Empty, OffsetPositioning,
    PositionedElementAnchor, PositionedElementOffsetBounds,
};
use warpui::{AppContext, EntityId, SingletonEntity, ViewContext};

use super::Workspace;
use crate::appearance::Appearance;
use crate::pane_group::{Direction, PaneId};
use crate::terminal::cli_agent_sessions::CLIAgentSessionsModel;
use crate::workspace::tab_group::TabGroupId;

const PREVIEW_FILL_ALPHA: u8 = 40;
const PREVIEW_BORDER_WIDTH: f32 = 2.;

/// Where the dragged tab would land if it were dropped now.
#[derive(Debug, Clone, Copy)]
pub(super) struct TabMergeTarget {
    source_tab_index: usize,
    pane: PaneDropTarget,
}

/// A pane of the active tab and the side of it a drop would split.
#[derive(Debug, Clone, Copy)]
pub(super) struct PaneDropTarget {
    pane_id: PaneId,
    direction: Direction,
    /// The part of the target pane the dropped content would take, relative
    /// to the pane's origin.
    preview: RectF,
}

impl Workspace {
    /// Called on every move of a tab drag. Returns whether the cursor is over a
    /// pane of the active tab, in which case the caller skips its reorder and
    /// detach-to-window handling.
    pub(super) fn update_tab_merge_target(
        &mut self,
        dragged_index: usize,
        drag_position: RectF,
        ctx: &mut ViewContext<Self>,
    ) -> bool {
        let target = self.tab_merge_target_at(dragged_index, drag_position.center(), ctx);
        let is_over_pane = target.is_some();
        if is_over_pane || self.tab_merge_target.is_some() {
            self.tab_merge_target = target;
            ctx.notify();
        }
        is_over_pane
    }

    /// Takes the pending target, if any, when the tab drag ends.
    pub(super) fn take_tab_merge_target(&mut self) -> Option<TabMergeTarget> {
        self.tab_merge_target.take()
    }

    fn tab_merge_target_at(
        &self,
        dragged_index: usize,
        cursor: Vector2F,
        ctx: &ViewContext<Self>,
    ) -> Option<TabMergeTarget> {
        if dragged_index == self.active_tab_index || dragged_index >= self.tabs.len() {
            return None;
        }
        Some(TabMergeTarget {
            source_tab_index: dragged_index,
            pane: self.pane_drop_target_at(cursor, ctx)?,
        })
    }

    /// The pane of the active tab under the cursor, split on its closest edge.
    fn pane_drop_target_at(
        &self,
        cursor: Vector2F,
        ctx: &ViewContext<Self>,
    ) -> Option<PaneDropTarget> {
        let group = self.active_tab_pane_group().as_ref(ctx);
        group.visible_pane_ids().into_iter().find_map(|pane_id| {
            let rect = ctx.element_position_by_id(pane_id.position_id())?;
            if !rect.contains_point(cursor) {
                return None;
            }
            let direction = direction_toward(rect, cursor);
            Some(PaneDropTarget {
                pane_id,
                direction,
                preview: preview_rect(rect.size(), direction),
            })
        })
    }

    /// Called on every move of a group drag. A group holding a single tab
    /// merges like that tab when dropped over a pane of the active tab;
    /// returns whether the cursor is over one, so the caller skips reordering.
    pub(super) fn update_group_merge_target(
        &mut self,
        group_id: TabGroupId,
        cursor: Vector2F,
        ctx: &mut ViewContext<Self>,
    ) -> bool {
        let sole_member = self
            .tabs
            .iter()
            .position(|tab| tab.group_id == Some(group_id))
            .filter(|_| super::group_has_single_member(&self.tabs, group_id));
        match sole_member {
            Some(index) => {
                self.update_tab_merge_target(index, RectF::new(cursor, Vector2F::zero()), ctx)
            }
            None => false,
        }
    }

    /// Called on every move of a Claude thread dragged from the Threads panel.
    pub(super) fn update_thread_split_target(
        &mut self,
        cursor: Vector2F,
        ctx: &mut ViewContext<Self>,
    ) {
        let target = self.pane_drop_target_at(cursor, ctx);
        if target.is_some() || self.thread_split_target.is_some() {
            self.thread_split_target = target;
            ctx.notify();
        }
    }

    /// Ends a thread drag over a pane of the active tab. A thread already
    /// running in this window moves its pane next to the target; otherwise
    /// the target is split on the highlighted side and `command` (which
    /// resumes the thread) runs there.
    pub(super) fn drop_thread(
        &mut self,
        session_id: &str,
        command: Option<&str>,
        ctx: &mut ViewContext<Self>,
    ) {
        let Some(target) = self.thread_split_target.take() else {
            return;
        };
        let running_in = CLIAgentSessionsModel::as_ref(ctx)
            .session_by_agent_session_id(session_id)
            .map(|(terminal_view_id, _)| terminal_view_id);
        if let Some(terminal_view_id) = running_in {
            self.move_terminal_pane_to(terminal_view_id, target, ctx);
        } else if let Some(command) = command {
            let terminal = self.active_tab_pane_group().update(ctx, |group, ctx| {
                group.split_terminal_pane_from(target.pane_id, target.direction, ctx)
            });
            if let Some(terminal) = terminal {
                terminal.update(ctx, |terminal, ctx| {
                    terminal.set_hidden_pending_command(command, ctx);
                });
            }
        }
        ctx.dispatch_global_action("workspace:save_app", ());
        ctx.notify();
    }

    /// Moves the pane showing `terminal_view_id`, from whichever tab of this
    /// window holds it, next to the target pane. A tab left empty closes.
    fn move_terminal_pane_to(
        &mut self,
        terminal_view_id: EntityId,
        target: PaneDropTarget,
        ctx: &mut ViewContext<Self>,
    ) {
        let Some((source_group, pane_id)) = self.tabs.iter().find_map(|tab| {
            let pane_id = tab
                .pane_group
                .as_ref(ctx)
                .find_pane_id_for_terminal_view(terminal_view_id, ctx)?;
            Some((tab.pane_group.clone(), pane_id))
        }) else {
            return;
        };
        if pane_id == target.pane_id {
            return;
        }
        let active_group = self.active_tab_pane_group().clone();
        let is_other_tab = source_group.id() != active_group.id();
        if is_other_tab {
            // Panes kept hidden for undo-close would keep the source tab open.
            source_group.update(ctx, |group, ctx| group.clear_hidden_closed_panes(ctx));
        }
        let Some(pane) =
            source_group.update(ctx, |group, ctx| group.remove_pane_for_move(&pane_id, ctx))
        else {
            return;
        };
        active_group.update(ctx, |group, ctx| {
            group.add_pane_sibling(target.pane_id, target.direction, pane, false, ctx);
        });
        if is_other_tab
            && source_group.as_ref(ctx).visible_pane_ids().is_empty()
            && let Some(index) = self
                .tabs
                .iter()
                .position(|tab| tab.pane_group.id() == source_group.id())
        {
            self.close_tab(index, true, false, ctx);
        }
        active_group.update(ctx, |group, ctx| group.focus_pane_by_id(pane_id, ctx));
    }

    /// Moves every visible pane of the dragged tab next to the target pane of
    /// the active tab. The emptied source tab is closed.
    pub(super) fn merge_tab_into_pane(
        &mut self,
        target: TabMergeTarget,
        ctx: &mut ViewContext<Self>,
    ) {
        if target.source_tab_index == self.active_tab_index {
            return;
        }
        let Some(source_group) = self.get_pane_group_view(target.source_tab_index).cloned() else {
            return;
        };
        let active_group = self.active_tab_pane_group().clone();

        // Panes kept hidden for undo-close still count as panes of the source
        // group, and would keep it from closing once its visible panes leave.
        source_group.update(ctx, |group, ctx| group.clear_hidden_closed_panes(ctx));

        let mut anchor = target.pane.pane_id;
        let mut first_moved = None;
        for pane_id in source_group.as_ref(ctx).visible_pane_ids() {
            let Some(pane) =
                source_group.update(ctx, |group, ctx| group.remove_pane_for_move(&pane_id, ctx))
            else {
                continue;
            };
            active_group.update(ctx, |group, ctx| {
                group.add_pane_sibling(anchor, target.pane.direction, pane, false, ctx);
            });
            anchor = pane_id;
            first_moved.get_or_insert(pane_id);
        }

        // The source group normally closes its tab when its last pane leaves;
        // close it here too in case something else kept it alive.
        if let Some(index) = self
            .tabs
            .iter()
            .position(|tab| tab.pane_group.id() == source_group.id())
            && source_group.as_ref(ctx).visible_pane_ids().is_empty()
        {
            self.close_tab(index, true, false, ctx);
        }

        if let Some(pane_id) = first_moved {
            active_group.update(ctx, |group, ctx| group.focus_pane_by_id(pane_id, ctx));
        }
        ctx.dispatch_global_action("workspace:save_app", ());
        ctx.notify();
    }

    /// The highlighted part of the target pane shown while dragging a tab or
    /// a thread.
    pub(super) fn render_tab_merge_preview(
        &self,
        app: &AppContext,
    ) -> Option<(Box<dyn Element>, OffsetPositioning)> {
        let target = self
            .tab_merge_target
            .map(|target| target.pane)
            .or(self.thread_split_target)?;
        let accent = Appearance::as_ref(app).theme().accent().into_solid();
        let element = ConstrainedBox::new(
            Container::new(Empty::new().finish())
                .with_background_color(ColorU {
                    a: PREVIEW_FILL_ALPHA,
                    ..accent
                })
                .with_border(Border::all(PREVIEW_BORDER_WIDTH).with_border_color(accent))
                .finish(),
        )
        .with_width(target.preview.width())
        .with_height(target.preview.height())
        .finish();
        let positioning = OffsetPositioning::offset_from_save_position_element(
            target.pane_id.position_id(),
            target.preview.origin(),
            PositionedElementOffsetBounds::WindowByPosition,
            PositionedElementAnchor::TopLeft,
            ChildAnchor::TopLeft,
        );
        Some((element, positioning))
    }
}

/// The pane edge closest to the cursor.
fn direction_toward(pane: RectF, cursor: Vector2F) -> Direction {
    let offset = cursor - pane.center();
    let x = offset.x() / pane.width().max(1.);
    let y = offset.y() / pane.height().max(1.);
    if y.abs() > x.abs() {
        if y > 0. {
            Direction::Down
        } else {
            Direction::Up
        }
    } else if x > 0. {
        Direction::Right
    } else {
        Direction::Left
    }
}

fn preview_rect(size: Vector2F, direction: Direction) -> RectF {
    let (width, height) = (size.x(), size.y());
    match direction {
        Direction::Left => RectF::new(vec2f(0., 0.), vec2f(width / 2., height)),
        Direction::Right => RectF::new(vec2f(width / 2., 0.), vec2f(width / 2., height)),
        Direction::Up => RectF::new(vec2f(0., 0.), vec2f(width, height / 2.)),
        Direction::Down => RectF::new(vec2f(0., height / 2.), vec2f(width, height / 2.)),
    }
}
