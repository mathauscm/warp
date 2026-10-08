//! VS Code-style minimap: a scaled-down overview of the file next to the
//! editor, with a slider for the visible part. Clicking or dragging on it
//! scrolls the editor.

use std::any::Any;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pathfinder_color::ColorU;
use warp_editor::render::model::RenderState;
use warpui::elements::{DispatchEventResult, EventHandler, Point};
use warpui::event::DispatchedEvent;
use warpui::geometry::rect::RectF;
use warpui::geometry::vector::{Vector2F, vec2f};
use warpui::{
    AfterLayoutContext, AppContext, ClipBounds, Element, EventContext, LayoutContext, ModelHandle,
    PaintContext, SizeConstraint,
};

use crate::code::editor::view::CodeEditorViewAction;

pub const MINIMAP_WIDTH: f32 = 90.;
/// Height of one line and width of one character in the minimap, like VS
/// Code's default (scale 1, blocks instead of characters).
const LINE_HEIGHT: f32 = 2.;
const CHAR_WIDTH: f32 = 1.;
const LEFT_PADDING: f32 = 6.;
const MAX_COLUMNS: usize = ((MINIMAP_WIDTH - LEFT_PADDING) / CHAR_WIDTH) as usize;
pub const MAX_LINES: usize = 20_000;
const TAB_WIDTH: usize = 4;
const TOKEN_ALPHA: u8 = 190;
const SLIDER_ALPHA: u8 = 30;
const SLIDER_DRAG_ALPHA: u8 = 55;

/// A run of same-colored, non-blank characters on one line.
#[derive(Debug, Clone, Copy)]
pub struct MinimapRun {
    column: u16,
    len: u16,
    color: ColorU,
}

pub type MinimapLines = Arc<Vec<Vec<MinimapRun>>>;

/// Builds the minimap content from the file text and a color lookup by char
/// offset (offsets start at 1, matching the editor buffer).
pub fn build_lines(
    text: &str,
    color_at: impl Fn(usize) -> Option<ColorU>,
    default_color: ColorU,
) -> MinimapLines {
    let mut lines = Vec::new();
    let mut runs: Vec<MinimapRun> = Vec::new();
    let mut column = 0usize;
    for (index, c) in text.chars().enumerate() {
        if c == '\n' {
            lines.push(std::mem::take(&mut runs));
            column = 0;
            if lines.len() >= MAX_LINES {
                break;
            }
            continue;
        }
        if column >= MAX_COLUMNS {
            continue;
        }
        if c == '\t' {
            column += TAB_WIDTH - column % TAB_WIDTH;
            continue;
        }
        if c.is_whitespace() {
            column += 1;
            continue;
        }
        let color = color_at(index + 1).unwrap_or(default_color);
        match runs.last_mut() {
            Some(last)
                if last.color == color && last.column as usize + last.len as usize == column =>
            {
                last.len += 1;
            }
            _ => runs.push(MinimapRun {
                column: column as u16,
                len: 1,
                color,
            }),
        }
        column += 1;
    }
    lines.push(runs);
    Arc::new(lines)
}

/// Layout numbers shared between painting and mouse handling.
#[derive(Debug, Clone, Copy)]
struct Geometry {
    bounds: RectF,
    /// How far the minimap content is scrolled, in minimap pixels.
    offset: f32,
    slider_height: f32,
    /// Editor scroll range in editor pixels.
    max_scroll_top: f32,
    /// Editor pixels per minimap line.
    editor_line_height: f32,
    viewport_height: f32,
    fits: bool,
}

impl Geometry {
    /// Editor scroll position that puts the minimap point `y` (window
    /// coordinates) under the middle of the slider.
    fn scroll_top_for(&self, y: f32) -> f32 {
        let local_y = y - self.bounds.min_y();
        let scroll_top = if self.fits {
            let line = (local_y + self.offset) / LINE_HEIGHT;
            line * self.editor_line_height - self.viewport_height / 2.
        } else {
            let track = (self.bounds.height() - self.slider_height).max(1.);
            let fraction = (local_y - self.slider_height / 2.) / track;
            fraction * self.max_scroll_top
        };
        scroll_top.clamp(0., self.max_scroll_top.max(0.))
    }
}

/// The minimap with its click and drag handling. `dragging` belongs to the
/// editor view, so a drag survives the re-renders its own scrolling causes.
pub fn render(
    lines: MinimapLines,
    render_state: ModelHandle<RenderState>,
    background: Option<ColorU>,
    slider_color: ColorU,
    dragging: Arc<AtomicBool>,
) -> Box<dyn Element> {
    let geometry: Rc<Cell<Option<Geometry>>> = Rc::new(Cell::new(None));
    let element = MinimapElement {
        lines,
        render_state,
        background,
        slider_color,
        geometry: geometry.clone(),
        dragging: dragging.clone(),
        size: None,
        origin: None,
    };

    let down_geometry = geometry.clone();
    let down_dragging = dragging.clone();
    let drag_geometry = geometry;
    let drag_dragging = dragging.clone();
    EventHandler::new(element.finish())
        .on_left_mouse_down(move |ctx, _, position| {
            let Some(geometry) = down_geometry.get() else {
                return DispatchEventResult::PropagateToParent;
            };
            if !geometry.bounds.contains_point(position) {
                return DispatchEventResult::PropagateToParent;
            }
            down_dragging.store(true, Ordering::Relaxed);
            ctx.dispatch_typed_action(CodeEditorViewAction::MinimapScrollTo {
                scroll_top: geometry.scroll_top_for(position.y()),
            });
            DispatchEventResult::StopPropagation
        })
        .on_mouse_dragged(move |ctx, _, position| {
            let Some(geometry) = drag_geometry.get() else {
                return DispatchEventResult::PropagateToParent;
            };
            if !drag_dragging.load(Ordering::Relaxed) {
                return DispatchEventResult::PropagateToParent;
            }
            ctx.dispatch_typed_action(CodeEditorViewAction::MinimapScrollTo {
                scroll_top: geometry.scroll_top_for(position.y()),
            });
            DispatchEventResult::StopPropagation
        })
        .on_left_mouse_up(move |_, _, _| {
            dragging.store(false, Ordering::Relaxed);
            DispatchEventResult::PropagateToParent
        })
        .finish()
}

struct MinimapElement {
    lines: MinimapLines,
    render_state: ModelHandle<RenderState>,
    background: Option<ColorU>,
    slider_color: ColorU,
    geometry: Rc<Cell<Option<Geometry>>>,
    dragging: Arc<AtomicBool>,
    size: Option<Vector2F>,
    origin: Option<Point>,
}

impl Element for MinimapElement {
    fn layout(
        &mut self,
        constraint: SizeConstraint,
        _ctx: &mut LayoutContext,
        _app: &AppContext,
    ) -> Vector2F {
        let height = if constraint.max.y().is_finite() {
            constraint.max.y()
        } else {
            constraint.min.y()
        };
        let size = vec2f(MINIMAP_WIDTH, height);
        self.size = Some(size);
        size
    }

    fn after_layout(&mut self, _ctx: &mut AfterLayoutContext, _app: &AppContext) {}

    fn paint(&mut self, origin: Vector2F, ctx: &mut PaintContext, app: &AppContext) {
        self.origin = Some(Point::from_vec2f(origin, ctx.scene.z_index()));
        let Some(size) = self.size else {
            return;
        };
        let bounds = RectF::new(origin, size);
        ctx.scene
            .start_layer(ClipBounds::BoundedByActiveLayerAnd(bounds));
        if let Some(background) = self.background {
            ctx.scene
                .draw_rect_without_hit_recording(bounds)
                .with_background(background);
        }

        let render_state = self.render_state.as_ref(app);
        let viewport = render_state.viewport();
        let scroll_top = viewport.scroll_top().as_f32();
        let viewport_height = viewport.height().as_f32();
        let content_height = render_state.height().as_f32().max(1.);
        let line_count = self.lines.len().max(1);
        let editor_line_height = content_height / line_count as f32;

        let minimap_height = line_count as f32 * LINE_HEIGHT;
        let max_scroll_top = (content_height - viewport_height).max(0.);
        let scroll_fraction = if max_scroll_top > 0. {
            (scroll_top / max_scroll_top).clamp(0., 1.)
        } else {
            0.
        };
        let fits = minimap_height <= size.y();
        let offset = if fits {
            0.
        } else {
            scroll_fraction * (minimap_height - size.y())
        };
        let slider_top = scroll_top / editor_line_height * LINE_HEIGHT - offset;
        let slider_height = (viewport_height / editor_line_height * LINE_HEIGHT).max(LINE_HEIGHT);

        self.geometry.set(Some(Geometry {
            bounds,
            offset,
            slider_height,
            max_scroll_top,
            editor_line_height,
            viewport_height,
            fits,
        }));

        let first_line = (offset / LINE_HEIGHT).floor() as usize;
        let visible_lines = (size.y() / LINE_HEIGHT).ceil() as usize + 1;
        for (index, runs) in self
            .lines
            .iter()
            .enumerate()
            .skip(first_line)
            .take(visible_lines)
        {
            let y = origin.y() + index as f32 * LINE_HEIGHT - offset;
            for run in runs {
                let x = origin.x() + LEFT_PADDING + run.column as f32 * CHAR_WIDTH;
                ctx.scene
                    .draw_rect_without_hit_recording(RectF::new(
                        vec2f(x, y),
                        vec2f(run.len as f32 * CHAR_WIDTH, LINE_HEIGHT - 0.5),
                    ))
                    .with_background(ColorU {
                        a: TOKEN_ALPHA,
                        ..run.color
                    });
            }
        }

        let slider_alpha = if self.dragging.load(Ordering::Relaxed) {
            SLIDER_DRAG_ALPHA
        } else {
            SLIDER_ALPHA
        };
        ctx.scene
            .draw_rect_without_hit_recording(RectF::new(
                vec2f(origin.x(), origin.y() + slider_top),
                vec2f(size.x(), slider_height),
            ))
            .with_background(ColorU {
                a: slider_alpha,
                ..self.slider_color
            });
        ctx.scene.stop_layer();
    }

    fn size(&self) -> Option<Vector2F> {
        self.size
    }

    fn origin(&self) -> Option<Point> {
        self.origin
    }

    fn parent_data(&self) -> Option<&dyn Any> {
        None
    }

    fn dispatch_event(
        &mut self,
        _event: &DispatchedEvent,
        _ctx: &mut EventContext,
        _app: &AppContext,
    ) -> bool {
        false
    }
}
