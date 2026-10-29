//! Claude's Markdown split into the blocks the transcript draws differently: prose runs, fenced
//! code and tables.

use std::sync::Arc;

use markdown_parser::{
    FormattedTable, FormattedText, FormattedTextFragment, FormattedTextInline, FormattedTextLine,
};

use super::paths::PathChip;

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Block {
    /// A prose run and the files and folders it names, drawn as chips.
    Prose(Arc<FormattedText>, Vec<PathChip>),
    Code {
        language: String,
        code: String,
    },
    Table(FormattedTable),
    /// A `>` quote, with its own blocks.
    Quote(Vec<Block>),
}

/// Splits `markdown` into blocks, keeping each prose run's lines together so it lays out as one
/// selectable text.
pub(super) fn split_blocks(markdown: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    for (is_quote, text) in quote_segments(markdown) {
        if is_quote {
            let inner = split_blocks(&text);
            if !inner.is_empty() {
                blocks.push(Block::Quote(inner));
            }
        } else {
            blocks.extend(split_unquoted(&text));
        }
    }
    blocks
}

/// `markdown` cut into runs of quoted (`>`) and unquoted lines, with the quote markers removed.
/// Lines inside code fences are never quotes.
fn quote_segments(markdown: &str) -> Vec<(bool, String)> {
    let mut segments: Vec<(bool, String)> = Vec::new();
    let mut in_fence = false;
    for line in markdown.split('\n') {
        let trimmed = line.trim_start();
        let quoted = if in_fence {
            None
        } else {
            trimmed
                .strip_prefix("> ")
                .or_else(|| trimmed.strip_prefix('>'))
        };
        if quoted.is_none() && trimmed.starts_with("```") {
            in_fence = !in_fence;
        }
        let (is_quote, text) = match quoted {
            Some(text) => (true, text),
            None => (false, line),
        };
        match segments.last_mut() {
            Some((last_is_quote, buffer)) if *last_is_quote == is_quote => {
                buffer.push('\n');
                buffer.push_str(text);
            }
            _ => segments.push((is_quote, text.to_owned())),
        }
    }
    segments
}

fn split_unquoted(markdown: &str) -> Vec<Block> {
    let closed;
    // While Claude is still writing a code block its fence isn't closed yet; close it so the
    // code already shows as code.
    let markdown = if has_open_fence(markdown) {
        closed = format!("{markdown}\n```");
        closed.as_str()
    } else {
        markdown
    };
    let lines = match markdown_parser::parse_markdown_with_gfm_tables(markdown) {
        Ok(text) => text.lines,
        Err(_) => [FormattedTextLine::Line(vec![
            FormattedTextFragment::plain_text(markdown),
        ])]
        .into(),
    };
    let mut blocks = Vec::new();
    let mut prose: Vec<FormattedTextLine> = Vec::new();
    let flush = |prose: &mut Vec<FormattedTextLine>, blocks: &mut Vec<Block>| {
        let text = FormattedText::new_trimmed(std::mem::take(prose));
        if text.lines.iter().any(|line| !is_blank(line)) {
            blocks.push(Block::Prose(Arc::new(text), Vec::new()));
        }
    };
    for line in lines {
        match line {
            FormattedTextLine::CodeBlock(code) => {
                flush(&mut prose, &mut blocks);
                blocks.push(Block::Code {
                    language: code.lang.trim().to_owned(),
                    code: code.code.trim_end_matches('\n').to_owned(),
                });
            }
            FormattedTextLine::Table(table) => {
                flush(&mut prose, &mut blocks);
                blocks.push(Block::Table(table));
            }
            line @ (FormattedTextLine::Heading(_)
            | FormattedTextLine::Line(_)
            | FormattedTextLine::OrderedList(_)
            | FormattedTextLine::UnorderedList(_)
            | FormattedTextLine::TaskList(_)
            | FormattedTextLine::LineBreak
            | FormattedTextLine::HorizontalRule
            | FormattedTextLine::Embedded(_)
            | FormattedTextLine::Image(_)) => prose.push(line),
        }
    }
    flush(&mut prose, &mut blocks);
    blocks
}

fn has_open_fence(markdown: &str) -> bool {
    markdown
        .lines()
        .filter(|line| line.trim_start().starts_with("```"))
        .count()
        % 2
        == 1
}

fn is_blank(line: &FormattedTextLine) -> bool {
    matches!(line, FormattedTextLine::LineBreak) || line.is_empty_line()
}

/// Plain text of a table cell.
pub(super) fn inline_text(inline: &FormattedTextInline) -> String {
    inline
        .iter()
        .map(|fragment| fragment.text.as_str())
        .collect()
}

/// Relative widths of a table's columns, from the longest text in each, so short columns like
/// numbers don't take as much room as descriptions.
pub(super) fn column_weights(table: &FormattedTable) -> Vec<f32> {
    const MIN_WEIGHT: f32 = 4.;
    const MAX_WEIGHT: f32 = 36.;
    (0..table.headers.len())
        .map(|column| {
            let longest = std::iter::once(&table.headers[column])
                .chain(table.rows.iter().filter_map(|row| row.get(column)))
                .map(|cell| inline_text(cell).chars().count())
                .max()
                .unwrap_or_default();
            (longest as f32).clamp(MIN_WEIGHT, MAX_WEIGHT)
        })
        .collect()
}

/// A display name for a fence's language tag, like "rust" or "TypeScript"; empty tags show as
/// "texto".
pub(super) fn language_label(language: &str) -> String {
    let language = language.split_whitespace().next().unwrap_or_default();
    match language.to_ascii_lowercase().as_str() {
        "" | "text" | "txt" | "plain" | "plaintext" | "warp-runnable-command" => "texto".to_owned(),
        "sh" | "shell" | "zsh" => "bash".to_owned(),
        "js" => "javascript".to_owned(),
        "ts" => "typescript".to_owned(),
        "py" => "python".to_owned(),
        "rs" => "rust".to_owned(),
        "md" => "markdown".to_owned(),
        _ => language.to_owned(),
    }
}
