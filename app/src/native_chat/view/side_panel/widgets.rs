//! The side panel's building blocks: cards, rows, bars, the switch and small buttons.

use pathfinder_color::ColorU;
use pathfinder_geometry::vector::vec2f;
use warpui::elements::{
    Border, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, DropShadow, Element,
    Empty, Expanded, Flex, Hoverable, Icon as SvgIcon, MainAxisAlignment, MainAxisSize,
    MouseStateHandle, ParentElement, Radius, Rect, Shrinkable, Stack, Text,
};
use warpui::fonts::{FamilyId, Properties, Weight};
use warpui::platform::Cursor;
use warpui::text_layout::ClipConfig;

use super::SidePanelAction;
use crate::native_chat::design::{Palette, Sizes};

pub(super) const CHEVRON_RIGHT_ICON: &str = "bundled/svg/picolas-chevron-right.svg";
pub(super) const CHEVRON_DOWN_ICON: &str = "bundled/svg/picolas-chevron-down.svg";
/// Shown on a card's header on hover, since dragging the card reorders the cards.
const GRIP_ICON: &str = "bundled/svg/picolas-grip.svg";
const GRIP_SIZE: f32 = 12.;
const DRAG_OUTLINE_WIDTH: f32 = 1.5;

pub(super) const CARD_PADDING: f32 = 6.;
pub(super) const ROW_PADDING_X: f32 = 8.;
pub(super) const ROW_PADDING_Y: f32 = 6.;
const ROW_RADIUS: f32 = 8.;
pub(super) const ICON_GAP: f32 = 10.;
const CHEVRON_SIZE: f32 = 11.;
const SWITCH_WIDTH: f32 = 28.;
const SWITCH_HEIGHT: f32 = 16.;
const SWITCH_INSET: f32 = 2.;
const ICON_BUTTON_PADDING: f32 = 4.;
const ICON_BUTTON_RADIUS: f32 = 6.;
const CRITICAL_PERCENT: f64 = 95.;

/// Everything a render function needs to look the part.
#[derive(Clone, Copy)]
pub(super) struct Look<'a> {
    pub palette: &'a Palette,
    pub sizes: &'a Sizes,
    pub font: FamilyId,
}

impl Look<'_> {
    pub fn text(
        &self,
        text: impl Into<String>,
        size: f32,
        color: ColorU,
        weight: Weight,
    ) -> Box<dyn Element> {
        Text::new_inline(text.into(), self.font, size)
            .with_style(Properties::default().weight(weight))
            .with_color(color)
            .with_clip(ClipConfig::ellipsis())
            .finish()
    }

    /// Body text of a row.
    pub fn label(&self, text: impl Into<String>, color: ColorU) -> Box<dyn Element> {
        self.text(text, self.sizes.small_font_size, color, Weight::Normal)
    }

    /// Small secondary text, like token counts.
    pub fn meta(&self, text: impl Into<String>, color: ColorU) -> Box<dyn Element> {
        self.text(text, self.sizes.tiny_font_size, color, Weight::Normal)
    }
}

pub(super) fn svg(path: &'static str, color: ColorU, size: f32) -> Box<dyn Element> {
    ConstrainedBox::new(SvgIcon::new(path, color).finish())
        .with_width(size)
        .with_height(size)
        .finish()
}

/// A rounded block of one color.
pub(super) fn swatch(color: ColorU, width: f32, height: f32, radius: f32) -> Box<dyn Element> {
    ConstrainedBox::new(
        Rect::new()
            .with_background_color(color)
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(radius)))
            .finish(),
    )
    .with_width(width)
    .with_height(height)
    .finish()
}

/// A card's contents, padded; [`card_frame`] gives it its surface.
pub(super) fn card(children: Vec<Box<dyn Element>>) -> Box<dyn Element> {
    Container::new(
        Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_children(children)
            .finish(),
    )
    .with_uniform_padding(CARD_PADDING)
    .finish()
}

/// The floating surface of a card: rounded, with a soft shadow, outlined in the accent color while
/// it's being dragged.
pub(super) fn card_frame(
    content: Box<dyn Element>,
    is_dragging: bool,
    look: Look<'_>,
) -> Box<dyn Element> {
    let palette = look.palette;
    let border = if is_dragging {
        Border::all(DRAG_OUTLINE_WIDTH).with_border_color(palette.accent.0)
    } else {
        Border::all(1.).with_border_color(palette.border.0)
    };
    Container::new(content)
        .with_background_color(palette.surface.0)
        .with_border(border)
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(
            look.sizes.card_radius,
        )))
        .with_drop_shadow(DropShadow {
            color: palette.shadow.0,
            offset: vec2f(0., 4.),
            blur_radius: look.sizes.shadow_blur,
            spread_radius: 0.,
        })
        .finish()
}

/// A card's title with its chevron; `trailing` sits at the right edge.
pub(super) fn card_header(
    title: &'static str,
    is_expanded: bool,
    trailing: Option<Box<dyn Element>>,
    mouse_state: MouseStateHandle,
    action: SidePanelAction,
    look: Look<'_>,
) -> Box<dyn Element> {
    let palette = look.palette;
    Hoverable::new(mouse_state, move |state| {
        let chevron = if is_expanded {
            CHEVRON_DOWN_ICON
        } else {
            CHEVRON_RIGHT_ICON
        };
        let chevron_color = if state.is_hovered() {
            palette.text_muted.0
        } else {
            palette.text_faint.0
        };
        let label = Flex::row()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(6.)
            .with_child(look.text(
                title,
                look.sizes.small_font_size + 0.5,
                palette.text.0,
                Weight::Semibold,
            ))
            .with_child(svg(chevron, chevron_color, CHEVRON_SIZE))
            .finish();
        let mut row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(8.)
            .with_child(label)
            .with_child(Expanded::new(1., Empty::new().finish()).finish());
        if let Some(trailing) = trailing {
            row.add_child(trailing);
        }
        if state.is_hovered() {
            row.add_child(svg(GRIP_ICON, palette.text_faint.0, GRIP_SIZE));
        }
        row_container(row.finish(), state.is_hovered(), look)
    })
    .with_cursor(Cursor::PointingHand)
    .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
    .finish()
}

/// Padding and rounded hover background shared by every row.
pub(super) fn row_container(
    child: Box<dyn Element>,
    is_hovered: bool,
    look: Look<'_>,
) -> Box<dyn Element> {
    let container = Container::new(
        ConstrainedBox::new(child)
            .with_min_height(look.sizes.icon_size + 4.)
            .finish(),
    )
    .with_horizontal_padding(ROW_PADDING_X)
    .with_vertical_padding(ROW_PADDING_Y)
    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(ROW_RADIUS)));
    if is_hovered {
        container
            .with_background_color(look.palette.surface_hover.0)
            .finish()
    } else {
        container.finish()
    }
}

/// `leading` (an icon), a label that ellipsizes, and `trailing` at the right edge.
pub(super) fn row_content(
    leading: Option<Box<dyn Element>>,
    label: Box<dyn Element>,
    trailing: Option<Box<dyn Element>>,
) -> Box<dyn Element> {
    let mut row = Flex::row()
        .with_main_axis_size(MainAxisSize::Max)
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(ICON_GAP);
    if let Some(leading) = leading {
        row.add_child(leading);
    }
    row.add_child(Expanded::new(1., label).finish());
    if let Some(trailing) = trailing {
        row.add_child(trailing);
    }
    row.finish()
}

/// A row that does `action` when clicked.
pub(super) fn clickable_row(
    mouse_state: MouseStateHandle,
    action: SidePanelAction,
    look: Look<'_>,
    build: impl FnOnce(bool) -> Box<dyn Element>,
) -> Box<dyn Element> {
    Hoverable::new(mouse_state, move |state| {
        row_container(build(state.is_hovered()), state.is_hovered(), look)
    })
    .with_cursor(Cursor::PointingHand)
    .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
    .with_defer_events_to_children()
    .finish()
}

/// A static row.
pub(super) fn plain_row(child: Box<dyn Element>, look: Look<'_>) -> Box<dyn Element> {
    row_container(child, false, look)
}

/// The on/off pill of a setting.
pub(super) fn switch(is_on: bool, is_enabled: bool, look: Look<'_>) -> Box<dyn Element> {
    let palette = look.palette;
    let track = match (is_on, is_enabled) {
        (true, true) => palette.accent.0,
        (true, false) => ColorU {
            a: 115,
            ..palette.accent.0
        },
        (false, true) => palette.border_strong.0,
        (false, false) => palette.surface_strong.0,
    };
    let thumb_color = if is_enabled {
        palette.on_accent.0
    } else {
        palette.text_faint.0
    };
    let thumb_size = SWITCH_HEIGHT - 2. * SWITCH_INSET;
    let thumb = swatch(thumb_color, thumb_size, thumb_size, thumb_size / 2.);
    let alignment = if is_on {
        MainAxisAlignment::End
    } else {
        MainAxisAlignment::Start
    };
    ConstrainedBox::new(
        Container::new(
            Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_main_axis_alignment(alignment)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(thumb)
                .finish(),
        )
        .with_uniform_padding(SWITCH_INSET)
        .with_background_color(track)
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(SWITCH_HEIGHT / 2.)))
        .finish(),
    )
    .with_width(SWITCH_WIDTH)
    .with_height(SWITCH_HEIGHT)
    .finish()
}

/// A clickable switch.
pub(super) fn switch_button(
    is_on: bool,
    is_enabled: bool,
    mouse_state: MouseStateHandle,
    action: SidePanelAction,
    look: Look<'_>,
) -> Box<dyn Element> {
    let hoverable = Hoverable::new(mouse_state, move |_| switch(is_on, is_enabled, look));
    if !is_enabled {
        return hoverable.finish();
    }
    hoverable
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
        .finish()
}

/// A track filled to `fraction` (0 to 1).
pub(in crate::native_chat::view) fn progress_bar(
    fraction: f64,
    fill: ColorU,
    track: ColorU,
    height: f32,
) -> Box<dyn Element> {
    let fraction = fraction.clamp(0., 1.) as f32;
    let radius = height / 2.;
    let mut row = Flex::row().with_main_axis_size(MainAxisSize::Max);
    if fraction > 0. {
        // A sliver still shows as a dot rather than vanishing.
        let filled = fraction.max(0.02);
        let block = ConstrainedBox::new(
            Rect::new()
                .with_background_color(fill)
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(radius)))
                .finish(),
        )
        .with_height(height)
        .finish();
        row.add_child(Expanded::new(filled, block).finish());
        if filled < 1. {
            row.add_child(Expanded::new(1. - filled, Empty::new().finish()).finish());
        }
    } else {
        row.add_child(Expanded::new(1., Empty::new().finish()).finish());
    }
    ConstrainedBox::new(
        Container::new(row.finish())
            .with_background_color(track)
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(radius)))
            .finish(),
    )
    .with_height(height)
    .finish()
}

/// A circular gauge filled clockwise to `fraction` (0 to 1), in steps of 5%.
pub(in crate::native_chat::view) fn ring(
    fraction: f64,
    fill: ColorU,
    track: ColorU,
    size: f32,
) -> Box<dyn Element> {
    let mut ring = Stack::new().with_child(svg(RING_TRACK_ICON, track, size));
    if let Some(arc) = ring_arc(fraction) {
        ring.add_child(svg(arc, fill, size));
    }
    ring.finish()
}

const RING_TRACK_ICON: &str = "bundled/svg/picolas-ring-track.svg";
const RING_ARCS: [&str; 20] = [
    "bundled/svg/picolas-ring-005.svg",
    "bundled/svg/picolas-ring-010.svg",
    "bundled/svg/picolas-ring-015.svg",
    "bundled/svg/picolas-ring-020.svg",
    "bundled/svg/picolas-ring-025.svg",
    "bundled/svg/picolas-ring-030.svg",
    "bundled/svg/picolas-ring-035.svg",
    "bundled/svg/picolas-ring-040.svg",
    "bundled/svg/picolas-ring-045.svg",
    "bundled/svg/picolas-ring-050.svg",
    "bundled/svg/picolas-ring-055.svg",
    "bundled/svg/picolas-ring-060.svg",
    "bundled/svg/picolas-ring-065.svg",
    "bundled/svg/picolas-ring-070.svg",
    "bundled/svg/picolas-ring-075.svg",
    "bundled/svg/picolas-ring-080.svg",
    "bundled/svg/picolas-ring-085.svg",
    "bundled/svg/picolas-ring-090.svg",
    "bundled/svg/picolas-ring-095.svg",
    "bundled/svg/picolas-ring-100.svg",
];

/// The arc of a ring filled to `fraction`; a sliver still shows the first step rather than
/// nothing.
pub(in crate::native_chat::view) fn ring_arc(fraction: f64) -> Option<&'static str> {
    if fraction.is_nan() || fraction <= 0. {
        return None;
    }
    let step = ((fraction.min(1.) * 20.).round() as usize).clamp(1, RING_ARCS.len());
    Some(RING_ARCS[step - 1])
}

/// A fixed-width progress bar, for summaries in collapsed headers.
pub(super) fn mini_bar(fraction: f64, fill: ColorU, track: ColorU) -> Box<dyn Element> {
    ConstrainedBox::new(progress_bar(fraction, fill, track, 4.))
        .with_width(40.)
        .finish()
}

/// An icon that does `action` when clicked, with a hover background.
pub(super) fn icon_button(
    path: &'static str,
    size: f32,
    mouse_state: MouseStateHandle,
    action: SidePanelAction,
    look: Look<'_>,
) -> Box<dyn Element> {
    let palette = look.palette;
    Hoverable::new(mouse_state, move |state| {
        let color = if state.is_hovered() {
            palette.text.0
        } else {
            palette.text_muted.0
        };
        let button = Container::new(svg(path, color, size))
            .with_uniform_padding(ICON_BUTTON_PADDING)
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(ICON_BUTTON_RADIUS)));
        if state.is_hovered() {
            button
                .with_background_color(palette.surface_strong.0)
                .finish()
        } else {
            button.finish()
        }
    })
    .with_cursor(Cursor::PointingHand)
    .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
    .finish()
}

/// Text that acts like a link: `color`, brighter on hover.
pub(super) fn text_link(
    text: &'static str,
    color: ColorU,
    mouse_state: MouseStateHandle,
    action: SidePanelAction,
    look: Look<'_>,
) -> Box<dyn Element> {
    Hoverable::new(mouse_state, move |state| {
        let weight = if state.is_hovered() {
            Weight::Semibold
        } else {
            Weight::Medium
        };
        look.text(text, look.sizes.tiny_font_size + 0.5, color, weight)
    })
    .with_cursor(Cursor::PointingHand)
    .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
    .finish()
}

/// A full-width secondary button.
pub(super) fn wide_button(
    text: &'static str,
    is_enabled: bool,
    mouse_state: MouseStateHandle,
    action: SidePanelAction,
    look: Look<'_>,
) -> Box<dyn Element> {
    let palette = look.palette;
    let hoverable = Hoverable::new(mouse_state, move |state| {
        let (background, color) = match (is_enabled, state.is_hovered()) {
            (false, _) => (palette.surface.0, palette.text_faint.0),
            (true, true) => (palette.surface_hover.0, palette.text.0),
            (true, false) => (palette.surface.0, palette.text.0),
        };
        let label = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_main_axis_alignment(MainAxisAlignment::Center)
            .with_child(Shrinkable::new(1., look.label(text, color)).finish())
            .finish();
        Container::new(label)
            .with_vertical_padding(7.)
            .with_horizontal_padding(ROW_PADDING_X)
            .with_background_color(background)
            .with_border(Border::all(1.).with_border_color(palette.border_strong.0))
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(
                look.sizes.chip_radius,
            )))
            .finish()
    });
    if !is_enabled {
        return hoverable.finish();
    }
    hoverable
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
        .finish()
}

/// A meter's color: `normal`, then the warning color from `warning_percent` and the error color
/// when nearly full.
pub(in crate::native_chat::view) fn meter_color(
    percent: f64,
    normal: ColorU,
    warning_percent: f64,
    palette: &Palette,
) -> ColorU {
    if percent >= CRITICAL_PERCENT {
        palette.error.0
    } else if percent >= warning_percent {
        palette.warning.0
    } else {
        normal
    }
}

/// A path that fades out at its start when it doesn't fit, keeping the folder's own name in view.
pub(super) fn path_label(
    path: String,
    size: f32,
    color: ColorU,
    look: Look<'_>,
) -> Box<dyn Element> {
    Flex::row()
        .with_main_axis_size(MainAxisSize::Max)
        .with_child(
            Shrinkable::new(
                1.,
                Text::new_inline(path, look.font, size)
                    .with_color(color)
                    .with_clip(ClipConfig::start())
                    .finish(),
            )
            .finish(),
        )
        .with_child(Expanded::new(1., Empty::new().finish()).finish())
        .finish()
}
