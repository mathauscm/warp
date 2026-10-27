//! Keeps a native web view lined up with a placeholder element. The web view is a child NSView
//! drawn above the whole GPU scene, so it's shown only while its placeholder is part of the latest
//! built scene and nothing (a menu, a modal, a drag) covers it.
//!
//! Presence is read from the position cache of the last built scene rather than tracked per drawn
//! frame: AppKit redraws the cached scene without painting elements (on resize, on key changes,
//! after a child view toggles), and treating those draws as "not painted" made the page flicker.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use pathfinder_color::ColorU;
use picolas_webview::WebView;
use warpui::elements::{Point, SavePosition};
use warpui::event::DispatchedEvent;
use warpui::geometry::rect::RectF;
use warpui::geometry::vector::{Vector2F, vec2f};
use warpui::zoom::Scale as _;
use warpui::{
    AfterLayoutContext, AppContext, Element, Event, EventContext, LayoutContext, PaintContext,
    SizeConstraint,
};

/// Shared between the view that owns the web view and the [`WebViewSlot`] it renders.
#[derive(Default)]
pub struct WebViewHost {
    web_view: RefCell<Option<WebView>>,
    /// Whether the placeholder is in the latest built scene.
    in_scene: Cell<bool>,
    covered: Cell<bool>,
    visible: Cell<bool>,
    frame: Cell<Option<(RectF, f32)>>,
    dark: Cell<Option<bool>>,
    background: Cell<Option<ColorU>>,
}

impl WebViewHost {
    pub fn set(&self, web_view: Option<WebView>) {
        // Dropping the old web view removes it from the window.
        *self.web_view.borrow_mut() = web_view;
        self.in_scene.set(false);
        self.visible.set(false);
        self.frame.set(None);
        self.dark.set(None);
        self.background.set(None);
        if let Some(web_view) = self.web_view.borrow().as_ref() {
            web_view.set_visible(false);
        }
    }

    pub fn has_web_view(&self) -> bool {
        self.web_view.borrow().is_some()
    }

    pub fn with<R>(&self, f: impl FnOnce(&WebView) -> R) -> Option<R> {
        self.web_view.borrow().as_ref().map(f)
    }

    pub fn is_visible(&self) -> bool {
        self.visible.get()
    }

    pub fn set_in_scene(&self, in_scene: bool) {
        if self.in_scene.replace(in_scene) != in_scene {
            self.apply_visibility();
        }
    }

    fn apply_visibility(&self) {
        let visible = self.in_scene.get() && !self.covered.get();
        if self.visible.get() == visible {
            return;
        }
        if self
            .with(|web_view| web_view.set_visible(visible))
            .is_some()
        {
            log::debug!(
                "Artifact web view {} (in scene: {}, covered: {})",
                if visible { "shown" } else { "hidden" },
                self.in_scene.get(),
                self.covered.get()
            );
            self.visible.set(visible);
        }
    }

    fn on_painted(&self, bounds: RectF, window_height: f32, dark: bool, background: ColorU) {
        let web_view = self.web_view.borrow();
        let Some(web_view) = web_view.as_ref() else {
            return;
        };
        if self.frame.get() != Some((bounds, window_height)) {
            log::debug!("Artifact web view frame {bounds:?} in a window {window_height} tall");
            web_view.set_frame(
                bounds.origin_x(),
                bounds.origin_y(),
                bounds.width(),
                bounds.height(),
                window_height,
            );
            self.frame.set(Some((bounds, window_height)));
        }
        if self.dark.get() != Some(dark) {
            web_view.set_dark_appearance(dark);
            self.dark.set(Some(dark));
        }
        if self.background.get() != Some(background) {
            let channel = |value: u8| f32::from(value) / 255.;
            web_view.set_background(
                channel(background.r),
                channel(background.g),
                channel(background.b),
                channel(background.a),
            );
            self.background.set(Some(background));
        }
    }

    fn set_covered(&self, covered: bool) {
        if self.covered.replace(covered) != covered {
            self.apply_visibility();
        }
    }
}

/// The placeholder the web view is laid over; paints `child` (the page color) underneath and saves
/// its position under `position_id` for the scene it's part of.
pub struct WebViewSlot {
    host: Rc<WebViewHost>,
    child: Box<dyn Element>,
    dark: bool,
    background: ColorU,
    origin: Option<Point>,
}

impl WebViewSlot {
    pub fn new(
        host: Rc<WebViewHost>,
        position_id: &str,
        child: Box<dyn Element>,
        dark: bool,
        background: ColorU,
    ) -> Self {
        Self {
            host,
            child: SavePosition::new(child, position_id)
                .for_single_frame()
                .finish(),
            dark,
            background,
            origin: None,
        }
    }

    /// Probes a few points of the slot, since a popover may cover only part of it.
    fn is_covered(&self, ctx: &EventContext) -> bool {
        let (Some(origin), Some(size)) = (self.origin, self.child.size()) else {
            return false;
        };
        let z_index = origin.z_index();
        let inset = 2.;
        let xs = [inset, size.x() / 2., size.x() - inset];
        let ys = [inset, size.y() / 2., size.y() - inset];
        xs.iter().any(|x| {
            ys.iter().any(|y| {
                let point = origin.xy() + vec2f(*x, *y);
                ctx.is_covered(Point::new(point.x(), point.y(), z_index))
            })
        })
    }
}

impl Element for WebViewSlot {
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
        self.origin = Some(Point::from_vec2f(origin, ctx.scene.z_index()));
        self.child.paint(origin, ctx, app);
        if let Some(size) = self.child.size() {
            // The scene is laid out in zoomed units; the native view is placed in points.
            let zoom = app.zoom_factor();
            self.host.on_painted(
                RectF::new(origin.scale_up(zoom), size.scale_up(zoom)),
                ctx.window_size.y().scale_up(zoom),
                self.dark,
                self.background,
            );
        }
    }

    fn size(&self) -> Option<Vector2F> {
        self.child.size()
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
        // A mouse move is replayed after every frame, so this keeps up with popovers opening and
        // closing over the slot.
        if matches!(event.raw_event(), Event::MouseMoved { .. }) {
            self.host.set_covered(self.is_covered(ctx));
        }
        self.child.dispatch_event(event, ctx, app)
    }
}
