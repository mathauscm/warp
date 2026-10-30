//! Drawing the chips of files and folders named in prose, and the quotes a message mentions.

use std::path::{Path, PathBuf};

use claude_chat::attachments::{AttachedFile, AttachmentKind};
use claude_chat::presentation::QuestionAnswer;

use markdown_parser::{FormattedText, Hyperlink};
use pathfinder_color::ColorU;
use pathfinder_geometry::vector::{Vector2F, vec2f};
use warp_core::ui::Icon as WarpIcon;
use warpui::elements::{
    Border, ChildAnchor, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, DropShadow,
    Flex, FormattedTextElement, Highlight, HighlightedRange, Hoverable, MainAxisSize,
    MouseStateHandle, OffsetPositioning, ParentAnchor, ParentElement, ParentOffsetBounds,
    PartialClickableElement, Point, Radius, SelectableElement, Shrinkable, Stack, Text,
};
use warpui::event::DispatchedEvent;
use warpui::platform::Cursor;
use warpui::text_layout::{ClipConfig, TextBorder, TextStyle};
use warpui::{
    AfterLayoutContext, AppContext, Element, EventContext, LayoutContext, PaintContext,
    SizeConstraint,
};

use super::TranscriptAction;
use super::paths::{PathChip, PathKind};
use super::render::{RowContext, Style, icon};
use crate::native_chat::design::Palette;

const ICON_FOLDER: &str = "bundled/svg/picolas-folder.svg";
const ICON_IMAGE: &str = "bundled/svg/picolas-image.svg";
const ICON_VIDEO: &str = "bundled/svg/picolas-film.svg";
const ICON_AUDIO: &str = "bundled/svg/picolas-music.svg";
const ICON_TEXT: &str = "bundled/svg/picolas-file.svg";
const ICON_CODE: &str = "bundled/svg/picolas-file-code.svg";
const ICON_ARCHIVE: &str = "bundled/svg/picolas-file-archive.svg";
const ICON_FILE: &str = "bundled/svg/picolas-file-blank.svg";

/// A path chip's icon, as a share of the font size.
const CHIP_ICON_RATIO: f32 = 0.86;
/// Space between a path chip's left edge and its icon.
const CHIP_ICON_INSET: f32 = 1.5;
const CHIP_RADIUS: u8 = 6;
/// A path chip's height, as a percentage of the font size.
const CHIP_HEIGHT_PERCENT: u8 = 132;
const TOOLTIP_MAX_WIDTH: f32 = 420.;
/// The height of the pills above a message: its quotes and attached files.
const PILL_HEIGHT: f32 = 26.;
/// Also gives the label's shrinkable row a finite width inside wrapping rows, which lay children
/// out unbounded.
const PILL_MAX_WIDTH: f32 = 420.;
const ANSWER_PILL_HEIGHT: f32 = 22.;
/// Keeps a long answer from widening its pill past a narrow pane.
const ANSWER_PILL_MAX_WIDTH: f32 = 320.;
const QUOTE_TOOLTIP_CHARS: usize = 600;
const PASTE_PREVIEW_LINES: usize = 6;
const PASTE_PREVIEW_LINE_CHARS: usize = 60;

fn chip_icon(kind: PathKind) -> &'static str {
    match kind {
        PathKind::Folder => ICON_FOLDER,
        PathKind::Image => ICON_IMAGE,
        PathKind::Video => ICON_VIDEO,
        PathKind::Audio => ICON_AUDIO,
        PathKind::Pdf => ICON_TEXT,
        PathKind::Code => ICON_CODE,
        PathKind::Archive => ICON_ARCHIVE,
        PathKind::File => ICON_FILE,
    }
}

/// The color a kind of path's icon and border take.
fn chip_tint(palette: &Palette, kind: PathKind) -> ColorU {
    match kind {
        PathKind::Folder | PathKind::Code => palette.accent.0,
        PathKind::Image => palette.kind_design.0,
        PathKind::Video => palette.kind_presentation.0,
        PathKind::Audio => palette.kind_motion.0,
        PathKind::Pdf => palette.error.0,
        PathKind::Archive => palette.warning.0,
        PathKind::File => palette.text_muted.0,
    }
}

fn chip_text_style(palette: &Palette, kind: PathKind) -> TextStyle {
    let tint = chip_tint(palette, kind);
    TextStyle::new()
        .with_foreground_color(palette.text.0)
        .with_background_color(mix(palette.page.0, tint, 0.1))
        .with_border(TextBorder {
            color: with_alpha(tint, 0x73),
            radius: CHIP_RADIUS,
            width: 1,
            line_height_ratio_override: Some(CHIP_HEIGHT_PERCENT),
        })
}

/// The id a chip's first character is saved under, for painting its icon and anchoring its
/// tooltip.
pub(super) fn chip_position_id(key: &str, chip: &PathChip) -> String {
    format!("native_chat_chip:{key}:{}:{}", chip.line, chip.chars.start)
}

/// Prose with its links and path chips clickable and styled, and the chips' icons painted over
/// the room left for them.
pub(super) fn prose_with_chips(
    cx: &RowContext,
    key: &str,
    mut element: FormattedTextElement,
    text: &FormattedText,
    chips: &[PathChip],
    font_size: f32,
) -> Box<dyn Element> {
    for chip in chips {
        element = element.with_saved_glyph_position(
            chip.chars.start,
            chip.line,
            chip_position_id(key, chip),
        );
    }
    element.register_handlers(|mut handlers, (index, line)| {
        for (range, link) in line.hyperlinks(false) {
            handlers = handlers
                .with_hoverable_char_range(
                    range.clone(),
                    MouseStateHandle::default(),
                    Some(Cursor::PointingHand),
                    |_, _, _| {},
                )
                .with_clickable_char_range(range, move |_, ctx, _| {
                    if let Hyperlink::Url(url) = &link {
                        ctx.dispatch_typed_action(TranscriptAction::OpenUrl(url.clone()));
                    }
                });
        }
        for chip in chips.iter().filter(|chip| chip.line == index) {
            let id = chip_position_id(key, chip);
            let path = chip.path.clone();
            let open = TranscriptAction::OpenPath {
                path: chip.path.clone(),
                kind: chip.kind,
            };
            handlers = handlers
                .with_hoverable_char_range(
                    chip.chars.clone(),
                    cx.view.mouse(&format!("chip:{id}")),
                    Some(Cursor::PointingHand),
                    move |hovered, ctx, _| {
                        ctx.dispatch_typed_action(TranscriptAction::HoverPath {
                            id: id.clone(),
                            path: path.clone(),
                            hovered,
                        });
                    },
                )
                .with_clickable_char_range(chip.chars.clone(), move |_, ctx, _| {
                    ctx.dispatch_typed_action(open.clone());
                });
        }
        handlers
    });
    let palette = cx.palette();
    for (index, line) in text.lines.iter().enumerate() {
        let links = line.hyperlinks(false).into_iter().map(|(range, _)| {
            (
                range,
                TextStyle::new().with_foreground_color(palette.link.0),
            )
        });
        let chip_ranges = chips
            .iter()
            .filter(|chip| chip.line == index)
            .map(|chip| (chip.chars.clone(), chip_text_style(palette, chip.kind)));
        let mut styles: Vec<HighlightedRange> = links
            .chain(chip_ranges)
            .filter(|(range, _)| !range.is_empty())
            .map(|(range, style)| HighlightedRange {
                highlight_indices: range.collect(),
                highlight: Highlight::new().with_text_style(style),
            })
            .collect();
        if styles.is_empty() {
            continue;
        }
        styles.sort_by_key(|style| style.highlight_indices[0]);
        element.add_styles(index, styles);
    }
    if chips.is_empty() {
        return element.finish();
    }
    let icon_size = (font_size * CHIP_ICON_RATIO).round();
    let icons = chips
        .iter()
        .map(|chip| {
            (
                chip_position_id(key, chip),
                icon(
                    chip_icon(chip.kind),
                    chip_tint(palette, chip.kind),
                    icon_size,
                ),
            )
        })
        .collect();
    ChipIcons {
        child: element.finish(),
        icons,
        icon_size,
    }
    .finish()
}

/// The full path of the chip under the pointer.
pub(super) fn path_tooltip(style: &Style, path: &Path) -> Box<dyn Element> {
    tooltip(style, path.display().to_string())
}

fn tooltip(style: &Style, text: String) -> Box<dyn Element> {
    let palette = &style.palette;
    ConstrainedBox::new(
        Container::new(
            Text::new(text, style.ui, style.sizes.tiny_font_size)
                .with_color(palette.text.0)
                .with_line_height_ratio(1.35)
                .with_selectable(false)
                .finish(),
        )
        .with_horizontal_padding(8.)
        .with_vertical_padding(5.)
        .with_background_color(palette.surface.0)
        .with_border(Border::all(1.).with_border_color(palette.border_strong.0))
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(6.)))
        .with_drop_shadow(DropShadow::new_with_standard_offset_and_spread(
            palette.shadow.0,
        ))
        .finish(),
    )
    .with_max_width(TOOLTIP_MAX_WIDTH)
    .finish()
}

/// A text a message mentions, as a one-line pill like the composer's; hovering it shows the
/// whole quote.
pub(super) fn quote_chip(cx: &RowContext, key: &str, quote: &str) -> Box<dyn Element> {
    let preview = quote.split_whitespace().collect::<Vec<_>>().join(" ");
    let full = if quote.chars().count() > QUOTE_TOOLTIP_CHARS {
        let cut: String = quote.chars().take(QUOTE_TOOLTIP_CHARS).collect();
        format!("{}…", cut.trim_end())
    } else {
        quote.to_owned()
    };
    pill(
        cx,
        cx.view.mouse(&format!("quote:{key}")),
        WarpIcon::MessageText.into(),
        cx.palette().accent.0,
        preview,
        full,
        None,
    )
}

/// A long text pasted into a message, as a pill that unfolds it (`toggle`); hovering it shows
/// its start.
pub(super) fn pasted_chip(
    cx: &RowContext,
    key: &str,
    number: usize,
    text: &str,
    lines: usize,
    toggle: TranscriptAction,
) -> Box<dyn Element> {
    let preview = paste_preview(text, lines);
    let lines = if lines == 1 {
        "1 linha".to_owned()
    } else {
        format!("{lines} linhas")
    };
    pill(
        cx,
        cx.view.mouse(&format!("pasted:{key}")),
        WarpIcon::FileCopy.into(),
        cx.palette().text_muted.0,
        format!("Texto colado {number} · {lines}"),
        preview,
        Some(toggle),
    )
}

/// The first lines of a paste, each cut short, so its hover preview stays small enough to fit
/// inside the window above the pill.
pub(super) fn paste_preview(text: &str, lines: usize) -> String {
    let mut preview: Vec<String> = text
        .lines()
        .take(PASTE_PREVIEW_LINES)
        .map(|line| {
            if line.chars().count() > PASTE_PREVIEW_LINE_CHARS {
                let cut: String = line.chars().take(PASTE_PREVIEW_LINE_CHARS).collect();
                format!("{}…", cut.trim_end())
            } else {
                line.to_owned()
            }
        })
        .collect();
    let hidden = lines.saturating_sub(PASTE_PREVIEW_LINES);
    if hidden > 0 {
        preview.push(if hidden == 1 {
            "… mais 1 linha".to_owned()
        } else {
            format!("… mais {hidden} linhas")
        });
    }
    preview.join("\n")
}

/// A file attached to a message, as a pill with its kind's icon that opens it; hovering it shows
/// its path.
pub(super) fn attached_file_chip(
    cx: &RowContext,
    key: &str,
    file: &AttachedFile,
) -> Box<dyn Element> {
    let kind = attachment_path_kind(file.kind);
    pill(
        cx,
        cx.view.mouse(&format!("file:{key}")),
        attachment_icon(file.kind).into(),
        chip_tint(cx.palette(), kind),
        file.name.clone(),
        file.path.clone(),
        Some(TranscriptAction::OpenPath {
            path: PathBuf::from(&file.path),
            kind,
        }),
    )
}

/// How an attachment opens, by its kind.
fn attachment_path_kind(kind: AttachmentKind) -> PathKind {
    match kind {
        AttachmentKind::Image => PathKind::Image,
        AttachmentKind::Pdf => PathKind::Pdf,
        AttachmentKind::Text => PathKind::Code,
        AttachmentKind::Folder => PathKind::Folder,
        AttachmentKind::Document
        | AttachmentKind::Spreadsheet
        | AttachmentKind::Presentation
        | AttachmentKind::Other => PathKind::File,
    }
}

/// The composer's icon for an attachment of this kind.
fn attachment_icon(kind: AttachmentKind) -> WarpIcon {
    match kind {
        AttachmentKind::Image => WarpIcon::Image,
        AttachmentKind::Pdf | AttachmentKind::Document => WarpIcon::File,
        AttachmentKind::Text => WarpIcon::Code2,
        AttachmentKind::Spreadsheet => WarpIcon::Grid,
        AttachmentKind::Presentation => WarpIcon::LayoutAlt01,
        AttachmentKind::Folder => WarpIcon::Folder,
        AttachmentKind::Other => WarpIcon::Paperclip,
    }
}

/// A one-line pill tinted with `tint`, showing `tooltip_text` on hover and dispatching `action`,
/// if any, on click.
fn pill(
    cx: &RowContext,
    mouse: MouseStateHandle,
    icon_path: &'static str,
    tint: ColorU,
    label: String,
    tooltip_text: String,
    action: Option<TranscriptAction>,
) -> Box<dyn Element> {
    let style = cx.style.clone();
    let hoverable = Hoverable::new(mouse, move |state| {
        let palette = &style.palette;
        let sizes = &style.sizes;
        let content = Flex::row()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(6.)
            .with_child(icon(
                icon_path,
                with_alpha(tint, 0xd0),
                sizes.small_icon_size,
            ))
            .with_child(
                Shrinkable::new(
                    1.,
                    Text::new_inline(label.clone(), style.ui, sizes.small_font_size)
                        .with_color(palette.text.0)
                        .with_clip(ClipConfig::ellipsis())
                        .with_selectable(false)
                        .finish(),
                )
                .finish(),
            )
            .finish();
        let border = if state.is_hovered() {
            with_alpha(tint, 0xb0)
        } else {
            with_alpha(tint, 0x73)
        };
        let chip = Container::new(
            ConstrainedBox::new(content)
                .with_height(PILL_HEIGHT)
                .with_max_width(PILL_MAX_WIDTH)
                .finish(),
        )
        .with_horizontal_padding(10.)
        .with_background_color(mix(palette.page.0, tint, 0.1))
        .with_border(Border::all(1.).with_border_color(border))
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(PILL_HEIGHT / 2.)))
        .finish();
        if !state.is_hovered() {
            return chip;
        }
        let mut stack = Stack::new().with_child(chip);
        // Below the pill: above it, a message near the top of the transcript clips the tooltip.
        stack.add_positioned_overlay_child(
            tooltip(&style, tooltip_text.clone()),
            OffsetPositioning::offset_from_parent(
                vec2f(0., 6.),
                ParentOffsetBounds::WindowByPosition,
                ParentAnchor::BottomRight,
                ChildAnchor::TopRight,
            ),
        );
        stack.finish()
    })
    .with_propagate_drag();
    match action {
        Some(action) => hoverable
            .with_cursor(Cursor::PointingHand)
            .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
            .finish(),
        None => hoverable.finish(),
    }
}

/// What the user answered to one question, as a small pill: its header, muted, then the answer;
/// hovering it shows the whole question.
pub(super) fn answer_chip(cx: &RowContext, key: &str, answer: &QuestionAnswer) -> Box<dyn Element> {
    let style = cx.style.clone();
    let header = answer.header.clone();
    let text = answer.answer.clone();
    let tooltip_text = format!("{}\n{}", answer.question, answer.answer);
    Hoverable::new(cx.view.mouse(&format!("answer:{key}")), move |state| {
        let palette = &style.palette;
        let sizes = &style.sizes;
        let tint = palette.accent.0;
        let mut content = Flex::row()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(5.);
        if let Some(header) = &header {
            content.add_child(
                Text::new_inline(header.clone(), style.ui, sizes.tiny_font_size + 0.5)
                    .with_color(palette.text_muted.0)
                    .with_selectable(false)
                    .finish(),
            );
        }
        content.add_child(
            Shrinkable::new(
                1.,
                Text::new_inline(text.clone(), style.ui, sizes.small_font_size)
                    .with_color(palette.text.0)
                    .with_clip(ClipConfig::ellipsis())
                    .with_selectable(false)
                    .finish(),
            )
            .finish(),
        );
        let border = if state.is_hovered() {
            with_alpha(tint, 0x99)
        } else {
            with_alpha(tint, 0x4d)
        };
        let chip = Container::new(
            ConstrainedBox::new(content.finish())
                .with_height(ANSWER_PILL_HEIGHT)
                .with_max_width(ANSWER_PILL_MAX_WIDTH)
                .finish(),
        )
        .with_horizontal_padding(9.)
        .with_background_color(mix(palette.page.0, tint, 0.08))
        .with_border(Border::all(1.).with_border_color(border))
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(
            ANSWER_PILL_HEIGHT / 2.,
        )))
        .finish();
        if !state.is_hovered() {
            return chip;
        }
        let mut stack = Stack::new().with_child(chip);
        stack.add_positioned_overlay_child(
            tooltip(&style, tooltip_text.clone()),
            OffsetPositioning::offset_from_parent(
                vec2f(0., 6.),
                ParentOffsetBounds::WindowByPosition,
                ParentAnchor::BottomLeft,
                ChildAnchor::TopLeft,
            ),
        );
        stack.finish()
    })
    .with_propagate_drag()
    .finish()
}

/// `from` moved `amount` (0 to 1) of the way to `to`, opaque.
fn mix(from: ColorU, to: ColorU, amount: f32) -> ColorU {
    let channel = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * amount).round() as u8;
    ColorU::new(
        channel(from.r, to.r),
        channel(from.g, to.g),
        channel(from.b, to.b),
        0xff,
    )
}

fn with_alpha(color: ColorU, alpha: u8) -> ColorU {
    ColorU { a: alpha, ..color }
}

/// Paints the path chips' icons over the room the text leaves for them, where the text says their
/// first characters landed.
struct ChipIcons {
    child: Box<dyn Element>,
    /// Each chip's saved position id and its icon.
    icons: Vec<(String, Box<dyn Element>)>,
    icon_size: f32,
}

impl Element for ChipIcons {
    fn layout(
        &mut self,
        constraint: SizeConstraint,
        ctx: &mut LayoutContext,
        app: &AppContext,
    ) -> Vector2F {
        let size = self.child.layout(constraint, ctx, app);
        let icon_constraint = SizeConstraint::strict(vec2f(self.icon_size, self.icon_size));
        for (_, icon) in &mut self.icons {
            icon.layout(icon_constraint, ctx, app);
        }
        size
    }

    fn after_layout(&mut self, ctx: &mut AfterLayoutContext, app: &AppContext) {
        self.child.after_layout(ctx, app);
        for (_, icon) in &mut self.icons {
            icon.after_layout(ctx, app);
        }
    }

    fn paint(&mut self, origin: Vector2F, ctx: &mut PaintContext, app: &AppContext) {
        // Its own namespace commits the glyph positions the text saves right away, so the icons
        // land on this frame's layout rather than the last one's.
        ctx.position_cache.start();
        self.child.paint(origin, ctx, app);
        ctx.position_cache.end();
        for (id, icon) in &mut self.icons {
            let Some(glyph) = ctx.position_cache.get_position(id.as_str()) else {
                continue;
            };
            let at = vec2f(
                glyph.min_x() + CHIP_ICON_INSET,
                glyph.min_y() + (glyph.height() - self.icon_size) / 2.,
            );
            icon.paint(at, ctx, app);
        }
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

    fn as_selectable_element(&self) -> Option<&dyn SelectableElement> {
        self.child.as_selectable_element()
    }
}
