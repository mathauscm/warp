//! Drawing the transcript's rows: bubbles, prose, code, tables, thinking, notices and work groups.

use std::path::Path;
use std::time::{Duration, SystemTime};

use claude_chat::presentation::{Row, WorkGroup};
use claude_chat::timeline::{
    AssistantText, Notice, NoticeKind, SlashCommandInvocation, Thinking, TimelineItem, UserMessage,
};
use markdown_parser::{FormattedTable, FormattedText, FormattedTextLine};
use pathfinder_color::ColorU;
use pathfinder_geometry::vector::Vector2F;
use warpui::elements::shimmering_text::{ShimmerConfig, ShimmeringTextElement};
use warpui::elements::{
    Border, ClippedScrollable, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, Empty,
    Expanded, Fill, Flex, FormattedTextElement, HeadingFontSizeMultipliers, Highlight,
    HighlightedRange, Hoverable, Icon as SvgIcon, MainAxisAlignment, MainAxisSize, ParentElement,
    Point, Radius, ScrollbarWidth, Shrinkable, Text,
};
use warpui::event::DispatchedEvent;
use warpui::fonts::{FamilyId, Properties, Weight};
use warpui::platform::Cursor;
use warpui::text_layout::{ClipConfig, TextBorder};
use warpui::{
    AfterLayoutContext, AppContext, Element, EventContext, LayoutContext, PaintContext,
    SingletonEntity, SizeConstraint,
};

use super::details::{self, format_time};
use super::markdown::{self, Block};
use super::{TranscriptAction, TranscriptView, tools};
use crate::native_chat::design::{self, ChatDesign, Palette, Sizes};

pub(super) const ICON_CHEVRON_RIGHT: &str = "bundled/svg/picolas-chevron-right.svg";
pub(super) const ICON_CHEVRON_DOWN: &str = "bundled/svg/picolas-chevron-down.svg";
pub(super) const ICON_COPY: &str = "bundled/svg/picolas-copy.svg";
pub(super) const ICON_CHECK: &str = "bundled/svg/picolas-check.svg";
const ICON_THINKING: &str = "bundled/svg/picolas-lightbulb.svg";
const ICON_ALERT: &str = "bundled/svg/picolas-alert.svg";
const ICON_STOP: &str = "bundled/svg/picolas-stop.svg";
const ICON_IMAGE: &str = "bundled/svg/picolas-image.svg";

/// Thumbnails of images attached to a message.
const THUMBNAIL_WIDTH: f32 = 120.;
const THUMBNAIL_HEIGHT: f32 = 90.;
/// The first line of a thinking block shown while it's folded.
const THINKING_PREVIEW_CHARS: usize = 90;
/// Space between a row's blocks (paragraph runs, code, tables).
const BLOCK_SPACING: f32 = 12.;
/// Space between rows inside a work group.
const NESTED_ROW_SPACING: f32 = 4.;
/// Height of the hover line under a message (time, copy).
const MESSAGE_FOOTER_HEIGHT: f32 = 24.;
const SMALL_BUTTON_PADDING: f32 = 4.;
const INLINE_CODE_RADIUS: u8 = 4;
/// Inline code's background height, as a percentage of the font size.
const INLINE_CODE_HEIGHT_PERCENT: u8 = 118;

/// The design tokens and fonts, read once per row.
#[derive(Clone)]
pub(super) struct Style {
    pub palette: Palette,
    pub sizes: Sizes,
    pub ui: FamilyId,
    pub mono: FamilyId,
}

impl Style {
    pub fn current(app: &AppContext) -> Self {
        let design = ChatDesign::as_ref(app);
        let fonts = design::fonts(app);
        Self {
            palette: design.palette(app).clone(),
            sizes: design.sizes().clone(),
            ui: fonts.ui,
            mono: fonts.mono,
        }
    }
}

/// What a row needs to draw itself.
pub(super) struct RowContext<'a> {
    pub view: &'a TranscriptView,
    pub items: &'a [TimelineItem],
    pub cwd: &'a Path,
    pub style: &'a Style,
    /// The top-level row being drawn, for toggles to re-measure it.
    pub row: usize,
    pub now: SystemTime,
}

impl RowContext<'_> {
    pub fn palette(&self) -> &Palette {
        &self.style.palette
    }

    pub fn sizes(&self) -> &Sizes {
        &self.style.sizes
    }

    pub fn toggle_action(&self, key: &str, expanded: bool) -> TranscriptAction {
        TranscriptAction::Toggle {
            key: key.to_owned(),
            row: self.row,
            expanded,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Rows

/// A row of the transcript; `nested` rows sit inside a work group or a subagent card.
pub(super) fn render_row(cx: &RowContext, row: &Row, nested: bool) -> Box<dyn Element> {
    match row {
        Row::UserBubble { index } => match &cx.items[*index] {
            TimelineItem::User(message) => render_user_message(cx, message),
            _ => Empty::new().finish(),
        },
        Row::AssistantProse { index } => match &cx.items[*index] {
            TimelineItem::Assistant(text) => render_assistant(cx, text, nested),
            _ => Empty::new().finish(),
        },
        Row::Thinking { index } => match &cx.items[*index] {
            TimelineItem::Thinking(thinking) => render_thinking(cx, thinking),
            _ => Empty::new().finish(),
        },
        Row::Tool { index } => match &cx.items[*index] {
            TimelineItem::Tool(tool) => tools::render_tool(cx, tool),
            _ => Empty::new().finish(),
        },
        Row::ToolBatch(batch) => tools::render_batch(cx, batch),
        Row::Notice { index } => match &cx.items[*index] {
            TimelineItem::Notice(notice) => render_notice(cx, notice),
            _ => Empty::new().finish(),
        },
        Row::WorkGroup(group) => render_work_group(cx, group),
        Row::TurnError { index } => match &cx.items[*index] {
            TimelineItem::TurnEnd(summary) => {
                error_card(cx, &details::turn_error_label(&summary.subtype))
            }
            _ => Empty::new().finish(),
        },
    }
}

/// The rows inside a work group or subagent card, one under the other.
pub(super) fn render_nested_rows(cx: &RowContext, rows: &[Row]) -> Box<dyn Element> {
    let mut column = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);
    for row in rows {
        let spacing = match row {
            Row::AssistantProse { .. } => BLOCK_SPACING,
            Row::UserBubble { .. }
            | Row::Thinking { .. }
            | Row::Tool { .. }
            | Row::ToolBatch(_)
            | Row::Notice { .. }
            | Row::WorkGroup(_)
            | Row::TurnError { .. } => NESTED_ROW_SPACING,
        };
        column.add_child(
            Container::new(render_row(cx, row, true))
                .with_padding_bottom(spacing)
                .finish(),
        );
    }
    column.finish()
}

fn render_user_message(cx: &RowContext, message: &UserMessage) -> Box<dyn Element> {
    let palette = cx.palette();
    let sizes = cx.sizes();
    let key = format!("user:{}", message.id);
    let copy_text = match &message.command {
        Some(command) => format!("/{} {}", command.name, command.args)
            .trim_end()
            .to_owned(),
        None => message.text.clone(),
    };
    let footer_time = message.timestamp.map(|time| format_time(time, cx.now));

    let mut column = Flex::column()
        .with_cross_axis_alignment(CrossAxisAlignment::End)
        .with_main_axis_size(MainAxisSize::Min);
    if !message.images.is_empty() {
        column.add_child(
            Container::new(render_thumbnails(cx, message))
                .with_padding_bottom(6.)
                .finish(),
        );
    }
    let bubble_content = match &message.command {
        Some(command) => render_command_text(cx, command),
        None => Text::new(message.text.trim().to_owned(), cx.style.ui, sizes.font_size)
            .with_color(palette.text.0)
            .with_line_height_ratio(sizes.line_height_ratio)
            .with_selection_color(palette.selection.0)
            .finish(),
    };
    let show_bubble = message.command.is_some() || !message.text.trim().is_empty();
    if show_bubble {
        let bubble = Container::new(bubble_content)
            .with_horizontal_padding(sizes.bubble_padding_x)
            .with_vertical_padding(sizes.bubble_padding_y)
            .with_background_color(palette.user_bubble.0)
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(sizes.bubble_radius)))
            .finish();
        column.add_child(FractionWidth::new(bubble, sizes.bubble_max_width_ratio).finish());
    }
    if let Some(output) = message
        .command
        .as_ref()
        .and_then(|command| command.output.as_deref())
        .filter(|output| !output.trim().is_empty())
    {
        column.add_child(render_command_output(cx, &key, output));
    }

    let hover = cx.view.mouse(&format!("hover:{key}"));
    let copy_mouse = cx.view.mouse(&format!("copy:{key}"));
    let copied = cx.view.is_copied(&key);
    let style = cx.style.clone();
    let row = Hoverable::new(hover, move |state| {
        let footer = message_footer(
            &style,
            state.is_hovered() || copied,
            footer_time,
            copy_mouse,
            copied,
            key,
            copy_text,
            MainAxisAlignment::End,
        );
        column.add_child(footer);
        column.finish()
    })
    .with_propagate_drag()
    .finish();
    Flex::row()
        .with_main_axis_alignment(MainAxisAlignment::End)
        .with_main_axis_size(MainAxisSize::Max)
        .with_child(Shrinkable::new(1., row).finish())
        .finish()
}

fn render_command_text(cx: &RowContext, command: &SlashCommandInvocation) -> Box<dyn Element> {
    let palette = cx.palette();
    let sizes = cx.sizes();
    let mut row = Flex::row()
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_main_axis_size(MainAxisSize::Min)
        .with_spacing(6.)
        .with_child(
            Text::new(
                format!("/{}", command.name),
                cx.style.mono,
                sizes.code_font_size,
            )
            .with_color(palette.accent.0)
            .with_style(Properties::default().weight(Weight::Medium))
            .finish(),
        );
    if !command.args.trim().is_empty() {
        row.add_child(
            Shrinkable::new(
                1.,
                Text::new(command.args.trim().to_owned(), cx.style.ui, sizes.font_size)
                    .with_color(palette.text.0)
                    .with_selection_color(palette.selection.0)
                    .finish(),
            )
            .finish(),
        );
    }
    row.finish()
}

/// What a local slash command like `/context` printed, folded under its chip.
fn render_command_output(cx: &RowContext, key: &str, output: &str) -> Box<dyn Element> {
    let toggle_key = format!("output:{key}");
    let expanded = cx.view.is_expanded(&toggle_key, false);
    let header = disclosure_line(
        cx,
        &toggle_key,
        None,
        if expanded {
            "Ocultar saída"
        } else {
            "Mostrar saída"
        },
        expanded,
        false,
    );
    let mut column = Flex::column()
        .with_cross_axis_alignment(CrossAxisAlignment::End)
        .with_child(Container::new(header).with_padding_top(4.).finish());
    if expanded {
        let text = details::strip_ansi(output);
        column.add_child(
            Container::new(mono_box(cx, &toggle_key, &text, cx.palette().text_muted.0))
                .with_padding_top(4.)
                .finish(),
        );
    }
    column.finish()
}

fn render_thumbnails(cx: &RowContext, message: &UserMessage) -> Box<dyn Element> {
    let palette = cx.palette();
    let mut row = Flex::row().with_spacing(6.);
    for (position, image) in message.images.iter().enumerate() {
        let source = match &image.path {
            Some(path) => Some(warpui::assets::asset_cache::AssetSource::LocalFile {
                path: path.clone(),
                content_version: None,
            }),
            None => cx
                .view
                .image_asset_id(&message.id, position)
                .map(|id| warpui::assets::asset_cache::AssetSource::Raw { id }),
        };
        let tile = match source {
            Some(source) => {
                warpui::elements::Image::new(source, warpui::image_cache::CacheOption::Original)
                    .cover()
                    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(8.)))
                    .finish()
            }
            None => Container::new(icon(ICON_IMAGE, palette.text_muted.0, 20.))
                .with_uniform_padding(35.)
                .finish(),
        };
        row.add_child(
            ConstrainedBox::new(
                Container::new(tile)
                    .with_background_color(palette.surface.0)
                    .with_border(Border::all(1.).with_border_color(palette.border.0))
                    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(8.)))
                    .finish(),
            )
            .with_width(THUMBNAIL_WIDTH)
            .with_height(THUMBNAIL_HEIGHT)
            .finish(),
        );
    }
    row.finish()
}

/// The line under a message: its time and a copy button, shown on hover. It always takes its
/// height so hovering doesn't move the rows below.
#[allow(clippy::too_many_arguments)]
fn message_footer(
    style: &Style,
    visible: bool,
    time: Option<String>,
    copy_mouse: warpui::elements::MouseStateHandle,
    copied: bool,
    key: String,
    copy_text: String,
    alignment: MainAxisAlignment,
) -> Box<dyn Element> {
    let mut row = Flex::row()
        .with_main_axis_alignment(alignment)
        .with_main_axis_size(MainAxisSize::Max)
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(2.);
    if visible {
        let palette = &style.palette;
        if let Some(time) = time {
            row.add_child(
                Container::new(
                    Text::new_inline(time, style.ui, style.sizes.tiny_font_size)
                        .with_color(palette.text_faint.0)
                        .with_selectable(false)
                        .finish(),
                )
                .with_padding_right(4.)
                .finish(),
            );
        }
        row.add_child(copy_button(
            style, copy_mouse, copied, key, copy_text, false,
        ));
    }
    ConstrainedBox::new(row.finish())
        .with_height(MESSAGE_FOOTER_HEIGHT)
        .finish()
}

/// A small copy icon that turns into a check for a moment after copying.
pub(super) fn copy_button(
    style: &Style,
    mouse: warpui::elements::MouseStateHandle,
    copied: bool,
    key: String,
    text: String,
    with_label: bool,
) -> Box<dyn Element> {
    let palette = style.palette.clone();
    let ui = style.ui;
    let font_size = style.sizes.tiny_font_size;
    let icon_size = style.sizes.small_icon_size;
    let radius = style.sizes.chip_radius;
    Hoverable::new(mouse, move |state| {
        let color = if state.is_hovered() || copied {
            palette.text.0
        } else {
            palette.text_muted.0
        };
        let mut content = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_main_axis_size(MainAxisSize::Min)
            .with_spacing(5.)
            .with_child(icon(
                if copied { ICON_CHECK } else { ICON_COPY },
                color,
                icon_size,
            ));
        if with_label {
            content.add_child(
                Text::new_inline(if copied { "Copiado" } else { "Copiar" }, ui, font_size)
                    .with_color(color)
                    .with_selectable(false)
                    .finish(),
            );
        }
        let mut button = Container::new(content.finish())
            .with_uniform_padding(SMALL_BUTTON_PADDING)
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(radius)));
        if state.is_hovered() {
            button = button.with_background_color(palette.surface_hover.0);
        }
        button.finish()
    })
    .with_cursor(Cursor::PointingHand)
    .on_click(move |ctx, _, _| {
        ctx.dispatch_typed_action(TranscriptAction::Copy {
            key: key.clone(),
            text: text.clone(),
        });
    })
    .finish()
}

fn render_assistant(cx: &RowContext, text: &AssistantText, nested: bool) -> Box<dyn Element> {
    let body = render_markdown(cx, &text.id, &text.markdown, MarkdownTone::Normal);
    if nested || text.is_streaming {
        return body;
    }
    let key = format!("assistant:{}", text.id);
    let hover = cx.view.mouse(&format!("hover:{key}"));
    let copy_mouse = cx.view.mouse(&format!("copy:{key}"));
    let copied = cx.view.is_copied(&key);
    let time = text.timestamp.map(|time| format_time(time, cx.now));
    let markdown = text.markdown.clone();
    let style = cx.style.clone();
    Hoverable::new(hover, move |state| {
        Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(body)
            .with_child(
                Container::new(message_footer(
                    &style,
                    state.is_hovered() || copied,
                    time,
                    copy_mouse,
                    copied,
                    key,
                    markdown,
                    MainAxisAlignment::Start,
                ))
                .with_padding_top(2.)
                .finish(),
            )
            .finish()
    })
    .with_propagate_drag()
    .finish()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MarkdownTone {
    Normal,
    /// Thinking, summaries and other secondary text: smaller and muted.
    Muted,
}

/// Claude's Markdown: prose runs as selectable rich text, code in rounded boxes, tables as grids.
pub(super) fn render_markdown(
    cx: &RowContext,
    cache_key: &str,
    markdown: &str,
    tone: MarkdownTone,
) -> Box<dyn Element> {
    let blocks = cx.view.blocks(cache_key, markdown);
    render_blocks(cx, cache_key, &blocks, tone)
}

fn render_blocks(
    cx: &RowContext,
    cache_key: &str,
    blocks: &[Block],
    tone: MarkdownTone,
) -> Box<dyn Element> {
    let mut column = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);
    for (position, block) in blocks.iter().enumerate() {
        let element = match block {
            Block::Prose(text) => render_prose(cx, text.clone(), tone),
            Block::Code { language, code } => {
                render_code_block(cx, &format!("{cache_key}#{position}"), language, code)
            }
            Block::Table(table) => render_table(cx, table, tone),
            Block::Quote(blocks) => render_quote(cx, &format!("{cache_key}#{position}"), blocks),
        };
        let spacing = if position + 1 == blocks.len() {
            0.
        } else {
            BLOCK_SPACING
        };
        column.add_child(
            Container::new(element)
                .with_padding_bottom(spacing)
                .finish(),
        );
    }
    column.finish()
}

/// Inline code's background: a small rounded box around the glyphs instead of a full-height band.
fn inline_code_border(cx: &RowContext) -> TextBorder {
    TextBorder {
        color: cx.palette().inline_code_background.0,
        radius: INLINE_CODE_RADIUS,
        width: 0,
        line_height_ratio_override: Some(INLINE_CODE_HEIGHT_PERCENT),
    }
}

fn render_quote(cx: &RowContext, key: &str, blocks: &[Block]) -> Box<dyn Element> {
    Container::new(render_blocks(cx, key, blocks, MarkdownTone::Muted))
        .with_padding_left(12.)
        .with_border(Border::left(2.).with_border_color(cx.palette().border_strong.0))
        .finish()
}

fn render_prose(
    cx: &RowContext,
    text: std::sync::Arc<FormattedText>,
    tone: MarkdownTone,
) -> Box<dyn Element> {
    let palette = cx.palette();
    let sizes = cx.sizes();
    let (font_size, color) = match tone {
        MarkdownTone::Normal => (sizes.font_size, palette.text.0),
        MarkdownTone::Muted => (sizes.small_font_size + 0.5, palette.text_muted.0),
    };
    FormattedTextElement::new_arc(
        text,
        font_size,
        cx.style.ui,
        cx.view.inline_code_font.unwrap_or(cx.style.mono),
        color,
        Default::default(),
    )
    .with_line_height_ratio(sizes.line_height_ratio)
    .with_heading_to_font_size_multipliers(HeadingFontSizeMultipliers {
        h1: 1.45,
        h2: 1.3,
        h3: 1.15,
        h4: 1.05,
        h5: 1.,
        h6: 1.,
    })
    .with_inline_code_properties(Some(color), Some(palette.inline_code_background.0))
    .with_inline_code_border(inline_code_border(cx))
    .with_hyperlink_font_color(palette.link.0)
    .with_selection_color(palette.selection.0)
    .set_selectable(true)
    .register_default_click_handlers(|url, ctx, _| {
        ctx.dispatch_typed_action(TranscriptAction::OpenUrl(url.url));
    })
    .finish()
}

fn render_code_block(cx: &RowContext, key: &str, language: &str, code: &str) -> Box<dyn Element> {
    let palette = cx.palette();
    let sizes = cx.sizes();
    let copy = copy_button(
        cx.style,
        cx.view.mouse(&format!("copy:{key}")),
        cx.view.is_copied(key),
        key.to_owned(),
        code.to_owned(),
        true,
    );
    let header = Flex::row()
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_main_axis_size(MainAxisSize::Max)
        .with_child(
            Expanded::new(
                1.,
                Text::new_inline(
                    markdown::language_label(language),
                    cx.style.ui,
                    sizes.tiny_font_size,
                )
                .with_color(palette.text_faint.0)
                .with_selectable(false)
                .finish(),
            )
            .finish(),
        )
        .with_child(copy)
        .finish();
    let body = code_text(cx, key, language, code);
    Container::new(
        Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(
                Container::new(header)
                    .with_padding_left(14.)
                    .with_padding_right(6.)
                    .with_padding_top(5.)
                    .finish(),
            )
            .with_child(
                Container::new(body)
                    .with_horizontal_padding(14.)
                    .with_padding_top(2.)
                    .with_padding_bottom(12.)
                    .finish(),
            )
            .finish(),
    )
    .with_background_color(palette.code_background.0)
    .with_border(Border::all(1.).with_border_color(palette.border.0))
    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(
        sizes.code_block_radius,
    )))
    .finish()
}

/// The code of a fenced block, monospaced, highlighted and selectable.
fn code_text(cx: &RowContext, key: &str, language: &str, code: &str) -> Box<dyn Element> {
    let palette = cx.palette();
    let highlights = cx.view.highlights(key, language, code, palette);
    Text::new(code.to_owned(), cx.style.mono, cx.sizes().code_font_size)
        .with_color(palette.text.0)
        .with_line_height_ratio(1.5)
        .with_selection_color(palette.selection.0)
        .with_highlights(highlights.iter().map(|(range, color)| HighlightedRange {
            highlight_indices: range.clone().collect(),
            highlight: Highlight::new().with_foreground_color(*color),
        }))
        .finish()
}

fn render_table(cx: &RowContext, table: &FormattedTable, tone: MarkdownTone) -> Box<dyn Element> {
    let palette = cx.palette();
    let sizes = cx.sizes();
    let weights = markdown::column_weights(table);
    let font_size = match tone {
        MarkdownTone::Normal => sizes.font_size - 1.,
        MarkdownTone::Muted => sizes.small_font_size,
    };
    let cell = |inline: &markdown_parser::FormattedTextInline, header: bool| {
        let text = FormattedText::new([FormattedTextLine::Line(inline.clone())]);
        let mut element = FormattedTextElement::new(
            text,
            font_size,
            cx.style.ui,
            cx.view.inline_code_font.unwrap_or(cx.style.mono),
            palette.text.0,
            Default::default(),
        )
        .with_line_height_ratio(1.4)
        .with_inline_code_properties(None, Some(palette.inline_code_background.0))
        .with_inline_code_border(inline_code_border(cx))
        .with_hyperlink_font_color(palette.link.0)
        .with_selection_color(palette.selection.0)
        .set_selectable(true);
        if header {
            element = element.with_weight(Weight::Semibold);
        }
        Container::new(element.finish())
            .with_horizontal_padding(12.)
            .with_vertical_padding(7.)
            .finish()
    };
    let row = |cells: &[markdown_parser::FormattedTextInline], header: bool, last: bool| {
        let mut row = Flex::row().with_cross_axis_alignment(CrossAxisAlignment::Start);
        for (column, weight) in weights.iter().enumerate() {
            let content = cells
                .get(column)
                .map_or_else(|| Empty::new().finish(), |inline| cell(inline, header));
            row.add_child(Expanded::new(*weight, content).finish());
        }
        let mut container = Container::new(row.finish());
        if header {
            container = container.with_background_color(palette.surface.0);
        }
        if !last {
            container =
                container.with_border(Border::bottom(1.).with_border_color(palette.border.0));
        }
        container.finish()
    };
    let mut column = Flex::column()
        .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
        .with_child(row(&table.headers, true, table.rows.is_empty()));
    for (position, cells) in table.rows.iter().enumerate() {
        column.add_child(row(cells, false, position + 1 == table.rows.len()));
    }
    Container::new(column.finish())
        .with_border(Border::all(1.).with_border_color(palette.border.0))
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(sizes.chip_radius)))
        .finish()
}

fn render_thinking(cx: &RowContext, thinking: &Thinking) -> Box<dyn Element> {
    let key = format!("thinking:{}", thinking.id);
    let expanded = cx.view.is_expanded(&key, false);
    let has_text = !thinking.text.trim().is_empty();
    let label = if thinking.is_streaming {
        "Pensando…".to_owned()
    } else {
        match thinking.duration_ms {
            Some(ms) if ms >= 1000 => format!(
                "Pensou por {}",
                claude_chat::presentation::format_duration_pt(ms)
            ),
            Some(_) | None => "Pensou".to_owned(),
        }
    };
    let preview = (!expanded && has_text && !thinking.is_streaming)
        .then(|| details::first_sentence(&thinking.text, THINKING_PREVIEW_CHARS));
    let header = disclosure_line(
        cx,
        &key,
        Some(ICON_THINKING),
        &label,
        expanded,
        thinking.is_streaming,
    );
    let header = match preview.filter(|preview| !preview.is_empty()) {
        Some(preview) => Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(header)
            .with_child(
                Shrinkable::new(
                    1.,
                    Container::new(
                        Text::new_inline(preview, cx.style.ui, cx.sizes().small_font_size)
                            .with_color(cx.palette().text_faint.0)
                            .with_clip(ClipConfig::ellipsis())
                            .with_selectable(false)
                            .finish(),
                    )
                    .with_padding_left(8.)
                    .finish(),
                )
                .finish(),
            )
            .finish(),
        None => header,
    };
    if !expanded || !has_text {
        return header;
    }
    Flex::column()
        .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
        .with_child(header)
        .with_child(indented_detail(
            cx,
            render_markdown(cx, &thinking.id, &thinking.text, MarkdownTone::Muted),
        ))
        .finish()
}

fn render_notice(cx: &RowContext, notice: &Notice) -> Box<dyn Element> {
    let palette = cx.palette();
    let sizes = cx.sizes();
    match &notice.kind {
        NoticeKind::Compacted {
            pre_tokens,
            post_tokens,
        } => divider(cx, &details::compacted_label(*pre_tokens, *post_tokens)),
        NoticeKind::CompactSummary(summary) => {
            let key = format!("summary:{}", notice.id);
            let expanded = cx.view.is_expanded(&key, false);
            let header = disclosure_line(
                cx,
                &key,
                Some(ICON_THINKING),
                "Resumo da conversa anterior",
                expanded,
                false,
            );
            if !expanded {
                return header;
            }
            Flex::column()
                .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
                .with_child(header)
                .with_child(indented_detail(
                    cx,
                    render_markdown(cx, &notice.id, summary, MarkdownTone::Muted),
                ))
                .finish()
        }
        NoticeKind::Interrupted => Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(7.)
            .with_child(icon(ICON_STOP, palette.text_muted.0, sizes.small_icon_size))
            .with_child(
                Text::new_inline("Interrompido por você", cx.style.ui, sizes.small_font_size)
                    .with_color(palette.text_muted.0)
                    .with_selectable(false)
                    .finish(),
            )
            .finish(),
        NoticeKind::Error(message) => error_card(cx, message),
        NoticeKind::Info(text) => Text::new(
            details::strip_ansi(text).trim().to_owned(),
            cx.style.ui,
            sizes.small_font_size,
        )
        .with_color(palette.text_muted.0)
        .with_selection_color(palette.selection.0)
        .finish(),
    }
}

fn render_work_group(cx: &RowContext, group: &WorkGroup) -> Box<dyn Element> {
    let expanded = cx.view.is_expanded(&group.id, !group.collapsed_by_default);
    let can_expand = !group.rows.is_empty();
    let header = disclosure_line(
        cx,
        &group.id,
        None,
        &group.summary,
        expanded && can_expand,
        group.is_live,
    );
    if !expanded || !can_expand {
        return header;
    }
    Flex::column()
        .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
        .with_child(Container::new(header).with_padding_bottom(8.).finish())
        .with_child(render_nested_rows(cx, &group.rows))
        .finish()
}

// ---------------------------------------------------------------------------------------------
// Pieces

/// A muted clickable line with an optional icon and a chevron, which folds what's under it.
/// `live` labels shimmer.
pub(super) fn disclosure_line(
    cx: &RowContext,
    key: &str,
    leading_icon: Option<&'static str>,
    label: &str,
    expanded: bool,
    live: bool,
) -> Box<dyn Element> {
    let style = cx.style.clone();
    let label = label.to_owned();
    let shimmer = live.then(|| cx.view.shimmer(key));
    let action = cx.toggle_action(key, expanded);
    Hoverable::new(cx.view.mouse(&format!("toggle:{key}")), move |state| {
        let palette = &style.palette;
        let sizes = &style.sizes;
        let color = if state.is_hovered() {
            palette.text.0
        } else {
            palette.text_muted.0
        };
        let mut row = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_main_axis_size(MainAxisSize::Min)
            .with_spacing(6.);
        if let Some(path) = leading_icon {
            row.add_child(icon(path, color, sizes.small_icon_size));
        }
        row.add_child(match shimmer {
            Some(handle) => ShimmeringTextElement::new(
                label,
                style.ui,
                sizes.small_font_size + 0.5,
                palette.text_muted.0,
                palette.shimmer.0,
                ShimmerConfig::default(),
                handle,
            )
            .finish(),
            None => Text::new_inline(label, style.ui, sizes.small_font_size + 0.5)
                .with_color(color)
                .with_selectable(false)
                .finish(),
        });
        if state.is_hovered() || expanded {
            row.add_child(icon(
                if expanded {
                    ICON_CHEVRON_DOWN
                } else {
                    ICON_CHEVRON_RIGHT
                },
                color,
                sizes.small_icon_size - 1.,
            ));
        }
        ConstrainedBox::new(row.finish())
            .with_height(sizes.tool_row_height)
            .finish()
    })
    .with_cursor(Cursor::PointingHand)
    .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
    .finish()
}

/// Detail under a disclosure line, indented under its label with a thin rail on the left.
pub(super) fn indented_detail(cx: &RowContext, child: Box<dyn Element>) -> Box<dyn Element> {
    Container::new(
        Container::new(child)
            .with_padding_left(12.)
            .with_border(Border::left(1.5).with_border_color(cx.palette().border_strong.0))
            .finish(),
    )
    .with_margin_left(6.)
    .with_padding_top(4.)
    .with_padding_bottom(6.)
    .finish()
}

/// Monospaced text in a rounded box that scrolls past `tool_output_max_height`.
pub(super) fn mono_box(cx: &RowContext, key: &str, text: &str, color: ColorU) -> Box<dyn Element> {
    let sizes = cx.sizes();
    let child = Text::new(text.to_owned(), cx.style.mono, sizes.code_font_size - 0.5)
        .with_color(color)
        .with_line_height_ratio(1.45)
        .with_selection_color(cx.palette().selection.0)
        .finish();
    scroll_box(cx, key, child, true)
}

/// `child` in a code-colored rounded box that scrolls past `tool_output_max_height`.
pub(super) fn scroll_box(
    cx: &RowContext,
    key: &str,
    child: Box<dyn Element>,
    padded: bool,
) -> Box<dyn Element> {
    let palette = cx.palette();
    let sizes = cx.sizes();
    let child = if padded {
        Container::new(child)
            .with_horizontal_padding(12.)
            .with_vertical_padding(10.)
            .finish()
    } else {
        child
    };
    let scrollable = ClippedScrollable::vertical(
        cx.view.scroll_handle(key),
        child,
        ScrollbarWidth::Auto,
        Fill::Solid(palette.border_strong.0),
        Fill::Solid(palette.text_faint.0),
        Fill::None,
    )
    .finish();
    Container::new(
        ConstrainedBox::new(scrollable)
            .with_max_height(sizes.tool_output_max_height)
            .finish(),
    )
    .with_background_color(palette.code_background.0)
    .with_border(Border::all(1.).with_border_color(palette.border.0))
    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(sizes.chip_radius)))
    .finish()
}

pub(super) fn error_card(cx: &RowContext, message: &str) -> Box<dyn Element> {
    let palette = cx.palette();
    let sizes = cx.sizes();
    Container::new(
        Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_spacing(9.)
            .with_child(
                Container::new(icon(ICON_ALERT, palette.error.0, sizes.icon_size))
                    .with_padding_top(1.)
                    .finish(),
            )
            .with_child(
                Shrinkable::new(
                    1.,
                    Text::new(
                        details::strip_ansi(message).trim().to_owned(),
                        cx.style.ui,
                        sizes.small_font_size + 0.5,
                    )
                    .with_color(palette.text.0)
                    .with_line_height_ratio(1.45)
                    .with_selection_color(palette.selection.0)
                    .finish(),
                )
                .finish(),
            )
            .finish(),
    )
    .with_horizontal_padding(12.)
    .with_vertical_padding(10.)
    .with_background_color(palette.error_background.0)
    .with_border(Border::all(1.).with_border_color(palette.error_border.0))
    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(sizes.card_radius)))
    .finish()
}

/// A centered label between two hairlines.
fn divider(cx: &RowContext, label: &str) -> Box<dyn Element> {
    let palette = cx.palette();
    let line = || {
        Expanded::new(
            1.,
            ConstrainedBox::new(
                Container::new(Empty::new().finish())
                    .with_background_color(palette.border.0)
                    .finish(),
            )
            .with_height(1.)
            .finish(),
        )
        .finish()
    };
    Flex::row()
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_main_axis_size(MainAxisSize::Max)
        .with_child(line())
        .with_child(
            Container::new(
                Text::new_inline(label.to_owned(), cx.style.ui, cx.sizes().tiny_font_size)
                    .with_color(palette.text_muted.0)
                    .with_selectable(false)
                    .finish(),
            )
            .with_horizontal_padding(12.)
            .finish(),
        )
        .with_child(line())
        .finish()
}

pub(super) fn icon(path: &'static str, color: ColorU, size: f32) -> Box<dyn Element> {
    ConstrainedBox::new(SvgIcon::new(path, color).finish())
        .with_width(size)
        .with_height(size)
        .finish()
}

/// Lays its child out at most `ratio` of the width it's given, like a chat bubble's max width.
pub(super) struct FractionWidth {
    child: Box<dyn Element>,
    ratio: f32,
}

impl FractionWidth {
    pub fn new(child: Box<dyn Element>, ratio: f32) -> Self {
        Self { child, ratio }
    }
}

impl Element for FractionWidth {
    fn layout(
        &mut self,
        constraint: SizeConstraint,
        ctx: &mut LayoutContext,
        app: &AppContext,
    ) -> Vector2F {
        let max_width = if constraint.max.x().is_finite() {
            (constraint.max.x() * self.ratio).max(constraint.min.x())
        } else {
            constraint.max.x()
        };
        let child_constraint = SizeConstraint {
            min: constraint.min,
            max: Vector2F::new(max_width, constraint.max.y()),
        };
        self.child.layout(child_constraint, ctx, app)
    }

    fn after_layout(&mut self, ctx: &mut AfterLayoutContext, app: &AppContext) {
        self.child.after_layout(ctx, app);
    }

    fn paint(&mut self, origin: Vector2F, ctx: &mut PaintContext, app: &AppContext) {
        self.child.paint(origin, ctx, app);
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

/// How often live rows ("Trabalhando há 12s") refresh their timers.
pub(super) const LIVE_TICK: Duration = Duration::from_secs(1);
