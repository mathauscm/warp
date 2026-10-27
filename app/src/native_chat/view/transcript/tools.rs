//! Tool calls in the transcript: one muted line each that opens into what the call did, runs of
//! calls summed up in one sentence, subagent cards, task lists and artifact cards.

use claude_chat::artifacts::{ArtifactKind, ArtifactMeta, is_artifact_tool, kind_label_pt};
use claude_chat::presentation::{
    self, Row, ToolBatch, ToolKind, ToolLabel, edit_stats, tool_label_in,
};
use claude_chat::timeline::{TodoStatus, ToolCall, ToolStatus};
use pathfinder_color::ColorU;
use serde_json::Value;
use warpui::Element;
use warpui::elements::shimmering_text::{ShimmerConfig, ShimmeringTextElement};
use warpui::elements::{
    Border, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, Empty, Expanded, Flex,
    Hoverable, MainAxisSize, ParentElement, Radius, Shrinkable, Text,
};
use warpui::fonts::{Properties, Weight};
use warpui::platform::Cursor;
use warpui::text_layout::ClipConfig;

use super::TranscriptAction;
use super::details::{self, DiffLineKind, MAX_DIFF_LINES};
use super::render::{
    ICON_CHEVRON_DOWN, ICON_CHEVRON_RIGHT, MarkdownTone, RowContext, Style, disclosure_line, icon,
    indented_detail, mono_box, render_markdown, render_nested_rows, scroll_box,
};

/// Lines of a tool's result shown when it's expanded.
const RESULT_PREVIEW_LINES: usize = 80;
/// Lines of an integration's input shown when it's expanded.
const INPUT_PREVIEW_LINES: usize = 24;
/// Gap between a tool's icon and its label; details indent to line up with the label.
const ICON_GAP: f32 = 8.;
const ARTIFACT_TILE_SIZE: f32 = 40.;

fn kind_icon(kind: ToolKind) -> &'static str {
    match kind {
        ToolKind::Command => "bundled/svg/picolas-terminal.svg",
        ToolKind::Read => "bundled/svg/picolas-file.svg",
        ToolKind::Edit => "bundled/svg/picolas-pencil.svg",
        ToolKind::Search => "bundled/svg/picolas-glob.svg",
        ToolKind::WebSearch | ToolKind::WebFetch => "bundled/svg/picolas-globe.svg",
        ToolKind::Subagent => "bundled/svg/picolas-agent.svg",
        ToolKind::Todo => "bundled/svg/picolas-todo.svg",
        ToolKind::Question => "bundled/svg/picolas-question.svg",
        ToolKind::Plan => "bundled/svg/picolas-plan.svg",
        ToolKind::Skill => "bundled/svg/picolas-sparkle.svg",
        ToolKind::Integration => "bundled/svg/picolas-plug.svg",
        ToolKind::Other => "bundled/svg/picolas-wrench.svg",
    }
}

fn is_running(status: ToolStatus) -> bool {
    match status {
        ToolStatus::Streaming | ToolStatus::AwaitingApproval | ToolStatus::Running => true,
        ToolStatus::Done | ToolStatus::Failed | ToolStatus::Denied | ToolStatus::Interrupted => {
            false
        }
    }
}

pub(super) fn render_tool(cx: &RowContext, tool: &ToolCall) -> Box<dyn Element> {
    let label = tool_label_in(tool, Some(cx.cwd));
    if is_artifact_tool(&tool.name)
        && let Some(meta) = cx.view.artifact(&tool.id)
    {
        return render_artifact_card(cx, tool, meta);
    }
    match label.kind {
        ToolKind::Subagent => return render_subagent(cx, tool, &label),
        ToolKind::Todo if tool.name == "TodoWrite" => return render_todos(cx, tool, &label),
        ToolKind::Command
        | ToolKind::Read
        | ToolKind::Edit
        | ToolKind::Search
        | ToolKind::WebSearch
        | ToolKind::WebFetch
        | ToolKind::Todo
        | ToolKind::Question
        | ToolKind::Plan
        | ToolKind::Skill
        | ToolKind::Integration
        | ToolKind::Other => {}
    }
    let key = format!("tool:{}", tool.id);
    let details = tool_details(cx, tool, &key);
    let expanded = details.is_some() && cx.view.is_expanded(&key, false);
    let line = tool_line(cx, tool, &label, &key, expanded, details.is_some());
    match details.filter(|_| expanded) {
        Some(details) => Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(line)
            .with_child(
                Container::new(details)
                    .with_padding_left(cx.sizes().icon_size + ICON_GAP)
                    .with_padding_top(2.)
                    .with_padding_bottom(6.)
                    .finish(),
            )
            .finish(),
        None => line,
    }
}

/// Icon, verb and target on one line, with diff stats and the call's status at the end.
fn tool_line(
    cx: &RowContext,
    tool: &ToolCall,
    label: &ToolLabel,
    key: &str,
    expanded: bool,
    expandable: bool,
) -> Box<dyn Element> {
    let style = cx.style.clone();
    let running = is_running(tool.status);
    let shimmer = running.then(|| cx.view.shimmer(key));
    let stats = edit_stats(tool).filter(|stats| stats.added + stats.removed > 0);
    let status = tool.status;
    let verb = label.verb.clone();
    let target = label.target.clone();
    let kind = label.kind;
    let action = cx.toggle_action(key, expanded);
    let hoverable = Hoverable::new(cx.view.mouse(&format!("toggle:{key}")), move |state| {
        let palette = &style.palette;
        let sizes = &style.sizes;
        let hovered = state.is_hovered() && expandable;
        let color = if hovered {
            palette.text.0
        } else {
            palette.text_muted.0
        };
        let icon_color = match status {
            ToolStatus::Failed => palette.error.0,
            ToolStatus::Streaming
            | ToolStatus::AwaitingApproval
            | ToolStatus::Running
            | ToolStatus::Done
            | ToolStatus::Denied
            | ToolStatus::Interrupted => color,
        };
        let font_size = sizes.small_font_size + 0.5;
        let mut row = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_main_axis_size(MainAxisSize::Max)
            .with_child(
                Container::new(icon(kind_icon(kind), icon_color, sizes.icon_size - 1.))
                    .with_padding_right(ICON_GAP)
                    .finish(),
            );
        let text = match &target {
            Some(target) => format!("{verb} {target}"),
            None => verb.clone(),
        };
        let label: Box<dyn Element> = match shimmer {
            Some(handle) => ShimmeringTextElement::new(
                text,
                style.ui,
                font_size,
                palette.text_muted.0,
                palette.shimmer.0,
                ShimmerConfig::default(),
                handle,
            )
            .finish(),
            None => {
                let mut line = Flex::row()
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_main_axis_size(MainAxisSize::Min)
                    .with_child(
                        Text::new_inline(verb, style.ui, font_size)
                            .with_color(color)
                            .with_style(Properties::default().weight(Weight::Medium))
                            .with_selectable(false)
                            .finish(),
                    );
                if let Some(target) = target {
                    let font = if kind == ToolKind::Command {
                        style.mono
                    } else {
                        style.ui
                    };
                    let size = if kind == ToolKind::Command {
                        sizes.code_font_size - 0.5
                    } else {
                        font_size
                    };
                    line.add_child(
                        Shrinkable::new(
                            1.,
                            Container::new(
                                Text::new_inline(target, font, size)
                                    .with_color(color)
                                    .with_clip(ClipConfig::ellipsis())
                                    .with_selectable(false)
                                    .finish(),
                            )
                            .with_padding_left(5.)
                            .finish(),
                        )
                        .finish(),
                    );
                }
                line.finish()
            }
        };
        row.add_child(Shrinkable::new(1., label).finish());
        if let Some(stats) = &stats {
            row.add_child(diff_stats(&style, stats.added, stats.removed));
        }
        if let Some(status) = status_label(status) {
            let status_color = match status {
                StatusLabel::Denied | StatusLabel::Waiting => palette.warning.0,
                StatusLabel::Failed => palette.error.0,
                StatusLabel::Interrupted => palette.text_faint.0,
            };
            row.add_child(
                Container::new(
                    Text::new_inline(status.text(), style.ui, sizes.tiny_font_size)
                        .with_color(status_color)
                        .with_selectable(false)
                        .finish(),
                )
                .with_padding_left(8.)
                .finish(),
            );
        }
        if expandable && (hovered || expanded) {
            row.add_child(
                Container::new(icon(
                    if expanded {
                        ICON_CHEVRON_DOWN
                    } else {
                        ICON_CHEVRON_RIGHT
                    },
                    color,
                    sizes.small_icon_size - 1.,
                ))
                .with_padding_left(6.)
                .finish(),
            );
        }
        ConstrainedBox::new(row.finish())
            .with_height(sizes.tool_row_height)
            .finish()
    });
    if !expandable {
        return hoverable.finish();
    }
    hoverable
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
        .finish()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusLabel {
    Waiting,
    Denied,
    Failed,
    Interrupted,
}

impl StatusLabel {
    fn text(self) -> &'static str {
        match self {
            StatusLabel::Waiting => "Aguardando aprovação",
            StatusLabel::Denied => "Negado",
            StatusLabel::Failed => "Falhou",
            StatusLabel::Interrupted => "Interrompido",
        }
    }
}

fn status_label(status: ToolStatus) -> Option<StatusLabel> {
    match status {
        ToolStatus::AwaitingApproval => Some(StatusLabel::Waiting),
        ToolStatus::Denied => Some(StatusLabel::Denied),
        ToolStatus::Failed => Some(StatusLabel::Failed),
        ToolStatus::Interrupted => Some(StatusLabel::Interrupted),
        ToolStatus::Streaming | ToolStatus::Running | ToolStatus::Done => None,
    }
}

/// "+12 −3" in the diff colors.
fn diff_stats(style: &Style, added: usize, removed: usize) -> Box<dyn Element> {
    let size = style.sizes.tiny_font_size;
    let mut row = Flex::row()
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_main_axis_size(MainAxisSize::Min)
        .with_spacing(5.);
    if added > 0 {
        row.add_child(
            Text::new_inline(format!("+{added}"), style.mono, size)
                .with_color(style.palette.diff_added.0)
                .with_selectable(false)
                .finish(),
        );
    }
    if removed > 0 {
        row.add_child(
            Text::new_inline(format!("−{removed}"), style.mono, size)
                .with_color(style.palette.diff_removed.0)
                .with_selectable(false)
                .finish(),
        );
    }
    Container::new(row.finish()).with_padding_left(8.).finish()
}

/// What an expanded tool line shows, or `None` when there's nothing more to see.
fn tool_details(cx: &RowContext, tool: &ToolCall, key: &str) -> Option<Box<dyn Element>> {
    let palette = cx.palette();
    let result = tool
        .result
        .as_ref()
        .map(|result| (details::clean_result_text(&result.text), result.is_error));
    let result_color = |is_error: bool| {
        if is_error {
            palette.error.0
        } else {
            palette.text_muted.0
        }
    };
    let preview = |text: &str| {
        let (text, hidden) = details::first_lines(text, RESULT_PREVIEW_LINES);
        if hidden == 0 {
            text
        } else {
            format!("{text}\n… mais {hidden} linhas")
        }
    };
    let input_text = |key: &str| tool.input.get(key).and_then(Value::as_str);
    match tool.name.as_str() {
        "Bash" | "Monitor" => {
            let command = input_text("command").unwrap_or_default();
            let mut column = Flex::column()
                .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
                .with_child(
                    Text::new(
                        format!("$ {}", command.trim()),
                        cx.style.mono,
                        cx.sizes().code_font_size - 0.5,
                    )
                    .with_color(palette.text.0)
                    .with_line_height_ratio(1.45)
                    .with_selection_color(palette.selection.0)
                    .finish(),
                );
            if let Some((output, is_error)) = result.filter(|(output, _)| !output.is_empty()) {
                column.add_child(
                    Container::new(
                        Text::new(
                            preview(&details::strip_ansi(&output)),
                            cx.style.mono,
                            cx.sizes().code_font_size - 0.5,
                        )
                        .with_color(result_color(is_error))
                        .with_line_height_ratio(1.45)
                        .with_selection_color(palette.selection.0)
                        .finish(),
                    )
                    .with_padding_top(6.)
                    .finish(),
                );
            }
            Some(scroll_box(cx, key, column.finish(), true))
        }
        "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => {
            if let Some((text, true)) = &result {
                return Some(mono_box(cx, key, text, palette.error.0));
            }
            let lines = details::tool_diff(tool);
            (!lines.is_empty()).then(|| render_diff(cx, key, &lines))
        }
        "WebFetch" | "WebSearch" | "AskUserQuestion" => {
            let (text, is_error) = result.filter(|(text, _)| !text.is_empty())?;
            if is_error {
                return Some(mono_box(cx, key, &text, palette.error.0));
            }
            Some(indented_detail(
                cx,
                render_markdown(cx, key, &preview(&text), MarkdownTone::Muted),
            ))
        }
        "ExitPlanMode" => {
            let plan = input_text("plan").filter(|plan| !plan.trim().is_empty())?;
            Some(indented_detail(
                cx,
                render_markdown(cx, key, plan, MarkdownTone::Muted),
            ))
        }
        "Read" | "Grep" | "Glob" | "LS" | "BashOutput" | "KillShell" | "KillBash" | "TaskStop"
        | "Skill" | "TaskCreate" | "TaskUpdate" | "TaskList" | "TaskGet" | "ToolSearch" => {
            let (text, is_error) = result.filter(|(text, _)| !text.is_empty())?;
            Some(mono_box(cx, key, &preview(&text), result_color(is_error)))
        }
        _ => {
            let mut sections = Vec::new();
            if tool
                .input
                .as_object()
                .is_some_and(|input| !input.is_empty())
            {
                let input = serde_json::to_string_pretty(&tool.input).unwrap_or_default();
                let (input, hidden) = details::first_lines(&input, INPUT_PREVIEW_LINES);
                sections.push((
                    if hidden == 0 {
                        input
                    } else {
                        format!("{input}\n…")
                    },
                    palette.text_muted.0,
                ));
            }
            if let Some((text, is_error)) = result.filter(|(text, _)| !text.is_empty()) {
                sections.push((preview(&text), result_color(is_error)));
            }
            if sections.is_empty() {
                return None;
            }
            let mut column = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);
            for (position, (text, color)) in sections.into_iter().enumerate() {
                column.add_child(
                    Container::new(
                        Text::new(text, cx.style.mono, cx.sizes().code_font_size - 0.5)
                            .with_color(color)
                            .with_line_height_ratio(1.45)
                            .with_selection_color(palette.selection.0)
                            .finish(),
                    )
                    .with_padding_top(if position == 0 { 0. } else { 8. })
                    .finish(),
                );
            }
            Some(scroll_box(cx, key, column.finish(), true))
        }
    }
}

fn render_diff(cx: &RowContext, key: &str, lines: &[details::DiffLine]) -> Box<dyn Element> {
    let palette = cx.palette();
    let font_size = cx.sizes().code_font_size - 0.5;
    let mut column = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);
    for line in lines.iter().take(MAX_DIFF_LINES) {
        let (prefix, background, color) = match line.kind {
            DiffLineKind::Added => ("+", Some(palette.diff_added_background.0), palette.text.0),
            DiffLineKind::Removed => ("−", Some(palette.diff_removed_background.0), palette.text.0),
            DiffLineKind::Context => (" ", None, palette.text_muted.0),
            DiffLineKind::Gap => ("", None, palette.text_faint.0),
        };
        let text = match line.kind {
            DiffLineKind::Gap => "⋯".to_owned(),
            DiffLineKind::Added | DiffLineKind::Removed | DiffLineKind::Context => {
                format!("{prefix} {}", line.text)
            }
        };
        let mut container = Container::new(
            Text::new(text, cx.style.mono, font_size)
                .with_color(color)
                .with_line_height_ratio(1.45)
                .with_selection_color(palette.selection.0)
                .finish(),
        )
        .with_horizontal_padding(12.);
        if let Some(background) = background {
            container = container.with_background_color(background);
        }
        column.add_child(container.finish());
    }
    if lines.len() > MAX_DIFF_LINES {
        column.add_child(
            Container::new(
                Text::new_inline(
                    format!("… mais {} linhas", lines.len() - MAX_DIFF_LINES),
                    cx.style.ui,
                    cx.sizes().tiny_font_size,
                )
                .with_color(palette.text_faint.0)
                .finish(),
            )
            .with_horizontal_padding(12.)
            .with_padding_top(4.)
            .finish(),
        );
    }
    scroll_box(
        cx,
        key,
        Container::new(column.finish())
            .with_vertical_padding(8.)
            .finish(),
        false,
    )
}

pub(super) fn render_batch(cx: &RowContext, batch: &ToolBatch) -> Box<dyn Element> {
    let key = format!("batch:{}", batch.id);
    let expanded = cx.view.is_expanded(&key, batch.is_live);
    let header = disclosure_line(
        cx,
        &key,
        Some(kind_icon(batch.kind)),
        &batch.summary,
        expanded,
        batch.is_live,
    );
    if !expanded {
        return header;
    }
    let rows: Vec<Row> = batch
        .items
        .iter()
        .map(|&index| match &cx.items[index] {
            claude_chat::timeline::TimelineItem::Thinking(_) => Row::Thinking { index },
            _ => Row::Tool { index },
        })
        .collect();
    Flex::column()
        .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
        .with_child(header)
        .with_child(indented_detail(cx, render_nested_rows(cx, &rows)))
        .finish()
}

fn render_subagent(cx: &RowContext, tool: &ToolCall, label: &ToolLabel) -> Box<dyn Element> {
    let palette = cx.palette();
    let sizes = cx.sizes();
    let key = format!("agent:{}", tool.id);
    let expanded = cx.view.is_expanded(&key, false);
    let running = is_running(tool.status);
    let actions = presentation::subagent_items(cx.items, &tool.id)
        .into_iter()
        .filter(|&index| {
            matches!(
                cx.items[index],
                claude_chat::timeline::TimelineItem::Tool(_)
            )
        })
        .count();
    let subagent_type = tool
        .input
        .get("subagent_type")
        .and_then(Value::as_str)
        .filter(|kind| !kind.is_empty() && *kind != "general-purpose")
        .map(str::to_owned);
    let title = match subagent_type {
        Some(kind) => format!("Agente {kind}"),
        None => "Agente".to_owned(),
    };
    let description = label.target.clone().unwrap_or_default();
    let trailing = if running {
        None
    } else {
        Some(match tool.status {
            ToolStatus::Failed => ("Falhou".to_owned(), palette.error.0),
            ToolStatus::Denied => ("Negado".to_owned(), palette.warning.0),
            ToolStatus::Interrupted => ("Interrompido".to_owned(), palette.text_faint.0),
            ToolStatus::Streaming
            | ToolStatus::AwaitingApproval
            | ToolStatus::Running
            | ToolStatus::Done => (
                format!("{actions} {}", if actions == 1 { "ação" } else { "ações" }),
                palette.text_faint.0,
            ),
        })
    };
    let shimmer = running.then(|| cx.view.shimmer(&key));
    let style = cx.style.clone();
    let action = cx.toggle_action(&key, expanded);
    let header = Hoverable::new(cx.view.mouse(&format!("toggle:{key}")), move |state| {
        let palette = &style.palette;
        let sizes = &style.sizes;
        let color = if state.is_hovered() {
            palette.text.0
        } else {
            palette.text_muted.0
        };
        let font_size = sizes.small_font_size + 0.5;
        let mut row = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_main_axis_size(MainAxisSize::Max)
            .with_child(
                Container::new(icon(kind_icon(ToolKind::Subagent), color, sizes.icon_size))
                    .with_padding_right(ICON_GAP)
                    .finish(),
            )
            .with_child(
                Text::new_inline(title, style.ui, font_size)
                    .with_color(palette.text.0)
                    .with_style(Properties::default().weight(Weight::Medium))
                    .with_selectable(false)
                    .finish(),
            );
        if !description.is_empty() {
            let text: Box<dyn Element> = match shimmer {
                Some(handle) => ShimmeringTextElement::new(
                    description,
                    style.ui,
                    font_size,
                    palette.text_muted.0,
                    palette.shimmer.0,
                    ShimmerConfig::default(),
                    handle,
                )
                .finish(),
                None => Text::new_inline(description, style.ui, font_size)
                    .with_color(color)
                    .with_clip(ClipConfig::ellipsis())
                    .with_selectable(false)
                    .finish(),
            };
            row.add_child(
                Shrinkable::new(1., Container::new(text).with_padding_left(7.).finish()).finish(),
            );
        } else {
            row.add_child(Expanded::new(1., Empty::new().finish()).finish());
        }
        if let Some((text, color)) = trailing {
            row.add_child(
                Container::new(
                    Text::new_inline(text, style.ui, sizes.tiny_font_size)
                        .with_color(color)
                        .with_selectable(false)
                        .finish(),
                )
                .with_padding_left(10.)
                .finish(),
            );
        }
        row.add_child(
            Container::new(icon(
                if expanded {
                    ICON_CHEVRON_DOWN
                } else {
                    ICON_CHEVRON_RIGHT
                },
                color,
                sizes.small_icon_size - 1.,
            ))
            .with_padding_left(8.)
            .finish(),
        );
        Container::new(
            ConstrainedBox::new(row.finish())
                .with_height(sizes.tool_row_height + 6.)
                .finish(),
        )
        .with_horizontal_padding(12.)
        .finish()
    })
    .with_cursor(Cursor::PointingHand)
    .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
    .finish();

    let mut card = Flex::column()
        .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
        .with_child(header);
    if expanded {
        let rows = presentation::build_subagent_rows(cx.items, &tool.id);
        let mut body = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);
        if let Some(prompt) = tool
            .input
            .get("prompt")
            .and_then(Value::as_str)
            .filter(|prompt| !prompt.trim().is_empty())
        {
            body.add_child(
                Container::new(render_markdown(
                    cx,
                    &format!("{key}:prompt"),
                    prompt,
                    MarkdownTone::Muted,
                ))
                .with_padding_bottom(10.)
                .finish(),
            );
        }
        if !rows.is_empty() {
            body.add_child(render_nested_rows(cx, &rows));
        }
        if let Some(result) = tool.result.as_ref().filter(|result| !result.is_error) {
            let text = details::clean_result_text(&result.text);
            if !text.is_empty() {
                body.add_child(
                    Container::new(render_markdown(
                        cx,
                        &format!("{key}:result"),
                        &text,
                        MarkdownTone::Muted,
                    ))
                    .with_padding_top(6.)
                    .finish(),
                );
            }
        }
        card.add_child(
            Container::new(body.finish())
                .with_horizontal_padding(14.)
                .with_padding_top(8.)
                .with_padding_bottom(12.)
                .with_border(Border::top(1.).with_border_color(palette.border.0))
                .finish(),
        );
    }
    Container::new(card.finish())
        .with_background_color(palette.code_background.0)
        .with_border(Border::all(1.).with_border_color(palette.border.0))
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(sizes.card_radius)))
        .with_vertical_margin(2.)
        .finish()
}

fn render_todos(cx: &RowContext, tool: &ToolCall, label: &ToolLabel) -> Box<dyn Element> {
    let palette = cx.palette();
    let sizes = cx.sizes();
    let key = format!("tool:{}", tool.id);
    let line = tool_line(cx, tool, label, &key, false, false);
    let todos = details::todo_lines(tool);
    if todos.is_empty() {
        return line;
    }
    let mut list = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);
    for todo in todos {
        let (path, icon_color, text_color) = match todo.status {
            TodoStatus::Completed => (
                "bundled/svg/picolas-circle-check.svg",
                palette.success.0,
                palette.text_faint.0,
            ),
            TodoStatus::InProgress => (
                "bundled/svg/picolas-circle-dot.svg",
                palette.accent.0,
                palette.text.0,
            ),
            TodoStatus::Pending => (
                "bundled/svg/picolas-circle.svg",
                palette.text_faint.0,
                palette.text_muted.0,
            ),
        };
        list.add_child(
            Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Start)
                .with_child(
                    Container::new(icon(path, icon_color, sizes.small_icon_size))
                        .with_padding_top(2.)
                        .with_padding_right(8.)
                        .finish(),
                )
                .with_child(
                    Shrinkable::new(
                        1.,
                        Text::new(todo.text, cx.style.ui, sizes.small_font_size + 0.5)
                            .with_color(text_color)
                            .with_line_height_ratio(1.4)
                            .finish(),
                    )
                    .finish(),
                )
                .finish(),
        );
    }
    Flex::column()
        .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
        .with_child(line)
        .with_child(
            Container::new(list.finish())
                .with_padding_left(sizes.icon_size + ICON_GAP)
                .with_padding_top(2.)
                .with_padding_bottom(6.)
                .finish(),
        )
        .finish()
}

fn artifact_icon(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Document => "bundled/svg/picolas-artifact-document.svg",
        ArtifactKind::Presentation => "bundled/svg/picolas-artifact-presentation.svg",
        ArtifactKind::Design => "bundled/svg/picolas-artifact-design.svg",
        ArtifactKind::Dashboard => "bundled/svg/picolas-artifact-dashboard.svg",
        ArtifactKind::Motion => "bundled/svg/picolas-artifact-motion.svg",
        ArtifactKind::Other => "bundled/svg/picolas-artifact-other.svg",
    }
}

fn artifact_color(kind: ArtifactKind, style: &Style) -> ColorU {
    let palette = &style.palette;
    match kind {
        ArtifactKind::Document => palette.kind_document.0,
        ArtifactKind::Presentation => palette.kind_presentation.0,
        ArtifactKind::Design => palette.kind_design.0,
        ArtifactKind::Dashboard => palette.kind_dashboard.0,
        ArtifactKind::Motion => palette.kind_motion.0,
        ArtifactKind::Other => palette.kind_other.0,
    }
}

/// A card for the artifact a tool call created or changed, which opens it.
fn render_artifact_card(cx: &RowContext, tool: &ToolCall, meta: &ArtifactMeta) -> Box<dyn Element> {
    let style = cx.style.clone();
    let key = format!("artifact:{}", tool.id);
    let open = TranscriptAction::OpenArtifact(meta.id.clone());
    let button_mouse = cx.view.mouse(&format!("open:{key}"));
    let title = meta.title.clone();
    let caption = if tool.name.ends_with("update_artifact") {
        format!(
            "{} · atualizado (v{})",
            kind_label_pt(meta.kind),
            meta.version
        )
    } else {
        kind_label_pt(meta.kind).to_owned()
    };
    let kind = meta.kind;
    let open_from_card = open.clone();
    Hoverable::new(cx.view.mouse(&format!("hover:{key}")), move |state| {
        let palette = &style.palette;
        let sizes = &style.sizes;
        let accent = artifact_color(kind, &style);
        let tile = ConstrainedBox::new(
            Container::new(
                warpui::elements::Align::new(icon(
                    artifact_icon(kind),
                    accent,
                    sizes.icon_size + 2.,
                ))
                .finish(),
            )
            .with_background_color(ColorU::new(accent.r, accent.g, accent.b, 0x24))
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(sizes.chip_radius)))
            .finish(),
        )
        .with_width(ARTIFACT_TILE_SIZE)
        .with_height(ARTIFACT_TILE_SIZE)
        .finish();
        let text = Flex::column()
            .with_child(
                Text::new_inline(title, style.ui, sizes.font_size)
                    .with_color(palette.text.0)
                    .with_style(Properties::default().weight(Weight::Medium))
                    .with_clip(ClipConfig::ellipsis())
                    .with_selectable(false)
                    .finish(),
            )
            .with_child(
                Container::new(
                    Text::new_inline(caption, style.ui, sizes.small_font_size)
                        .with_color(palette.text_muted.0)
                        .with_selectable(false)
                        .finish(),
                )
                .with_padding_top(2.)
                .finish(),
            )
            .finish();
        let palette_for_button = palette.clone();
        let ui = style.ui;
        let button_font = sizes.small_font_size;
        let radius = sizes.chip_radius;
        let open = open.clone();
        let button = Hoverable::new(button_mouse, move |state| {
            let background = if state.is_hovered() {
                palette_for_button.surface_hover.0
            } else {
                palette_for_button.surface_strong.0
            };
            Container::new(
                Text::new_inline("Abrir", ui, button_font)
                    .with_color(palette_for_button.text.0)
                    .with_style(Properties::default().weight(Weight::Medium))
                    .with_selectable(false)
                    .finish(),
            )
            .with_horizontal_padding(14.)
            .with_vertical_padding(6.)
            .with_background_color(background)
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(radius)))
            .finish()
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, _, _| ctx.dispatch_typed_action(open.clone()))
        .finish();
        let border = if state.is_hovered() {
            palette.border_strong.0
        } else {
            palette.border.0
        };
        Container::new(
            Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_main_axis_size(MainAxisSize::Max)
                .with_child(tile)
                .with_child(
                    Shrinkable::new(
                        1.,
                        Container::new(text).with_horizontal_padding(12.).finish(),
                    )
                    .finish(),
                )
                .with_child(Expanded::new(1., Empty::new().finish()).finish())
                .with_child(button)
                .finish(),
        )
        .with_uniform_padding(10.)
        .with_background_color(palette.surface.0)
        .with_border(Border::all(1.).with_border_color(border))
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(sizes.card_radius)))
        .with_vertical_margin(4.)
        .finish()
    })
    .with_cursor(Cursor::PointingHand)
    .on_click(move |ctx, _, _| ctx.dispatch_typed_action(open_from_card.clone()))
    .finish()
}
