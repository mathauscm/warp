//! A responsive grid: as many equal columns as fit, each item stretched to the column width.

use warpui::elements::Point;
use warpui::event::DispatchedEvent;
use warpui::geometry::vector::{Vector2F, vec2f};
use warpui::{
    AfterLayoutContext, AppContext, Element, EventContext, LayoutContext, PaintContext,
    SizeConstraint,
};

use super::helpers::{grid_columns, grid_item_width};

pub struct Grid {
    children: Vec<Box<dyn Element>>,
    min_item_width: f32,
    gap: f32,
    max_columns: Option<usize>,
    /// Each child's offset from the grid's origin, from the last layout.
    offsets: Vec<Vector2F>,
    size: Option<Vector2F>,
    origin: Option<Point>,
}

impl Grid {
    pub fn new(children: Vec<Box<dyn Element>>, min_item_width: f32, gap: f32) -> Self {
        Self {
            children,
            min_item_width,
            gap,
            max_columns: None,
            offsets: Vec::new(),
            size: None,
            origin: None,
        }
    }

    pub fn with_max_columns(mut self, max_columns: usize) -> Self {
        self.max_columns = Some(max_columns);
        self
    }
}

impl Element for Grid {
    fn layout(
        &mut self,
        constraint: SizeConstraint,
        ctx: &mut LayoutContext,
        app: &AppContext,
    ) -> Vector2F {
        let available = constraint.max.x();
        let columns = grid_columns(available, self.min_item_width, self.gap, self.max_columns);
        let item_width = if available.is_finite() {
            grid_item_width(available, columns, self.gap)
        } else {
            self.min_item_width
        };
        let item_constraint =
            SizeConstraint::new(vec2f(item_width, 0.), vec2f(item_width, f32::INFINITY));

        self.offsets.clear();
        let mut y = 0.;
        for row in self.children.chunks_mut(columns) {
            let mut row_height: f32 = 0.;
            for (column, child) in row.iter_mut().enumerate() {
                let size = child.layout(item_constraint, ctx, app);
                row_height = row_height.max(size.y());
                self.offsets
                    .push(vec2f(column as f32 * (item_width + self.gap), y));
            }
            y += row_height + self.gap;
        }
        let height = (y - self.gap).max(0.);
        let width = if available.is_finite() {
            available
        } else {
            columns as f32 * item_width
        };
        let size = vec2f(width, height).max(constraint.min);
        self.size = Some(size);
        size
    }

    fn after_layout(&mut self, ctx: &mut AfterLayoutContext, app: &AppContext) {
        for child in &mut self.children {
            child.after_layout(ctx, app);
        }
    }

    fn paint(&mut self, origin: Vector2F, ctx: &mut PaintContext, app: &AppContext) {
        self.origin = Some(Point::from_vec2f(origin, ctx.scene.z_index()));
        for (child, offset) in self.children.iter_mut().zip(&self.offsets) {
            child.paint(origin + *offset, ctx, app);
        }
    }

    fn size(&self) -> Option<Vector2F> {
        self.size
    }

    fn origin(&self) -> Option<Point> {
        self.origin
    }

    fn dispatch_event(
        &mut self,
        event: &DispatchedEvent,
        ctx: &mut EventContext,
        app: &AppContext,
    ) -> bool {
        let mut handled = false;
        for child in &mut self.children {
            handled |= child.dispatch_event(event, ctx, app);
        }
        handled
    }
}

/// Gives its child the full width up to `max_width`, centered in any extra space.
pub struct MaxWidthCentered {
    child: Box<dyn Element>,
    max_width: f32,
    size: Option<Vector2F>,
}

impl MaxWidthCentered {
    pub fn new(child: Box<dyn Element>, max_width: f32) -> Self {
        Self {
            child,
            max_width,
            size: None,
        }
    }
}

impl Element for MaxWidthCentered {
    fn layout(
        &mut self,
        constraint: SizeConstraint,
        ctx: &mut LayoutContext,
        app: &AppContext,
    ) -> Vector2F {
        let width = constraint.max.x().min(self.max_width);
        let child_constraint = SizeConstraint::new(
            vec2f(width, constraint.min.y()),
            vec2f(width, constraint.max.y()),
        );
        let child_size = self.child.layout(child_constraint, ctx, app);
        let size = vec2f(
            if constraint.max.x().is_finite() {
                constraint.max.x()
            } else {
                child_size.x()
            },
            child_size.y(),
        );
        self.size = Some(size);
        size
    }

    fn after_layout(&mut self, ctx: &mut AfterLayoutContext, app: &AppContext) {
        self.child.after_layout(ctx, app);
    }

    fn paint(&mut self, origin: Vector2F, ctx: &mut PaintContext, app: &AppContext) {
        let (Some(size), Some(child_size)) = (self.size, self.child.size()) else {
            return;
        };
        let offset = ((size.x() - child_size.x()) / 2.).max(0.);
        self.child.paint(origin + vec2f(offset, 0.), ctx, app);
    }

    fn size(&self) -> Option<Vector2F> {
        self.size
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
