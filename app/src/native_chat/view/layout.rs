//! Where the chat pane puts the conversation and the side panel's cards, by the pane's width.

use std::any::Any;

use warpui::elements::Point;
use warpui::event::DispatchedEvent;
use warpui::geometry::vector::{Vector2F, vec2f};
use warpui::{
    AfterLayoutContext, AppContext, Element, EventContext, LayoutContext, PaintContext,
    SizeConstraint,
};

use crate::native_chat::design::Sizes;

/// The narrowest the conversation gets with the cards docked beside it; narrower panes open the
/// cards as a popover instead.
pub const MIN_DOCKED_CONVERSATION_WIDTH: f32 = 560.;
/// The conversation's side padding in panes narrower than [`NARROW_PANE_WIDTH`].
pub const NARROW_COLUMN_PADDING: f32 = 12.;
/// The conversation's side padding while the pane is narrower than the widest column.
pub const MEDIUM_COLUMN_PADDING: f32 = 20.;
pub const NARROW_PANE_WIDTH: f32 = 480.;
/// The corner layer's own inset; the cards add `side_panel_padding` around themselves.
pub const CORNER_INSET: f32 = 4.;

/// The widest the conversation's column gets, padding included.
pub fn conversation_max_width(sizes: &Sizes) -> f32 {
    sizes.column_max_width + 4. * sizes.column_padding
}

/// The width of the column the docked cards take at the pane's right edge.
pub fn docked_cards_width(sizes: &Sizes) -> f32 {
    sizes.side_panel_width + 2. * sizes.side_panel_padding + CORNER_INSET
}

/// The narrowest pane that fits the cards beside the conversation.
pub fn min_width_for_docked_cards(sizes: &Sizes) -> f32 {
    MIN_DOCKED_CONVERSATION_WIDTH.min(sizes.column_max_width)
        + 2. * MEDIUM_COLUMN_PADDING
        + docked_cards_width(sizes)
}

/// Whether the cards dock beside the conversation; otherwise they only open as a popover.
pub fn has_room_for_cards(pane_width: f32, sizes: &Sizes) -> bool {
    pane_width >= min_width_for_docked_cards(sizes)
}

/// Space left of the conversation while the cards are docked, so that the conversation stays
/// centered in the whole pane when it has room, and slides left of center as the pane narrows
/// rather than under the cards.
pub fn docked_left_inset(pane_width: f32, sizes: &Sizes) -> f32 {
    let cards = docked_cards_width(sizes);
    (pane_width - cards - conversation_max_width(sizes)).clamp(0., cards)
}

/// Lays its child out in what's left of the width after a left inset computed from that width.
pub struct LeftInset {
    child: Box<dyn Element>,
    inset_for_width: Box<dyn Fn(f32) -> f32>,
    inset: f32,
    size: Option<Vector2F>,
    origin: Option<Point>,
}

impl LeftInset {
    pub fn new(inset_for_width: impl Fn(f32) -> f32 + 'static, child: Box<dyn Element>) -> Self {
        Self {
            child,
            inset_for_width: Box::new(inset_for_width),
            inset: 0.,
            size: None,
            origin: None,
        }
    }
}

impl Element for LeftInset {
    fn layout(
        &mut self,
        constraint: SizeConstraint,
        ctx: &mut LayoutContext,
        app: &AppContext,
    ) -> Vector2F {
        let max_width = constraint.max.x();
        self.inset = if max_width.is_finite() {
            (self.inset_for_width)(max_width).clamp(0., max_width)
        } else {
            0.
        };
        let child_constraint = SizeConstraint::new(
            vec2f(
                (constraint.min.x() - self.inset).max(0.),
                constraint.min.y(),
            ),
            vec2f(max_width - self.inset, constraint.max.y()),
        );
        let child_size = self.child.layout(child_constraint, ctx, app);
        let size = vec2f(child_size.x() + self.inset, child_size.y());
        self.size = Some(size);
        size
    }

    fn after_layout(&mut self, ctx: &mut AfterLayoutContext, app: &AppContext) {
        self.child.after_layout(ctx, app);
    }

    fn paint(&mut self, origin: Vector2F, ctx: &mut PaintContext, app: &AppContext) {
        self.origin = Some(Point::from_vec2f(origin, ctx.scene.z_index()));
        self.child.paint(origin + vec2f(self.inset, 0.), ctx, app);
    }

    fn size(&self) -> Option<Vector2F> {
        self.size
    }

    fn origin(&self) -> Option<Point> {
        self.origin
    }

    fn parent_data(&self) -> Option<&dyn Any> {
        self.child.parent_data()
    }

    fn dispatch_event(
        &mut self,
        event: &DispatchedEvent,
        ctx: &mut EventContext,
        app: &AppContext,
    ) -> bool {
        self.child.dispatch_event(event, ctx, app)
    }
}
