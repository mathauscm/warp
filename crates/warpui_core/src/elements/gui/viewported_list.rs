//! Module containing the definition of [`List`], an element that holds elements of various sizes
//! and only lays out the elements that are visible in the viewport.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::{AddAssign, Range};
use std::rc::Rc;
use std::sync::Arc;

use derivative::Derivative;
use derive_more::AddAssign;
use ordered_float::OrderedFloat;
use pathfinder_geometry::rect::RectF;
use pathfinder_geometry::vector::{Vector2F, vec2f};
use sum_tree::SumTree;

use super::new_scrollable::{NewScrollableElement, ScrollableAxis};
use super::{
    AppContext, Axis, ContentAnchor, Element, ScrollData, ScrollableElement, SelectableElement,
    Selection, SelectionFragment, SizeConstraint, SmartSelectFn, order_and_concatenate_fragments,
};
use crate::ClipBounds;
use crate::text::word_boundaries::WordBoundariesPolicy;
use crate::text::{IsRect, SelectionDirection, SelectionType};
use crate::units::{IntoPixels, Pixels};

#[derive(Debug, Clone, Copy, Default, PartialEq, PartialOrd)]
pub struct ScrollOffset {
    /// The item that is scrolled to.
    list_item_index: Count,
    /// Number of pixels offset from the start of the item.
    offset_from_start: Pixels,
}

impl ScrollOffset {
    /// The item that is scrolled to.
    pub fn list_item_index(&self) -> usize {
        self.list_item_index.0
    }

    /// Number of pixels offset from the start of the item.
    pub fn offset_from_start(&self) -> Pixels {
        self.offset_from_start
    }
}

/// Holds the callback function used to adjust scroll position
/// when an element's height is invalidated and re-measured.
#[allow(clippy::type_complexity)]
pub struct ScrollPreservation<T> {
    /// Called after re-measuring to compute a scroll adjustment.
    /// Arguments: (invalidated_index, captured_context, app_context) -> new_scroll_offset
    ///
    /// Only called for the currently scrolled item, so the callback does not
    /// need access to the list state inner.
    adjustment_fn: Box<dyn Fn(usize, &T, &AppContext) -> Option<Pixels>>,
}

/// Internal state of the [`List`] that is shared across multiple renders of the element.
#[derive(Clone)]
pub struct ListState<T>(Rc<RefCell<ListStateInner<T>>>);

impl<T> ListState<T> {
    /// Creates a new ListState with scroll preservation.
    ///
    /// The `adjustment_fn` is called after re-measuring to compute a new absolute scroll offset.
    /// It receives the invalidated index and the captured scroll context.
    /// Only called for the currently scrolled item.
    ///
    /// Returns the list state and a receiver for scroll events. The list sends
    /// the current scroll offset through the channel whenever the user scrolls.
    pub fn new_with_scroll_preservation(
        render_fn: impl Fn(usize, ScrollOffset, &AppContext) -> Box<dyn Element> + 'static,
        adjustment_fn: impl Fn(usize, &T, &AppContext) -> Option<Pixels> + 'static,
    ) -> (Self, async_channel::Receiver<ScrollOffset>) {
        let render_fn = Arc::new(render_fn);
        let (tx, rx) = async_channel::bounded(5);
        let inner = ListStateInner::new_with_scroll_preservation(render_fn, adjustment_fn, tx);
        (Self(Rc::new(RefCell::new(inner))), rx)
    }

    /// Adds an item to the list.
    pub fn add_item(&self) {
        let mut inner = self.0.borrow_mut();
        inner.add_item();
    }

    /// Invalidates the height of the item at the given index, forcing it
    /// to be re-measured on the next layout pass. If scroll preservation is
    /// configured and the current scroll item's height becomes `None`,
    /// the adjustment function will run during layout to preserve scroll position.
    pub fn invalidate_height_for_index(&self, index: usize) {
        self.0.borrow_mut().invalidate_height_for_index(index);
    }

    /// Removes a specific item from the list.
    pub fn remove(&self, index: usize) {
        self.0.borrow_mut().remove(index);
    }

    pub fn scroll_to(&self, index: usize) {
        self.0.borrow_mut().scroll_to(index, None);
    }

    /// An offset of 0 means the top of the item is at the top of the viewport
    /// A negative offset means we've moved up from that, so the item above is visible
    /// A positive offset means we've moved down from that to partway through the item
    pub fn scroll_to_with_offset(&self, index: usize, offset_from_start: Pixels) {
        self.0
            .borrow_mut()
            .scroll_to(index, Some(offset_from_start));
    }

    pub fn is_scrolled_to_item(&self, index: usize) -> bool {
        self.0.borrow().scroll_top.list_item_index.0 == index
    }

    /// The item at an absolute point, on or off screen, going by where the list was last painted.
    pub fn item_at(&self, absolute_point: Vector2F) -> Option<usize> {
        self.0.borrow().item_at(absolute_point)
    }

    /// Scrolls by `delta` pixels, towards the end of the list when positive.
    pub fn scroll_by(&self, delta: Pixels) {
        self.0.borrow_mut().scroll_by(delta);
    }

    pub fn get_scroll_index(&self) -> usize {
        self.0.borrow().scroll_top.list_item_index.0
    }

    pub fn get_scroll_offset(&self) -> Pixels {
        self.0.borrow().scroll_top.offset_from_start
    }

    pub fn get_viewport_height(&self) -> Pixels {
        self.0.borrow().viewport_height
    }

    /// Sets the persistent scroll context used by explicit height invalidation
    /// during layout. Call this when scrolling settles so the adjustment
    /// function has context to recompute scroll position.
    pub fn set_scroll_context(&self, context: Option<T>) {
        self.0.borrow_mut().current_scroll_context = context;
    }

    pub fn is_vertical_range_visible(
        &self,
        item_index: usize,
        start_offset: Pixels,
        end_offset: Pixels,
    ) -> bool {
        let inner = self.0.borrow();

        // Get absolute positions of the targets
        let (start_absolute, end_absolute) = {
            let mut cursor = inner.content.cursor::<Count, Height>();
            cursor.seek(&Count(item_index), sum_tree::SeekBias::Right);
            let item_start = cursor.start().0.0;
            (item_start + start_offset, item_start + end_offset)
        };

        // Get absolute viewport range
        let viewport_top = inner.scroll_top_pixels();
        let viewport_bottom = viewport_top + inner.viewport_height;

        start_absolute >= viewport_top
            && start_absolute <= viewport_bottom
            && end_absolute >= viewport_top
            && end_absolute <= viewport_bottom
    }
}

impl ListState<()> {
    pub fn new(
        render_fn: impl Fn(usize, ScrollOffset, &AppContext) -> Box<dyn Element> + 'static,
    ) -> Self {
        let render_fn = Arc::new(render_fn);
        Self(Rc::new(RefCell::new(ListStateInner::new(render_fn))))
    }
}

type ListItemRenderFn = dyn Fn(usize, ScrollOffset, &AppContext) -> Box<dyn Element>;

/// An element that holds elements of various sizes and only lays out the elements that are visible in the viewport.
/// If each element is provably the same size, consider using [`UniformList`] instead for a vastly simpler API.
///
/// In order for viewporting to work, the [`List`] element assumes that each item does not change in height once it
/// is laid out. If an item's height changes, [`ListState::invalidate_height_for_index`] must be called in order
/// to invalidate the cached height of the item.
pub struct List<T: 'static = ()> {
    list_state: ListState<T>,
    children: Vec<Box<dyn Element>>,
    /// The index of the item in `children[0]`.
    first_child_index: usize,
    size: Vector2F,
    origin: Option<super::Point>,
}

impl<T: 'static> List<T> {
    pub fn new(list_state: ListState<T>) -> Self {
        Self {
            list_state,
            children: Vec::new(),
            first_child_index: 0,
            size: Vector2F::zero(),
            origin: None,
        }
    }

    /// The laid-out element of the item at `index`, if it's in the viewport.
    fn child_at(&self, index: usize) -> Option<&dyn SelectableElement> {
        self.children
            .get(index.checked_sub(self.first_child_index)?)?
            .as_selectable_element()
    }

    /// Notes the text of the items on screen while there's a selection, so a selection that
    /// scrolled partly off screen can still be read.
    fn cache_selection_text(&self, child_bounds: &[RectF], selection: Option<Selection>) {
        let mut list_state = self.list_state.0.borrow_mut();
        let Some(selection) = selection.filter(|selection| selection.start != selection.end) else {
            list_state.selection_text.clear();
            return;
        };
        let endpoint_items =
            [selection.start, selection.end].map(|point| list_state.item_at(point));
        for (position, bounds) in child_bounds.iter().enumerate() {
            let index = self.first_child_index + position;
            let Some(child) = self.child_at(index) else {
                continue;
            };
            list_state
                .selection_text
                .whole
                .entry(index)
                .or_insert_with(|| {
                    // Points just outside the item, so text elements select all of it.
                    child
                        .get_selection(
                            bounds.origin() - vec2f(0., 1.),
                            bounds.lower_right() + vec2f(0., 1.),
                            IsRect::False,
                        )
                        .map(order_and_concatenate_fragments)
                        .unwrap_or_default()
                });
            if endpoint_items.contains(&Some(index)) {
                let selected = child
                    .get_selection(selection.start, selection.end, selection.is_rect)
                    .map(order_and_concatenate_fragments)
                    .unwrap_or_default();
                list_state.selection_text.selected.insert(index, selected);
            }
        }
    }

    fn scroll_vertically(&mut self, delta: Pixels, ctx: &mut super::EventContext) {
        let mut list_state = self.list_state.0.borrow_mut();

        let viewport_height = self.size.y().into_pixels();
        let scroll_max = (list_state.approximate_height() - viewport_height).max(Pixels::zero());
        let current_scroll_top = list_state.scroll_top_pixels();
        let new_scroll_top = (current_scroll_top - delta)
            .max(Pixels::zero())
            .min(scroll_max);

        list_state.scroll_top = list_state.absolute_pixels_to_scroll_offset(new_scroll_top);
        list_state.broadcast_scroll_event();

        ctx.notify();
    }

    fn vertical_scroll_data(&self) -> ScrollData {
        let list_state = self.list_state.0.borrow();
        ScrollData {
            scroll_start: list_state.scroll_top_pixels(),
            visible_px: self.size.y().into_pixels(),
            total_size: list_state.approximate_height(),
        }
    }

    /// Find all visible items in the viewport, and call the render function.
    /// Update the size sumtree and return the new child elements.
    fn render_visible_items(
        &self,
        child_constraint: super::SizeConstraint,
        list_state: &mut ListStateInner<T>,
        ctx: &mut super::LayoutContext,
        app: &AppContext,
    ) -> (Range<usize>, Vec<Box<dyn Element>>) {
        // Iterate through items and layout only those that fit in the viewport
        let mut measured_items = Vec::new();
        let mut rendered_height = 0.;
        let mut children = vec![];
        let mut cursor = list_state.content.cursor::<Count, Count>();
        cursor.seek(
            &list_state.scroll_top.list_item_index,
            sum_tree::SeekBias::Right,
        );

        let cursor_start = *cursor.start();

        for (index, _) in cursor.enumerate() {
            // Break if we've filled the viewport.
            if rendered_height >= list_state.viewport_height.as_f32() {
                break;
            }

            let mut element =
                (list_state.render_fn)(index + cursor_start.0, list_state.scroll_top, app);
            let element_size = element.layout(child_constraint, ctx, app);

            measured_items.push(ListItem {
                height: Some(element_size.y().into_pixels()),
            });
            children.push(element);

            // If this is the first item, the element could only be partially in the viewport.
            // If that's the case, we only want to include the portion that is actually in the viewport.
            if index == 0 {
                rendered_height +=
                    element_size.y() - list_state.scroll_top.offset_from_start.as_f32();
            } else {
                rendered_height += element_size.y();
            }
        }

        // Update the sum tree with the newly measured items.
        let measured_range = cursor_start.0..(cursor_start.0 + measured_items.len());
        let new_items = {
            let mut cursor = list_state.content.cursor::<Count, ()>();

            let mut new_items =
                cursor.slice(&Count(measured_range.start), sum_tree::SeekBias::Right);
            new_items.extend(measured_items);

            cursor.seek(&Count(measured_range.end), sum_tree::SeekBias::Right);

            new_items.push_tree(cursor.suffix());
            new_items
        };

        list_state.content = new_items;
        list_state.last_measured_index = list_state
            .content
            .summary()
            .measured_count
            .saturating_sub(1);

        (measured_range, children)
    }
}

impl<T: 'static> SelectableElement for List<T> {
    fn get_selection(
        &self,
        selection_start: Vector2F,
        selection_end: Vector2F,
        is_rect: IsRect,
    ) -> Option<Vec<SelectionFragment>> {
        let list_state = self.list_state.0.borrow();
        let (Some(start_item), Some(end_item)) = (
            list_state.item_at(selection_start),
            list_state.item_at(selection_end),
        ) else {
            return None;
        };
        let (first, last) = (start_item.min(end_item), start_item.max(end_item));
        let cache = &list_state.selection_text;
        // The items are stacked vertically, like the lines of a vertical Flex. Items scrolled off
        // screen aren't laid out, so their text comes from when they were last on screen.
        let text = (first..=last)
            .filter_map(|index| match self.child_at(index) {
                Some(child) => child
                    .get_selection(selection_start, selection_end, is_rect)
                    .map(order_and_concatenate_fragments),
                None if index != first && index != last => cache.whole.get(&index).cloned(),
                None => cache
                    .selected
                    .get(&index)
                    .or_else(|| cache.whole.get(&index))
                    .cloned(),
            })
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>();
        if text.is_empty() {
            return None;
        }
        Some(vec![SelectionFragment {
            text: text.join("\n"),
            origin: self.origin?,
        }])
    }

    fn expand_selection(
        &self,
        point: Vector2F,
        direction: SelectionDirection,
        unit: SelectionType,
        word_boundaries_policy: &WordBoundariesPolicy,
    ) -> Option<Vector2F> {
        // A point off screen has no laid-out item to expand within.
        let index = self.list_state.0.borrow().item_at(point)?;
        self.child_at(index)?;
        let mut expanded = None;
        for child in &self.children {
            let Some(selection) = child.as_selectable_element().and_then(|child| {
                child.expand_selection(point, direction, unit, word_boundaries_policy)
            }) else {
                continue;
            };
            match direction {
                SelectionDirection::Backward => return Some(selection),
                SelectionDirection::Forward => expanded = Some(selection),
            }
        }
        expanded
    }

    fn is_point_semantically_before(
        &self,
        absolute_point: Vector2F,
        absolute_point_other: Vector2F,
    ) -> Option<bool> {
        let (index, other_index) = {
            let list_state = self.list_state.0.borrow();
            (
                list_state.item_at(absolute_point)?,
                list_state.item_at(absolute_point_other)?,
            )
        };
        if index != other_index {
            return Some(index < other_index);
        }
        self.child_at(index)
            .and_then(|child| {
                child.is_point_semantically_before(absolute_point, absolute_point_other)
            })
            .or_else(|| {
                Some(
                    (absolute_point.y(), absolute_point.x())
                        < (absolute_point_other.y(), absolute_point_other.x()),
                )
            })
    }

    fn smart_select(
        &self,
        absolute_point: Vector2F,
        smart_select_fn: SmartSelectFn,
    ) -> Option<(Vector2F, Vector2F)> {
        self.children.iter().find_map(|child| {
            child
                .as_selectable_element()?
                .smart_select(absolute_point, smart_select_fn)
        })
    }

    fn calculate_clickable_bounds(&self, current_selection: Option<Selection>) -> Vec<RectF> {
        self.children
            .iter()
            .filter_map(|child| child.as_selectable_element())
            .flat_map(|child| child.calculate_clickable_bounds(current_selection))
            .collect()
    }

    fn content_anchor(&self, absolute_point: Vector2F) -> Option<ContentAnchor> {
        let list_state = self.list_state.0.borrow();
        let origin = list_state.origin?;
        let content_y = list_state.content_y(absolute_point)?;
        let item = list_state.item_at_content_y(content_y)?;
        Some(ContentAnchor {
            item,
            offset: vec2f(
                absolute_point.x() - origin.x(),
                (content_y - list_state.item_top(item)).as_f32(),
            ),
        })
    }

    fn content_anchor_position(&self, anchor: ContentAnchor) -> Option<Vector2F> {
        let list_state = self.list_state.0.borrow();
        let origin = list_state.origin?;
        let item_top = list_state.item_top(anchor.item) - list_state.scroll_top_pixels();
        Some(origin + vec2f(0., item_top.as_f32()) + anchor.offset)
    }
}

impl<T: 'static> Element for List<T> {
    fn as_selectable_element(&self) -> Option<&dyn SelectableElement> {
        Some(self as &dyn SelectableElement)
    }

    fn layout(
        &mut self,
        constraint: super::SizeConstraint,
        ctx: &mut super::LayoutContext,
        app: &super::AppContext,
    ) -> Vector2F {
        // Create a child constraint with unbounded vertical height.
        // List items should be laid out at their natural height - the List handles
        // viewport clipping and scroll management internally.
        let child_constraint = SizeConstraint {
            min: vec2f(constraint.min.x(), 0.),
            max: vec2f(constraint.max.x(), f32::INFINITY),
        };

        self.children.clear();
        let mut list_state = self.list_state.0.borrow_mut();

        // Check if the current scroll item's height has been invalidated (set to None)
        // before we measure anything. This drives scroll preservation after rendering.
        let scroll_item_index = list_state.scroll_top.list_item_index.0;
        let scroll_item_was_invalidated = list_state.height_for_index(scroll_item_index).is_none();

        {
            // Ensure every item up to the scroll position is measured so that the sum tree is up-to-date
            // before we seek into it by height.
            if list_state.scroll_top.list_item_index.0 > list_state.last_measured_index {
                let start_index = list_state.last_measured_index;
                let end_index = list_state.scroll_top.list_item_index.0;

                let new_items = {
                    let mut cursor = list_state.content.cursor::<Count, Count>();
                    let mut new_items =
                        cursor.slice(&Count(start_index), sum_tree::SeekBias::Right);
                    for index in 0..(end_index - start_index) {
                        let mut element =
                            (list_state.render_fn)(index + start_index, list_state.scroll_top, app);
                        let element_size = element.layout(child_constraint, ctx, app);
                        new_items.push(ListItem {
                            height: Some(element_size.y().into_pixels()),
                        });
                        cursor.next();
                    }
                    new_items.push_tree(cursor.suffix());
                    new_items
                };

                list_state.content = new_items;
                list_state.last_measured_index = list_state
                    .content
                    .summary()
                    .measured_count
                    .saturating_sub(1);
            }
        }

        // If we have a negative offset, we should render the previous item(s) above the current item
        // However we only render items from the current index downwards, so we convert to a positive offset on an earlier item
        if list_state.scroll_top.offset_from_start < Pixels::zero() {
            list_state.scroll_top = list_state.absolute_pixels_to_scroll_offset(
                list_state.scroll_top_pixels().max(Pixels::zero()),
            );
        }

        let size = Vector2F::new(constraint.max.x(), constraint.max.y());
        let viewport_height = size.y();

        list_state.viewport_height = viewport_height.into_pixels();

        let mut rendered_range;
        (rendered_range, self.children) =
            self.render_visible_items(child_constraint, &mut list_state, ctx, app);

        // Ensure the scroll top never exceeds the maximum scroll position.
        let max_scroll_top = list_state.max_scroll_offset(viewport_height.into_pixels());
        if list_state.scroll_top > max_scroll_top {
            list_state.scroll_top = max_scroll_top;
            (rendered_range, self.children) =
                self.render_visible_items(child_constraint, &mut list_state, ctx, app);
        }

        // If the current scroll item was invalidated (height was None at layout start),
        // apply scroll preservation to maintain visual position.
        if scroll_item_was_invalidated
            && let Some(scroll_ctx) = &list_state.current_scroll_context
            && let Some(scroll_preservation) = &list_state.scroll_preservation
            && let Some(new_scroll_offset) =
                (scroll_preservation.adjustment_fn)(scroll_item_index, scroll_ctx, app)
        {
            list_state.scroll_top.offset_from_start = new_scroll_offset;
            (rendered_range, self.children) =
                self.render_visible_items(child_constraint, &mut list_state, ctx, app);
        }

        self.first_child_index = rendered_range.start;
        self.size = size;
        size
    }

    fn after_layout(&mut self, ctx: &mut super::AfterLayoutContext, app: &super::AppContext) {
        for child in &mut self.children {
            child.after_layout(ctx, app);
        }
    }

    fn paint(&mut self, origin: Vector2F, ctx: &mut super::PaintContext, app: &super::AppContext) {
        self.origin = Some(super::Point::from_vec2f(origin, ctx.scene.z_index()));
        ctx.scene.start_layer(ClipBounds::BoundedBy(RectF::new(
            origin,
            self.size().expect("size should be set at paint time"),
        )));

        let mut origin = {
            let mut list_state = self.list_state.0.borrow_mut();
            list_state.origin = Some(origin);
            // Offset the origin by the scroll top offset since the child may be only partially in
            // the viewport.
            origin - vec2f(0., list_state.scroll_top.offset_from_start.as_f32())
        };

        let mut child_bounds = Vec::with_capacity(self.children.len());
        for child in &mut self.children {
            child.paint(origin, ctx, app);
            let child_size = child.size().expect("Child should exist at paint time");
            child_bounds.push(RectF::new(origin, child_size));
            origin.set_y(origin.y() + child_size.y());
        }

        ctx.scene.stop_layer();
        self.cache_selection_text(&child_bounds, ctx.current_selection);
    }

    fn size(&self) -> Option<Vector2F> {
        Some(self.size)
    }

    fn origin(&self) -> Option<super::Point> {
        self.origin
    }

    fn dispatch_event(
        &mut self,
        event: &crate::event::DispatchedEvent,
        ctx: &mut super::EventContext,
        app: &super::AppContext,
    ) -> bool {
        let mut handled = false;
        for child in &mut self.children {
            let child_dispatch = child.dispatch_event(event, ctx, app);
            handled |= child_dispatch;
        }

        handled
    }
}

// T: 'static is required by the struct definition (List<T: 'static>), which
// needs it because ScrollableElement::finish_scrollable requires Self: 'static.
impl<T: 'static> ScrollableElement for List<T> {
    fn scroll_data(&self, _app: &super::AppContext) -> Option<super::ScrollData> {
        Some(self.vertical_scroll_data())
    }

    fn scroll(&mut self, delta: Pixels, ctx: &mut super::EventContext) {
        self.scroll_vertically(delta, ctx);
    }

    fn should_handle_scroll_wheel(&self) -> bool {
        true
    }
}

impl<T: 'static> NewScrollableElement for List<T> {
    fn axis(&self) -> ScrollableAxis {
        ScrollableAxis::Vertical
    }

    fn scroll_data(&self, axis: Axis, _app: &AppContext) -> Option<ScrollData> {
        match axis {
            Axis::Horizontal => None,
            Axis::Vertical => Some(self.vertical_scroll_data()),
        }
    }

    fn axis_should_handle_scroll_wheel(&self, _axis: super::Axis) -> bool {
        true
    }

    fn scroll(&mut self, delta: Pixels, axis: super::Axis, ctx: &mut super::EventContext) {
        match axis {
            Axis::Horizontal => {}
            Axis::Vertical => self.scroll_vertically(delta, ctx),
        }
    }
}

#[derive(Clone, Derivative)]
#[derivative(Debug)]
struct ListItem {
    /// Whether this element has been laid out and painted.
    height: Option<Pixels>,
}

#[derive(Debug, Clone, Default)]
struct LayoutSummary {
    // Total height of all items in the sum tree.
    height: Pixels,
    // Total number of items in the sum tree.
    count: usize,
    // Number of items that have been measured.
    measured_count: usize,
}

impl sum_tree::Item for ListItem {
    type Summary = LayoutSummary;

    fn summary(&self) -> Self::Summary {
        let height = self.height;
        LayoutSummary {
            height: height.unwrap_or_default(),
            count: 1,
            measured_count: height.is_some() as usize,
        }
    }
}

impl AddAssign<&LayoutSummary> for LayoutSummary {
    fn add_assign(&mut self, rhs: &LayoutSummary) {
        self.height += rhs.height;
        self.count += rhs.count;
        self.measured_count += rhs.measured_count;
    }
}

/// Height of a list item, in pixels.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Height(OrderedFloat<Pixels>);

impl From<Pixels> for Height {
    fn from(value: Pixels) -> Self {
        Self(OrderedFloat(value))
    }
}

impl<'a> sum_tree::Dimension<'a, LayoutSummary> for Height {
    fn add_summary(&mut self, summary: &'a LayoutSummary) {
        self.0 += summary.height
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, AddAssign)]
struct Count(usize);

impl<'a> sum_tree::Dimension<'a, LayoutSummary> for Count {
    fn add_summary(&mut self, summary: &'a LayoutSummary) {
        self.0 += summary.count;
    }
}

struct ListStateInner<T> {
    content: SumTree<ListItem>,
    scroll_top: ScrollOffset,
    /// The last known measured item where every item _up to_ this item has been measured.
    ///
    /// This can differ from the number of measured items in the sumtree if an item in the middle
    /// of the measured range was invalidated.
    last_measured_index: usize,
    render_fn: Arc<ListItemRenderFn>,
    viewport_height: Pixels,
    /// Optional scroll preservation callback.
    scroll_preservation: Option<ScrollPreservation<T>>,
    /// Persistently stored scroll context, updated by the consumer (e.g. code
    /// review view) on every scroll event. Used during layout to adjust scroll
    /// position when the current scroll item's height was invalidated.
    current_scroll_context: Option<T>,
    /// Scroll position updates are sent through this channel whenever the
    /// user scrolls. Created by [`ListState::new_with_scroll_preservation`].
    /// Cleared automatically if the receiver is dropped (channel closed).
    scroll_tx: Option<async_channel::Sender<ScrollOffset>>,
    /// Where the list was last painted, in window coordinates.
    origin: Option<Vector2F>,
    selection_text: SelectionTextCache,
}

/// The text of items seen on screen while there's a selection, by item index.
#[derive(Default)]
struct SelectionTextCache {
    /// All of an item's text.
    whole: HashMap<usize, String>,
    /// The selected part of an item that holds an end of the selection.
    selected: HashMap<usize, String>,
}

impl SelectionTextCache {
    fn clear(&mut self) {
        self.whole.clear();
        self.selected.clear();
    }

    /// Forgets the items from `index` on, whose content or position changed.
    fn forget_from(&mut self, index: usize) {
        self.whole.retain(|item, _| *item < index);
        self.selected.retain(|item, _| *item < index);
    }
}

impl ListStateInner<()> {
    fn new(render_fn: Arc<ListItemRenderFn>) -> Self {
        Self {
            content: SumTree::new(),
            scroll_top: ScrollOffset::default(),
            last_measured_index: 0,
            render_fn,
            viewport_height: Pixels::zero(),
            scroll_preservation: None,
            current_scroll_context: None,
            scroll_tx: None,
            origin: None,
            selection_text: SelectionTextCache::default(),
        }
    }
}

impl<T> ListStateInner<T> {
    fn new_with_scroll_preservation(
        render_fn: Arc<ListItemRenderFn>,
        adjustment_fn: impl Fn(usize, &T, &AppContext) -> Option<Pixels> + 'static,
        scroll_tx: async_channel::Sender<ScrollOffset>,
    ) -> Self {
        Self {
            content: SumTree::new(),
            scroll_top: ScrollOffset::default(),
            last_measured_index: 0,
            render_fn,
            viewport_height: Pixels::zero(),
            scroll_preservation: Some(ScrollPreservation {
                adjustment_fn: Box::new(adjustment_fn),
            }),
            current_scroll_context: None,
            scroll_tx: Some(scroll_tx),
            origin: None,
            selection_text: SelectionTextCache::default(),
        }
    }
}

impl<T> ListStateInner<T> {
    /// Gets the height of an item at the given index.
    pub fn height_for_index(&self, index: usize) -> Option<Pixels> {
        let mut cursor = self.content.cursor::<Count, ()>();
        cursor.seek(&Count(index), sum_tree::SeekBias::Right);
        cursor.item().and_then(|item| item.height)
    }

    /// Converts an absolute pixel position from the top into a (item_index, offset) scroll position.
    /// This handles both measured and unmeasured regions of the list.
    fn absolute_pixels_to_scroll_offset(&self, absolute_pixels: Pixels) -> ScrollOffset {
        // If we're scrolling to something that hasn't been measured, we determine which
        // item to scroll to based on the average height of the measured items. We can't
        // seek into the sum tree because the sum tree only has the height of _measured_ items
        // and we still want to scroll to an approximate position in the list instead of just to the end.
        // If the new position is within the measured range, we can directly seek into the sum tree
        // to get an exact location to scroll to.
        let approximate_item = (absolute_pixels / self.average_height_per_measured_item()).as_f32();
        let approximate_item = approximate_item.min(self.content.summary().count as f32);

        let approximate_scroll_item = approximate_item.floor() as usize;
        let approximate_scroll_offset =
            (approximate_item.fract()).into_pixels() * self.average_height_per_measured_item();

        if approximate_scroll_item > self.last_measured_index {
            ScrollOffset {
                list_item_index: Count(approximate_scroll_item),
                offset_from_start: approximate_scroll_offset,
            }
        } else {
            let new_scroll_item = {
                let mut cursor = self.content.cursor::<Height, Count>();
                cursor.seek(&Height(absolute_pixels.into()), sum_tree::SeekBias::Right);
                *cursor.start()
            };

            let height = {
                let mut cursor = self.content.cursor::<Count, Height>();
                cursor.seek(&new_scroll_item, sum_tree::SeekBias::Right);
                cursor.start().0
            };

            let offset = absolute_pixels - height.0;

            ScrollOffset {
                list_item_index: new_scroll_item,
                offset_from_start: offset,
            }
        }
    }

    /// Returns the approximate height of the list based on the elements that have been measured so far.
    /// If all elements have been measured, this returns the exact height.
    fn approximate_height(&self) -> Pixels {
        let summary = self.content.summary();

        if summary.count == summary.measured_count {
            return summary.height;
        }
        let total_height = summary.height;
        let total_items = summary.count;
        let measured_items = summary.measured_count;

        if measured_items == 0 {
            return Pixels::zero();
        }

        ((total_height.as_f32() / measured_items as f32) * total_items as f32).into_pixels()
    }

    /// Returns the average height of the items that have been measured so far.
    fn average_height_per_measured_item(&self) -> Pixels {
        let summary = self.content.summary();
        let total_height = summary.height;
        let measured_items = summary.measured_count;

        (total_height.as_f32() / measured_items as f32).into_pixels()
    }

    fn invalidate_height_for_index(&mut self, index: usize) {
        let (new_tree, last_measured) = {
            let mut cursor = self.content.cursor::<Count, ()>();
            let mut new_items = cursor.slice(&Count(index), sum_tree::SeekBias::Right);
            // The last measured item is now the last measured item _before_ the index we're invalidating.
            let last_measured = new_items.summary().measured_count.saturating_sub(1);

            let list_item = ListItem { height: None };

            new_items.push(list_item);
            cursor.next();
            new_items.push_tree(cursor.suffix());
            (new_items, last_measured)
        };

        self.content = new_tree;
        self.last_measured_index = self.last_measured_index.min(last_measured);
        self.selection_text.forget_from(index);
    }

    fn remove(&mut self, index: usize) {
        let (new_tree, last_measured) = {
            let mut cursor = self.content.cursor::<Count, ()>();
            let mut new_items = cursor.slice(&Count(index), sum_tree::SeekBias::Right);
            cursor.next();
            // The last measured item is now the last measured item _before_ the index we're invalidating.
            let last_measured = new_items.summary().measured_count.saturating_sub(1);
            new_items.push_tree(cursor.suffix());
            (new_items, last_measured)
        };

        self.content = new_tree;
        self.last_measured_index = self.last_measured_index.min(last_measured);
        if self.scroll_top.list_item_index.0 > index {
            self.scroll_top.list_item_index.0 -= 1;
        }
        self.selection_text.forget_from(index);
    }

    fn scroll_by(&mut self, delta: Pixels) {
        let max_scroll_top = (self.approximate_height() - self.viewport_height).max(Pixels::zero());
        let scroll_top = (self.scroll_top_pixels() + delta)
            .max(Pixels::zero())
            .min(max_scroll_top);
        self.scroll_top = self.absolute_pixels_to_scroll_offset(scroll_top);
        self.broadcast_scroll_event();
    }

    /// How far below the top of the content an absolute point is, going by where the list was
    /// last painted.
    fn content_y(&self, absolute_point: Vector2F) -> Option<Pixels> {
        let origin = self.origin?;
        Some((absolute_point.y() - origin.y()).into_pixels() + self.scroll_top_pixels())
    }

    /// The item at `content_y`; the last item when it's past the end.
    fn item_at_content_y(&self, content_y: Pixels) -> Option<usize> {
        let count = self.content.summary().count;
        if count == 0 {
            return None;
        }
        let mut cursor = self.content.cursor::<Height, Count>();
        cursor.seek(
            &Height(content_y.max(Pixels::zero()).into()),
            sum_tree::SeekBias::Right,
        );
        Some(cursor.start().0.min(count - 1))
    }

    /// The item at an absolute point, on or off screen.
    fn item_at(&self, absolute_point: Vector2F) -> Option<usize> {
        self.item_at_content_y(self.content_y(absolute_point)?)
    }

    /// How far below the top of the content an item starts. Items not yet measured count as
    /// empty, which keeps the ones below the viewport below it.
    fn item_top(&self, index: usize) -> Pixels {
        let mut cursor = self.content.cursor::<Count, Height>();
        cursor.seek(&Count(index), sum_tree::SeekBias::Right);
        cursor.start().0.0
    }

    /// Number of pixels scrolled from the top.
    fn scroll_top_pixels(&self) -> Pixels {
        let mut cursor = self.content.cursor::<Count, Height>();
        cursor.seek(&self.scroll_top.list_item_index, sum_tree::SeekBias::Right);
        cursor.start().0.0 + self.scroll_top.offset_from_start
    }

    fn add_item(&mut self) {
        self.content.push(ListItem { height: None });
    }

    fn scroll_to(&mut self, index: usize, offset_from_start: Option<Pixels>) {
        let new_scroll_top = ScrollOffset {
            list_item_index: Count(index),
            offset_from_start: offset_from_start.unwrap_or(Pixels::zero()),
        };
        self.scroll_top = new_scroll_top;
        self.broadcast_scroll_event();
    }

    /// Sends the current scroll position through the channel, if any.
    /// If the channel is full, the event is silently dropped (the consumer
    /// uses debouncing and will catch up on the next event).
    fn broadcast_scroll_event(&mut self) {
        if let Some(tx) = &self.scroll_tx {
            if tx.is_closed() {
                self.scroll_tx = None;
            } else {
                let _ = tx.try_send(self.scroll_top);
            }
        }
    }

    /// Returns the maximum scroll offset based on the approximate height of the list.
    fn max_scroll_offset(&self, viewport_height: Pixels) -> ScrollOffset {
        let max_scroll_top = (self.approximate_height() - viewport_height).max(Pixels::zero());

        let index = {
            let mut cursor = self.content.cursor::<Height, Count>();
            cursor.seek(&Height(max_scroll_top.into()), sum_tree::SeekBias::Right);
            *cursor.start()
        };

        let height = {
            let mut cursor = self.content.cursor::<Count, Height>();
            cursor.seek(&index, sum_tree::SeekBias::Right);
            cursor.start().0
        };

        let offset = max_scroll_top - height.0;

        ScrollOffset {
            list_item_index: index,
            offset_from_start: offset,
        }
    }
}

#[cfg(test)]
#[path = "viewported_list_tests.rs"]
mod tests;
