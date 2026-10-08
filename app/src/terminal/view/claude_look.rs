//! Panes running Claude Code take the look of the Claude desktop app: its warm
//! dark background, text colors and SF Mono (the app's code font). The cells
//! stay monospace, and the theme's look comes back when Claude exits.

use std::sync::OnceLock;

use pathfinder_color::ColorU;
use warpui::fonts::FamilyId;
use warpui::{AppContext, SingletonEntity, ViewContext};

use super::TerminalView;
use crate::appearance::Appearance;
use crate::terminal::CLIAgent;
use crate::terminal::cli_agent_sessions::CLIAgentSessionsModel;
use crate::terminal::color::{self, BrightColors, Colors, NormalColors, PrimaryColors};

/// The Claude app's dark page background.
pub(super) const BACKGROUND: ColorU = rgb(0x262624);
/// The Claude app's main text color.
const FOREGROUND: ColorU = rgb(0xfaf9f5);

/// Font candidates, in order: SF Mono's names across macOS versions, then Menlo.
const FONT_FAMILIES: &[&str] = &["SF Mono", ".SF NS Mono", "SFMono-Regular", "Menlo"];
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

/// A warm ANSI palette built around the Claude app's grays, blue accent and
/// clay brand color, so TUIs that use ANSI colors match the app.
fn claude_app_colors() -> Colors {
    Colors::new(
        PrimaryColors::new(FOREGROUND, BACKGROUND),
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
            black: rgb(0x9c9a92),
            red: rgb(0xd97757),
            green: rgb(0xa8d3a8),
            yellow: rgb(0xf0cd8f),
            blue: rgb(0x9cc4ee),
            magenta: rgb(0xd0c0f6),
            cyan: rgb(0x9fd6d7),
            white: FOREGROUND,
        },
    )
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

    /// Whether Claude Code is the CLI agent running in this pane.
    pub(super) fn runs_claude(&self, app: &AppContext) -> bool {
        CLIAgentSessionsModel::as_ref(app)
            .session(self.view_id)
            .is_some_and(|session| session.agent == CLIAgent::Claude)
    }

    /// Uses the Claude app palette while Claude runs here, otherwise the
    /// theme's terminal colors.
    pub(super) fn apply_terminal_colors(
        &mut self,
        use_claude_look: bool,
        ctx: &mut ViewContext<Self>,
    ) {
        let colors = if use_claude_look {
            claude_app_colors()
        } else {
            Appearance::as_ref(ctx).theme().clone().into()
        };
        let list = color::List::from(&colors);
        self.model.lock().update_colors(list);
        self.colors = list;
        ctx.notify();
    }
}
