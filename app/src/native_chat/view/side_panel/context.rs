//! "Contexto": how full the context window is and what fills it.

use claude_chat::protocol::ContextUsage;
use claude_chat::reducer::ConversationStatus;
use pathfinder_color::ColorU;
use warpui::AppContext;
use warpui::elements::{
    Border, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, Element, Empty, Expanded,
    Flex, MainAxisAlignment, MainAxisSize, ParentElement, Radius, Rect,
};
use warpui::fonts::Weight;

use super::format::{
    self, CategoryTone, ContextFill, ContextRow, abbreviate_home, context_fill, context_rows,
    format_percent, format_share, format_tokens,
};
use super::widgets::{self, Look, ROW_PADDING_X};
use super::{SidePanelAction, SidePanelSection, SidePanelView};
use crate::native_chat::design::Palette;

const BAR_HEIGHT: f32 = 8.;
const SEGMENT_RADIUS: f32 = 2.;
const SEGMENT_GAP: f32 = 2.;
const LEGEND_SIZE: f32 = 8.;
const SHARE_COLUMN_WIDTH: f32 = 36.;
const LEGEND_ROW_SPACING: f32 = 7.;
const SUBLIST_INDENT: f32 = 14.;
/// Tools listed under "Ferramentas MCP"; some setups have hundreds.
const MAX_LISTED_TOOLS: usize = 40;
const WARNING_PERCENT: f64 = 80.;

impl SidePanelView {
    pub(super) fn render_context(&self, look: Look<'_>, app: &AppContext) -> Box<dyn Element> {
        let palette = look.palette;
        let section = SidePanelSection::Context;
        let is_expanded = self.is_expanded(section);
        let session = self.session.as_ref(app);
        let conversation = session.conversation();
        let usage = conversation.context_usage();
        let fill = context_fill(
            usage,
            conversation.live_context_tokens(),
            conversation.context_window(),
        );
        let percent = fill.and_then(|fill| fill.percent);

        let summary = percent.filter(|_| !is_expanded).map(|percent| {
            Flex::row()
                .with_main_axis_size(MainAxisSize::Min)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(8.)
                .with_child(look.meta(format_percent(percent), palette.text_muted.0))
                .with_child(widgets::mini_bar(
                    percent / 100.,
                    fill_color(percent, palette),
                    palette.surface_strong.0,
                ))
                .finish()
        });
        let mut children = vec![widgets::card_header(
            "Contexto",
            is_expanded,
            summary,
            self.mouse("header:context"),
            SidePanelAction::ToggleSection(section),
            look,
        )];
        if !is_expanded {
            return widgets::card(children);
        }

        let mut body = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(LEGEND_ROW_SPACING);
        match fill {
            Some(fill) => {
                body.add_child(render_fill_summary(fill, look));
                let rows = usage.map(context_rows).unwrap_or_default();
                if rows.is_empty() {
                    body.add_child(widgets::progress_bar(
                        fill.percent.unwrap_or_default() / 100.,
                        fill_color(fill.percent.unwrap_or_default(), palette),
                        palette.surface_strong.0,
                        BAR_HEIGHT,
                    ));
                } else {
                    body.add_child(render_segmented_bar(&rows, fill, look));
                    body.add_child(Empty::new().finish());
                    for row in &rows {
                        body.add_child(render_legend_row(row, look));
                    }
                }
            }
            None => body.add_child(look.meta("Calculando…", palette.text_faint.0)),
        }
        children.push(
            Container::new(body.finish())
                .with_horizontal_padding(ROW_PADDING_X)
                .with_padding_top(2.)
                .with_padding_bottom(6.)
                .finish(),
        );

        if let Some(usage) = usage {
            children.extend(self.render_context_lists(usage, look));
        }

        let is_compacting = conversation.is_compacting();
        let can_compact = session.has_transcript()
            && !is_compacting
            && conversation.status() == ConversationStatus::Idle;
        children.push(
            Container::new(widgets::wide_button(
                if is_compacting {
                    "Compactando…"
                } else {
                    "Compactar conversa"
                },
                can_compact,
                self.mouse("compact"),
                SidePanelAction::Compact,
                look,
            ))
            .with_horizontal_padding(ROW_PADDING_X - 4.)
            .with_padding_top(4.)
            .with_padding_bottom(4.)
            .finish(),
        );
        widgets::card(children)
    }

    /// The expandable "Ferramentas MCP" and "Arquivos de memória" lists.
    fn render_context_lists(&self, usage: &ContextUsage, look: Look<'_>) -> Vec<Box<dyn Element>> {
        let palette = look.palette;
        let mut children = Vec::new();
        if !usage.mcp_tools.is_empty() {
            children.push(self.render_list_header(
                "Ferramentas MCP",
                usage.mcp_tools.len(),
                self.show_mcp_tools,
                SidePanelAction::ToggleMcpTools,
                look,
            ));
            if self.show_mcp_tools {
                let mut tools = usage.mcp_tools.iter().collect::<Vec<_>>();
                tools.sort_by_key(|tool| std::cmp::Reverse(tool.tokens.unwrap_or_default()));
                let mut list = sublist();
                for tool in tools.iter().take(MAX_LISTED_TOOLS) {
                    let color = if tool.is_loaded {
                        palette.text_muted.0
                    } else {
                        palette.text_faint.0
                    };
                    list.add_child(render_sublist_row(
                        look.meta(format::display_tool_name(&tool.name), color),
                        tool.tokens,
                        look,
                    ));
                }
                if tools.len() > MAX_LISTED_TOOLS {
                    list.add_child(look.meta(
                        format!("e mais {}", tools.len() - MAX_LISTED_TOOLS),
                        palette.text_faint.0,
                    ));
                }
                children.push(finish_sublist(list));
            }
        }
        if !usage.memory_files.is_empty() {
            children.push(self.render_list_header(
                "Arquivos de memória",
                usage.memory_files.len(),
                self.show_memory_files,
                SidePanelAction::ToggleMemoryFiles,
                look,
            ));
            if self.show_memory_files {
                let home = dirs::home_dir();
                let mut list = sublist();
                for file in &usage.memory_files {
                    let path = abbreviate_home(std::path::Path::new(&file.path), home.as_deref());
                    let label = widgets::path_label(
                        path,
                        look.sizes.tiny_font_size,
                        palette.text_muted.0,
                        look,
                    );
                    list.add_child(render_sublist_row(label, file.tokens, look));
                }
                children.push(finish_sublist(list));
            }
        }
        children
    }

    fn render_list_header(
        &self,
        title: &'static str,
        count: usize,
        is_open: bool,
        action: SidePanelAction,
        look: Look<'_>,
    ) -> Box<dyn Element> {
        let palette = look.palette;
        widgets::clickable_row(self.mouse(title), action, look, move |is_hovered| {
            let color = if is_hovered {
                palette.text.0
            } else {
                palette.text_muted.0
            };
            let chevron = if is_open {
                widgets::CHEVRON_DOWN_ICON
            } else {
                widgets::CHEVRON_RIGHT_ICON
            };
            let label = Flex::row()
                .with_main_axis_size(MainAxisSize::Min)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(6.)
                .with_child(look.label(title, color))
                .with_child(widgets::svg(chevron, palette.text_faint.0, 10.))
                .finish();
            widgets::row_content(
                None,
                label,
                Some(look.meta(count.to_string(), palette.text_faint.0)),
            )
        })
    }
}

/// "8% usado" and "78.4k / 1M".
fn render_fill_summary(fill: ContextFill, look: Look<'_>) -> Box<dyn Element> {
    let palette = look.palette;
    let used = match fill.percent {
        Some(percent) => format!("{} usado", format_percent(percent)),
        None => "Em uso".to_owned(),
    };
    let amount = match fill.window {
        Some(window) => format!("{} / {}", format_tokens(fill.used), format_tokens(window)),
        None => format_tokens(fill.used),
    };
    Flex::row()
        .with_main_axis_size(MainAxisSize::Max)
        .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_child(look.text(
            used,
            look.sizes.small_font_size,
            palette.text.0,
            Weight::Medium,
        ))
        .with_child(look.meta(amount, palette.text_muted.0))
        .finish()
}

/// One segment per category that takes space, then the free remainder.
fn render_segmented_bar(
    rows: &[ContextRow],
    fill: ContextFill,
    look: Look<'_>,
) -> Box<dyn Element> {
    let palette = look.palette;
    let mut bar = Flex::row()
        .with_main_axis_size(MainAxisSize::Max)
        .with_spacing(SEGMENT_GAP);
    let mut filled = 0;
    for row in rows {
        if row.is_deferred || row.tone == CategoryTone::Free || row.tokens == 0 {
            continue;
        }
        filled += row.tokens;
        bar.add_child(
            Expanded::new(row.tokens as f32, segment(tone_color(row.tone, palette))).finish(),
        );
    }
    let window = fill.window.unwrap_or(filled);
    let free = window.saturating_sub(filled.max(fill.used));
    if free > 0 {
        bar.add_child(Expanded::new(free as f32, segment(palette.surface_strong.0)).finish());
    }
    ConstrainedBox::new(bar.finish())
        .with_height(BAR_HEIGHT)
        .finish()
}

fn segment(color: ColorU) -> Box<dyn Element> {
    ConstrainedBox::new(
        Rect::new()
            .with_background_color(color)
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(SEGMENT_RADIUS)))
            .finish(),
    )
    .with_height(BAR_HEIGHT)
    .finish()
}

/// A colored square, the category, its tokens and its share of the window.
fn render_legend_row(row: &ContextRow, look: Look<'_>) -> Box<dyn Element> {
    let palette = look.palette;
    let color = tone_color(row.tone, palette);
    let legend = if row.is_deferred || row.tone == CategoryTone::Free {
        let color = if row.is_deferred {
            color
        } else {
            palette.border_strong.0
        };
        Container::new(
            ConstrainedBox::new(Empty::new().finish())
                .with_width(LEGEND_SIZE - 2.)
                .with_height(LEGEND_SIZE - 2.)
                .finish(),
        )
        .with_border(Border::all(1.).with_border_color(color))
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(SEGMENT_RADIUS)))
        .finish()
    } else {
        widgets::swatch(color, LEGEND_SIZE, LEGEND_SIZE, SEGMENT_RADIUS)
    };
    let (label_color, number_color) = if row.is_deferred {
        (palette.text_faint.0, palette.text_faint.0)
    } else {
        (palette.text.0, palette.text_muted.0)
    };
    let share = row.percent.map(format_share).unwrap_or_default();
    Flex::row()
        .with_main_axis_size(MainAxisSize::Max)
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(8.)
        .with_child(legend)
        .with_child(Expanded::new(1., look.meta(row.label.clone(), label_color)).finish())
        .with_child(look.meta(format_tokens(row.tokens), number_color))
        .with_child(
            ConstrainedBox::new(
                Flex::row()
                    .with_main_axis_size(MainAxisSize::Max)
                    .with_main_axis_alignment(MainAxisAlignment::End)
                    .with_child(look.meta(share, palette.text_faint.0))
                    .finish(),
            )
            .with_width(SHARE_COLUMN_WIDTH)
            .finish(),
        )
        .finish()
}

fn sublist() -> Flex {
    Flex::column()
        .with_main_axis_size(MainAxisSize::Min)
        .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
        .with_spacing(5.)
}

fn finish_sublist(list: Flex) -> Box<dyn Element> {
    Container::new(list.finish())
        .with_padding_left(ROW_PADDING_X + SUBLIST_INDENT)
        .with_padding_right(ROW_PADDING_X)
        .with_padding_top(2.)
        .with_padding_bottom(6.)
        .finish()
}

fn render_sublist_row(
    label: Box<dyn Element>,
    tokens: Option<u64>,
    look: Look<'_>,
) -> Box<dyn Element> {
    let mut row = Flex::row()
        .with_main_axis_size(MainAxisSize::Max)
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(8.)
        .with_child(Expanded::new(1., label).finish());
    if let Some(tokens) = tokens {
        row.add_child(look.meta(format_tokens(tokens), look.palette.text_faint.0));
    }
    row.finish()
}

fn tone_color(tone: CategoryTone, palette: &Palette) -> ColorU {
    match tone {
        CategoryTone::SystemPrompt => palette.text_faint.0,
        CategoryTone::SystemTools => palette.text_muted.0,
        CategoryTone::McpTools => palette.info.0,
        CategoryTone::McpInstructions => ColorU {
            a: 150,
            ..palette.info.0
        },
        CategoryTone::Agents => palette.link.0,
        CategoryTone::Memory => palette.success.0,
        CategoryTone::Skills => palette.warning.0,
        CategoryTone::Messages => palette.accent.0,
        CategoryTone::AutocompactBuffer => palette.border_strong.0,
        CategoryTone::Free => palette.surface_strong.0,
        CategoryTone::Other => palette.text_faint.0,
    }
}

fn fill_color(percent: f64, palette: &Palette) -> ColorU {
    widgets::meter_color(percent, palette.accent.0, WARNING_PERCENT, palette)
}
