use std::collections::HashMap;
use std::ops::{Neg, RangeInclusive};

use pathfinder_color::ColorU;
use warpui::fonts::{FamilyId, Properties, Weight};
use warpui::geometry::rect::RectF;
use warpui::geometry::vector::{Vector2F, vec2f};
use warpui::{AppContext, Element, EntityId, PaintContext};

use super::model::SecretHandle;
use super::model::ansi::{CursorShape, CursorStyle};
use super::model::grid::{Dimensions as _, RespectDisplayedOutput};
use super::model::image_map::StoredImageMetadata;
use crate::settings::EnforceMinimumContrast;
use crate::terminal::grid_renderer::{CellGlyphCache, render_cursor, render_grid};
use crate::terminal::model::ObfuscateSecrets;
use crate::terminal::model::blockgrid::{BlockGrid, CursorDisplayPoint};
use crate::terminal::model::grid::grid_handler::Link;
use crate::terminal::model::index::Point;
use crate::terminal::prompt_card::{PromptCardStyle, find_prompt_rules, paint_prompt_card};
use crate::terminal::{SizeInfo, color};
use crate::themes::theme::WarpTheme;

pub struct GridRenderParams {
    pub warp_theme: WarpTheme,
    pub font_family: FamilyId,
    pub font_size: f32,
    pub font_weight: Weight,
    pub line_height_ratio: f32,
    pub enforce_minimum_contrast: EnforceMinimumContrast,
    pub obfuscate_secrets: ObfuscateSecrets,
    pub size_info: SizeInfo,
    pub cell_size: Vector2F,
    pub use_ligature_rendering: bool,
    /// When true, suppresses cursor rendering for CLI agents when rich input is open. For agents that draw their own cursor (SHOW_CURSOR off),
    /// the cursor cell is skipped. For agents that let Warp draw the cursor
    /// (SHOW_CURSOR on), the `draw_cursor` call and cursor contrast colouring
    /// are suppressed instead.
    pub hide_cursor_cell: bool,
    /// Draws a thin bar where the program asked for a block cursor.
    pub beam_cursor: bool,
    /// Replaces the rules around Claude Code's prompt with a rounded card.
    pub prompt_card: Option<PromptCardStyle>,
}

impl GridRenderParams {
    /// The shape to draw for a cursor the program asked to show as `shape`.
    pub fn cursor_shape(&self, shape: CursorShape) -> CursorShape {
        match shape {
            CursorShape::Block | CursorShape::HollowBlock if self.beam_cursor => CursorShape::Beam,
            CursorShape::Block
            | CursorShape::HollowBlock
            | CursorShape::Underline
            | CursorShape::Beam
            | CursorShape::Hidden => shape,
        }
    }
}

pub struct BlockGridParams {
    pub grid_render_params: GridRenderParams,
    pub colors: color::List,
    pub override_colors: color::OverrideList,
    pub bounds: RectF,
}
pub trait BlockGridRenderer {
    #[allow(clippy::too_many_arguments)]
    fn draw<'a>(
        &self,
        grid_origin: Vector2F,
        origin: Vector2F,
        glyphs: &mut CellGlyphCache,
        alpha: u8,
        highlighted_url: Option<&Link>,
        link_tool_tip: Option<&Link>,
        hovered_secret: Option<SecretHandle>,
        ordered_matches: Option<impl Iterator<Item = &'a RangeInclusive<Point>>>,
        focused_match_range: Option<&RangeInclusive<Point>>,
        properties: Properties,
        block_grid_params: &BlockGridParams,
        visible_cursor_shape: Option<CursorShape>,
        image_metadata: &HashMap<u32, StoredImageMetadata>,
        ctx: &mut PaintContext,
        app: &AppContext,
    );

    fn draw_with_default_params(
        &self,
        grid_origin: Vector2F,
        origin: Vector2F,
        block_grid_params: &BlockGridParams,
        ctx: &mut PaintContext,
        app: &AppContext,
    );

    #[allow(clippy::too_many_arguments)]
    fn draw_cursor(
        &self,
        grid_origin: Vector2F,
        grid_render_params: &GridRenderParams,
        ctx: &mut PaintContext,
        terminal_view_id: EntityId,
        cursor_hint_text: Option<&mut Box<dyn Element>>,
        color: ColorU,
        app: &AppContext,
    );
}

impl BlockGridRenderer for BlockGrid {
    #[allow(clippy::too_many_arguments)]
    fn draw<'a>(
        &self,
        grid_origin: Vector2F,
        origin: Vector2F,
        glyphs: &mut CellGlyphCache,
        alpha: u8,
        highlighted_url: Option<&Link>,
        link_tool_tip: Option<&Link>,
        hovered_secret: Option<SecretHandle>,
        ordered_matches: Option<impl Iterator<Item = &'a RangeInclusive<Point>>>,
        focused_match_range: Option<&RangeInclusive<Point>>,
        properties: Properties,
        block_grid_params: &BlockGridParams,
        visible_cursor_shape: Option<CursorShape>,
        image_metadata: &HashMap<u32, StoredImageMetadata>,
        ctx: &mut PaintContext,
        app: &AppContext,
    ) {
        let start_row = hidden_rows_above(
            grid_origin,
            origin,
            block_grid_params.grid_render_params.cell_size,
        );
        let hidden_rows_below = hidden_rows_below(
            grid_origin,
            origin,
            block_grid_params.grid_render_params.cell_size,
            block_grid_params.bounds,
            self.len_displayed(),
        );
        let end_row = self.len_displayed().saturating_sub(hidden_rows_below);

        let grid = self.grid_handler();

        let grid_render_params = &block_grid_params.grid_render_params;
        let prompt_rules = grid_render_params
            .prompt_card
            .filter(|_| grid.displayed_output_rows().is_none())
            .and_then(|style| {
                let rules = find_prompt_rules(grid, 0..self.len_displayed())?;
                Some((style, rules))
            });
        let hidden_rows = prompt_rules.map(|(style, rules)| {
            paint_prompt_card(
                rules,
                style,
                grid_origin,
                grid_render_params.size_info.padding_x_px().as_f32(),
                grid_render_params.cell_size,
                grid.columns(),
                block_grid_params.bounds,
                ctx,
            );
            rules.rows()
        });

        let obfuscate_secrets = block_grid_params
            .grid_render_params
            .obfuscate_secrets
            .and(&grid.get_secret_obfuscation());
        render_grid(
            grid,
            start_row,
            end_row,
            &block_grid_params.colors,
            &block_grid_params.override_colors,
            &block_grid_params.grid_render_params.warp_theme,
            properties,
            block_grid_params.grid_render_params.font_family,
            block_grid_params.grid_render_params.font_size,
            block_grid_params.grid_render_params.line_height_ratio,
            block_grid_params.grid_render_params.cell_size,
            block_grid_params
                .grid_render_params
                .size_info
                .padding_x_px(),
            grid_origin,
            glyphs,
            alpha,
            highlighted_url,
            link_tool_tip,
            ordered_matches,
            focused_match_range,
            block_grid_params
                .grid_render_params
                .enforce_minimum_contrast,
            obfuscate_secrets,
            hovered_secret,
            block_grid_params.grid_render_params.use_ligature_rendering,
            visible_cursor_shape
                .map(|shape| block_grid_params.grid_render_params.cursor_shape(shape)),
            RespectDisplayedOutput::Yes,
            image_metadata,
            None,
            block_grid_params.grid_render_params.hide_cursor_cell,
            hidden_rows.as_ref().map_or(&[], |rows| rows.as_slice()),
            ctx,
            app,
        );
    }

    fn draw_with_default_params(
        &self,
        grid_origin: Vector2F,
        origin: Vector2F,
        block_grid_params: &BlockGridParams,
        ctx: &mut PaintContext,
        app: &AppContext,
    ) {
        let mut glyphs = CellGlyphCache::default();
        self.draw(
            grid_origin,
            origin,
            &mut glyphs,
            255,
            None,
            None,
            None,
            None::<std::iter::Empty<&RangeInclusive<Point>>>,
            None,
            Properties::default(),
            block_grid_params,
            None,
            &HashMap::new(),
            ctx,
            app,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_cursor(
        &self,
        grid_origin: Vector2F,
        grid_render_params: &GridRenderParams,
        ctx: &mut PaintContext,
        terminal_view_id: EntityId,
        cursor_hint_text: Option<&mut Box<dyn Element>>,
        color: ColorU,
        app: &AppContext,
    ) {
        let cursor_style = self.cursor_style();
        let (cursor_display_point, is_cursor_on_wide_char, cursor_style, cursor_hint_text) =
            match self.cursor_display_point() {
                Some(CursorDisplayPoint::Visible(cursor_display_point)) => (
                    cursor_display_point,
                    self.grid_handler().is_cursor_on_wide_char(),
                    cursor_style,
                    cursor_hint_text,
                ),
                Some(CursorDisplayPoint::HiddenCache(cursor_display_point)) => (
                    cursor_display_point,
                    false,
                    CursorStyle {
                        shape: CursorShape::Hidden,
                        ..cursor_style
                    },
                    None,
                ),
                None => return,
            };

        render_cursor(
            grid_render_params,
            cursor_display_point,
            is_cursor_on_wide_char,
            cursor_style,
            grid_render_params.size_info.padding_x_px(),
            grid_origin,
            color,
            ctx,
            terminal_view_id,
            cursor_hint_text,
            app,
        )
    }
}

fn lines_to_pixels(line: usize, cell_size: Vector2F) -> Vector2F {
    vec2f(0., line as f32 * cell_size.y())
}

fn pixels_to_lines(coord: Vector2F, cell_size: Vector2F) -> usize {
    (coord.y() / cell_size.y()) as usize
}

/// Computes the number of rows that are not visible in the current grid above the viewport.
fn hidden_rows_above(grid_origin: Vector2F, origin: Vector2F, cell_size: Vector2F) -> usize {
    ((grid_origin.y() - origin.y()) / cell_size.y())
        .neg()
        .max(0.)
        .floor() as usize
}

/// Computes the number of rows that are not visible in the grid below the viewport.
fn hidden_rows_below(
    grid_origin: Vector2F,
    origin: Vector2F,
    cell_size: Vector2F,
    bounds: RectF,
    grid_len: usize,
) -> usize {
    let grid_end = grid_origin + lines_to_pixels(grid_len, cell_size);
    let lower_left = origin.y() + bounds.height();
    pixels_to_lines(grid_end - lower_left, cell_size)
}
