//! Controls that float over the card around Claude Code's prompt (see `prompt_card`), like a chat
//! app's composer. Claude's status line gives way to Warp's controls row: the model and effort
//! (a menu switches them), the permission mode (a click cycles it, like Shift+Tab), Claude's other
//! status bits and a send button (stop, while Claude works). Chips for the selections mentioned in
//! the next message sit on top of the card.
//!
//! Mentions stay out of Claude's prompt until the message is sent: pressing Enter with the prompt
//! showing pastes them as quotes after the typed text, then submits.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Duration;

use instant::Instant;
use pathfinder_color::ColorU;
use warp_core::ui::Icon as WarpIcon;
use warp_core::ui::theme::{ColorScheme, Fill as WarpThemeFill};
use warp_terminal::model::escape_sequences::{BRACKETED_PASTE_END, BRACKETED_PASTE_START};
use warpui::r#async::Timer;
use warpui::elements::{
    Align, Border, ChildAnchor, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment,
    Dismiss, DispatchEventResult, DragAxis, Draggable, DraggableState, DropShadow, Element, Empty,
    EventHandler, Flex, Hoverable, Icon as SvgIcon, MainAxisAlignment, MainAxisSize,
    MouseStateHandle, OffsetPositioning, Padding, ParentElement, PositionedElementAnchor,
    PositionedElementOffsetBounds, Radius, SavePosition, Stack, Text,
};
use warpui::fonts::{FamilyId, Properties, Weight};
use warpui::geometry::vector::vec2f;
use warpui::platform::Cursor;
use warpui::{AppContext, SingletonEntity, ViewContext};

use super::claude_look::ClaudePalette;
use super::claude_mention::quote;
use super::{TerminalAction, TerminalView};
use crate::appearance::Appearance;
use crate::claude_threads::project_slug;
use crate::settings::AppEditorSettings;
use crate::terminal::TerminalModel;
use crate::terminal::cli_agent_sessions::{CLIAgentSessionStatus, CLIAgentSessionsModel};
use crate::terminal::model::cell::Flags;
use crate::terminal::model::grid::Dimensions as _;
use crate::terminal::model::index::Point;
use crate::terminal::prompt_card::find_prompt_rules;
use crate::view_components::DismissibleToast;
use crate::workspace::ToastStack;

const FONT_SIZE: f32 = 12.;
const ICON_SIZE: f32 = 12.;
const CHEVRON_SIZE: f32 = 10.;
const CHIP_HEIGHT: f32 = 20.;
const CHIP_HORIZONTAL_PADDING: f32 = 8.;
const CHIP_GAP: f32 = 6.;
/// Space between the mention chips and the top of the card.
const CHIPS_OFFSET: f32 = 6.;
/// Lines the chips up with the prompt text inside the card.
const CHIPS_LEFT_INSET: f32 = 14.;
/// Characters of a mention shown in its chip.
const CHIP_PREVIEW_CHARS: usize = 40;
/// Chips shown before the rest collapse into a "+N".
const MAX_VISIBLE_CHIPS: usize = 3;
const CONTROL_HEIGHT: f32 = 22.;
const CONTROL_HORIZONTAL_PADDING: f32 = 7.;
const CONTROL_GAP: f32 = 2.;
const DIVIDER_HEIGHT: f32 = 12.;
const SEND_BUTTON_SIZE: f32 = 22.;
const SEND_ICON_SIZE: f32 = 12.;
/// Lets Claude take in the pasted quotes before the Enter that submits them.
const SUBMIT_DELAY: Duration = Duration::from_millis(60);
/// How often the model shown in the composer is re-read from the transcript, at most.
const MODEL_REFRESH_INTERVAL: Duration = Duration::from_secs(3);
/// The end of a transcript holds the latest assistant message, with its model and effort.
const TRANSCRIPT_TAIL_BYTES: u64 = 256 * 1024;
/// Claude's hint after the permission mode in its status line.
const MODE_HINT: &str = "(shift+tab to cycle)";
/// What Shift+Tab sends, which cycles Claude's permission mode.
const SHIFT_TAB: &[u8] = b"\x1b[Z";
const BACKSPACE: &[u8] = b"\x7f";
/// How long the cursor stays on, and then off, while it blinks.
const CURSOR_BLINK_INTERVAL: Duration = Duration::from_millis(500);
/// Time between the keys that erase the prompt.
const KEY_SPACING: Duration = Duration::from_millis(3);
const CURSOR_RIGHT: &[u8] = b"\x1b[C";
/// Backspace and Delete, as the legacy encoding and the kitty keyboard protocol (which Claude
/// turns on) send them.
const ERASE_KEYS: [&[u8]; 6] = [
    b"\x7f",
    b"\x08",
    b"\x1b[127u",
    b"\x1b[127;1u",
    b"\x1b[3~",
    b"\x1b[3;1~",
];

/// Models offered in the model menu, with the id `/model` takes.
const MODELS: [(&str, &str); 4] = [
    ("Opus 5.5", "claude-opus-5-5"),
    ("Sonnet 5.5", "claude-sonnet-5-5"),
    ("Haiku 4.5", "claude-haiku-4-5-20251001"),
    ("Fable 5.1", "claude-fable-5-1"),
];
/// Effort levels on the picker's slider, as `/effort` takes them.
const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
/// What `/effort` takes to go back to the model's default.
const DEFAULT_EFFORT: &str = "auto";
const PICKER_WIDTH: f32 = 236.;
const PICKER_PADDING: f32 = 12.;
const PICKER_CORNER_RADIUS: f32 = 12.;
const PICKER_BORDER: f32 = 1.;
/// Space between the model chip and the picker above it.
const PICKER_OFFSET: f32 = 8.;
const PICKER_TITLE_FONT_SIZE: f32 = 13.;
const PICKER_FONT_SIZE: f32 = 12.;
const SLIDER_HEIGHT: f32 = 18.;
/// The picker's width inside its padding and border, which the slider spans.
const SLIDER_WIDTH: f32 = PICKER_WIDTH - 2. * (PICKER_PADDING + PICKER_BORDER);
const SLIDER_KNOB_SIZE: f32 = 14.;
const SLIDER_DOT_SIZE: f32 = 3.;
/// How far from each end of the track the knob's middle stops.
const SLIDER_INSET: f32 = SLIDER_HEIGHT / 2.;
/// Time given to one `/model` or `/effort` before the next is typed.
const COMMAND_SPACING: Duration = Duration::from_millis(400);
/// How long the knob takes to settle on a level after it's let go.
const SLIDER_SETTLE_DURATION: Duration = Duration::from_millis(160);
const SLIDER_FRAME: Duration = Duration::from_millis(16);
const PICKER_ICON_SIZE: f32 = 13.;
const RESET_ICON: &str = "bundled/svg/picolas-reset.svg";
const CHEVRON_RIGHT_ICON: &str = "bundled/svg/picolas-chevron-right.svg";
const CHECK_ICON: &str = "bundled/svg/picolas-check.svg";

/// Accent of quote chips, the same blue as quotes in a chat app.
const QUOTE_ACCENT: ColorU = ColorU {
    r: 0x46,
    g: 0x84,
    b: 0xe5,
    a: 0xff,
};
/// The Claude app's send button.
const SEND_COLOR: ColorU = ColorU {
    r: 0xd9,
    g: 0x77,
    b: 0x57,
    a: 0xff,
};
const STOP_COLOR: ColorU = ColorU {
    r: 0xe5,
    g: 0x48,
    b: 0x4d,
    a: 0xff,
};
const BUTTON_ICON_COLOR: ColorU = ColorU {
    r: 0xff,
    g: 0xff,
    b: 0xff,
    a: 0xff,
};
/// Auto and accept-edits modes, which let Claude act without asking.
const AUTO_MODE_COLOR: ColorU = ColorU {
    r: 0xe4,
    g: 0xb7,
    b: 0x6a,
    a: 0xff,
};
const PLAN_MODE_COLOR: ColorU = ColorU {
    r: 0x7c,
    g: 0xc2,
    b: 0xc4,
    a: 0xff,
};

/// A selection waiting to be sent along with the next message.
#[derive(Debug, Clone)]
pub(super) struct PendingMention {
    text: String,
    remove_button_mouse_state: MouseStateHandle,
}

/// The model and effort Claude runs with, as shown in the composer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ClaudeModelLabel {
    model: String,
    effort: Option<String>,
}

/// State of the composer controls of a Claude pane.
pub(super) struct ClaudeComposer {
    mentions: Vec<PendingMention>,
    model: Option<ClaudeModelLabel>,
    model_refreshed_at: Option<Instant>,
    /// The page of the model and effort picker, while it's open.
    picker: Option<PickerPage>,
    /// Set by ⌘A: the next key replaces everything typed in the prompt.
    is_prompt_selected: bool,
    /// The off half of the cursor's blink.
    is_cursor_blinked_off: bool,
    /// Bumped whenever the blink restarts; ticks of an older blink stop.
    cursor_blink_epoch: usize,
    /// Where the knob of the effort slider is held, from 0 (left end) to 1, while it's pressed.
    slider_drag: Option<f32>,
    /// The effort let go at on the slider, which Claude switches to when the picker closes.
    pending_effort: Option<usize>,
    /// The knob gliding onto the level it was let go at.
    slider_settle: Option<SliderSettle>,
    slider_drag_state: DraggableState,
    model_chip_mouse_state: MouseStateHandle,
    picker_mouse_states: PickerMouseStates,
    mode_pill_mouse_state: MouseStateHandle,
    send_button_mouse_state: MouseStateHandle,
}

impl ClaudeComposer {
    pub(super) fn new() -> Self {
        Self {
            mentions: Vec::new(),
            model: None,
            model_refreshed_at: None,
            picker: None,
            is_prompt_selected: false,
            is_cursor_blinked_off: false,
            cursor_blink_epoch: 0,
            slider_drag: None,
            pending_effort: None,
            slider_settle: None,
            slider_drag_state: Default::default(),
            model_chip_mouse_state: Default::default(),
            picker_mouse_states: Default::default(),
            mode_pill_mouse_state: Default::default(),
            send_button_mouse_state: Default::default(),
        }
    }
}

/// A choice in the model and effort picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelChoice {
    Model {
        label: &'static str,
        id: &'static str,
    },
    Effort(&'static str),
}

/// The two pages of the model and effort picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PickerPage {
    /// The effort slider, under the model's name.
    Effort,
    /// The list of models.
    Models,
}

/// Something done in the model and effort picker.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ClaudePickerAction {
    ShowModels,
    /// The model at this index of `MODELS`.
    ChooseModel(usize),
    /// The effort slider is pressed, or dragged, at this window x.
    PressSlider(f32),
    DragSlider(f32),
    /// The effort slider is let go: its knob settles on the nearest level, which Claude switches
    /// to.
    ReleaseSlider,
    ResetEffort,
    Close,
}

/// The knob of the effort slider moving from where it was let go to its level.
#[derive(Debug, Clone, Copy)]
struct SliderSettle {
    from: f32,
    to: f32,
    started_at: Instant,
}

impl SliderSettle {
    fn position(&self) -> Option<f32> {
        let progress =
            self.started_at.elapsed().as_secs_f32() / SLIDER_SETTLE_DURATION.as_secs_f32();
        (progress < 1.).then(|| {
            let eased = 1. - (1. - progress).powi(3);
            self.from + (self.to - self.from) * eased
        })
    }
}

/// Where the knob sits for the effort at `index`, from 0 to 1.
fn effort_position(index: usize) -> f32 {
    index as f32 / (EFFORTS.len() - 1) as f32
}

/// The width of the slider's fill for a knob at `position`: a pill from the track's left end to
/// past the knob's middle, never narrower than it is tall.
fn slider_fill_width(position: f32) -> f32 {
    2. * SLIDER_INSET + position.clamp(0., 1.) * (SLIDER_WIDTH - 2. * SLIDER_INSET)
}

/// The effort level nearest to a knob position.
fn nearest_effort(position: f32) -> usize {
    (position.clamp(0., 1.) * (EFFORTS.len() - 1) as f32).round() as usize
}

/// A knob position for window x `x` on a track spanning `track_left..track_right`.
fn slider_position(x: f32, track_left: f32, track_right: f32) -> f32 {
    let span = track_right - track_left - 2. * SLIDER_INSET;
    if span <= 0. {
        return 0.;
    }
    ((x - track_left - SLIDER_INSET) / span).clamp(0., 1.)
}

#[derive(Default)]
struct PickerMouseStates {
    model_link: MouseStateHandle,
    reset: MouseStateHandle,
    slider: MouseStateHandle,
    models: [MouseStateHandle; MODELS.len()],
}

/// "claude-opus-5-5" → "Opus 5.5", "claude-sonnet-4-5-20250929" → "Sonnet 4.5".
fn model_label(model_id: &str) -> Option<String> {
    let id = model_id.trim().strip_prefix("claude-")?;
    let id = id.split('[').next().unwrap_or(id);
    let mut parts = id.split('-');
    let family = parts.next().filter(|family| !family.is_empty())?;
    let mut family_chars = family.chars();
    let family = family_chars
        .next()
        .map(|first| first.to_uppercase().chain(family_chars).collect::<String>())?;
    // Dates (20250929) end the version.
    let version = parts
        .take_while(|part| part.len() <= 2 && part.chars().all(|c| c.is_ascii_digit()))
        .collect::<Vec<_>>()
        .join(".");
    Some(if version.is_empty() {
        family
    } else {
        format!("{family} {version}")
    })
}

fn effort_label(effort: &str) -> String {
    match effort {
        "low" => "Baixo".to_owned(),
        "medium" => "Médio".to_owned(),
        "high" => "Alto".to_owned(),
        "xhigh" => "Extra alto".to_owned(),
        "max" => "Máximo".to_owned(),
        other => other.to_owned(),
    }
}

/// The value of the last `"key":"value"` pair in `text`.
fn last_string_field(text: &str, key: &str) -> Option<String> {
    let marker = format!("\"{key}\":\"");
    let start = text.rfind(&marker)? + marker.len();
    let end = text[start..].find('"')? + start;
    Some(text[start..end].to_owned())
}

/// The string at `key` at the top level of Claude's `settings.json`.
fn settings_string(settings: &str, key: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(settings)
        .ok()?
        .get(key)?
        .as_str()
        .map(str::to_owned)
}

/// Reads the model and effort of the conversation `session` (its folder and id) from its
/// transcript, falling back to Claude's settings.
fn read_model_label(session: Option<(PathBuf, String)>) -> Option<ClaudeModelLabel> {
    use std::io::{Read as _, Seek as _, SeekFrom};

    let claude_dir = dirs::home_dir()?.join(".claude");
    let transcript = session.map(|(cwd, session_id)| {
        claude_dir
            .join("projects")
            .join(project_slug(&cwd))
            .join(format!("{session_id}.jsonl"))
    });
    let tail = transcript.and_then(|transcript| {
        let mut file = std::fs::File::open(transcript).ok()?;
        let len = file.metadata().ok()?.len();
        file.seek(SeekFrom::Start(len.saturating_sub(TRANSCRIPT_TAIL_BYTES)))
            .ok()?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).ok()?;
        Some(String::from_utf8_lossy(&bytes).into_owned())
    });
    let settings = std::fs::read_to_string(claude_dir.join("settings.json")).ok();

    // A new session has no transcript yet; it runs with the model and effort from the settings.
    let model = tail
        .as_deref()
        .and_then(|tail| last_string_field(tail, "model"))
        .or_else(|| {
            settings
                .as_deref()
                .and_then(|settings| settings_string(settings, "model"))
        })
        .and_then(|model| model_label(&model))
        .unwrap_or_else(|| "Claude".to_owned());
    let effort = tail
        .as_deref()
        .and_then(|tail| last_string_field(tail, "effort"))
        .or_else(|| {
            settings
                .as_deref()
                .and_then(|settings| settings_string(settings, "effortLevel"))
        })
        .map(|effort| effort_label(&effort));
    Some(ClaudeModelLabel { model, effort })
}

/// The keys that erase everything typed in Claude's prompt: Right arrows to the end of the text,
/// then a Backspace per character. Rows that wrap count a line break each, so the counts can run
/// a little over, which Claude ignores at the ends of the text.
fn erase_prompt_keys(rows: &[String], cursor: Option<(usize, usize)>) -> Vec<&'static [u8]> {
    let row_lengths: Vec<usize> = rows.iter().map(|row| row.chars().count()).collect();
    let line_breaks = row_lengths.len().saturating_sub(1);
    let total = row_lengths.iter().sum::<usize>() + line_breaks;
    let after_cursor = match cursor {
        Some((row, col)) if row < row_lengths.len() => {
            row_lengths[row].saturating_sub(col)
                + row_lengths[row + 1..].iter().sum::<usize>()
                + (row_lengths.len() - 1 - row)
        }
        _ => 0,
    };
    std::iter::repeat_n(CURSOR_RIGHT, after_cursor)
        .chain(std::iter::repeat_n(BACKSPACE, total))
        .collect()
}

/// One line, collapsed whitespace, cut to fit a chip.
fn chip_preview(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= CHIP_PREVIEW_CHARS {
        return line;
    }
    let mut preview: String = line.chars().take(CHIP_PREVIEW_CHARS - 1).collect();
    preview.push('…');
    preview
}

/// Claude's permission mode, as its status line shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PermissionMode {
    /// The arrows or pause sign before the mode's name.
    symbol: String,
    label: String,
}

/// Claude's status line under the prompt, split into what Warp's controls show.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct StatusLine {
    mode: Option<PermissionMode>,
    /// The other bits on the left, like the count of background agents.
    left: Vec<String>,
    /// What Claude right-aligns, like the worktree and branch.
    right: Option<String>,
}

/// "⏵⏵ auto mode on (shift+tab to cycle) · ← 2 agents      ⎇ Worktree · main" → mode "Auto mode",
/// left ["← 2 agents"], right "⎇ Worktree · main".
fn parse_status_line(line: &str) -> StatusLine {
    // Cells Claude never wrote hold NUL rather than a space.
    let line = line.replace('\0', " ");
    let line = line.trim();
    let (left, right) = match line.find("   ") {
        Some(gap) => (&line[..gap], line[gap..].trim()),
        None => (line, ""),
    };
    let mut status = StatusLine {
        right: (!right.is_empty()).then(|| right.to_owned()),
        ..Default::default()
    };
    for segment in left.split(" · ").map(str::trim).filter(|s| !s.is_empty()) {
        let is_mode =
            status.mode.is_none() && (segment.contains(MODE_HINT) || segment.ends_with(" on"));
        if !is_mode {
            status.left.push(segment.to_owned());
            continue;
        }
        let segment = segment.replace(MODE_HINT, "");
        let segment = segment.trim();
        let name_start = segment
            .find(|c: char| c.is_alphanumeric())
            .unwrap_or(segment.len());
        let name = segment[name_start..].trim();
        let name = name.strip_suffix(" on").unwrap_or(name);
        let mut chars = name.chars();
        let label = chars
            .next()
            .map(|first| first.to_uppercase().chain(chars).collect())
            .unwrap_or_default();
        status.mode = Some(PermissionMode {
            symbol: segment[..name_start].trim().to_owned(),
            label,
        });
    }
    status
}

/// What the composer reads off Claude's prompt in the grid.
struct PromptState {
    first_line: String,
    /// The prompt's rows, from the column the text starts at, without trailing blanks. Empty
    /// without text.
    rows: Vec<String>,
    /// Where the cursor is in `rows`, as (row, column).
    cursor: Option<(usize, usize)>,
    status_line: String,
    /// Nothing typed yet: a single line holding at most Claude's dimmed placeholder.
    is_empty: bool,
}

/// Reads the prompt when the card shows it with Claude's status line under it (so no menu is
/// open there).
fn prompt_state(model: &TerminalModel) -> Option<PromptState> {
    let (grid, rows) = if model.is_alt_screen_active() {
        let grid = model.alt_screen().grid_handler();
        (grid, 0..grid.visible_rows())
    } else {
        let output = model.block_list().active_block().output_grid();
        (output.grid_handler(), 0..output.len_displayed())
    };
    let rules = find_prompt_rules(grid, rows)?;
    let status_row = rules.status_row()?;
    let columns = grid.columns();
    let row_text = |row_idx: usize, from: usize| -> Option<String> {
        let row = grid.row(row_idx)?;
        Some((from..columns).map(|col| row[col].c).collect())
    };
    let first_row = grid.row(rules.top + 1)?;
    let is_single_line = rules.bottom == rules.top + 2;
    let only_placeholder = (2..columns).all(|col| {
        let cell = &first_row[col];
        matches!(cell.c, ' ' | '\0') || cell.flags().contains(Flags::DIM)
    });
    let cursor_at_start = grid.cursor_point() == Point::new(rules.top + 1, 2);
    let is_empty = is_single_line && (only_placeholder || cursor_at_start);
    let rows = if is_empty {
        Vec::new()
    } else {
        (rules.top + 1..rules.bottom)
            .filter_map(|row| row_text(row, 2))
            .map(|line| line.replace('\0', " ").trim_end().to_owned())
            .collect()
    };
    let cursor_point = grid.cursor_point();
    let cursor = (rules.top < cursor_point.row && cursor_point.row < rules.bottom).then(|| {
        (
            cursor_point.row - rules.top - 1,
            cursor_point.col.saturating_sub(2),
        )
    });
    Some(PromptState {
        first_line: row_text(rules.top + 1, 2)?,
        rows,
        cursor,
        status_line: row_text(status_row, 0)?,
        is_empty,
    })
}

impl TerminalView {
    /// Holds `text` until the next message is sent.
    pub(super) fn add_claude_mention(&mut self, text: String, ctx: &mut ViewContext<Self>) {
        self.claude_composer.mentions.push(PendingMention {
            text,
            remove_button_mouse_state: Default::default(),
        });
        ctx.notify();
    }

    pub(super) fn remove_claude_mention(&mut self, index: usize, ctx: &mut ViewContext<Self>) {
        if index < self.claude_composer.mentions.len() {
            self.claude_composer.mentions.remove(index);
            ctx.notify();
        }
    }

    /// Sends what's typed in Claude's prompt, like pressing Enter.
    pub(super) fn submit_claude_prompt(&mut self, ctx: &mut ViewContext<Self>) {
        self.write_user_bytes_to_pty(b"\r".to_vec(), ctx);
        self.focus_terminal(ctx);
    }

    /// Stops Claude's current turn, like pressing Escape.
    pub(super) fn interrupt_claude(&mut self, ctx: &mut ViewContext<Self>) {
        self.write_user_bytes_to_pty(b"\x1b".to_vec(), ctx);
        self.focus_terminal(ctx);
    }

    /// Whether the cursor is in the off half of its blink, so it isn't drawn.
    pub(super) fn is_claude_cursor_blinked_off(&self) -> bool {
        self.claude_composer.is_cursor_blinked_off
    }

    /// Shows the cursor and starts its blink over, like a text field does on every key press.
    /// Warp draws the cursor of Claude's prompt, so it blinks it too, when cursor blinking is on.
    pub(super) fn restart_claude_cursor_blink(&mut self, ctx: &mut ViewContext<Self>) {
        if self.claude_composer.is_cursor_blinked_off {
            self.claude_composer.is_cursor_blinked_off = false;
            ctx.notify();
        }
        if !self.runs_claude(ctx) {
            return;
        }
        self.claude_composer.cursor_blink_epoch += 1;
        self.schedule_claude_cursor_blink(ctx);
    }

    fn schedule_claude_cursor_blink(&mut self, ctx: &mut ViewContext<Self>) {
        let epoch = self.claude_composer.cursor_blink_epoch;
        ctx.spawn(Timer::after(CURSOR_BLINK_INTERVAL), move |me, _, ctx| {
            me.blink_claude_cursor(epoch, ctx);
        });
    }

    fn blink_claude_cursor(&mut self, epoch: usize, ctx: &mut ViewContext<Self>) {
        if epoch != self.claude_composer.cursor_blink_epoch {
            return;
        }
        let should_blink = AppEditorSettings::as_ref(ctx).cursor_blink_enabled()
            && ctx.focused_view_id(ctx.window_id()) == Some(self.view_id)
            && self.runs_claude(ctx);
        if !should_blink {
            // The blink stops with the cursor showing.
            if std::mem::take(&mut self.claude_composer.is_cursor_blinked_off) {
                ctx.notify();
            }
            return;
        }
        self.claude_composer.is_cursor_blinked_off = !self.claude_composer.is_cursor_blinked_off;
        ctx.notify();
        self.schedule_claude_cursor_blink(ctx);
    }

    pub(super) fn is_claude_prompt_selected(&self) -> bool {
        self.claude_composer.is_prompt_selected
    }

    /// Selects everything typed in Claude's prompt, as ⌘A does in a text field. Returns whether
    /// there was a prompt with text to select.
    pub(super) fn select_claude_prompt(&mut self, ctx: &mut ViewContext<Self>) -> bool {
        if !self.runs_claude(ctx) {
            return false;
        }
        let has_text = prompt_state(&self.model.lock()).is_some_and(|state| !state.is_empty);
        if has_text {
            self.claude_composer.is_prompt_selected = true;
            self.focus_terminal(ctx);
        }
        has_text
    }

    /// The text of the prompt while it's selected, for ⌘C.
    pub(super) fn selected_claude_prompt_text(&self) -> Option<String> {
        if !self.claude_composer.is_prompt_selected {
            return None;
        }
        prompt_state(&self.model.lock())
            .map(|state| state.rows.join("\n"))
            .filter(|text| !text.is_empty())
    }

    /// Ends the prompt selection, if any. While it was on, a Backspace or Delete erases the
    /// prompt, and typed or pasted text replaces it; returns whether `bytes` was used up that
    /// way. Any other key just ends the selection.
    pub(super) fn replace_selected_claude_prompt(
        &mut self,
        bytes: &[u8],
        ctx: &mut ViewContext<Self>,
    ) -> bool {
        if !std::mem::take(&mut self.claude_composer.is_prompt_selected) {
            return false;
        }
        ctx.notify();
        let is_erase = ERASE_KEYS.contains(&bytes);
        let is_text = bytes.starts_with(BRACKETED_PASTE_START)
            || bytes
                .first()
                .is_some_and(|&byte| byte >= 0x20 && byte != 0x7f);
        if !is_erase && !is_text {
            return false;
        }
        let Some(state) = prompt_state(&self.model.lock()) else {
            return false;
        };
        let mut keys: VecDeque<Vec<u8>> = erase_prompt_keys(&state.rows, state.cursor)
            .into_iter()
            .map(<[u8]>::to_vec)
            .collect();
        if !is_erase {
            keys.push_back(bytes.to_vec());
        }
        self.send_keys_one_by_one(keys, ctx);
        true
    }

    /// Writes `keys` a moment apart, so Claude reads each as a key press rather than a paste.
    fn send_keys_one_by_one(&mut self, mut keys: VecDeque<Vec<u8>>, ctx: &mut ViewContext<Self>) {
        let Some(key) = keys.pop_front() else {
            return;
        };
        self.write_user_bytes_to_pty(key, ctx);
        if !keys.is_empty() {
            ctx.spawn(Timer::after(KEY_SPACING), move |me, _, ctx| {
                me.send_keys_one_by_one(keys, ctx);
            });
        }
    }

    /// Ends the prompt selection without touching the prompt, like a click elsewhere does.
    pub(super) fn deselect_claude_prompt(&mut self, ctx: &mut ViewContext<Self>) {
        if std::mem::take(&mut self.claude_composer.is_prompt_selected) {
            ctx.notify();
        }
    }

    /// Moves Claude to its next permission mode, like pressing Shift+Tab.
    pub(super) fn cycle_claude_mode(&mut self, ctx: &mut ViewContext<Self>) {
        self.write_user_bytes_to_pty(SHIFT_TAB.to_vec(), ctx);
        self.focus_terminal(ctx);
    }

    pub(super) fn toggle_claude_model_menu(&mut self, ctx: &mut ViewContext<Self>) {
        if self.claude_composer.picker.is_some() {
            self.handle_claude_picker_action(ClaudePickerAction::Close, ctx);
            return;
        }
        self.claude_composer.picker = Some(PickerPage::Effort);
        ctx.notify();
    }

    pub(super) fn handle_claude_picker_action(
        &mut self,
        action: ClaudePickerAction,
        ctx: &mut ViewContext<Self>,
    ) {
        let choices = match action {
            ClaudePickerAction::ShowModels => {
                self.claude_composer.picker = Some(PickerPage::Models);
                Vec::new()
            }
            ClaudePickerAction::ChooseModel(index) => {
                let mut choices = self.close_claude_picker();
                choices.extend(
                    MODELS
                        .get(index)
                        .map(|&(label, id)| ModelChoice::Model { label, id }),
                );
                choices
            }
            ClaudePickerAction::PressSlider(x) | ClaudePickerAction::DragSlider(x) => {
                self.claude_composer.slider_settle = None;
                self.claude_composer.slider_drag = self.slider_position_at(x, ctx);
                ctx.notify();
                return;
            }
            ClaudePickerAction::ReleaseSlider => {
                let Some(position) = self.claude_composer.slider_drag.take() else {
                    return;
                };
                let index = nearest_effort(position);
                self.claude_composer.pending_effort = Some(index);
                self.claude_composer.slider_settle = Some(SliderSettle {
                    from: position,
                    to: effort_position(index),
                    started_at: Instant::now(),
                });
                self.animate_slider_settle(ctx);
                return;
            }
            ClaudePickerAction::ResetEffort => {
                self.close_claude_picker();
                vec![ModelChoice::Effort(DEFAULT_EFFORT)]
            }
            ClaudePickerAction::Close => self.close_claude_picker(),
        };
        ctx.notify();
        self.focus_terminal(ctx);
        self.apply_claude_model_choices(choices, ctx);
    }

    /// Closes the picker, and returns the effort picked on its slider, which Claude switches to
    /// now: typed while the picker is open, `/effort` would redraw Claude's prompt under it.
    fn close_claude_picker(&mut self) -> Vec<ModelChoice> {
        let composer = &mut self.claude_composer;
        composer.picker = None;
        composer.slider_drag = None;
        composer.slider_settle = None;
        let pending = composer.pending_effort.take();
        pending
            .filter(|&index| Some(index) != self.current_effort_index())
            .map(|index| ModelChoice::Effort(EFFORTS[index]))
            .into_iter()
            .collect()
    }

    /// Runs the first choice now and the next ones a moment apart, so each command is done
    /// before the next is typed.
    fn apply_claude_model_choices(
        &mut self,
        choices: Vec<ModelChoice>,
        ctx: &mut ViewContext<Self>,
    ) {
        let mut choices = choices.into_iter();
        let Some(first) = choices.next() else {
            return;
        };
        self.apply_claude_model_choice(first, ctx);
        let rest: Vec<_> = choices.collect();
        if !rest.is_empty() {
            ctx.spawn(Timer::after(COMMAND_SPACING), move |me, _, ctx| {
                me.apply_claude_model_choices(rest, ctx);
            });
        }
    }

    /// The level of the effort Claude runs with, when it's one of `EFFORTS`.
    fn current_effort_index(&self) -> Option<usize> {
        let effort = self.claude_composer.model.as_ref()?.effort.as_deref()?;
        EFFORTS
            .iter()
            .position(|&level| effort_label(level) == effort)
    }

    /// The knob position for window x `x` on the effort slider.
    fn slider_position_at(&self, x: f32, ctx: &ViewContext<Self>) -> Option<f32> {
        let track = ctx.element_position_by_id(self.claude_effort_track_position_id())?;
        Some(slider_position(x, track.min_x(), track.max_x()))
    }

    fn claude_effort_track_position_id(&self) -> String {
        format!("terminal_view:claude_effort_track_{}", self.view_id)
    }

    /// Re-renders every frame until the knob has settled.
    fn animate_slider_settle(&mut self, ctx: &mut ViewContext<Self>) {
        ctx.notify();
        if self
            .claude_composer
            .slider_settle
            .is_some_and(|settle| settle.position().is_none())
        {
            self.claude_composer.slider_settle = None;
            return;
        }
        ctx.spawn(Timer::after(SLIDER_FRAME), |me, _, ctx| {
            if me.claude_composer.slider_settle.is_some() {
                me.animate_slider_settle(ctx);
            }
        });
    }

    /// Where the slider's knob is drawn: under the cursor while held, gliding while it settles,
    /// and otherwise on the current level.
    fn slider_knob_position(&self) -> Option<f32> {
        self.claude_composer
            .slider_drag
            .or_else(|| {
                self.claude_composer
                    .slider_settle
                    .and_then(|settle| settle.position())
            })
            .or_else(|| {
                self.claude_composer
                    .pending_effort
                    .or_else(|| self.current_effort_index())
                    .map(effort_position)
            })
    }

    /// Runs Claude's `/model` or `/effort` for `choice`. Typed into a prompt that holds text, or
    /// while Claude works, the command would land in the middle of that, so it's refused instead.
    fn apply_claude_model_choice(&mut self, choice: ModelChoice, ctx: &mut ViewContext<Self>) {
        let is_busy = CLIAgentSessionsModel::as_ref(ctx)
            .session(self.view_id)
            .is_some_and(|session| {
                session.is_working_on_prompt()
                    || matches!(session.status, CLIAgentSessionStatus::Blocked { .. })
            });
        let is_prompt_empty = prompt_state(&self.model.lock()).is_some_and(|state| state.is_empty);
        let refusal = if is_busy {
            Some("O Claude está ocupado. Troque o modelo quando ele terminar.")
        } else if !is_prompt_empty {
            Some("Envie ou apague o texto do prompt para trocar o modelo.")
        } else {
            None
        };
        if let Some(message) = refusal {
            let window_id = ctx.window_id();
            ToastStack::handle(ctx).update(ctx, |toast_stack, ctx| {
                toast_stack.add_ephemeral_toast(
                    DismissibleToast::error(message.to_owned()),
                    window_id,
                    ctx,
                );
            });
            return;
        }

        let command = match choice {
            ModelChoice::Model { label, id } => {
                if let Some(model) = &mut self.claude_composer.model {
                    model.model = label.to_owned();
                }
                format!("/model {id}")
            }
            ModelChoice::Effort(effort) => {
                if let Some(model) = &mut self.claude_composer.model {
                    model.effort = (effort != DEFAULT_EFFORT).then(|| effort_label(effort));
                }
                // Re-read the effort Claude settles on for "auto".
                self.claude_composer.model_refreshed_at = None;
                format!("/effort {effort}")
            }
        };
        #[cfg(feature = "local_tty")]
        self.submit_text_to_cli_agent_pty(command, ctx);
        #[cfg(not(feature = "local_tty"))]
        let _ = command;
        ctx.notify();
    }

    /// When `bytes` is the Enter that sends a message to Claude, pastes the pending mentions as
    /// quotes after the typed text and submits a moment later. Returns whether it took over the
    /// Enter.
    pub(super) fn submit_with_claude_mentions(
        &mut self,
        bytes: &[u8],
        ctx: &mut ViewContext<Self>,
    ) -> bool {
        if bytes != b"\r" || self.claude_composer.mentions.is_empty() || !self.runs_claude(ctx) {
            return false;
        }
        // A slash command or a shell command (`!`) takes no quotes.
        let accepts_quotes = prompt_state(&self.model.lock()).is_some_and(|state| {
            let text = state.first_line.trim_start();
            !text.starts_with('/') && !text.starts_with('!')
        });
        if !accepts_quotes {
            return false;
        }

        let quotes = std::mem::take(&mut self.claude_composer.mentions)
            .into_iter()
            .map(|mention| quote(&mention.text).trim_end().to_owned())
            .collect::<Vec<_>>()
            .join("\n\n");
        let mut paste = Vec::with_capacity(
            BRACKETED_PASTE_START.len() + quotes.len() + 2 + BRACKETED_PASTE_END.len(),
        );
        paste.extend_from_slice(BRACKETED_PASTE_START);
        paste.extend_from_slice(b"\n\n");
        paste.extend_from_slice(quotes.as_bytes());
        paste.extend_from_slice(BRACKETED_PASTE_END);
        self.write_user_bytes_to_pty(paste, ctx);
        ctx.spawn(Timer::after(SUBMIT_DELAY), |me, _, ctx| {
            me.write_user_bytes_to_pty(b"\r".to_vec(), ctx);
        });
        ctx.notify();
        true
    }

    /// Re-reads the model and effort of the Claude session here, at most every few seconds.
    pub(super) fn refresh_claude_model(&mut self, ctx: &mut ViewContext<Self>) {
        if self
            .claude_composer
            .model_refreshed_at
            .is_some_and(|at| at.elapsed() < MODEL_REFRESH_INTERVAL)
        {
            return;
        }
        let session = CLIAgentSessionsModel::as_ref(ctx)
            .session(self.view_id)
            .and_then(|session| {
                let context = &session.session_context;
                Some((
                    PathBuf::from(context.cwd.as_deref()?),
                    context.session_id.clone()?,
                ))
            });
        self.claude_composer.model_refreshed_at = Some(Instant::now());
        ctx.spawn(
            async move { read_model_label(session) },
            |me, label, ctx| {
                if label.is_some() && me.claude_composer.model != label {
                    me.claude_composer.model = label;
                    ctx.notify();
                }
            },
        );
    }

    /// Save-position id of the model chip, which the model menu opens above.
    fn claude_model_chip_position_id(&self) -> String {
        format!("terminal_view:claude_model_chip_{}", self.view_id)
    }

    /// The composer controls and where they go: the mention chips on top of the card, and in the
    /// controls row the model, mode and status bits on the left and the send button on the right.
    pub(super) fn render_claude_composer(
        &self,
        model: &TerminalModel,
        app: &AppContext,
    ) -> Vec<(Box<dyn Element>, OffsetPositioning)> {
        if !self.runs_claude(app) {
            return Vec::new();
        }
        let Some(prompt) = prompt_state(model) else {
            return Vec::new();
        };
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let is_dark = ClaudePalette::current(app).map_or_else(
            || theme.inferred_color_scheme() == ColorScheme::LightOnDark,
            ClaudePalette::is_dark,
        );
        let style = ControlStyle {
            font_family: crate::appearance::system_ui_font_family_cached(app)
                .unwrap_or_else(|| appearance.ui_font_family()),
            strong_text: theme.main_text_color(theme.background()),
            text: theme.sub_text_color(theme.background()),
            hover: warp_core::ui::theme::color::internal_colors::fg_overlay_2(theme),
            divider: warp_core::ui::theme::color::internal_colors::fg_overlay_3(theme),
        };
        let mut controls = Vec::new();

        if !self.claude_composer.mentions.is_empty() {
            controls.push((
                self.render_mention_chips(is_dark, style),
                OffsetPositioning::offset_from_save_position_element(
                    self.claude_card_position_id(),
                    vec2f(CHIPS_LEFT_INSET, -CHIPS_OFFSET),
                    PositionedElementOffsetBounds::Unbounded,
                    PositionedElementAnchor::TopLeft,
                    ChildAnchor::BottomLeft,
                ),
            ));
        }

        let status = parse_status_line(&prompt.status_line);
        let controls_position_id = self.claude_controls_position_id();
        controls.push((
            self.render_left_controls(&status, style),
            OffsetPositioning::offset_from_save_position_element(
                controls_position_id.clone(),
                vec2f(-CONTROL_HORIZONTAL_PADDING, 0.),
                PositionedElementOffsetBounds::Unbounded,
                PositionedElementAnchor::MiddleLeft,
                ChildAnchor::MiddleLeft,
            ),
        ));

        let is_working = CLIAgentSessionsModel::as_ref(app)
            .session(self.view_id)
            .is_some_and(|session| session.is_working_on_prompt());
        let mut right = Flex::row()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(8.);
        if let Some(text) = status.right {
            right.add_child(render_label(text, style.text, style.font_family));
        }
        right.add_child(render_send_button(
            self.claude_composer.send_button_mouse_state.clone(),
            is_working,
        ));
        controls.push((
            right.finish(),
            OffsetPositioning::offset_from_save_position_element(
                controls_position_id,
                vec2f(0., 0.),
                PositionedElementOffsetBounds::Unbounded,
                PositionedElementAnchor::MiddleRight,
                ChildAnchor::MiddleRight,
            ),
        ));

        controls
    }

    /// The model and effort picker over the model chip, which floats above every other layer
    /// like the footer's menus. Clicking outside it closes it.
    pub(super) fn render_claude_picker(
        &self,
        app: &AppContext,
    ) -> Option<(Box<dyn Element>, OffsetPositioning)> {
        let page = self.claude_composer.picker?;
        if !self.runs_claude(app) {
            return None;
        }
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let background = theme.surface_2();
        let colors = PickerColors {
            font_family: crate::appearance::system_ui_font_family_cached(app)
                .unwrap_or_else(|| appearance.ui_font_family()),
            text: theme.main_text_color(background),
            sub_text: theme.sub_text_color(background),
            hover: warp_core::ui::theme::color::internal_colors::fg_overlay_2(theme),
            track: warp_core::ui::theme::color::internal_colors::fg_overlay_2(theme),
            on_fill: WarpThemeFill::Solid(BUTTON_ICON_COLOR).with_opacity(60),
        };
        let content = match page {
            PickerPage::Effort => self.render_effort_page(colors),
            PickerPage::Models => self.render_models_page(colors),
        };
        let card = ConstrainedBox::new(
            Container::new(content)
                .with_uniform_padding(PICKER_PADDING)
                .with_background(background)
                .with_border(Border::all(PICKER_BORDER).with_border_fill(
                    warp_core::ui::theme::color::internal_colors::fg_overlay_3(theme),
                ))
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(PICKER_CORNER_RADIUS)))
                .with_drop_shadow(DropShadow::default())
                .finish(),
        )
        .with_width(PICKER_WIDTH)
        .finish();
        let picker = Dismiss::new(card)
            .on_dismiss(|ctx, _| {
                ctx.dispatch_typed_action(TerminalAction::ClaudePicker(ClaudePickerAction::Close));
            })
            .finish();
        Some((
            picker,
            OffsetPositioning::offset_from_save_position_element(
                self.claude_model_chip_position_id(),
                vec2f(0., -PICKER_OFFSET),
                PositionedElementOffsetBounds::WindowByPosition,
                PositionedElementAnchor::TopLeft,
                ChildAnchor::BottomLeft,
            ),
        ))
    }

    /// The effort's name, the model (which opens the list of models) and the slider, with a
    /// button that goes back to the model's default effort.
    fn render_effort_page(&self, colors: PickerColors) -> Box<dyn Element> {
        let states = &self.claude_composer.picker_mouse_states;
        let knob = self.slider_knob_position();
        // While the knob is held, the title follows the level it would settle on.
        let title = self
            .claude_composer
            .slider_drag
            .map(nearest_effort)
            .or(self.claude_composer.pending_effort)
            .or_else(|| self.current_effort_index())
            .map(|index| effort_label(EFFORTS[index]))
            .unwrap_or_else(|| "Padrão".to_owned());
        let model_name = self
            .claude_composer
            .model
            .as_ref()
            .map_or_else(|| "Claude".to_owned(), |model| model.model.clone());

        let model_link = Hoverable::new(states.model_link.clone(), move |state| {
            let color = if state.is_hovered() {
                colors.text
            } else {
                colors.sub_text
            };
            Flex::row()
                .with_main_axis_size(MainAxisSize::Min)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(2.)
                .with_child(
                    Text::new_inline(model_name, colors.font_family, PICKER_FONT_SIZE)
                        .with_color(color.into())
                        .finish(),
                )
                .with_child(render_svg(CHEVRON_RIGHT_ICON, color, 10.))
                .finish()
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(|ctx, _, _| {
            ctx.dispatch_typed_action(TerminalAction::ClaudePicker(ClaudePickerAction::ShowModels));
        })
        .finish();
        let heading = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(2.)
            .with_child(
                Text::new_inline(title, colors.font_family, PICKER_TITLE_FONT_SIZE)
                    .with_style(Properties::default().weight(Weight::Medium))
                    .with_color(SEND_COLOR)
                    .finish(),
            )
            .with_child(model_link)
            .finish();
        let reset = Hoverable::new(states.reset.clone(), move |state| {
            let color = if state.is_hovered() {
                colors.text
            } else {
                colors.sub_text
            };
            render_svg(RESET_ICON, color, PICKER_ICON_SIZE)
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(|ctx, _, _| {
            ctx.dispatch_typed_action(TerminalAction::ClaudePicker(
                ClaudePickerAction::ResetEffort,
            ));
        })
        .finish();
        let mut header = Stack::new().with_child(Align::new(heading).finish());
        header.add_positioned_child(
            reset,
            OffsetPositioning::offset_from_parent(
                vec2f(0., 0.),
                warpui::elements::ParentOffsetBounds::ParentByPosition,
                warpui::elements::ParentAnchor::TopRight,
                ChildAnchor::TopRight,
            ),
        );

        Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(10.)
            .with_child(header.finish())
            .with_child(Align::new(self.render_effort_slider(knob, colors)).finish())
            .finish()
    }

    /// A track with a dot per effort level, filled up to the knob. The knob follows the cursor
    /// while it's held, and glides onto the nearest level when let go.
    fn render_effort_slider(&self, knob: Option<f32>, colors: PickerColors) -> Box<dyn Element> {
        // The picker has a fixed width, so the track's geometry is known up front and the fill
        // can be one fully rounded shape, a pill at every position.
        let span = SLIDER_WIDTH - 2. * SLIDER_INSET;
        let knob_middle = knob.map(|knob| SLIDER_INSET + knob.clamp(0., 1.) * span);
        let pill = |width: f32, height: f32, color: WarpThemeFill| {
            ConstrainedBox::new(
                Container::new(Empty::new().finish())
                    .with_background(color)
                    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(height / 2.)))
                    .finish(),
            )
            .with_width(width)
            .with_height(height)
            .finish()
        };

        let mut layers = Stack::new().with_child(pill(SLIDER_WIDTH, SLIDER_HEIGHT, colors.track));
        if let Some(knob) = knob {
            layers.add_child(pill(
                slider_fill_width(knob),
                SLIDER_HEIGHT,
                WarpThemeFill::Solid(SEND_COLOR),
            ));
        }
        for index in 0..EFFORTS.len() {
            let x = SLIDER_INSET + effort_position(index) * span;
            let is_filled = knob_middle.is_some_and(|knob_middle| x < knob_middle);
            let color = if is_filled {
                colors.on_fill
            } else {
                colors.sub_text
            };
            layers.add_child(
                Container::new(pill(SLIDER_DOT_SIZE, SLIDER_DOT_SIZE, color))
                    .with_padding_left(x - SLIDER_DOT_SIZE / 2.)
                    .with_padding_top((SLIDER_HEIGHT - SLIDER_DOT_SIZE) / 2.)
                    .finish(),
            );
        }
        if let Some(knob_middle) = knob_middle {
            layers.add_child(
                Container::new(pill(
                    SLIDER_KNOB_SIZE,
                    SLIDER_KNOB_SIZE,
                    WarpThemeFill::Solid(BUTTON_ICON_COLOR),
                ))
                .with_padding_left(knob_middle - SLIDER_KNOB_SIZE / 2.)
                .with_padding_top((SLIDER_HEIGHT - SLIDER_KNOB_SIZE) / 2.)
                .finish(),
            );
        }
        let track = SavePosition::new(
            ConstrainedBox::new(layers.finish())
                .with_width(SLIDER_WIDTH)
                .with_height(SLIDER_HEIGHT)
                .finish(),
            &self.claude_effort_track_position_id(),
        )
        .finish();
        let blank = || Empty::new().finish();

        // Pressing jumps the knob under the cursor; dragging, even past the ends, moves it along.
        let drag_state = self.claude_composer.slider_drag_state.clone();
        let draggable = Draggable::new(self.claude_composer.slider_drag_state.clone(), track)
            .with_drag_axis(DragAxis::HorizontalOnly)
            .with_alternate_drag_element(blank())
            .with_keep_original_visible(true)
            .on_drag(move |ctx, _, _, _| {
                if let Some(mouse) = drag_state.dragging_mouse_position() {
                    ctx.dispatch_typed_action(TerminalAction::ClaudePicker(
                        ClaudePickerAction::DragSlider(mouse.x()),
                    ));
                }
            })
            .on_drop(|ctx, _, _, _| {
                ctx.dispatch_typed_action(TerminalAction::ClaudePicker(
                    ClaudePickerAction::ReleaseSlider,
                ));
            })
            .finish();
        let pressable = EventHandler::new(draggable)
            .with_always_handle()
            .on_left_mouse_down(|ctx, _, position| {
                ctx.dispatch_typed_action(TerminalAction::ClaudePicker(
                    ClaudePickerAction::PressSlider(position.x()),
                ));
                DispatchEventResult::StopPropagation
            })
            .on_left_mouse_up(|ctx, _, _| {
                ctx.dispatch_typed_action(TerminalAction::ClaudePicker(
                    ClaudePickerAction::ReleaseSlider,
                ));
                DispatchEventResult::StopPropagation
            })
            .finish();
        Hoverable::new(
            self.claude_composer.picker_mouse_states.slider.clone(),
            |_| pressable,
        )
        .with_cursor(Cursor::PointingHand)
        .finish()
    }

    /// "Selecionar modelo" and the models, the current one checked.
    fn render_models_page(&self, colors: PickerColors) -> Box<dyn Element> {
        let current = self
            .claude_composer
            .model
            .as_ref()
            .map(|model| model.model.as_str());
        let mut list = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(2.)
            .with_child(
                Container::new(
                    Text::new_inline("Selecionar modelo", colors.font_family, PICKER_FONT_SIZE)
                        .with_color(colors.sub_text.into())
                        .finish(),
                )
                .with_horizontal_padding(8.)
                .with_padding_bottom(6.)
                .finish(),
            );
        for (index, (label, _)) in MODELS.iter().enumerate() {
            let is_current = current == Some(*label);
            let label = *label;
            let item = Hoverable::new(
                self.claude_composer.picker_mouse_states.models[index].clone(),
                move |state| {
                    let mut row = Flex::row()
                        .with_main_axis_size(MainAxisSize::Max)
                        .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                        .with_cross_axis_alignment(CrossAxisAlignment::Center)
                        .with_child(
                            Text::new_inline(label, colors.font_family, PICKER_FONT_SIZE + 1.)
                                .with_color(colors.text.into())
                                .finish(),
                        );
                    if is_current {
                        row.add_child(render_svg(CHECK_ICON, colors.text, PICKER_ICON_SIZE));
                    }
                    let mut item =
                        Container::new(ConstrainedBox::new(row.finish()).with_height(22.).finish())
                            .with_horizontal_padding(8.)
                            .with_vertical_padding(5.)
                            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(8.)));
                    if state.is_hovered() {
                        item = item.with_background(colors.hover);
                    }
                    item.finish()
                },
            )
            .with_cursor(Cursor::PointingHand)
            .on_click(move |ctx, _, _| {
                ctx.dispatch_typed_action(TerminalAction::ClaudePicker(
                    ClaudePickerAction::ChooseModel(index),
                ));
            })
            .finish();
            list.add_child(item);
        }
        list.finish()
    }

    fn render_mention_chips(&self, is_dark: bool, style: ControlStyle) -> Box<dyn Element> {
        let mut chips = Flex::row()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(CHIP_GAP);
        for (index, mention) in self
            .claude_composer
            .mentions
            .iter()
            .enumerate()
            .take(MAX_VISIBLE_CHIPS)
        {
            chips.add_child(render_mention_chip(
                index,
                mention,
                is_dark,
                style.font_family,
            ));
        }
        let hidden = self
            .claude_composer
            .mentions
            .len()
            .saturating_sub(MAX_VISIBLE_CHIPS);
        if hidden > 0 {
            chips.add_child(render_label(
                format!("+{hidden}"),
                style.text,
                style.font_family,
            ));
        }
        chips.finish()
    }

    /// The model chip, then the permission mode and Claude's other status bits.
    fn render_left_controls(&self, status: &StatusLine, style: ControlStyle) -> Box<dyn Element> {
        let mut left = Flex::row()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(CONTROL_GAP);

        let (model_name, effort) = match &self.claude_composer.model {
            Some(label) => (label.model.clone(), label.effort.clone()),
            None => ("Claude".to_owned(), None),
        };
        let model_chip = render_model_chip(
            self.claude_composer.model_chip_mouse_state.clone(),
            model_name,
            effort,
            style,
        );
        left.add_child(
            SavePosition::new(model_chip, &self.claude_model_chip_position_id()).finish(),
        );

        if let Some(mode) = &status.mode {
            left.add_child(render_divider(style));
            let color = mode_color(&mode.label).unwrap_or_else(|| style.text.into_solid());
            let text = if mode.symbol.is_empty() {
                mode.label.clone()
            } else {
                format!("{} {}", mode.symbol, mode.label)
            };
            left.add_child(render_control(
                self.claude_composer.mode_pill_mouse_state.clone(),
                TerminalAction::CycleClaudeMode,
                style,
                None,
                text,
                WarpThemeFill::Solid(color),
                false,
            ));
        }
        for bit in &status.left {
            left.add_child(render_divider(style));
            left.add_child(
                Container::new(render_label(bit.clone(), style.text, style.font_family))
                    .with_horizontal_padding(CONTROL_HORIZONTAL_PADDING)
                    .finish(),
            );
        }
        left.finish()
    }
}

/// Colors shared by the composer controls, resolved once per render.
#[derive(Clone, Copy)]
struct ControlStyle {
    font_family: FamilyId,
    /// The model's name in the model chip.
    strong_text: WarpThemeFill,
    text: WarpThemeFill,
    hover: WarpThemeFill,
    divider: WarpThemeFill,
}

/// The color of a permission mode: amber for the modes that act without asking, teal for plan.
fn mode_color(label: &str) -> Option<ColorU> {
    let label = label.to_lowercase();
    if label.contains("plan") {
        Some(PLAN_MODE_COLOR)
    } else if label.contains("auto") || label.contains("accept") || label.contains("bypass") {
        Some(AUTO_MODE_COLOR)
    } else {
        None
    }
}

fn render_icon(icon: WarpIcon, color: WarpThemeFill, size: f32) -> Box<dyn Element> {
    ConstrainedBox::new(icon.to_warpui_icon(color).finish())
        .with_width(size)
        .with_height(size)
        .finish()
}

fn render_label(text: String, color: WarpThemeFill, font_family: FamilyId) -> Box<dyn Element> {
    Text::new_inline(text, font_family, FONT_SIZE)
        .with_color(color.into())
        .finish()
}

fn render_divider(style: ControlStyle) -> Box<dyn Element> {
    Container::new(
        ConstrainedBox::new(
            Container::new(warpui::elements::Empty::new().finish())
                .with_background(style.divider)
                .finish(),
        )
        .with_width(1.)
        .with_height(DIVIDER_HEIGHT)
        .finish(),
    )
    .with_horizontal_padding(4.)
    .finish()
}

/// A borderless control like a chat composer's: text with an optional icon and chevron, filled
/// on hover.
fn render_control(
    mouse_state: MouseStateHandle,
    action: TerminalAction,
    style: ControlStyle,
    icon: Option<(WarpIcon, WarpThemeFill)>,
    text: String,
    text_color: WarpThemeFill,
    has_chevron: bool,
) -> Box<dyn Element> {
    Hoverable::new(mouse_state, move |state| {
        let mut content = Flex::row()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(6.);
        if let Some((icon, color)) = icon {
            content.add_child(render_icon(icon, color, ICON_SIZE));
        }
        content.add_child(
            Text::new_inline(text.clone(), style.font_family, FONT_SIZE)
                .with_style(Properties::default().weight(Weight::Medium))
                .with_color(text_color.into())
                .finish(),
        );
        if has_chevron {
            content.add_child(render_icon(WarpIcon::ChevronDown, style.text, CHEVRON_SIZE));
        }
        let mut control = Container::new(
            ConstrainedBox::new(content.finish())
                .with_height(CONTROL_HEIGHT)
                .finish(),
        )
        .with_horizontal_padding(CONTROL_HORIZONTAL_PADDING)
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(8.)));
        if state.is_hovered() {
            control = control.with_background(style.hover);
        }
        control.finish()
    })
    .with_cursor(Cursor::PointingHand)
    .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
    .finish()
}

/// Colors and font of the model and effort picker, resolved once per render.
#[derive(Clone, Copy)]
struct PickerColors {
    font_family: FamilyId,
    text: WarpThemeFill,
    sub_text: WarpThemeFill,
    hover: WarpThemeFill,
    track: WarpThemeFill,
    /// The dots over the slider's fill.
    on_fill: WarpThemeFill,
}

/// The Claude logo, the model in the text color and the effort a step dimmer, then a chevron.
fn render_model_chip(
    mouse_state: MouseStateHandle,
    model: String,
    effort: Option<String>,
    style: ControlStyle,
) -> Box<dyn Element> {
    Hoverable::new(mouse_state, move |state| {
        let mut content = Flex::row()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(5.)
            .with_child(render_icon(
                WarpIcon::ClaudeLogo,
                WarpThemeFill::Solid(SEND_COLOR),
                ICON_SIZE,
            ))
            .with_child(
                Text::new_inline(model.clone(), style.font_family, FONT_SIZE)
                    .with_style(Properties::default().weight(Weight::Medium))
                    .with_color(style.strong_text.into())
                    .finish(),
            );
        if let Some(effort) = &effort {
            content.add_child(
                Text::new_inline(effort.clone(), style.font_family, FONT_SIZE)
                    .with_color(style.text.into())
                    .finish(),
            );
        }
        content.add_child(render_icon(WarpIcon::ChevronDown, style.text, CHEVRON_SIZE));
        let mut control = Container::new(
            ConstrainedBox::new(content.finish())
                .with_height(CONTROL_HEIGHT)
                .finish(),
        )
        .with_horizontal_padding(CONTROL_HORIZONTAL_PADDING)
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(8.)));
        if state.is_hovered() {
            control = control.with_background(style.hover);
        }
        control.finish()
    })
    .with_cursor(Cursor::PointingHand)
    .on_click(|ctx, _, _| ctx.dispatch_typed_action(TerminalAction::ToggleClaudeModelMenu))
    .finish()
}

/// One of the picker's outline icons, from `bundled/svg`.
fn render_svg(path: &'static str, color: WarpThemeFill, size: f32) -> Box<dyn Element> {
    ConstrainedBox::new(SvgIcon::new(path, color.into_solid()).finish())
        .with_width(size)
        .with_height(size)
        .finish()
}

fn with_alpha(color: ColorU, alpha: u8) -> ColorU {
    ColorU { a: alpha, ..color }
}

/// A quote chip: icon, the start of the mentioned text and a button that drops it.
fn render_mention_chip(
    index: usize,
    mention: &PendingMention,
    is_dark: bool,
    font_family: FamilyId,
) -> Box<dyn Element> {
    let text_color = if is_dark {
        ColorU {
            r: 0xce,
            g: 0xdd,
            b: 0xf4,
            a: 0xff,
        }
    } else {
        ColorU {
            r: 0x1d,
            g: 0x4f,
            b: 0x9a,
            a: 0xff,
        }
    };
    let preview = chip_preview(&mention.text);
    let remove_button = Hoverable::new(mention.remove_button_mouse_state.clone(), move |state| {
        let alpha = if state.is_hovered() { 0xff } else { 0xa0 };
        render_icon(
            WarpIcon::X,
            WarpThemeFill::Solid(with_alpha(text_color, alpha)),
            10.,
        )
    })
    .with_cursor(Cursor::PointingHand)
    .on_click(move |ctx, _, _| {
        ctx.dispatch_typed_action(TerminalAction::RemoveClaudeMention(index));
    })
    .finish();

    let content = Flex::row()
        .with_main_axis_size(MainAxisSize::Min)
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(6.)
        .with_child(render_icon(
            WarpIcon::MessageText,
            WarpThemeFill::Solid(with_alpha(text_color, 0xc0)),
            ICON_SIZE,
        ))
        .with_child(
            Text::new_inline(preview, font_family, FONT_SIZE)
                .with_style(Properties::default().weight(Weight::Medium))
                .with_color(text_color)
                .finish(),
        )
        .with_child(remove_button)
        .finish();
    let background = if is_dark {
        ColorU {
            r: 0x1b,
            g: 0x26,
            b: 0x38,
            a: 0xff,
        }
    } else {
        ColorU {
            r: 0xe6,
            g: 0xef,
            b: 0xfc,
            a: 0xff,
        }
    };
    Container::new(
        ConstrainedBox::new(content)
            .with_height(CHIP_HEIGHT)
            .finish(),
    )
    .with_horizontal_padding(CHIP_HORIZONTAL_PADDING)
    .with_background(WarpThemeFill::Solid(background))
    .with_border(Border::all(1.).with_border_color(with_alpha(QUOTE_ACCENT, 0x80)))
    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(CHIP_HEIGHT / 2.)))
    .finish()
}

/// The round send button, which stops Claude's turn while it works.
fn render_send_button(mouse_state: MouseStateHandle, is_working: bool) -> Box<dyn Element> {
    let (icon, color, action) = if is_working {
        (
            WarpIcon::StopFilled,
            STOP_COLOR,
            TerminalAction::InterruptClaude,
        )
    } else {
        (
            WarpIcon::ArrowUp,
            SEND_COLOR,
            TerminalAction::SubmitClaudePrompt,
        )
    };
    Hoverable::new(mouse_state, move |state| {
        let alpha = if state.is_hovered() { 0xff } else { 0xe6 };
        Container::new(render_icon(
            icon,
            WarpThemeFill::Solid(BUTTON_ICON_COLOR),
            SEND_ICON_SIZE,
        ))
        .with_padding(Padding::uniform((SEND_BUTTON_SIZE - SEND_ICON_SIZE) / 2.))
        .with_background(WarpThemeFill::Solid(with_alpha(color, alpha)))
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(
            SEND_BUTTON_SIZE / 2.,
        )))
        .finish()
    })
    .with_cursor(Cursor::PointingHand)
    .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
    .finish()
}

#[cfg(test)]
#[path = "claude_composer_tests.rs"]
mod tests;
