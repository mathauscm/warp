use std::cell::RefCell;
use std::rc::Rc;

use pathfinder_geometry::rect::RectF;

use super::*;
use crate::elements::new_scrollable::SingleAxisConfig;
use crate::elements::{
    ClippedScrollStateHandle, EventHandler, Fill, List, ListState, NewScrollable, SelectableElement,
};
use crate::platform::WindowStyle;
use crate::units::IntoPixels;
use crate::{App, Entity, EntityIdSet, Presenter, TypedActionView, WindowInvalidation};

/// Selectable element that paints a hit-recorded rect (as `Container` does), so content inside a
/// layer-starting parent registers as covered at lower z-indexes.
#[derive(Default)]
struct LayeredSelectableProbe {
    origin: Option<Point>,
}

const PROBE_SIZE: f32 = 100.;

impl Element for LayeredSelectableProbe {
    fn layout(
        &mut self,
        _constraint: SizeConstraint,
        _ctx: &mut LayoutContext,
        _app: &AppContext,
    ) -> Vector2F {
        vec2f(PROBE_SIZE, PROBE_SIZE)
    }

    fn after_layout(&mut self, _ctx: &mut AfterLayoutContext, _app: &AppContext) {}

    fn paint(&mut self, origin: Vector2F, ctx: &mut PaintContext, _app: &AppContext) {
        self.origin = Some(Point::from_vec2f(origin, ctx.scene.z_index()));
        ctx.scene
            .draw_rect_with_hit_recording(RectF::new(origin, vec2f(PROBE_SIZE, PROBE_SIZE)))
            .with_background(Fill::None);
    }

    fn size(&self) -> Option<Vector2F> {
        Some(vec2f(PROBE_SIZE, PROBE_SIZE))
    }

    fn origin(&self) -> Option<Point> {
        self.origin
    }

    fn dispatch_event(
        &mut self,
        _event: &DispatchedEvent,
        _ctx: &mut EventContext,
        _app: &AppContext,
    ) -> bool {
        false
    }

    fn as_selectable_element(&self) -> Option<&dyn SelectableElement> {
        Some(self)
    }
}

impl SelectableElement for LayeredSelectableProbe {
    fn get_selection(
        &self,
        _selection_start: Vector2F,
        _selection_end: Vector2F,
        _is_rect: IsRect,
    ) -> Option<Vec<SelectionFragment>> {
        Some(vec![SelectionFragment {
            text: "probe".to_string(),
            origin: self.origin?,
        }])
    }

    fn expand_selection(
        &self,
        _absolute_point: Vector2F,
        _direction: SelectionDirection,
        _unit: SelectionType,
        _word_boundaries_policy: &WordBoundariesPolicy,
    ) -> Option<Vector2F> {
        None
    }

    fn is_point_semantically_before(
        &self,
        absolute_point: Vector2F,
        absolute_point_other: Vector2F,
    ) -> Option<bool> {
        Some(
            (absolute_point.y(), absolute_point.x())
                < (absolute_point_other.y(), absolute_point_other.x()),
        )
    }

    fn smart_select(
        &self,
        _absolute_point: Vector2F,
        _smart_select_fn: crate::elements::SmartSelectFn,
    ) -> Option<(Vector2F, Vector2F)> {
        None
    }

    fn calculate_clickable_bounds(&self, _current_selection: Option<Selection>) -> Vec<RectF> {
        Vec::new()
    }
}

#[derive(Default)]
struct ScrollableSelectionView {
    selection_handle: SelectionHandle,
    captured_selection: Rc<RefCell<Option<Option<String>>>>,
}

impl Entity for ScrollableSelectionView {
    type Event = ();
}

impl crate::core::View for ScrollableSelectionView {
    fn ui_name() -> &'static str {
        "selectable_area_test_view"
    }

    fn render(&self, _: &AppContext) -> Box<dyn Element> {
        let captured_selection = self.captured_selection.clone();
        // Mirrors the element sandwich around collapsible reasoning block bodies: an
        // EventHandler wrapping a clipped scrollable.
        SelectableArea::new(
            self.selection_handle.clone(),
            move |args, _, _| {
                *captured_selection.borrow_mut() = Some(args.selection);
            },
            EventHandler::new(
                NewScrollable::vertical(
                    SingleAxisConfig::Clipped {
                        handle: ClippedScrollStateHandle::default(),
                        child: Box::new(LayeredSelectableProbe::default()),
                    },
                    Fill::None,
                    Fill::None,
                    Fill::None,
                )
                .finish(),
            )
            .finish(),
        )
        .finish()
    }
}

impl TypedActionView for ScrollableSelectionView {
    type Action = ();
}

/// A drag starting on content painted in a child's own layers must start a selection and yield
/// the child's fragments on mouse up. Covers the mouse-down hit test at the child subtree's max
/// z-index and `EventHandler`'s forwarding of selection queries.
#[test]
fn selection_starts_on_content_painted_in_child_layers() {
    App::test((), |mut app| async move {
        let (window_id, view) = app.add_window(WindowStyle::NotStealFocus, |_| {
            ScrollableSelectionView::default()
        });
        let (selection_handle, captured_selection) = view.read(&app, |view, _| {
            (
                view.selection_handle.clone(),
                view.captured_selection.clone(),
            )
        });

        let root_view_id = view.id();
        app.update(move |ctx| {
            let mut presenter = Presenter::new(window_id);
            let mut updated = EntityIdSet::default();
            updated.insert(root_view_id);
            let invalidation = WindowInvalidation {
                updated,
                ..Default::default()
            };
            presenter.invalidate(invalidation, ctx);
            presenter.build_scene(vec2f(200., 200.), 1., None, ctx);
            let presenter = Rc::new(RefCell::new(presenter));

            ctx.simulate_window_event(
                Event::LeftMouseDown {
                    position: vec2f(10., 10.),
                    modifiers: Default::default(),
                    click_count: 1,
                    is_first_mouse: false,
                },
                window_id,
                presenter.clone(),
            );
            ctx.simulate_window_event(
                Event::LeftMouseDragged {
                    position: vec2f(60., 10.),
                    modifiers: Default::default(),
                },
                window_id,
                presenter.clone(),
            );
            ctx.simulate_window_event(
                Event::LeftMouseUp {
                    position: vec2f(60., 10.),
                    modifiers: Default::default(),
                },
                window_id,
                presenter,
            );
        });

        assert_eq!(
            *captured_selection.borrow(),
            Some(Some("probe".to_string())),
            "the drag should have produced a selection from the layered child",
        );
        assert!(!selection_handle.is_selecting());
    });
}

/// Like [`ScrollableSelectionView`], with the content in a viewported [`List`].
struct ListSelectionView {
    selection_handle: SelectionHandle,
    captured_selection: Rc<RefCell<Option<Option<String>>>>,
    list_state: ListState<()>,
}

impl Default for ListSelectionView {
    fn default() -> Self {
        let list_state = ListState::new(|_, _, _| Box::new(LayeredSelectableProbe::default()));
        list_state.add_item();
        Self {
            selection_handle: Default::default(),
            captured_selection: Default::default(),
            list_state,
        }
    }
}

impl Entity for ListSelectionView {
    type Event = ();
}

impl crate::core::View for ListSelectionView {
    fn ui_name() -> &'static str {
        "selectable_list_test_view"
    }

    fn render(&self, _: &AppContext) -> Box<dyn Element> {
        let captured_selection = self.captured_selection.clone();
        SelectableArea::new(
            self.selection_handle.clone(),
            move |args, _, _| {
                *captured_selection.borrow_mut() = Some(args.selection);
            },
            List::new(self.list_state.clone()).finish(),
        )
        .finish()
    }
}

impl TypedActionView for ListSelectionView {
    type Action = ();
}

#[test]
fn selection_reaches_the_items_of_a_viewported_list() {
    App::test((), |mut app| async move {
        let (window_id, view) =
            app.add_window(WindowStyle::NotStealFocus, |_| ListSelectionView::default());
        let captured_selection = view.read(&app, |view, _| view.captured_selection.clone());

        let root_view_id = view.id();
        app.update(move |ctx| {
            let mut presenter = Presenter::new(window_id);
            let mut updated = EntityIdSet::default();
            updated.insert(root_view_id);
            presenter.invalidate(
                WindowInvalidation {
                    updated,
                    ..Default::default()
                },
                ctx,
            );
            presenter.build_scene(vec2f(200., 200.), 1., None, ctx);
            let presenter = Rc::new(RefCell::new(presenter));
            for event in [
                Event::LeftMouseDown {
                    position: vec2f(10., 10.),
                    modifiers: Default::default(),
                    click_count: 1,
                    is_first_mouse: false,
                },
                Event::LeftMouseDragged {
                    position: vec2f(60., 10.),
                    modifiers: Default::default(),
                },
                Event::LeftMouseUp {
                    position: vec2f(60., 10.),
                    modifiers: Default::default(),
                },
            ] {
                ctx.simulate_window_event(event, window_id, presenter.clone());
            }
        });

        assert_eq!(
            *captured_selection.borrow(),
            Some(Some("probe".to_string()))
        );
    });
}

const ROW_HEIGHT: f32 = 40.;
const ROW_COUNT: usize = 10;
const WINDOW_SIZE: f32 = 200.;

/// A row of a list whose text is selected whenever the selection reaches it.
struct RowProbe {
    index: usize,
    origin: Option<Point>,
}

impl RowProbe {
    fn bounds(&self) -> Option<RectF> {
        Some(RectF::new(
            self.origin?.xy(),
            vec2f(WINDOW_SIZE, ROW_HEIGHT),
        ))
    }
}

impl Element for RowProbe {
    fn layout(
        &mut self,
        _constraint: SizeConstraint,
        _ctx: &mut LayoutContext,
        _app: &AppContext,
    ) -> Vector2F {
        vec2f(WINDOW_SIZE, ROW_HEIGHT)
    }

    fn after_layout(&mut self, _ctx: &mut AfterLayoutContext, _app: &AppContext) {}

    fn paint(&mut self, origin: Vector2F, ctx: &mut PaintContext, _app: &AppContext) {
        self.origin = Some(Point::from_vec2f(origin, ctx.scene.z_index()));
    }

    fn size(&self) -> Option<Vector2F> {
        Some(vec2f(WINDOW_SIZE, ROW_HEIGHT))
    }

    fn origin(&self) -> Option<Point> {
        self.origin
    }

    fn dispatch_event(
        &mut self,
        _event: &DispatchedEvent,
        _ctx: &mut EventContext,
        _app: &AppContext,
    ) -> bool {
        false
    }

    fn as_selectable_element(&self) -> Option<&dyn SelectableElement> {
        Some(self)
    }
}

impl SelectableElement for RowProbe {
    fn get_selection(
        &self,
        selection_start: Vector2F,
        selection_end: Vector2F,
        _is_rect: IsRect,
    ) -> Option<Vec<SelectionFragment>> {
        let bounds = self.bounds()?;
        let (top, bottom) = (
            selection_start.y().min(selection_end.y()),
            selection_start.y().max(selection_end.y()),
        );
        let is_selected = top < bounds.max_y() && bottom > bounds.min_y();
        Some(vec![SelectionFragment {
            text: if is_selected {
                format!("row {}", self.index)
            } else {
                String::new()
            },
            origin: self.origin?,
        }])
    }

    fn expand_selection(
        &self,
        _absolute_point: Vector2F,
        _direction: SelectionDirection,
        _unit: SelectionType,
        _word_boundaries_policy: &WordBoundariesPolicy,
    ) -> Option<Vector2F> {
        None
    }

    fn is_point_semantically_before(
        &self,
        absolute_point: Vector2F,
        absolute_point_other: Vector2F,
    ) -> Option<bool> {
        Some(
            (absolute_point.y(), absolute_point.x())
                < (absolute_point_other.y(), absolute_point_other.x()),
        )
    }

    fn smart_select(
        &self,
        _absolute_point: Vector2F,
        _smart_select_fn: crate::elements::SmartSelectFn,
    ) -> Option<(Vector2F, Vector2F)> {
        None
    }

    fn calculate_clickable_bounds(&self, _current_selection: Option<Selection>) -> Vec<RectF> {
        Vec::new()
    }
}

type CapturedUpdate = Option<(Option<String>, Option<SelectionDragEnd>)>;

/// A list taller than the window, in an autoscrolling selectable area.
struct ScrollingListView {
    selection_handle: SelectionHandle,
    captured: Rc<RefCell<CapturedUpdate>>,
    list_state: ListState<()>,
}

impl Default for ScrollingListView {
    fn default() -> Self {
        let list_state = ListState::new(|index, _, _| {
            Box::new(RowProbe {
                index,
                origin: None,
            })
        });
        for _ in 0..ROW_COUNT {
            list_state.add_item();
        }
        Self {
            selection_handle: Default::default(),
            captured: Default::default(),
            list_state,
        }
    }
}

impl Entity for ScrollingListView {
    type Event = ();
}

impl crate::core::View for ScrollingListView {
    fn ui_name() -> &'static str {
        "scrolling_list_test_view"
    }

    fn render(&self, _: &AppContext) -> Box<dyn Element> {
        let captured = self.captured.clone();
        let list_state = self.list_state.clone();
        SelectableArea::new(
            self.selection_handle.clone(),
            move |args, _, _| {
                *captured.borrow_mut() = Some((args.selection, args.ended_drag));
            },
            List::new(self.list_state.clone()).finish(),
        )
        .with_autoscroll(move |step, _| list_state.scroll_by(step.into_pixels()))
        .finish()
    }
}

impl TypedActionView for ScrollingListView {
    type Action = ();
}

/// Renders `view`'s window into a presenter, for sending it events.
fn present(
    window_id: crate::WindowId,
    root_view_id: crate::EntityId,
    ctx: &mut AppContext,
) -> Rc<RefCell<Presenter>> {
    let mut presenter = Presenter::new(window_id);
    let mut updated = EntityIdSet::default();
    updated.insert(root_view_id);
    presenter.invalidate(
        WindowInvalidation {
            updated,
            ..Default::default()
        },
        ctx,
    );
    presenter.build_scene(vec2f(WINDOW_SIZE, WINDOW_SIZE), 1., None, ctx);
    Rc::new(RefCell::new(presenter))
}

fn redraw(presenter: &Rc<RefCell<Presenter>>, ctx: &mut AppContext) {
    presenter
        .borrow_mut()
        .build_scene(vec2f(WINDOW_SIZE, WINDOW_SIZE), 1., None, ctx);
}

fn mouse_down(position: Vector2F) -> Event {
    Event::LeftMouseDown {
        position,
        modifiers: Default::default(),
        click_count: 1,
        is_first_mouse: false,
    }
}

fn mouse_dragged(position: Vector2F) -> Event {
    Event::LeftMouseDragged {
        position,
        modifiers: Default::default(),
    }
}

fn mouse_up(position: Vector2F) -> Event {
    Event::LeftMouseUp {
        position,
        modifiers: Default::default(),
    }
}

/// Scrolling after selecting must leave the selection on the rows it was made on, not on whatever
/// scrolled under its old screen position.
#[test]
fn selection_stays_on_its_rows_when_the_list_scrolls() {
    App::test((), |mut app| async move {
        let (window_id, view) =
            app.add_window(WindowStyle::NotStealFocus, |_| ScrollingListView::default());
        let (captured, list_state) = view.read(&app, |view, _| {
            (view.captured.clone(), view.list_state.clone())
        });
        let root_view_id = view.id();
        app.update(move |ctx| {
            let presenter = present(window_id, root_view_id, ctx);
            for event in [
                mouse_down(vec2f(10., 50.)),
                mouse_dragged(vec2f(60., 60.)),
                mouse_up(vec2f(60., 60.)),
            ] {
                ctx.simulate_window_event(event, window_id, presenter.clone());
            }
            list_state.scroll_by(ROW_HEIGHT.into_pixels());
            redraw(&presenter, ctx);
            // A click elsewhere reports the selection again without ending a drag.
            ctx.simulate_window_event(mouse_up(vec2f(10., 150.)), window_id, presenter);
        });

        assert_eq!(
            *captured.borrow(),
            Some((Some("row 1".to_owned()), None)),
            "the selection should still be on row 1 after scrolling a row down",
        );
    });
}

/// Dragging past the bottom edge scrolls the list and extends the selection, and the selection's
/// text includes the rows that scrolled off the top.
#[test]
fn dragging_past_the_bottom_autoscrolls_and_keeps_the_rows_scrolled_away() {
    App::test((), |mut app| async move {
        let (window_id, view) =
            app.add_window(WindowStyle::NotStealFocus, |_| ScrollingListView::default());
        let (captured, list_state) = view.read(&app, |view, _| {
            (view.captured.clone(), view.list_state.clone())
        });
        let root_view_id = view.id();
        // The test window's own presenter lays the list out again at its size once the update
        // ends, so the scroll position is read within it.
        let scroll_index = app.update(move |ctx| {
            let presenter = present(window_id, root_view_id, ctx);
            ctx.simulate_window_event(mouse_down(vec2f(10., 10.)), window_id, presenter.clone());
            ctx.simulate_window_event(
                mouse_dragged(vec2f(10., WINDOW_SIZE + 60.)),
                window_id,
                presenter.clone(),
            );
            for _ in 0..60 {
                redraw(&presenter, ctx);
            }
            ctx.simulate_window_event(
                mouse_up(vec2f(10., WINDOW_SIZE + 60.)),
                window_id,
                presenter,
            );
            list_state.get_scroll_index()
        });

        assert!(
            scroll_index > 0,
            "holding the pointer past the bottom should have scrolled the list",
        );
        let (text, ended_drag) = captured
            .borrow()
            .clone()
            .expect("the selection was reported");
        let all_rows = (0..ROW_COUNT)
            .map(|index| format!("row {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(text, Some(all_rows));
        let ended_drag = ended_drag.expect("the mouse-up ended a drag");
        assert!(!ended_drag.is_reversed);
        assert!(
            ended_drag.tail.y() < WINDOW_SIZE,
            "the tail stays within the area: {ended_drag:?}",
        );
    });
}

/// Dragging past the top edge scrolls back up, and reports the drag as reversed.
#[test]
fn dragging_past_the_top_autoscrolls_backwards() {
    App::test((), |mut app| async move {
        let (window_id, view) =
            app.add_window(WindowStyle::NotStealFocus, |_| ScrollingListView::default());
        let (captured, list_state) = view.read(&app, |view, _| {
            (view.captured.clone(), view.list_state.clone())
        });
        let root_view_id = view.id();
        list_state.scroll_to(ROW_COUNT - 5);
        let (start_index, end_index) = app.update(move |ctx| {
            let presenter = present(window_id, root_view_id, ctx);
            let start_index = list_state.get_scroll_index();
            let start = vec2f(10., WINDOW_SIZE - 10.);
            ctx.simulate_window_event(mouse_down(start), window_id, presenter.clone());
            ctx.simulate_window_event(
                mouse_dragged(vec2f(10., -60.)),
                window_id,
                presenter.clone(),
            );
            for _ in 0..60 {
                redraw(&presenter, ctx);
            }
            ctx.simulate_window_event(mouse_up(vec2f(10., -60.)), window_id, presenter);
            (start_index, list_state.get_scroll_index())
        });

        assert_eq!((start_index, end_index), (ROW_COUNT - 5, 0));
        let (text, ended_drag) = captured
            .borrow()
            .clone()
            .expect("the selection was reported");
        let all_rows = (0..ROW_COUNT)
            .map(|index| format!("row {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(text, Some(all_rows));
        assert!(ended_drag.expect("the mouse-up ended a drag").is_reversed);
    });
}
