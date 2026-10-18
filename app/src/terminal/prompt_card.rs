//! Claude Code frames its prompt with two full-width rules of `─`. Panes running Claude hide
//! those rules and paint a rounded card behind the prompt instead, which also wraps the status
//! line under it (mode, worktree), like the controls row of a chat app's composer.

use std::ops::Range;

use pathfinder_color::ColorU;
use warpui::PaintContext;
use warpui::elements::{Border, CornerRadius, Fill, Radius};
use warpui::geometry::rect::RectF;
use warpui::geometry::vector::{Vector2F, vec2f};

use super::model::grid::Dimensions as _;
use super::model::grid::grid_handler::GridHandler;

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
const CORNER_RADIUS: f32 = 12.;
const BORDER_WIDTH: f32 = 1.;
/// How far the card reaches past the text on each side, at most.
const MAX_HORIZONTAL_INSET: f32 = 10.;

/// Colors of the card drawn behind the prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptCardStyle {
    pub fill: ColorU,
    pub border: ColorU,
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
    pub fn rows(&self) -> [usize; 2] {
        [self.top, self.bottom]
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

/// Paints the card from the middle of the top rule down to the status lines (or the middle of the
/// bottom rule, without them). Rects paint below glyphs, so the text stays on top of it.
#[allow(clippy::too_many_arguments)]
pub fn paint_prompt_card(
    rules: PromptRules,
    style: PromptCardStyle,
    grid_origin: Vector2F,
    padding_x: f32,
    cell_size: Vector2F,
    columns: usize,
    bounds: RectF,
    ctx: &mut PaintContext,
) {
    let inset = (padding_x - 4.).clamp(2., MAX_HORIZONTAL_INSET);
    let origin_y = grid_origin.y().round();
    let top = origin_y + (rules.top as f32 + 0.5) * cell_size.y();
    let bottom = if rules.footer_rows > 0 {
        let end_row = (rules.bottom + 1 + rules.footer_rows) as f32;
        origin_y + (end_row + FOOTER_PADDING_ROWS) * cell_size.y()
    } else {
        origin_y + (rules.bottom as f32 + 0.5) * cell_size.y()
    };
    let left = (grid_origin.x() + padding_x - inset).max(bounds.min_x() + 1.);
    let right = (grid_origin.x() + padding_x + columns as f32 * cell_size.x() + inset)
        .min(bounds.max_x() - 1.);
    if right <= left || bottom <= top {
        return;
    }

    ctx.scene
        .draw_rect_without_hit_recording(RectF::new(
            vec2f(left, top),
            vec2f(right - left, bottom - top),
        ))
        .with_background(Fill::Solid(style.fill))
        .with_border(Border::all(BORDER_WIDTH).with_border_color(style.border))
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(CORNER_RADIUS)));
}

#[cfg(test)]
#[path = "prompt_card_tests.rs"]
mod tests;
