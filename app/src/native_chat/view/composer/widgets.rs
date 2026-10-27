//! Small building blocks shared by the composer's parts: text, icons, buttons and chips.

use pathfinder_color::ColorU;
use warp_core::ui::Icon as WarpIcon;
use warpui::elements::{
    Border, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, Element, Empty, Flex,
    Hoverable, Icon, MainAxisAlignment, MainAxisSize, MouseStateHandle, ParentElement, Radius,
    Text,
};
use warpui::fonts::{FamilyId, Properties, Weight};
use warpui::platform::Cursor;
use warpui::text_layout::ClipConfig;

use super::ComposerAction;
use crate::native_chat::design::{Palette, Sizes};

/// Everything a render needs from the design tokens, resolved once.
#[derive(Clone)]
pub struct Look {
    pub palette: Palette,
    pub sizes: Sizes,
    pub ui_font: FamilyId,
    pub mono_font: FamilyId,
}

impl Look {
    pub fn text(&self, text: impl Into<String>, size: f32, color: ColorU) -> Box<dyn Element> {
        Text::new_inline(text.into(), self.ui_font, size)
            .with_color(color)
            .with_clip(ClipConfig::end())
            .finish()
    }

    pub fn medium_text(
        &self,
        text: impl Into<String>,
        size: f32,
        color: ColorU,
    ) -> Box<dyn Element> {
        Text::new_inline(text.into(), self.ui_font, size)
            .with_style(Properties::default().weight(Weight::Medium))
            .with_color(color)
            .with_clip(ClipConfig::end())
            .finish()
    }

    /// Text that wraps onto as many lines as it needs.
    pub fn paragraph(&self, text: impl Into<String>, size: f32, color: ColorU) -> Box<dyn Element> {
        Text::new(text.into(), self.ui_font, size)
            .soft_wrap(true)
            .with_color(color)
            .with_line_height_ratio(1.4)
            .finish()
    }

    pub fn mono(&self, text: impl Into<String>, color: ColorU) -> Box<dyn Element> {
        Text::new(text.into(), self.mono_font, self.sizes.code_font_size)
            .soft_wrap(true)
            .with_color(color)
            .with_line_height_ratio(1.4)
            .finish()
    }

    pub fn radius(&self, radius: f32) -> CornerRadius {
        CornerRadius::with_all(Radius::Pixels(radius))
    }
}

pub fn icon(icon: WarpIcon, color: ColorU, size: f32) -> Box<dyn Element> {
    svg(icon.into(), color, size)
}

pub fn svg(path: &'static str, color: ColorU, size: f32) -> Box<dyn Element> {
    ConstrainedBox::new(Icon::new(path, color).finish())
        .with_width(size)
        .with_height(size)
        .finish()
}

pub fn spacer(width: f32, height: f32) -> Box<dyn Element> {
    ConstrainedBox::new(Empty::new().finish())
        .with_width(width)
        .with_height(height)
        .finish()
}

/// A filled rounded rectangle.
pub fn block(width: f32, height: f32, radius: f32, color: ColorU) -> Box<dyn Element> {
    ConstrainedBox::new(
        Container::new(Empty::new().finish())
            .with_background_color(color)
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(radius)))
            .finish(),
    )
    .with_width(width)
    .with_height(height)
    .finish()
}

pub fn with_alpha(color: ColorU, alpha: u8) -> ColorU {
    ColorU { a: alpha, ..color }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    /// Filled with the accent color.
    Primary,
    /// Bordered.
    Secondary,
    /// Text only, filled on hover.
    Ghost,
}

/// A pill button with a label and an optional key hint (like "↵") after it.
pub fn button(
    look: &Look,
    mouse_state: MouseStateHandle,
    label: impl Into<String>,
    hint: Option<&'static str>,
    kind: ButtonKind,
    action: ComposerAction,
) -> Box<dyn Element> {
    let label = label.into();
    let look = look.clone();
    Hoverable::new(mouse_state, move |state| {
        let palette = &look.palette;
        let hovered = state.is_hovered();
        let (background, text_color, border) = match kind {
            ButtonKind::Primary => (
                Some(if hovered {
                    with_alpha(palette.accent.0, 0xe6)
                } else {
                    palette.accent.0
                }),
                palette.on_accent.0,
                None,
            ),
            ButtonKind::Secondary => (
                Some(if hovered {
                    palette.surface_hover.0
                } else {
                    palette.surface.0
                }),
                palette.text.0,
                Some(palette.border_strong.0),
            ),
            ButtonKind::Ghost => (
                hovered.then_some(palette.surface_hover.0),
                if hovered {
                    palette.text.0
                } else {
                    palette.text_muted.0
                },
                None,
            ),
        };
        let mut row = Flex::row()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(6.)
            .with_child(look.medium_text(label.clone(), look.sizes.small_font_size, text_color));
        if let Some(hint) = hint {
            row.add_child(look.text(
                hint,
                look.sizes.tiny_font_size,
                with_alpha(text_color, 0xa0),
            ));
        }
        let mut container = Container::new(
            ConstrainedBox::new(
                Flex::row()
                    .with_main_axis_size(MainAxisSize::Min)
                    .with_main_axis_alignment(MainAxisAlignment::Center)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_child(row.finish())
                    .finish(),
            )
            .with_height(look.sizes.chip_height + 2.)
            .finish(),
        )
        .with_horizontal_padding(12.)
        .with_corner_radius(look.radius(look.sizes.chip_radius + 1.));
        if let Some(background) = background {
            container = container.with_background_color(background);
        }
        if let Some(border) = border {
            container = container.with_border(Border::all(1.).with_border_color(border));
        }
        container.finish()
    })
    .with_cursor(Cursor::PointingHand)
    .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
    .finish()
}

/// A round icon button filled on hover.
pub fn icon_button(
    look: &Look,
    mouse_state: MouseStateHandle,
    warp_icon: WarpIcon,
    size: f32,
    action: ComposerAction,
) -> Box<dyn Element> {
    let look = look.clone();
    Hoverable::new(mouse_state, move |state| {
        let palette = &look.palette;
        let color = if state.is_hovered() {
            palette.text.0
        } else {
            palette.text_muted.0
        };
        let mut container = Container::new(icon(warp_icon, color, look.sizes.icon_size))
            .with_uniform_padding((size - look.sizes.icon_size) / 2.)
            .with_corner_radius(look.radius(size / 2.));
        if state.is_hovered() {
            container = container.with_background_color(palette.surface_hover.0);
        }
        container.finish()
    })
    .with_cursor(Cursor::PointingHand)
    .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
    .finish()
}

/// A borderless footer control: text with an optional leading icon and a chevron, filled on
/// hover.
pub fn footer_control(
    look: &Look,
    mouse_state: MouseStateHandle,
    leading: Option<(WarpIcon, ColorU)>,
    parts: Vec<(String, ColorU)>,
    action: ComposerAction,
) -> Box<dyn Element> {
    let look = look.clone();
    Hoverable::new(mouse_state, move |state| {
        let palette = &look.palette;
        let mut row = Flex::row()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(5.);
        if let Some((warp_icon, color)) = leading {
            row.add_child(icon(warp_icon, color, look.sizes.small_icon_size));
        }
        for (text, color) in &parts {
            row.add_child(look.text(text.clone(), look.sizes.small_font_size, *color));
        }
        row.add_child(icon(
            WarpIcon::ChevronDown,
            palette.text_faint.0,
            look.sizes.small_icon_size - 3.,
        ));
        let mut container = Container::new(
            ConstrainedBox::new(row.finish())
                .with_height(look.sizes.chip_height - 2.)
                .finish(),
        )
        .with_horizontal_padding(8.)
        .with_corner_radius(look.radius(look.sizes.chip_radius));
        if state.is_hovered() {
            container = container.with_background_color(palette.surface_hover.0);
        }
        container.finish()
    })
    .with_cursor(Cursor::PointingHand)
    .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
    .finish()
}

/// A keyboard key, like the digit next to a question's option.
pub fn key_cap(look: &Look, label: impl Into<String>, highlighted: bool) -> Box<dyn Element> {
    let palette = &look.palette;
    let (background, color) = if highlighted {
        (palette.accent.0, palette.on_accent.0)
    } else {
        (palette.surface_strong.0, palette.text_muted.0)
    };
    ConstrainedBox::new(
        Container::new(
            Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_main_axis_alignment(MainAxisAlignment::Center)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(look.medium_text(label, look.sizes.tiny_font_size, color))
                .finish(),
        )
        .with_background_color(background)
        .with_corner_radius(look.radius(5.))
        .finish(),
    )
    .with_width(18.)
    .with_height(18.)
    .finish()
}
