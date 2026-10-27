//! What the transcript says about tool calls, notices and turns, worked out from the timeline.

use std::time::SystemTime;

use chrono::{DateTime, Local};
use claude_chat::timeline::{TodoStatus, ToolCall};
use serde_json::Value;
use similar::{ChangeTag, TextDiff};

/// Diffs longer than this are cut, with a note saying how many lines were left out.
pub(super) const MAX_DIFF_LINES: usize = 400;
const DIFF_CONTEXT_LINES: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DiffLineKind {
    Added,
    Removed,
    Context,
    /// Where an elided stretch of unchanged lines was.
    Gap,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DiffLine {
    pub kind: DiffLineKind,
    pub text: String,
}

/// The lines an Edit, MultiEdit or Write call changed: from the patch in its result when Claude
/// Code reported one, else from its input.
pub(super) fn tool_diff(tool: &ToolCall) -> Vec<DiffLine> {
    let structured = tool
        .result
        .as_ref()
        .and_then(|result| result.structured.as_ref());
    if let Some(lines) = structured.and_then(patch_lines) {
        return lines;
    }
    let text = |value: &Value, key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    match tool.name.as_str() {
        "Edit" => diff_texts(
            &text(&tool.input, "old_string"),
            &text(&tool.input, "new_string"),
        ),
        "MultiEdit" => {
            let mut lines = Vec::new();
            for edit in tool
                .input
                .get("edits")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if !lines.is_empty() {
                    lines.push(DiffLine {
                        kind: DiffLineKind::Gap,
                        text: String::new(),
                    });
                }
                lines.extend(diff_texts(
                    &text(edit, "old_string"),
                    &text(edit, "new_string"),
                ));
            }
            lines
        }
        "Write" => {
            let content = text(&tool.input, "content");
            match structured
                .and_then(|structured| structured.get("originalFile"))
                .and_then(Value::as_str)
            {
                Some(original) => diff_texts(original, &content),
                None => content
                    .lines()
                    .map(|line| DiffLine {
                        kind: DiffLineKind::Added,
                        text: line.to_owned(),
                    })
                    .collect(),
            }
        }
        _ => Vec::new(),
    }
}

/// The lines of Claude Code's `structuredPatch` hunks.
fn patch_lines(structured: &Value) -> Option<Vec<DiffLine>> {
    let hunks = structured.get("structuredPatch")?.as_array()?;
    if hunks.is_empty() {
        return None;
    }
    let mut lines = Vec::new();
    for (position, hunk) in hunks.iter().enumerate() {
        if position > 0 {
            lines.push(DiffLine {
                kind: DiffLineKind::Gap,
                text: String::new(),
            });
        }
        for line in hunk
            .get("lines")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            let (kind, text) = match line.chars().next() {
                Some('+') => (DiffLineKind::Added, &line[1..]),
                Some('-') => (DiffLineKind::Removed, &line[1..]),
                Some(' ') => (DiffLineKind::Context, &line[1..]),
                _ => (DiffLineKind::Context, line),
            };
            lines.push(DiffLine {
                kind,
                text: text.to_owned(),
            });
        }
    }
    Some(lines)
}

/// A line diff of two texts with a few unchanged lines around each change.
pub(super) fn diff_texts(old: &str, new: &str) -> Vec<DiffLine> {
    let diff = TextDiff::from_lines(old, new);
    let mut lines = Vec::new();
    for (position, group) in diff.grouped_ops(DIFF_CONTEXT_LINES).iter().enumerate() {
        if position > 0 {
            lines.push(DiffLine {
                kind: DiffLineKind::Gap,
                text: String::new(),
            });
        }
        for op in group {
            for change in diff.iter_changes(op) {
                let kind = match change.tag() {
                    ChangeTag::Insert => DiffLineKind::Added,
                    ChangeTag::Delete => DiffLineKind::Removed,
                    ChangeTag::Equal => DiffLineKind::Context,
                };
                lines.push(DiffLine {
                    kind,
                    text: change.value().trim_end_matches(['\n', '\r']).to_owned(),
                });
            }
        }
    }
    lines
}

/// A tool result as worth reading: without the `<system-reminder>` blocks Claude Code appends
/// for the model, and without trailing blank lines.
pub(super) fn clean_result_text(text: &str) -> String {
    const OPEN: &str = "<system-reminder>";
    const CLOSE: &str = "</system-reminder>";
    let mut cleaned = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        cleaned.push_str(&rest[..start]);
        rest = match rest[start..].find(CLOSE) {
            Some(end) => &rest[start + end + CLOSE.len()..],
            None => "",
        };
    }
    cleaned.push_str(rest);
    cleaned.trim_end().trim_start_matches('\n').to_owned()
}

/// At most `max_lines` lines of `text`, and how many were left out.
pub(super) fn first_lines(text: &str, max_lines: usize) -> (String, usize) {
    let total = text.lines().count();
    if total <= max_lines {
        return (text.to_owned(), 0);
    }
    let kept: Vec<&str> = text.lines().take(max_lines).collect();
    (kept.join("\n"), total - max_lines)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TodoLine {
    pub text: String,
    pub status: TodoStatus,
}

/// The task list a TodoWrite call wrote, or the task a TaskCreate / TaskUpdate call touched.
pub(super) fn todo_lines(tool: &ToolCall) -> Vec<TodoLine> {
    let status = |value: Option<&Value>| match value.and_then(Value::as_str) {
        Some("completed") => TodoStatus::Completed,
        Some("in_progress") => TodoStatus::InProgress,
        _ => TodoStatus::Pending,
    };
    match tool.name.as_str() {
        "TodoWrite" => tool
            .input
            .get("todos")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|todo| {
                Some(TodoLine {
                    text: todo.get("content")?.as_str()?.to_owned(),
                    status: status(todo.get("status")),
                })
            })
            .collect(),
        "TaskCreate" | "TaskUpdate" => {
            let text = ["subject", "description", "activeForm"]
                .iter()
                .find_map(|key| tool.input.get(*key).and_then(Value::as_str));
            text.map(|text| TodoLine {
                text: text.to_owned(),
                status: status(tool.input.get("status")),
            })
            .into_iter()
            .collect()
        }
        _ => Vec::new(),
    }
}

/// "950", "12.3k", "180k", "1.2M".
pub(super) fn format_tokens(tokens: u64) -> String {
    match tokens {
        0..1_000 => tokens.to_string(),
        1_000..10_000 => trim_decimal(format!("{:.1}", tokens as f64 / 1_000.)) + "k",
        10_000..1_000_000 => format!("{}k", (tokens as f64 / 1_000.).round()),
        _ => trim_decimal(format!("{:.1}", tokens as f64 / 1_000_000.)) + "M",
    }
}

fn trim_decimal(text: String) -> String {
    text.strip_suffix(".0").map(str::to_owned).unwrap_or(text)
}

/// "Conversa compactada · 180k → 22k tokens".
pub(super) fn compacted_label(pre_tokens: Option<u64>, post_tokens: Option<u64>) -> String {
    match (pre_tokens, post_tokens) {
        (Some(pre), Some(post)) => format!(
            "Conversa compactada · {} → {} tokens",
            format_tokens(pre),
            format_tokens(post)
        ),
        (Some(pre), None) => format!("Conversa compactada · {} tokens", format_tokens(pre)),
        (None, _) => "Conversa compactada".to_owned(),
    }
}

/// Why a turn ended in error, from the `result` subtype.
pub(super) fn turn_error_label(subtype: &str) -> String {
    match subtype {
        "error_max_turns" => "Claude parou: limite de turnos atingido.".to_owned(),
        "error_max_budget_usd" => "Claude parou: limite de custo atingido.".to_owned(),
        "error_during_execution" => "A resposta foi interrompida por um erro.".to_owned(),
        "" | "success" => "A resposta terminou com um erro.".to_owned(),
        other => format!("A resposta terminou com um erro ({other})."),
    }
}

/// "14:03" today, "26/10 14:03" on other days.
pub(super) fn format_time(time: SystemTime, now: SystemTime) -> String {
    let time: DateTime<Local> = time.into();
    let now: DateTime<Local> = now.into();
    if time.date_naive() == now.date_naive() {
        time.format("%H:%M").to_string()
    } else {
        time.format("%d/%m %H:%M").to_string()
    }
}

/// The first line of a thinking block, for its collapsed line.
pub(super) fn first_sentence(text: &str, max_chars: usize) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .trim_matches('*');
    if line.chars().count() <= max_chars {
        return line.to_owned();
    }
    let cut: String = line.chars().take(max_chars).collect();
    format!("{}…", cut.trim_end())
}

/// `text` without terminal color and cursor escape sequences.
pub(super) fn strip_ansi(text: &str) -> String {
    let mut stripped = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(char) = chars.next() {
        if char != '\u{1b}' {
            stripped.push(char);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                while let Some(next) = chars.next() {
                    if next == '\u{7}' {
                        break;
                    }
                    if next == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    stripped
}
