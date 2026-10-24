//! Panes running Claude Code take the look of the Claude desktop app: its warm
//! background, text colors and SF Mono (the app's code font). The cells stay
//! monospace, and the theme's look comes back when Claude exits. The colors
//! follow the [`ClaudeTheme`] setting, which can also hand them back to the
//! Warp theme.

use std::sync::OnceLock;

use pathfinder_color::ColorU;
use pathfinder_geometry::vector::vec2f;
use settings::Setting;
use warp_core::ui::Icon as WarpIcon;
use warp_core::ui::theme::color::internal_colors;
use warpui::elements::{
    ChildAnchor, ConstrainedBox, Container, CornerRadius, Element, Hoverable, OffsetPositioning,
    Padding, ParentAnchor, ParentElement, ParentOffsetBounds, Radius, Stack,
};
use warpui::fonts::FamilyId;
use warpui::platform::{Cursor, SystemTheme};
use warpui::{AppContext, SingletonEntity, ViewContext};

use super::{TerminalAction, TerminalView};
use crate::appearance::Appearance;
use crate::settings::{ClaudeTheme, ThemeSettings};
use crate::terminal::CLIAgent;
use crate::terminal::cli_agent_sessions::CLIAgentSessionsModel;
use crate::terminal::color::{self, BrightColors, Colors, NormalColors, PrimaryColors};
use crate::terminal::prompt_card::{PromptCardControls, PromptCardStyle};

/// Horizontal padding of a Claude pane, wider than a terminal's so the prompt card has room on
/// both sides.
pub(super) const PADDING_X: f32 = 18.;
/// Size of the X that closes a lone Claude pane.
const CLOSE_ICON_SIZE: f32 = 14.;
/// The Claude app's dark page background.
const DARK_BACKGROUND: ColorU = rgb(0x262624);
/// The Claude app's main text color in dark mode.
const DARK_FOREGROUND: ColorU = rgb(0xfaf9f5);
/// The Claude app's light page background.
const LIGHT_BACKGROUND: ColorU = rgb(0xfaf9f5);
/// The Claude app's main text color in light mode.
const LIGHT_FOREGROUND: ColorU = rgb(0x141413);

/// Font candidates, in order: SF Mono's names across macOS versions, Menlo, then
/// close monospace fonts for systems without them.
const FONT_FAMILIES: &[&str] = &[
    "SF Mono",
    ".SF NS Mono",
    "SFMono-Regular",
    "Menlo",
    "JetBrains Mono",
    "Adwaita Mono",
    "Noto Sans Mono",
    "DejaVu Sans Mono",
];
const FONT_SIZE: f32 = 14.;
const LINE_HEIGHT_RATIO: f32 = 1.4;

static FONT_FAMILY: OnceLock<Option<FamilyId>> = OnceLock::new();

/// The font a Claude pane renders and sizes its grid with.
#[derive(Debug, Clone, Copy)]
pub(super) struct ClaudeFont {
    pub family: FamilyId,
    pub size: f32,
    pub line_height_ratio: f32,
}

#[cfg(not(target_family = "wasm"))]
fn load_font_family(ctx: &mut AppContext) -> Option<FamilyId> {
    warpui::fonts::Cache::handle(ctx).update(ctx, |font_cache: &mut warpui::fonts::Cache, _| {
        FONT_FAMILIES
            .iter()
            .find_map(|name| font_cache.get_or_load_system_font(name).ok())
    })
}

#[cfg(target_family = "wasm")]
fn load_font_family(_ctx: &mut AppContext) -> Option<FamilyId> {
    None
}

const fn rgb(hex: u32) -> ColorU {
    ColorU {
        r: (hex >> 16) as u8,
        g: (hex >> 8) as u8,
        b: hex as u8,
        a: 0xff,
    }
}

/// The Claude app palette a pane paints with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClaudePalette {
    Dark,
    Light,
}

impl ClaudePalette {
    /// The palette the [`ClaudeTheme`] setting resolves to, or `None` when Claude panes use the
    /// Warp theme.
    pub(super) fn current(app: &AppContext) -> Option<Self> {
        match *ThemeSettings::as_ref(app).claude_theme.value() {
            ClaudeTheme::ClaudeDark => Some(Self::Dark),
            ClaudeTheme::ClaudeLight => Some(Self::Light),
            ClaudeTheme::System => Some(match app.system_theme() {
                SystemTheme::Light => Self::Light,
                SystemTheme::Dark => Self::Dark,
            }),
            ClaudeTheme::Warp => None,
        }
    }

    pub(super) fn background(self) -> ColorU {
        match self {
            Self::Dark => DARK_BACKGROUND,
            Self::Light => LIGHT_BACKGROUND,
        }
    }

    pub(super) fn is_dark(self) -> bool {
        match self {
            Self::Dark => true,
            Self::Light => false,
        }
    }

    /// Fill and border of the Claude app's message box: a raised surface with a hairline border.
    fn prompt_card_colors(self) -> (ColorU, ColorU) {
        match self {
            Self::Dark => (rgb(0x30302e), rgb(0x45443f)),
            Self::Light => (rgb(0xffffff), rgb(0xdedcd1)),
        }
    }

    fn foreground(self) -> ColorU {
        match self {
            Self::Dark => DARK_FOREGROUND,
            Self::Light => LIGHT_FOREGROUND,
        }
    }

    fn muted_foreground(self) -> ColorU {
        match self {
            Self::Dark => rgb(0x9c9a92),
            Self::Light => rgb(0x73726c),
        }
    }

    /// A warm ANSI palette built around the Claude app's grays, blue accent and clay brand
    /// color, so TUIs that use ANSI colors match the app.
    fn colors(self) -> Colors {
        let foreground = self.foreground();
        match self {
            Self::Dark => Colors::new(
                PrimaryColors::new(foreground, DARK_BACKGROUND),
                NormalColors {
                    black: rgb(0x30302e),
                    red: rgb(0xe5736f),
                    green: rgb(0x8cbf8e),
                    yellow: rgb(0xe4b76a),
                    blue: rgb(0x74abe2),
                    magenta: rgb(0xb9a3ef),
                    cyan: rgb(0x7cc2c4),
                    white: rgb(0xc2c0b6),
                },
                BrightColors {
                    black: self.muted_foreground(),
                    red: rgb(0xd97757),
                    green: rgb(0xa8d3a8),
                    yellow: rgb(0xf0cd8f),
                    blue: rgb(0x9cc4ee),
                    magenta: rgb(0xd0c0f6),
                    cyan: rgb(0x9fd6d7),
                    white: foreground,
                },
            ),
            // White maps to grays that stay readable on the light page, since TUIs often print
            // white text assuming a dark background.
            Self::Light => Colors::new(
                PrimaryColors::new(foreground, LIGHT_BACKGROUND),
                NormalColors {
                    black: foreground,
                    red: rgb(0xb5332e),
                    green: rgb(0x3d7a3f),
                    yellow: rgb(0x8a6100),
                    blue: rgb(0x2c64a8),
                    magenta: rgb(0x7650b8),
                    cyan: rgb(0x22767a),
                    white: rgb(0x87867f),
                },
                BrightColors {
                    black: self.muted_foreground(),
                    red: rgb(0xd97757),
                    green: rgb(0x4f8f52),
                    yellow: rgb(0xa87a10),
                    blue: rgb(0x3a7bc8),
                    magenta: rgb(0x8b67cf),
                    cyan: rgb(0x2f8c90),
                    white: rgb(0x3d3d3a),
                },
            ),
        }
    }
}

impl TerminalView {
    /// The Claude font while Claude runs here, once it has been loaded.
    pub(super) fn claude_font(&self, app: &AppContext) -> Option<ClaudeFont> {
        let family = (*FONT_FAMILY.get()?)?;
        self.runs_claude(app).then_some(ClaudeFont {
            family,
            size: FONT_SIZE,
            line_height_ratio: LINE_HEIGHT_RATIO,
        })
    }

    /// Switches the pane to (or back from) the Claude look: colors, then the
    /// font, which changes the grid size and makes the TUI redraw.
    pub(super) fn apply_claude_look(&mut self, use_claude_look: bool, ctx: &mut ViewContext<Self>) {
        if use_claude_look && FONT_FAMILY.get().is_none() {
            let family = load_font_family(ctx);
            let _ = FONT_FAMILY.set(family);
        }
        self.apply_terminal_colors(use_claude_look, ctx);
        self.refresh_size(ctx);
    }

    /// The card that replaces the rules around Claude's prompt while Claude runs here.
    pub(super) fn claude_prompt_card(&self, app: &AppContext) -> Option<PromptCardStyle> {
        if !self.runs_claude(app) {
            return None;
        }
        let (fill, border) = match ClaudePalette::current(app) {
            Some(palette) => palette.prompt_card_colors(),
            None => {
                let theme = Appearance::as_ref(app).theme();
                (
                    internal_colors::fg_overlay_1(theme).into_solid(),
                    internal_colors::fg_overlay_2(theme).into_solid(),
                )
            }
        };
        Some(PromptCardStyle {
            fill,
            border,
            controls: Some(PromptCardControls {
                card_position_id: self.claude_card_position_id(),
                controls_position_id: self.claude_controls_position_id(),
            }),
            selection: self.is_claude_prompt_selected().then(|| {
                Appearance::as_ref(app)
                    .theme()
                    .text_selection_color()
                    .into_solid()
            }),
        })
    }

    /// Save-position id of the card around Claude's prompt; the mention chips sit on top of it.
    pub(super) fn claude_card_position_id(&self) -> String {
        format!("terminal_view:claude_card_{}", self.view_id)
    }

    /// Save-position id of the card's controls row, in place of Claude's status line.
    pub(super) fn claude_controls_position_id(&self) -> String {
        format!("terminal_view:claude_controls_{}", self.view_id)
    }

    /// Whether Claude Code is the CLI agent running in this pane.
    pub(super) fn runs_claude(&self, app: &AppContext) -> bool {
        CLIAgentSessionsModel::as_ref(app)
            .session(self.view_id)
            .is_some_and(|session| session.agent == CLIAgent::Claude)
    }

    /// Paints the pane with the Claude palette's background and, when the pane
    /// is alone in its tab (so it has no header), floats a close button in its
    /// top right corner. That's how a Claude session opened from the Threads
    /// panel closes while the tab list is hidden.
    pub(super) fn wrap_claude_pane(
        &self,
        element: Box<dyn Element>,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let palette = ClaudePalette::current(app);
        let element = match palette {
            Some(palette) => Container::new(element)
                .with_background_color(palette.background())
                .finish(),
            None => element,
        };
        if self.split_pane_state(app).is_in_split_pane() {
            return element;
        }

        let theme = Appearance::as_ref(app).theme();
        let (icon_color, hover_background) = match palette {
            Some(palette) => {
                let mut hover = palette.foreground();
                hover.a = 0x1a;
                (palette.muted_foreground().into(), hover.into())
            }
            None => (
                theme.sub_text_color(theme.background()),
                internal_colors::fg_overlay_2(theme),
            ),
        };
        let close_button =
            Hoverable::new(self.claude_close_button_mouse_state.clone(), move |state| {
                let icon = ConstrainedBox::new(WarpIcon::X.to_warpui_icon(icon_color).finish())
                    .with_width(CLOSE_ICON_SIZE)
                    .with_height(CLOSE_ICON_SIZE)
                    .finish();
                let mut button = Container::new(icon)
                    .with_padding(Padding::uniform(4.))
                    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.)));
                if state.is_hovered() {
                    button = button.with_background(hover_background);
                }
                button.finish()
            })
            .with_cursor(Cursor::PointingHand)
            .on_click(|ctx, _, _| ctx.dispatch_typed_action(TerminalAction::Close))
            .finish();

        let mut stack = Stack::new().with_child(element);
        stack.add_positioned_overlay_child(
            close_button,
            OffsetPositioning::offset_from_parent(
                vec2f(-8., 8.),
                ParentOffsetBounds::ParentByPosition,
                ParentAnchor::TopRight,
                ChildAnchor::TopRight,
            ),
        );
        stack.finish()
    }

    /// Uses the Claude app palette while Claude runs here (unless the
    /// [`ClaudeTheme`] setting picks the Warp theme), otherwise the theme's
    /// terminal colors.
    pub(super) fn apply_terminal_colors(
        &mut self,
        use_claude_look: bool,
        ctx: &mut ViewContext<Self>,
    ) {
        let palette = use_claude_look
            .then(|| ClaudePalette::current(ctx))
            .flatten();
        let colors = match palette {
            Some(palette) => palette.colors(),
            None => Appearance::as_ref(ctx).theme().clone().into(),
        };
        let list = color::List::from(&colors);
        self.model.lock().update_colors(list);
        self.colors = list;
        ctx.notify();
    }
}
