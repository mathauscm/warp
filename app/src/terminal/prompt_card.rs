//! Claude Code frames its prompt with two full-width rules of `─`. Panes running Claude hide
//! those rules and paint a rounded card behind the prompt instead, which also wraps the status
//! line under it (mode, worktree), like the controls row of a chat app's composer.

use std::ops::Range;

use pathfinder_color::ColorU;
use warpui::PaintContext;
use warpui::elements::{Border, CornerRadius, Fill, Radius};
use warpui::geometry::rect::RectF;
use warpui::geometry::vector::{Vector2F, vec2f};

use super::grid_renderer::HiddenCells;
use super::model::grid::Dimensions as _;
use super::model::grid::grid_handler::GridHandler;
use super::model::index::Point;

const RULE_CHAR: char = '─';
/// The share of the grid's width a rule has to span, so short dividers inside the output don't
/// count.
const MIN_RULE_WIDTH_RATIO: f32 = 0.6;
/// The prompt sits at the bottom of Claude's output; below it there are only the status line and
/// menus such as slash command suggestions.
const MAX_ROWS_BELOW_PROMPT: usize = 24;
/// The prompt box grows with the text typed into it.
const MAX_PROMPT_ROWS: usize = 40;
/// Status lines under the prompt that the card wraps; taller content there is a menu.
const MAX_FOOTER_ROWS: usize = 3;
/// Space between the last status line and the card's bottom edge, in rows.
const FOOTER_PADDING_ROWS: f32 = 0.35;
const CORNER_RADIUS: f32 = 14.;
const BORDER_WIDTH: f32 = 1.;
/// Space between the card and the pane's sides.
const SIDE_MARGIN: f32 = 12.;
/// Space between the controls row's right end and the card's border.
const CONTROLS_RIGHT_INSET: f32 = 10.;
/// Space the composer controls need from the middle of the status line down to the card's bottom
/// edge: half the send button plus a gap.
const CONTROLS_BOTTOM_ROOM: f32 = 16.;
/// Claude's marker at the start of the prompt, hidden inside the card.
const PROMPT_SYMBOL: char = '❯';
/// The column where the prompt text starts, after the marker and a space.
const PROMPT_TEXT_COLUMN: usize = 2;

/// Colors of the card drawn behind the prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptCardStyle {
    pub fill: ColorU,
    pub border: ColorU,
    /// Set when Warp draws its own controls in the card: Claude's status line is hidden, and the
    /// card's rects are cached for the elements that float over it.
    pub controls: Option<PromptCardControls>,
    /// Set while the whole prompt is selected (⌘A): its text gets this highlight.
    pub selection: Option<ColorU>,
}

/// Save-position ids of the card's rects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptCardControls {
    pub card_position_id: String,
    /// The row of Claude's status line, which Warp's controls replace.
    pub controls_position_id: String,
}

/// The grid rows of the rules above and below the prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptRules {
    pub top: usize,
    pub bottom: usize,
    /// Status lines right under the bottom rule.
    pub footer_rows: usize,
}

impl PromptRules {
    /// The last status line, where the card's controls go.
    pub fn status_row(&self) -> Option<usize> {
        (self.footer_rows > 0).then(|| self.bottom + self.footer_rows)
    }
}

/// Whether a row holding `chars` is one of the rules framing the prompt.
fn is_rule_row(chars: impl IntoIterator<Item = char>, columns: usize) -> bool {
    let mut rule_chars = 0;
    for c in chars {
        match c {
            RULE_CHAR => rule_chars += 1,
            ' ' | '\0' => {}
            _ => return false,
        }
    }
    columns > 0 && rule_chars as f32 >= columns as f32 * MIN_RULE_WIDTH_RATIO
}

/// Finds the last pair of rules among `rows`, given whether each row is a rule or blank.
fn find_rules(
    rows: Range<usize>,
    is_rule: impl Fn(usize) -> bool,
    is_blank: impl Fn(usize) -> bool,
) -> Option<PromptRules> {
    let search_start = rows
        .start
        .max(rows.end.saturating_sub(MAX_ROWS_BELOW_PROMPT));
    let bottom = (search_start..rows.end).rev().find(|row| is_rule(*row))?;
    let top_search_start = rows.start.max(bottom.saturating_sub(MAX_PROMPT_ROWS));
    let top = (top_search_start..bottom.saturating_sub(1))
        .rev()
        .find(|row| is_rule(*row))?;
    let footer_rows = (bottom + 1..rows.end)
        .take_while(|row| !is_blank(*row))
        .count();
    // A taller block under the prompt is a menu, which shouldn't sit inside the card.
    let footer_rows = if footer_rows <= MAX_FOOTER_ROWS {
        footer_rows
    } else {
        0
    };
    Some(PromptRules {
        top,
        bottom,
        footer_rows,
    })
}

/// Finds the rules around Claude's prompt among the grid rows in `rows`.
pub fn find_prompt_rules(grid: &GridHandler, rows: Range<usize>) -> Option<PromptRules> {
    let columns = grid.columns();
    let row_chars = |row_idx: usize| {
        grid.row(row_idx)
            .map(|row| (0..columns).map(|col| row[col].c).collect::<Vec<_>>())
    };
    find_rules(
        rows,
        |row_idx| row_chars(row_idx).is_some_and(|chars| is_rule_row(chars, columns)),
        |row_idx| {
            row_chars(row_idx).is_none_or(|chars| chars.iter().all(|c| matches!(c, ' ' | '\0')))
        },
    )
}

/// What a painted prompt card hides from the grid: the two rules, Claude's `❯` marker and, when
/// Warp draws its own controls, Claude's status line.
pub struct PromptCardMask {
    rows: Vec<usize>,
    prompt_symbol: Option<Point>,
}

impl PromptCardMask {
    pub fn hidden_cells(&self) -> HiddenCells<'_> {
        HiddenCells {
            rows: &self.rows,
            cells: self.prompt_symbol.as_slice(),
        }
    }
}

/// Where the card goes in the pane, and the grid it's drawn over.
pub struct PromptCardFrame {
    pub grid_origin: Vector2F,
    pub padding_x: f32,
    pub cell_size: Vector2F,
    /// The pane's bounds; the card keeps a margin from their sides and stays within them.
    pub bounds: RectF,
}

/// Finds Claude's prompt among the grid rows in `rows` and paints the card behind it, from the
/// middle of the top rule down to the status lines (or the bottom rule, without them). Rects paint
/// below glyphs, so the text stays on top.
pub fn paint_prompt_card(
    grid: &GridHandler,
    rows: Range<usize>,
    style: &PromptCardStyle,
    frame: &PromptCardFrame,
    ctx: &mut PaintContext,
) -> Option<PromptCardMask> {
    let rules = find_prompt_rules(grid, rows)?;
    let cell_size = frame.cell_size;
    let origin_y = frame.grid_origin.y().round();
    let row_top = |row: usize| origin_y + row as f32 * cell_size.y();
    let top = row_top(rules.top) + cell_size.y() / 2.;
    let bottom = match rules.status_row() {
        Some(status_row) => row_top(status_row + 1) + FOOTER_PADDING_ROWS * cell_size.y(),
        None => row_top(rules.bottom + 1),
    }
    // In a short pane the status line is the grid's last row; keep the card's edge visible.
    .min(frame.bounds.max_y() - BORDER_WIDTH);
    let left = frame.bounds.min_x() + SIDE_MARGIN;
    let right = frame.bounds.max_x() - SIDE_MARGIN;
    if right <= left || bottom <= top {
        return None;
    }
    let card = RectF::new(vec2f(left, top), vec2f(right - left, bottom - top));
    ctx.scene
        .draw_rect_without_hit_recording(card)
        .with_background(Fill::Solid(style.fill))
        .with_border(Border::all(BORDER_WIDTH).with_border_color(style.border))
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(CORNER_RADIUS)));

    let mut hidden_rows = vec![rules.top, rules.bottom];
    if let Some(controls) = &style.controls {
        ctx.position_cache
            .cache_position_indefinitely(controls.card_position_id.clone(), card);
        if let Some(status_row) = rules.status_row() {
            hidden_rows.push(status_row);
            let text_left =
                frame.grid_origin.x() + frame.padding_x + PROMPT_TEXT_COLUMN as f32 * cell_size.x();
            let controls_right = right - CONTROLS_RIGHT_INSET;
            // When the card's edge is pulled up to the pane's, the controls move up with it
            // rather than spill past it.
            let status_middle = row_top(status_row) + cell_size.y() / 2.;
            let lift = (status_middle + CONTROLS_BOTTOM_ROOM - bottom).max(0.);
            ctx.position_cache.cache_position_indefinitely(
                controls.controls_position_id.clone(),
                RectF::new(
                    vec2f(text_left, row_top(status_row) - lift),
                    vec2f((controls_right - text_left).max(0.), cell_size.y()),
                ),
            );
        }
    }

    if let Some(selection) = style.selection {
        let text_left = frame.grid_origin.x() + frame.padding_x;
        for row_index in rules.top + 1..rules.bottom {
            let Some(row) = grid.row(row_index) else {
                continue;
            };
            let Some(last) = (PROMPT_TEXT_COLUMN..grid.columns())
                .rev()
                .find(|&col| !matches!(row[col].c, ' ' | '\0'))
            else {
                continue;
            };
            let start_x = text_left + PROMPT_TEXT_COLUMN as f32 * cell_size.x();
            let width = (last + 1 - PROMPT_TEXT_COLUMN) as f32 * cell_size.x();
            ctx.scene
                .draw_rect_without_hit_recording(RectF::new(
                    vec2f(start_x, row_top(row_index)),
                    vec2f(width, cell_size.y()),
                ))
                .with_background(Fill::Solid(selection));
        }
    }

    let first_prompt_row = rules.top + 1;
    let prompt_symbol = grid
        .row(first_prompt_row)
        .filter(|row| grid.columns() > 0 && row[0].c == PROMPT_SYMBOL)
        .map(|_| Point::new(first_prompt_row, 0));
    Some(PromptCardMask {
        rows: hidden_rows,
        prompt_symbol,
    })
}

#[cfg(test)]
#[path = "prompt_card_tests.rs"]
mod tests;
