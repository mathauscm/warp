//! Every color, size and radius of the native chat and the artifacts, as tokens. With the Claude
//! theme setting on "Warp" the palette is derived from the current Warp theme; otherwise it is one of
//! the Claude desktop app's palettes. The accent comes from the chat accent setting, and the sizes
//! scale from the chat font size setting. `picolas_design.toml` in the data dir overrides any token on top of all that (`[dark]` applies
//! to every dark palette, `[light]` to every light one) and is re-read whenever it changes, so the
//! look can be tuned while the app runs.
//!
//! ```toml
//! [dark]
//! accent = "#4c8df6"
//! [sizes]
//! font_size = 15.0
//! column_max_width = 780.0
//! ```

use std::path::{Path, PathBuf};
use std::time::Duration;

use notify_debouncer_full::notify::{RecursiveMode, WatchFilter};
use pathfinder_color::ColorU;
use serde::Deserialize;
use settings::Setting as _;
use warp_core::ui::theme::{ColorScheme, WarpTheme};
use warpui::fonts::FamilyId;
use warpui::platform::SystemTheme;
use warpui::{AppContext, Entity, ModelContext, ModelHandle, SingletonEntity};
use watcher::{BulkFilesystemWatcher, BulkFilesystemWatcherEvent};

use crate::appearance::{Appearance, AppearanceEvent};
use crate::settings::{
    ClaudeChatAccent, ClaudeChatFont, ClaudeChatFontSize, ClaudeTheme, ThemeSettings,
};

const FILE_NAME: &str = "picolas_design.toml";
const WATCH_DEBOUNCE: Duration = Duration::from_millis(150);

/// A color written as `"#rrggbb"` or `"#rrggbbaa"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hex(pub ColorU);

impl Hex {
    const fn rgb(hex: u32) -> Self {
        Self(ColorU {
            r: (hex >> 16) as u8,
            g: (hex >> 8) as u8,
            b: hex as u8,
            a: 0xff,
        })
    }

    const fn rgba(hex: u32) -> Self {
        Self(ColorU {
            r: (hex >> 24) as u8,
            g: (hex >> 16) as u8,
            b: (hex >> 8) as u8,
            a: hex as u8,
        })
    }

    fn parse(text: &str) -> Option<Self> {
        let digits = text.trim().strip_prefix('#')?;
        let value = u32::from_str_radix(digits, 16).ok()?;
        match digits.len() {
            6 => Some(Self::rgb(value)),
            8 => Some(Self::rgba(value)),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for Hex {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Hex::parse(&text).ok_or_else(|| serde::de::Error::custom(format!("invalid color {text:?}")))
    }
}

/// The colors of one appearance (dark or light).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Palette {
    /// The chat page behind the messages.
    pub page: Hex,
    /// The side panel and other secondary columns.
    pub sidebar: Hex,
    /// Raised surfaces: the composer, cards, menus, code blocks.
    pub surface: Hex,
    pub surface_hover: Hex,
    /// A surface inside a surface, like a selected option.
    pub surface_strong: Hex,
    pub border: Hex,
    pub border_strong: Hex,
    pub text: Hex,
    pub text_muted: Hex,
    pub text_faint: Hex,
    pub accent: Hex,
    /// Text on top of `accent`.
    pub on_accent: Hex,
    pub user_bubble: Hex,
    pub code_background: Hex,
    pub inline_code_background: Hex,
    pub link: Hex,
    pub success: Hex,
    pub warning: Hex,
    pub error: Hex,
    pub info: Hex,
    pub diff_added: Hex,
    pub diff_removed: Hex,
    pub selection: Hex,
    pub shadow: Hex,
    /// Behind lines a diff adds or removes.
    pub diff_added_background: Hex,
    pub diff_removed_background: Hex,
    /// The soft card behind an error in the transcript, and its outline.
    pub error_background: Hex,
    pub error_border: Hex,
    /// The bright band that sweeps across live labels like "Trabalhando há 12s".
    pub shimmer: Hex,
    /// Syntax colors of code blocks.
    pub syntax_keyword: Hex,
    pub syntax_function: Hex,
    pub syntax_string: Hex,
    pub syntax_type: Hex,
    pub syntax_number: Hex,
    pub syntax_comment: Hex,
    pub syntax_property: Hex,
    pub syntax_tag: Hex,
    /// The artifact kinds' colors: the gallery's "new" tiles, card icons and placeholders.
    pub kind_document: Hex,
    pub kind_presentation: Hex,
    pub kind_design: Hex,
    pub kind_dashboard: Hex,
    pub kind_motion: Hex,
    pub kind_other: Hex,
}

impl Palette {
    fn dark() -> Self {
        Self {
            page: Hex::rgb(0x262624),
            sidebar: Hex::rgb(0x1f1e1d),
            surface: Hex::rgb(0x30302e),
            surface_hover: Hex::rgb(0x383835),
            surface_strong: Hex::rgb(0x41403c),
            border: Hex::rgb(0x3a3a37),
            border_strong: Hex::rgb(0x4a4a46),
            text: Hex::rgb(0xfaf9f5),
            text_muted: Hex::rgb(0xa6a39b),
            text_faint: Hex::rgb(0x77756e),
            accent: Hex::rgb(0x4c8df6),
            on_accent: Hex::rgb(0xffffff),
            user_bubble: Hex::rgb(0x3a3936),
            code_background: Hex::rgb(0x1f1e1d),
            inline_code_background: Hex::rgba(0xfaf9f512),
            link: Hex::rgb(0x9cc4ee),
            success: Hex::rgb(0x8cbf8e),
            warning: Hex::rgb(0xe4b76a),
            error: Hex::rgb(0xe5736f),
            info: Hex::rgb(0x74abe2),
            diff_added: Hex::rgb(0x5fb06a),
            diff_removed: Hex::rgb(0xe5736f),
            selection: Hex::rgba(0x4c8df640),
            shadow: Hex::rgba(0x00000059),
            diff_added_background: Hex::rgba(0x5fb06a24),
            diff_removed_background: Hex::rgba(0xe5736f24),
            error_background: Hex::rgba(0xe5736f14),
            error_border: Hex::rgba(0xe5736f3d),
            shimmer: Hex::rgb(0xfaf9f5),
            syntax_keyword: Hex::rgb(0xe88fb4),
            syntax_function: Hex::rgb(0x8fb8e8),
            syntax_string: Hex::rgb(0xa9c98b),
            syntax_type: Hex::rgb(0xe6c17c),
            syntax_number: Hex::rgb(0xcfa3e6),
            syntax_comment: Hex::rgb(0x85827a),
            syntax_property: Hex::rgb(0x92cbc2),
            syntax_tag: Hex::rgb(0xe88fb4),
            kind_document: Hex::rgb(0x3f74c9),
            kind_presentation: Hex::rgb(0xd25a3c),
            kind_design: Hex::rgb(0x7d5bd6),
            kind_dashboard: Hex::rgb(0xb83a52),
            kind_motion: Hex::rgb(0x1f9a8f),
            kind_other: Hex::rgb(0x6f6d66),
        }
    }

    fn light() -> Self {
        Self {
            page: Hex::rgb(0xfaf9f5),
            sidebar: Hex::rgb(0xf3f1ea),
            surface: Hex::rgb(0xffffff),
            surface_hover: Hex::rgb(0xf3f1ea),
            surface_strong: Hex::rgb(0xe8e6dc),
            border: Hex::rgb(0xe5e3d9),
            border_strong: Hex::rgb(0xd1cfc5),
            text: Hex::rgb(0x141413),
            text_muted: Hex::rgb(0x73726c),
            text_faint: Hex::rgb(0x9c9a92),
            accent: Hex::rgb(0x2563eb),
            on_accent: Hex::rgb(0xffffff),
            user_bubble: Hex::rgb(0xf0eee6),
            code_background: Hex::rgb(0xf5f4ef),
            inline_code_background: Hex::rgba(0x1414130c),
            link: Hex::rgb(0x2c64a8),
            success: Hex::rgb(0x3d7a3f),
            warning: Hex::rgb(0x8a6100),
            error: Hex::rgb(0xb5332e),
            info: Hex::rgb(0x2c64a8),
            diff_added: Hex::rgb(0x3d7a3f),
            diff_removed: Hex::rgb(0xb5332e),
            selection: Hex::rgba(0x2563eb2e),
            shadow: Hex::rgba(0x0000001f),
            diff_added_background: Hex::rgba(0x3d7a3f1c),
            diff_removed_background: Hex::rgba(0xb5332e1a),
            error_background: Hex::rgba(0xb5332e0d),
            error_border: Hex::rgba(0xb5332e33),
            shimmer: Hex::rgb(0x141413),
            syntax_keyword: Hex::rgb(0xb0306a),
            syntax_function: Hex::rgb(0x2c5f9e),
            syntax_string: Hex::rgb(0x3d7a3f),
            syntax_type: Hex::rgb(0x8a6100),
            syntax_number: Hex::rgb(0x8a3fa3),
            syntax_comment: Hex::rgb(0x9c9a92),
            syntax_property: Hex::rgb(0x23766d),
            syntax_tag: Hex::rgb(0xb0306a),
            kind_document: Hex::rgb(0x2f6bd0),
            kind_presentation: Hex::rgb(0xd4542f),
            kind_design: Hex::rgb(0x7a4fd8),
            kind_dashboard: Hex::rgb(0xb52e48),
            kind_motion: Hex::rgb(0x13897f),
            kind_other: Hex::rgb(0x8a877e),
        }
    }
}

impl Palette {
    /// A palette in the colors of `theme`: its background for the page, blends of its background
    /// and foreground for surfaces, borders and secondary text. Status, syntax and artifact-kind
    /// colors come from the Claude palette of the same lightness, which is tuned for legibility.
    pub fn from_warp_theme(theme: &WarpTheme) -> Self {
        let background = theme.background().into_solid();
        let foreground = theme.foreground().into_solid();
        let tint = |amount| Hex(mix(background, foreground, amount));
        let base = if theme_is_dark(theme) {
            Self::dark()
        } else {
            Self::light()
        };
        Self {
            page: tint(0.),
            sidebar: tint(0.03),
            surface: tint(0.06),
            surface_hover: tint(0.1),
            surface_strong: tint(0.15),
            border: tint(0.12),
            border_strong: tint(0.2),
            text: tint(1.),
            text_muted: tint(0.62),
            text_faint: tint(0.42),
            user_bubble: tint(0.09),
            code_background: tint(0.035),
            inline_code_background: tint(0.1),
            shimmer: tint(1.),
            ..base
        }
    }

    /// Sets the accent and the tokens that follow it: the text on it and the selection.
    pub fn with_accent(self, accent: ColorU) -> Self {
        let accent = ColorU { a: 0xff, ..accent };
        let selection_alpha = if is_dark_color(self.page.0) {
            0x40
        } else {
            0x2e
        };
        Self {
            accent: Hex(accent),
            on_accent: Hex(if is_dark_color(accent) {
                ColorU::white()
            } else {
                ColorU::new(0x14, 0x14, 0x13, 0xff)
            }),
            selection: Hex(ColorU {
                a: selection_alpha,
                ..accent
            }),
            ..self
        }
    }
}

/// The accent color `choice` stands for on a dark or light palette.
pub fn accent_color(choice: ClaudeChatAccent, dark: bool, theme: &WarpTheme) -> ColorU {
    let (on_dark, on_light) = match choice {
        ClaudeChatAccent::Blue => (0x4c8df6, 0x2563eb),
        ClaudeChatAccent::Purple => (0x9b7cf4, 0x7c3aed),
        ClaudeChatAccent::Green => (0x3fae6e, 0x15803d),
        ClaudeChatAccent::Orange => (0xd97757, 0xc96442),
        ClaudeChatAccent::Pink => (0xe5609d, 0xdb2777),
        ClaudeChatAccent::Theme => return theme.accent().into_solid(),
    };
    Hex::rgb(if dark { on_dark } else { on_light }).0
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

/// Whether white text reads better on `color` than near-black text.
fn is_dark_color(color: ColorU) -> bool {
    let linear = |channel: u8| {
        let value = channel as f32 / 255.;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    let luminance = 0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b);
    // Where the contrast against white equals the contrast against near-black.
    luminance < 0.18
}

fn theme_is_dark(theme: &WarpTheme) -> bool {
    matches!(theme.inferred_color_scheme(), ColorScheme::LightOnDark)
}

/// Sizes in logical pixels.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Sizes {
    pub font_size: f32,
    pub line_height_ratio: f32,
    pub small_font_size: f32,
    pub tiny_font_size: f32,
    pub title_font_size: f32,
    pub code_font_size: f32,
    /// The widest the message column gets; it centers in wider panes.
    pub column_max_width: f32,
    pub column_padding: f32,
    pub message_spacing: f32,
    pub bubble_radius: f32,
    pub bubble_padding_x: f32,
    pub bubble_padding_y: f32,
    /// The user bubble's widest share of the column.
    pub bubble_max_width_ratio: f32,
    pub composer_radius: f32,
    pub composer_padding: f32,
    pub composer_min_height: f32,
    pub composer_max_height: f32,
    pub composer_bottom_margin: f32,
    pub card_radius: f32,
    pub chip_radius: f32,
    pub chip_height: f32,
    pub button_size: f32,
    pub icon_size: f32,
    pub small_icon_size: f32,
    pub tool_row_height: f32,
    pub work_group_max_height: f32,
    pub side_panel_width: f32,
    pub side_panel_padding: f32,
    pub header_height: f32,
    pub shadow_blur: f32,
    pub code_block_radius: f32,
    /// The tallest a tool's output or diff gets before it scrolls.
    pub tool_output_max_height: f32,
    /// Space above the first and below the last row of the transcript.
    pub transcript_padding: f32,
    pub scroll_button_size: f32,
    pub gallery_card_width: f32,
    pub gallery_card_height: f32,
    pub gallery_tile_size: f32,
    pub gallery_tile_height: f32,
    pub gallery_title_font_size: f32,
    /// The widest the gallery gets; it centers in wider panes.
    pub gallery_max_width: f32,
    pub gallery_padding: f32,
    /// Space between cards, and between tiles.
    pub gallery_gap: f32,
}

impl Default for Sizes {
    fn default() -> Self {
        Self {
            font_size: 14.5,
            line_height_ratio: 1.55,
            small_font_size: 12.5,
            tiny_font_size: 11.,
            title_font_size: 15.,
            code_font_size: 12.5,
            column_max_width: 720.,
            column_padding: 32.,
            message_spacing: 18.,
            bubble_radius: 16.,
            bubble_padding_x: 14.,
            bubble_padding_y: 9.,
            bubble_max_width_ratio: 0.8,
            composer_radius: 18.,
            composer_padding: 12.,
            composer_min_height: 56.,
            composer_max_height: 240.,
            composer_bottom_margin: 18.,
            card_radius: 12.,
            chip_radius: 8.,
            chip_height: 26.,
            button_size: 30.,
            icon_size: 16.,
            small_icon_size: 13.,
            tool_row_height: 26.,
            work_group_max_height: 300.,
            side_panel_width: 248.,
            side_panel_padding: 12.,
            header_height: 44.,
            shadow_blur: 18.,
            code_block_radius: 10.,
            tool_output_max_height: 280.,
            transcript_padding: 24.,
            scroll_button_size: 32.,
            gallery_card_width: 300.,
            gallery_card_height: 220.,
            gallery_tile_size: 120.,
            gallery_tile_height: 84.,
            gallery_title_font_size: 30.,
            gallery_max_width: 1180.,
            gallery_padding: 40.,
            gallery_gap: 18.,
        }
    }
}

impl Sizes {
    /// These sizes with the text at `font_size` and everything sized by the text (other text,
    /// icons, row heights, bubble padding) scaled along. Widths and radii keep their size.
    pub fn scaled_to(self, font_size: f32) -> Self {
        let factor = font_size / self.font_size;
        let scale = |value: f32| (value * factor * 2.).round() / 2.;
        Self {
            font_size,
            small_font_size: scale(self.small_font_size),
            tiny_font_size: scale(self.tiny_font_size),
            title_font_size: scale(self.title_font_size),
            code_font_size: scale(self.code_font_size),
            message_spacing: scale(self.message_spacing),
            bubble_padding_x: scale(self.bubble_padding_x),
            bubble_padding_y: scale(self.bubble_padding_y),
            composer_min_height: scale(self.composer_min_height),
            chip_height: scale(self.chip_height),
            button_size: scale(self.button_size),
            icon_size: scale(self.icon_size),
            small_icon_size: scale(self.small_icon_size),
            tool_row_height: scale(self.tool_row_height),
            ..self
        }
    }
}

/// The chat's body text size for `choice`, following `warp_font_size` (the terminal text size the
/// user set in Warp) when it doesn't name a size.
pub fn chat_font_size(choice: ClaudeChatFontSize, warp_font_size: f32) -> f32 {
    choice
        .points()
        .unwrap_or_else(|| warp_font_size.clamp(10., 24.))
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct DesignTokens {
    pub dark: Palette,
    pub light: Palette,
    pub sizes: Sizes,
}

impl Default for DesignTokens {
    fn default() -> Self {
        Self {
            dark: Palette::dark(),
            light: Palette::light(),
            sizes: Sizes::default(),
        }
    }
}

impl Default for Palette {
    /// Missing keys of a `[dark]` or `[light]` table fall back to the dark palette; the file's
    /// tables are merged over the right defaults in [`DesignTokens::from_toml`].
    fn default() -> Self {
        Self::dark()
    }
}

impl DesignTokens {
    /// Reads overrides, keeping each palette's own defaults for the keys the file leaves out.
    pub fn from_toml(text: &str) -> Result<Self, toml::de::Error> {
        let mut value: toml::Table = toml::from_str(text)?;
        let mut tokens = Self::default();
        if let Some(dark) = value.remove("dark") {
            tokens.dark = merge(Palette::dark(), dark)?;
        }
        if let Some(light) = value.remove("light") {
            tokens.light = merge(Palette::light(), light)?;
        }
        if let Some(sizes) = value.remove("sizes") {
            tokens.sizes = merge(Sizes::default(), sizes)?;
        }
        Ok(tokens)
    }
}

/// What `picolas_design.toml` sets. The palette tables are kept as written so they can be laid over
/// whichever palette is in use.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DesignFile {
    pub dark: toml::Table,
    pub light: toml::Table,
    pub sizes: toml::Table,
}

impl DesignFile {
    pub fn from_toml(text: &str) -> Result<Self, toml::de::Error> {
        // Rejects the whole file when any token is invalid.
        DesignTokens::from_toml(text)?;
        let mut value: toml::Table = toml::from_str(text)?;
        let mut table = |key| match value.remove(key) {
            Some(toml::Value::Table(table)) => table,
            _ => toml::Table::new(),
        };
        Ok(Self {
            dark: table("dark"),
            light: table("light"),
            sizes: table("sizes"),
        })
    }

    /// The default sizes scaled to `font_size`, with the file's sizes laid over them.
    pub fn sizes_at(&self, font_size: f32) -> Sizes {
        let sizes = Sizes::default().scaled_to(font_size);
        if self.sizes.is_empty() {
            return sizes;
        }
        merge(sizes.clone(), toml::Value::Table(self.sizes.clone())).unwrap_or_else(|err| {
            log::warn!("Ignoring the sizes of {FILE_NAME}: {err}");
            sizes
        })
    }

    fn overrides_for(&self, palette: Palette) -> Palette {
        let table = if is_dark_color(palette.page.0) {
            &self.dark
        } else {
            &self.light
        };
        if table.is_empty() {
            return palette;
        }
        merge(palette.clone(), toml::Value::Table(table.clone())).unwrap_or_else(|err| {
            log::warn!("Ignoring the palette overrides of {FILE_NAME}: {err}");
            palette
        })
    }
}

/// The palettes the chat picks from, with the accent setting and the file's overrides applied.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedPalettes {
    pub claude_dark: Palette,
    pub claude_light: Palette,
    /// Derived from the current Warp theme.
    pub warp: Palette,
}

impl ResolvedPalettes {
    pub fn resolve(file: &DesignFile, theme: &WarpTheme, accent: ClaudeChatAccent) -> Self {
        let finish = |palette: Palette, dark: bool| {
            file.overrides_for(palette.with_accent(accent_color(accent, dark, theme)))
        };
        let warp_is_dark = theme_is_dark(theme);
        Self {
            claude_dark: finish(Palette::dark(), true),
            claude_light: finish(Palette::light(), false),
            warp: finish(Palette::from_warp_theme(theme), warp_is_dark),
        }
    }
}

/// Lays `overrides` over the TOML form of `defaults`.
fn merge<T: serde::Serialize + serde::de::DeserializeOwned>(
    defaults: T,
    overrides: toml::Value,
) -> Result<T, toml::de::Error> {
    let mut base = toml::Value::try_from(defaults).expect("design tokens serialize to TOML");
    if let (Some(base), toml::Value::Table(overrides)) = (base.as_table_mut(), overrides) {
        base.extend(overrides);
    }
    base.try_into()
}

impl serde::Serialize for Hex {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let ColorU { r, g, b, a } = self.0;
        serializer.serialize_str(&format!("#{r:02x}{g:02x}{b:02x}{a:02x}"))
    }
}

impl serde::Serialize for Palette {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let fields = [
            ("page", self.page),
            ("sidebar", self.sidebar),
            ("surface", self.surface),
            ("surface_hover", self.surface_hover),
            ("surface_strong", self.surface_strong),
            ("border", self.border),
            ("border_strong", self.border_strong),
            ("text", self.text),
            ("text_muted", self.text_muted),
            ("text_faint", self.text_faint),
            ("accent", self.accent),
            ("on_accent", self.on_accent),
            ("user_bubble", self.user_bubble),
            ("code_background", self.code_background),
            ("inline_code_background", self.inline_code_background),
            ("link", self.link),
            ("success", self.success),
            ("warning", self.warning),
            ("error", self.error),
            ("info", self.info),
            ("diff_added", self.diff_added),
            ("diff_removed", self.diff_removed),
            ("selection", self.selection),
            ("shadow", self.shadow),
            ("diff_added_background", self.diff_added_background),
            ("diff_removed_background", self.diff_removed_background),
            ("error_background", self.error_background),
            ("error_border", self.error_border),
            ("shimmer", self.shimmer),
            ("syntax_keyword", self.syntax_keyword),
            ("syntax_function", self.syntax_function),
            ("syntax_string", self.syntax_string),
            ("syntax_type", self.syntax_type),
            ("syntax_number", self.syntax_number),
            ("syntax_comment", self.syntax_comment),
            ("syntax_property", self.syntax_property),
            ("syntax_tag", self.syntax_tag),
            ("kind_document", self.kind_document),
            ("kind_presentation", self.kind_presentation),
            ("kind_design", self.kind_design),
            ("kind_dashboard", self.kind_dashboard),
            ("kind_motion", self.kind_motion),
            ("kind_other", self.kind_other),
        ];
        let mut map = serializer.serialize_map(Some(fields.len()))?;
        for (key, value) in fields {
            map.serialize_entry(key, &value)?;
        }
        map.end()
    }
}

impl serde::Serialize for Sizes {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let fields = [
            ("font_size", self.font_size),
            ("line_height_ratio", self.line_height_ratio),
            ("small_font_size", self.small_font_size),
            ("tiny_font_size", self.tiny_font_size),
            ("title_font_size", self.title_font_size),
            ("code_font_size", self.code_font_size),
            ("column_max_width", self.column_max_width),
            ("column_padding", self.column_padding),
            ("message_spacing", self.message_spacing),
            ("bubble_radius", self.bubble_radius),
            ("bubble_padding_x", self.bubble_padding_x),
            ("bubble_padding_y", self.bubble_padding_y),
            ("bubble_max_width_ratio", self.bubble_max_width_ratio),
            ("composer_radius", self.composer_radius),
            ("composer_padding", self.composer_padding),
            ("composer_min_height", self.composer_min_height),
            ("composer_max_height", self.composer_max_height),
            ("composer_bottom_margin", self.composer_bottom_margin),
            ("card_radius", self.card_radius),
            ("chip_radius", self.chip_radius),
            ("chip_height", self.chip_height),
            ("button_size", self.button_size),
            ("icon_size", self.icon_size),
            ("small_icon_size", self.small_icon_size),
            ("tool_row_height", self.tool_row_height),
            ("work_group_max_height", self.work_group_max_height),
            ("side_panel_width", self.side_panel_width),
            ("side_panel_padding", self.side_panel_padding),
            ("header_height", self.header_height),
            ("shadow_blur", self.shadow_blur),
            ("code_block_radius", self.code_block_radius),
            ("tool_output_max_height", self.tool_output_max_height),
            ("transcript_padding", self.transcript_padding),
            ("scroll_button_size", self.scroll_button_size),
            ("gallery_card_width", self.gallery_card_width),
            ("gallery_card_height", self.gallery_card_height),
            ("gallery_tile_size", self.gallery_tile_size),
            ("gallery_tile_height", self.gallery_tile_height),
            ("gallery_title_font_size", self.gallery_title_font_size),
            ("gallery_max_width", self.gallery_max_width),
            ("gallery_padding", self.gallery_padding),
            ("gallery_gap", self.gallery_gap),
        ];
        let mut map = serializer.serialize_map(Some(fields.len()))?;
        for (key, value) in fields {
            map.serialize_entry(key, &value)?;
        }
        map.end()
    }
}

/// The current design tokens; views read them on every render and re-render on
/// [`ChatDesignEvent::Changed`].
pub struct ChatDesign {
    file: DesignFile,
    palettes: ResolvedPalettes,
    sizes: Sizes,
    path: PathBuf,
    /// Kept alive so the file stays watched.
    _watcher: Option<ModelHandle<BulkFilesystemWatcher>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatDesignEvent {
    Changed,
}

impl ChatDesign {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        crate::appearance::system_ui_font_family(ctx);
        let path = warp_core::paths::data_dir().join(FILE_NAME);
        let file = read_file(&path);
        let palettes = resolve_palettes(&file, ctx);
        let sizes = resolve_sizes(&file, ctx);
        ctx.subscribe_to_model(&Appearance::handle(ctx), |me, _, event, ctx| match event {
            AppearanceEvent::ThemeChanged => me.refresh(ctx),
            AppearanceEvent::MonospaceFontSizeChanged { .. } => me.refresh(ctx),
            _ => {}
        });
        ctx.subscribe_to_model(&ThemeSettings::handle(ctx), |me, _, _, ctx| {
            me.refresh(ctx);
            // The Claude theme and the chat font pick among cached values without changing them.
            ctx.emit(ChatDesignEvent::Changed);
        });
        let watcher = path.parent().map(|dir| {
            let watcher = ctx.add_model(|ctx| BulkFilesystemWatcher::new(WATCH_DEBOUNCE, ctx));
            let registration = watcher.update(ctx, |watcher, _| {
                watcher.register_path(dir, WatchFilter::accept_all(), RecursiveMode::NonRecursive)
            });
            ctx.background_executor()
                .spawn(async move {
                    if let Err(err) = registration.await {
                        log::warn!("Can't watch the design tokens file: {err:#}");
                    }
                })
                .detach();
            ctx.subscribe_to_model(&watcher, |me, _, event, ctx| {
                me.on_files_changed(event, ctx)
            });
            watcher
        });
        Self {
            file,
            palettes,
            sizes,
            path,
            _watcher: watcher,
        }
    }

    /// The sizes, scaled to the chat font size setting.
    pub fn sizes(&self) -> &Sizes {
        &self.sizes
    }

    /// The palette the Claude theme setting picks.
    pub fn palette(&self, app: &AppContext) -> &Palette {
        match *ThemeSettings::as_ref(app).claude_theme.value() {
            ClaudeTheme::Warp => &self.palettes.warp,
            ClaudeTheme::ClaudeDark | ClaudeTheme::ClaudeLight | ClaudeTheme::System => {
                if is_dark(app) {
                    &self.palettes.claude_dark
                } else {
                    &self.palettes.claude_light
                }
            }
        }
    }

    fn refresh(&mut self, ctx: &mut ModelContext<Self>) {
        let palettes = resolve_palettes(&self.file, ctx);
        let sizes = resolve_sizes(&self.file, ctx);
        if palettes != self.palettes || sizes != self.sizes {
            self.palettes = palettes;
            self.sizes = sizes;
            ctx.emit(ChatDesignEvent::Changed);
        }
    }

    fn on_files_changed(
        &mut self,
        event: &BulkFilesystemWatcherEvent,
        ctx: &mut ModelContext<Self>,
    ) {
        let file_name = self.path.file_name();
        let touched = event
            .added
            .iter()
            .chain(&event.modified)
            .chain(&event.deleted)
            .chain(event.moved.iter().flat_map(|(to, from)| [to, from]))
            .any(|path| path.file_name() == file_name);
        if !touched {
            return;
        }
        let file = read_file(&self.path);
        if file != self.file {
            self.file = file;
            self.palettes = resolve_palettes(&self.file, ctx);
            self.sizes = resolve_sizes(&self.file, ctx);
            ctx.emit(ChatDesignEvent::Changed);
        }
    }
}

fn resolve_palettes(file: &DesignFile, app: &AppContext) -> ResolvedPalettes {
    ResolvedPalettes::resolve(
        file,
        Appearance::as_ref(app).theme(),
        *ThemeSettings::as_ref(app).claude_chat_accent.value(),
    )
}

fn resolve_sizes(file: &DesignFile, app: &AppContext) -> Sizes {
    file.sizes_at(chat_font_size(
        *ThemeSettings::as_ref(app).claude_chat_font_size.value(),
        Appearance::as_ref(app).monospace_font_size(),
    ))
}

fn read_file(path: &Path) -> DesignFile {
    let Ok(text) = std::fs::read_to_string(path) else {
        return DesignFile::default();
    };
    DesignFile::from_toml(&text).unwrap_or_else(|err| {
        log::warn!("Ignoring {}: {err}", path.display());
        DesignFile::default()
    })
}

/// The fonts of the chat: the chat font setting's family (the system font, SF Pro, by default) for
/// prose and controls, the terminal's font for code.
#[derive(Debug, Clone, Copy)]
pub struct ChatFonts {
    pub ui: FamilyId,
    pub mono: FamilyId,
}

pub fn fonts(app: &AppContext) -> ChatFonts {
    let appearance = Appearance::as_ref(app);
    let ui = match *ThemeSettings::as_ref(app).claude_chat_font.value() {
        ClaudeChatFont::System => crate::appearance::system_ui_font_family_cached(app)
            .unwrap_or_else(|| appearance.ui_font_family()),
        ClaudeChatFont::Warp => appearance.ui_font_family(),
    };
    ChatFonts {
        ui,
        mono: appearance.monospace_font_family(),
    }
}

/// Whether the chat uses its dark palette: the Claude theme setting decides, and "Warp" follows
/// the Warp theme.
pub fn is_dark(app: &AppContext) -> bool {
    match *ThemeSettings::as_ref(app).claude_theme.value() {
        ClaudeTheme::ClaudeDark => true,
        ClaudeTheme::ClaudeLight => false,
        ClaudeTheme::System => matches!(app.system_theme(), SystemTheme::Dark),
        ClaudeTheme::Warp => theme_is_dark(Appearance::as_ref(app).theme()),
    }
}

impl Entity for ChatDesign {
    type Event = ChatDesignEvent;
}

impl SingletonEntity for ChatDesign {}

#[cfg(test)]
#[path = "design_tests.rs"]
mod tests;
