//! Controls that float over the card around Claude Code's prompt (see `prompt_card`), like a chat
//! app's composer. Claude's status line gives way to Warp's controls row: the model and effort
//! (a menu switches them), the permission mode (a click cycles it, like Shift+Tab), Claude's other
//! status bits and a send button (stop, while Claude works). Chips for the selections mentioned in
//! the next message sit on top of the card.
//!
//! Mentions stay out of Claude's prompt until the message is sent: pressing Enter with the prompt
//! showing pastes them as quotes after the typed text, then submits.

use std::path::PathBuf;
use std::time::Duration;

use instant::Instant;
use pathfinder_color::ColorU;
use warp_core::ui::Icon as WarpIcon;
use warp_core::ui::theme::{ColorScheme, Fill as WarpThemeFill};
use warp_terminal::model::escape_sequences::{BRACKETED_PASTE_END, BRACKETED_PASTE_START};
use warpui::r#async::Timer;
use warpui::elements::{
    Border, ChildAnchor, ChildView, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment,
    Element, Flex, Hoverable, MainAxisSize, MouseStateHandle, OffsetPositioning, Padding,
    ParentElement, PositionedElementAnchor, PositionedElementOffsetBounds, Radius, SavePosition,
    Text,
};
use warpui::fonts::{FamilyId, Properties, Weight};
use warpui::geometry::vector::vec2f;
use warpui::platform::Cursor;
use warpui::{AppContext, SingletonEntity, ViewContext, ViewHandle};

use super::claude_look::ClaudePalette;
use super::claude_mention::quote;
use super::{TerminalAction, TerminalView};
use crate::appearance::Appearance;
use crate::claude_threads::project_slug;
use crate::context_chips::display_menu::{
    ChipMenuType, DisplayChipMenu, GenericMenuItem, PromptDisplayMenuEvent,
};
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

/// Models offered in the model menu, with the id `/model` takes.
const MODELS: [(&str, &str); 4] = [
    ("Opus 5.5", "claude-opus-5-5"),
    ("Sonnet 5.5", "claude-sonnet-5-5"),
    ("Haiku 4.5", "claude-haiku-4-5-20251001"),
    ("Fable 5.1", "claude-fable-5-1"),
];
/// Effort levels offered in the model menu, as `/effort` takes them.
const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

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
    model_menu: ViewHandle<DisplayChipMenu>,
    is_model_menu_open: bool,
    model_chip_mouse_state: MouseStateHandle,
    mode_pill_mouse_state: MouseStateHandle,
    send_button_mouse_state: MouseStateHandle,
}

impl ClaudeComposer {
    pub(super) fn new(ctx: &mut ViewContext<TerminalView>) -> Self {
        // `CodeReview` is the menu type without a search input.
        let model_menu = ctx.add_typed_action_view(|ctx| {
            DisplayChipMenu::new(model_menu_items(None), None, ChipMenuType::CodeReview, ctx)
        });
        ctx.subscribe_to_view(&model_menu, |me, _, event, ctx| {
            me.handle_claude_model_menu_event(event, ctx);
        });
        Self {
            mentions: Vec::new(),
            model: None,
            model_refreshed_at: None,
            model_menu,
            is_model_menu_open: false,
            model_chip_mouse_state: Default::default(),
            mode_pill_mouse_state: Default::default(),
            send_button_mouse_state: Default::default(),
        }
    }
}

/// A choice in the model menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelChoice {
    Model {
        label: &'static str,
        id: &'static str,
    },
    Effort(&'static str),
}

#[derive(Debug, Clone)]
struct ModelMenuItem {
    choice: ModelChoice,
    is_current: bool,
}

impl GenericMenuItem for ModelMenuItem {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn name(&self) -> String {
        match self.choice {
            ModelChoice::Model { label, .. } => label.to_owned(),
            ModelChoice::Effort(effort) => {
                format!("Esforço {}", effort_label(effort).to_lowercase())
            }
        }
    }

    fn icon(&self, _app: &AppContext) -> Option<warp_core::ui::Icon> {
        Some(match (self.is_current, self.choice) {
            (true, _) => WarpIcon::Check,
            (false, ModelChoice::Model { .. }) => WarpIcon::ClaudeLogo,
            (false, ModelChoice::Effort(_)) => WarpIcon::Lightning,
        })
    }

    fn action_data(&self) -> String {
        self.name()
    }
}

fn model_menu_items(current: Option<&ClaudeModelLabel>) -> Vec<ModelMenuItem> {
    let models = MODELS.into_iter().map(|(label, id)| ModelMenuItem {
        choice: ModelChoice::Model { label, id },
        is_current: current.is_some_and(|current| current.model == label),
    });
    let efforts = EFFORTS.into_iter().map(|effort| ModelMenuItem {
        choice: ModelChoice::Effort(effort),
        is_current: current
            .and_then(|current| current.effort.as_deref())
            .is_some_and(|current| current == effort_label(effort)),
    });
    models.chain(efforts).collect()
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

/// Reads the model and effort of the conversation `session_id` in `cwd` from its transcript,
/// falling back to the effort in Claude's settings.
fn read_model_label(cwd: PathBuf, session_id: String) -> Option<ClaudeModelLabel> {
    use std::io::{Read as _, Seek as _, SeekFrom};

    let claude_dir = dirs::home_dir()?.join(".claude");
    let transcript = claude_dir
        .join("projects")
        .join(project_slug(&cwd))
        .join(format!("{session_id}.jsonl"));
    let tail = std::fs::File::open(transcript).ok().and_then(|mut file| {
        let len = file.metadata().ok()?.len();
        file.seek(SeekFrom::Start(len.saturating_sub(TRANSCRIPT_TAIL_BYTES)))
            .ok()?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).ok()?;
        Some(String::from_utf8_lossy(&bytes).into_owned())
    });
    let settings = std::fs::read_to_string(claude_dir.join("settings.json")).ok();

    let model = tail
        .as_deref()
        .and_then(|tail| last_string_field(tail, "model"))
        .and_then(|model| model_label(&model))
        .unwrap_or_else(|| "Claude".to_owned());
    let effort = tail
        .as_deref()
        .and_then(|tail| last_string_field(tail, "effort"))
        .or_else(|| {
            settings
                .as_deref()
                .and_then(|settings| last_string_field(settings, "effortLevel"))
        })
        .map(|effort| effort_label(&effort));
    Some(ClaudeModelLabel { model, effort })
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
    Some(PromptState {
        first_line: row_text(rules.top + 1, 2)?,
        status_line: row_text(status_row, 0)?,
        is_empty: is_single_line && (only_placeholder || cursor_at_start),
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

    /// Moves Claude to its next permission mode, like pressing Shift+Tab.
    pub(super) fn cycle_claude_mode(&mut self, ctx: &mut ViewContext<Self>) {
        self.write_user_bytes_to_pty(SHIFT_TAB.to_vec(), ctx);
        self.focus_terminal(ctx);
    }

    pub(super) fn toggle_claude_model_menu(&mut self, ctx: &mut ViewContext<Self>) {
        let open = !self.claude_composer.is_model_menu_open;
        self.claude_composer.is_model_menu_open = open;
        if open {
            let items = model_menu_items(self.claude_composer.model.as_ref());
            self.claude_composer.model_menu.update(ctx, |menu, ctx| {
                menu.update_menu_items(items, ctx);
                menu.reset_selected_index();
            });
            ctx.focus(&self.claude_composer.model_menu);
        } else {
            self.focus_terminal(ctx);
        }
        ctx.notify();
    }

    fn handle_claude_model_menu_event(
        &mut self,
        event: &PromptDisplayMenuEvent,
        ctx: &mut ViewContext<Self>,
    ) {
        let choice = match event {
            PromptDisplayMenuEvent::MenuAction(menu_event) => menu_event
                .action_item
                .as_any()
                .downcast_ref::<ModelMenuItem>()
                .map(|item| item.choice),
            PromptDisplayMenuEvent::CloseMenu => None,
        };
        self.claude_composer.is_model_menu_open = false;
        self.focus_terminal(ctx);
        ctx.notify();
        if let Some(choice) = choice {
            self.apply_claude_model_choice(choice, ctx);
        }
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
                    model.effort = Some(effort_label(effort));
                }
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
        let Some((cwd, session_id)) = CLIAgentSessionsModel::as_ref(ctx)
            .session(self.view_id)
            .and_then(|session| {
                let context = &session.session_context;
                Some((
                    PathBuf::from(context.cwd.as_deref()?),
                    context.session_id.clone()?,
                ))
            })
        else {
            return;
        };
        self.claude_composer.model_refreshed_at = Some(Instant::now());
        ctx.spawn(
            async move { read_model_label(cwd, session_id) },
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
            font_family: appearance.ui_font_family(),
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

    /// The model menu, which floats above every other layer like the footer's menus.
    pub(super) fn render_claude_model_menu(
        &self,
        app: &AppContext,
    ) -> Option<(Box<dyn Element>, OffsetPositioning)> {
        if !self.claude_composer.is_model_menu_open || !self.runs_claude(app) {
            return None;
        }
        Some((
            ChildView::new(&self.claude_composer.model_menu).finish(),
            OffsetPositioning::offset_from_save_position_element(
                self.claude_model_chip_position_id(),
                vec2f(0., -6.),
                PositionedElementOffsetBounds::WindowByPosition,
                PositionedElementAnchor::TopLeft,
                ChildAnchor::BottomLeft,
            ),
        ))
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

        let model_text = match &self.claude_composer.model {
            Some(label) => match &label.effort {
                Some(effort) => format!("{} · {effort}", label.model),
                None => label.model.clone(),
            },
            None => "Claude".to_owned(),
        };
        let model_chip = render_control(
            self.claude_composer.model_chip_mouse_state.clone(),
            TerminalAction::ToggleClaudeModelMenu,
            style,
            Some((WarpIcon::ClaudeLogo, WarpThemeFill::Solid(SEND_COLOR))),
            model_text,
            style.text,
            true,
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
