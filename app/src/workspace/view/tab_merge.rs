//! Dropping a dragged tab onto a pane of the active tab merges the tab's panes
//! into the active tab as splits, keeping their sessions running.

use pathfinder_color::ColorU;
use pathfinder_geometry::rect::RectF;
use pathfinder_geometry::vector::{Vector2F, vec2f};
use warpui::elements::{
    Border, ChildAnchor, ConstrainedBox, Container, Element, Empty, OffsetPositioning,
    PositionedElementAnchor, PositionedElementOffsetBounds,
};
use warpui::{AppContext, SingletonEntity, ViewContext};

use super::Workspace;
use crate::appearance::Appearance;
use crate::pane_group::{Direction, PaneId};

const PREVIEW_FILL_ALPHA: u8 = 40;
const PREVIEW_BORDER_WIDTH: f32 = 2.;

/// Where the dragged tab would land if it were dropped now.
#[derive(Debug, Clone, Copy)]
pub(super) struct TabMergeTarget {
    source_tab_index: usize,
    pane_id: PaneId,
    direction: Direction,
    /// The part of the target pane the merged tab would take, relative to the
    /// pane's origin.
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
        let group = self.active_tab_pane_group().as_ref(ctx);
        group.visible_pane_ids().into_iter().find_map(|pane_id| {
            let rect = ctx.element_position_by_id(pane_id.position_id())?;
            if !rect.contains_point(cursor) {
                return None;
            }
            let direction = direction_toward(rect, cursor);
            Some(TabMergeTarget {
                source_tab_index: dragged_index,
                pane_id,
                direction,
                preview: preview_rect(rect.size(), direction),
            })
        })
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

        let mut anchor = target.pane_id;
        let mut first_moved = None;
        for pane_id in source_group.as_ref(ctx).visible_pane_ids() {
            let Some(pane) =
                source_group.update(ctx, |group, ctx| group.remove_pane_for_move(&pane_id, ctx))
            else {
                continue;
            };
            active_group.update(ctx, |group, ctx| {
                group.add_pane_sibling(anchor, target.direction, pane, false, ctx);
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

    /// The highlighted part of the target pane shown while dragging.
    pub(super) fn render_tab_merge_preview(
        &self,
        app: &AppContext,
    ) -> Option<(Box<dyn Element>, OffsetPositioning)> {
        let target = self.tab_merge_target?;
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
