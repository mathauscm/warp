//! The transcript's scroller: a viewported list that only lays out the rows on screen, stays at
//! the bottom while Claude writes unless the user scrolls up, and re-measures its rows when its
//! width changes.

use std::cell::Cell;
use std::rc::Rc;

use pathfinder_geometry::vector::{Vector2F, vec2f};
use warpui::elements::new_scrollable::{NewScrollableElement, ScrollableAxis};
use warpui::elements::{Axis, List, ListState, Point, ScrollData, SelectableElement};
use warpui::event::DispatchedEvent;
use warpui::units::{IntoPixels, Pixels};
use warpui::{
    AfterLayoutContext, AppContext, Element, EventContext, LayoutContext, PaintContext,
    SizeConstraint,
};

/// How close to the end counts as being at the bottom.
const BOTTOM_TOLERANCE: f32 = 4.;

/// What the view and its scroller share across frames.
#[derive(Debug)]
pub(super) struct ViewportState {
    /// Whether to keep the last row in view as rows grow.
    pub stick_to_bottom: Cell<bool>,
    pub row_count: Cell<usize>,
    width: Cell<f32>,
    /// The list's scroll position, visible height and content height at the last layout or
    /// scroll, in pixels.
    scroll_start: Cell<f32>,
    visible: Cell<f32>,
    total: Cell<f32>,
}

impl Default for ViewportState {
    fn default() -> Self {
        Self {
            stick_to_bottom: Cell::new(true),
            row_count: Cell::new(0),
            width: Cell::new(0.),
            scroll_start: Cell::new(0.),
            visible: Cell::new(0.),
            total: Cell::new(0.),
        }
    }
}

impl ViewportState {
    /// Forgets the measured width, so every row is measured again on the next layout.
    pub fn remeasure(&self) {
        self.width.set(0.);
    }

    /// How far the list was scrolled at the last layout or scroll, in pixels.
    pub fn scroll_start(&self) -> f32 {
        self.scroll_start.get()
    }

    /// The list's visible height at the last layout, in pixels.
    pub fn visible_height(&self) -> f32 {
        self.visible.get()
    }

    /// Whether, at the last layout or scroll, rows went on below the visible ones.
    pub fn has_more_below(&self) -> bool {
        self.total.get() - self.visible.get() - self.scroll_start.get() > BOTTOM_TOLERANCE
    }
}

pub(super) struct Viewport {
    list: List<()>,
    list_state: ListState<()>,
    state: Rc<ViewportState>,
}

impl Viewport {
    pub fn new(list_state: ListState<()>, state: Rc<ViewportState>) -> Self {
        Self {
            list: List::new(list_state.clone()),
            list_state,
            state,
        }
    }

    fn record_scroll_data(&self, app: &AppContext) {
        if let Some(data) = NewScrollableElement::scroll_data(&self.list, Axis::Vertical, app) {
            self.state.scroll_start.set(data.scroll_start.as_f32());
            self.state.visible.set(data.visible_px.as_f32());
            self.state.total.set(data.total_size.as_f32());
        }
    }
}

impl Element for Viewport {
    fn layout(
        &mut self,
        constraint: SizeConstraint,
        ctx: &mut LayoutContext,
        app: &AppContext,
    ) -> Vector2F {
        let row_count = self.state.row_count.get();
        let width = constraint.max.x();
        if (self.state.width.get() - width).abs() > 0.5 {
            self.state.width.set(width);
            for index in 0..row_count {
                self.list_state.invalidate_height_for_index(index);
            }
        }
        if self.state.stick_to_bottom.get() && row_count > 0 {
            // Past the end on purpose: the list clamps to its real bottom once it measured the rows.
            self.list_state
                .scroll_to_with_offset(row_count - 1, f32::MAX.sqrt().into_pixels());
        }
        let size = self.list.layout(constraint, ctx, app);
        self.record_scroll_data(app);
        size
    }

    fn after_layout(&mut self, ctx: &mut AfterLayoutContext, app: &AppContext) {
        self.list.after_layout(ctx, app);
    }

    fn paint(&mut self, origin: Vector2F, ctx: &mut PaintContext, app: &AppContext) {
        self.list.paint(origin, ctx, app);
    }

    fn size(&self) -> Option<Vector2F> {
        self.list.size()
    }

    fn origin(&self) -> Option<Point> {
        self.list.origin()
    }

    fn dispatch_event(
        &mut self,
        event: &DispatchedEvent,
        ctx: &mut EventContext,
        app: &AppContext,
    ) -> bool {
        self.list.dispatch_event(event, ctx, app)
    }

    fn as_selectable_element(&self) -> Option<&dyn SelectableElement> {
        self.list.as_selectable_element()
    }
}

impl NewScrollableElement for Viewport {
    fn axis(&self) -> ScrollableAxis {
        ScrollableAxis::Vertical
    }

    fn scroll_data(&self, axis: Axis, app: &AppContext) -> Option<ScrollData> {
        NewScrollableElement::scroll_data(&self.list, axis, app)
    }

    fn axis_should_handle_scroll_wheel(&self, axis: Axis) -> bool {
        self.list.axis_should_handle_scroll_wheel(axis)
    }

    fn scroll(&mut self, delta: Pixels, axis: Axis, ctx: &mut EventContext) {
        let was_sticking = self.state.stick_to_bottom.get();
        NewScrollableElement::scroll(&mut self.list, delta, axis, ctx);
        // The same clamping the list applies, since the event has no app context to ask it.
        let state = &self.state;
        let max_start = (state.total.get() - state.visible.get()).max(0.);
        let start = (state.scroll_start.get() - delta.as_f32()).clamp(0., max_start);
        state.scroll_start.set(start);
        state
            .stick_to_bottom
            .set(start >= max_start - BOTTOM_TOLERANCE);
        if was_sticking != self.state.stick_to_bottom.get() {
            ctx.notify();
        }
    }
}

/// An overlay placed over the list's content at render, kept on that content when the list
/// scrolls before it's painted, as when it sticks to the bottom while Claude writes.
pub(super) struct ScrollFollow {
    child: Box<dyn Element>,
    state: Rc<ViewportState>,
    /// The list's scroll position when the overlay was placed.
    placed_at: f32,
}

impl ScrollFollow {
    pub fn new(child: Box<dyn Element>, state: Rc<ViewportState>, placed_at: f32) -> Self {
        Self {
            child,
            state,
            placed_at,
        }
    }
}

impl Element for ScrollFollow {
    fn layout(
        &mut self,
        constraint: SizeConstraint,
        ctx: &mut LayoutContext,
        app: &AppContext,
    ) -> Vector2F {
        self.child.layout(constraint, ctx, app)
    }

    fn after_layout(&mut self, ctx: &mut AfterLayoutContext, app: &AppContext) {
        self.child.after_layout(ctx, app);
    }

    fn paint(&mut self, origin: Vector2F, ctx: &mut PaintContext, app: &AppContext) {
        let scrolled = self.state.scroll_start() - self.placed_at;
        self.child.paint(origin - vec2f(0., scrolled), ctx, app);
    }

    fn size(&self) -> Option<Vector2F> {
        self.child.size()
    }

    fn origin(&self) -> Option<Point> {
        self.child.origin()
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
