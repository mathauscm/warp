//! The composer's decisions that don't need a UI: permission modes, the "/" command menu, "@"
//! mentions, quoted mentions, labels for models, efforts and permission suggestions.

use claude_chat::artifacts::ArtifactKind;
use claude_chat::presentation::{self, DiffStats};
use claude_chat::protocol::{ContextUsage, ModelInfo, SlashCommand};
use claude_chat::timeline::{TodoItem, TodoStatus, ToolCall, ToolStatus};
use serde_json::Value;

use crate::native_chat::view::SidePanelSection;

/// A permission mode Claude Code runs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PermissionMode {
    /// What `set_permission_mode` takes.
    pub value: &'static str,
    pub label: &'static str,
    pub description: &'static str,
}

/// In Shift+Tab order.
pub const PERMISSION_MODES: [PermissionMode; 5] = [
    PermissionMode {
        value: "default",
        label: "Perguntar",
        description: "Pede aprovação antes de editar arquivos e rodar comandos",
    },
    PermissionMode {
        value: "acceptEdits",
        label: "Aceitar edições",
        description: "Edita arquivos sem perguntar; comandos ainda pedem aprovação",
    },
    PermissionMode {
        value: "plan",
        label: "Plano",
        description: "Explora e planeja sem alterar nada",
    },
    PermissionMode {
        value: "auto",
        label: "Automático",
        description: "Aprova ações de rotina; o resto ainda pergunta",
    },
    PermissionMode {
        value: "bypassPermissions",
        label: "Acesso total",
        description: "Edita e roda comandos sem pedir aprovação",
    },
];

/// The index in [`PERMISSION_MODES`] of a mode Claude reports; unknown modes read as the default.
pub fn permission_mode_index(mode: Option<&str>) -> usize {
    let mode = match mode {
        Some("dontAsk") => "bypassPermissions",
        Some(mode) => mode,
        None => "default",
    };
    PERMISSION_MODES
        .iter()
        .position(|candidate| candidate.value == mode)
        .unwrap_or(0)
}

/// The mode Shift+Tab switches to from `mode`.
pub fn next_permission_mode(mode: Option<&str>) -> &'static PermissionMode {
    &PERMISSION_MODES[(permission_mode_index(mode) + 1) % PERMISSION_MODES.len()]
}

// --- slash commands -----------------------------------------------------------------------

/// What a slash command does in the app instead of being sent to Claude.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiCommand {
    ModelPicker,
    EffortPicker,
    OpenSidePanel(SidePanelSection),
    ToggleRemoteControl,
    Compact,
    NewChat,
    Rename,
}

/// Commands the app handles itself, with the descriptions the menu shows for them.
const UI_COMMANDS: [(&str, &str, UiCommand); 11] = [
    ("model", "Trocar o modelo", UiCommand::ModelPicker),
    (
        "effort",
        "Ajustar o esforço de raciocínio",
        UiCommand::EffortPicker,
    ),
    (
        "context",
        "Ver o uso da janela de contexto",
        UiCommand::OpenSidePanel(SidePanelSection::Context),
    ),
    (
        "mcp",
        "Gerenciar conectores MCP",
        UiCommand::OpenSidePanel(SidePanelSection::Connectors),
    ),
    (
        "remote-control",
        "Ligar ou desligar o Remote Control",
        UiCommand::ToggleRemoteControl,
    ),
    (
        "usage",
        "Ver o uso do plano",
        UiCommand::OpenSidePanel(SidePanelSection::Usage),
    ),
    (
        "cost",
        "Ver o custo desta sessão",
        UiCommand::OpenSidePanel(SidePanelSection::Usage),
    ),
    (
        "compact",
        "Resumir a conversa para liberar contexto",
        UiCommand::Compact,
    ),
    ("clear", "Começar um chat novo", UiCommand::NewChat),
    ("new", "Começar um chat novo", UiCommand::NewChat),
    ("rename", "Renomear esta conversa", UiCommand::Rename),
];

pub fn ui_command(name: &str) -> Option<UiCommand> {
    UI_COMMANDS
        .iter()
        .find(|(command, _, _)| *command == name)
        .map(|(_, _, action)| *action)
}

/// A row of the "/" menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandEntry {
    /// Without the leading `/`.
    pub name: String,
    pub description: String,
    pub argument_hint: String,
    pub aliases: Vec<String>,
    /// The app's and Claude Code's own commands, listed before skills and custom commands.
    pub builtin: bool,
}

/// The app's commands, then Claude's (without the ones the app already lists).
pub fn command_entries<'a>(cli: impl Iterator<Item = &'a SlashCommand>) -> Vec<CommandEntry> {
    let mut entries: Vec<CommandEntry> = UI_COMMANDS
        .iter()
        .map(|(name, description, _)| CommandEntry {
            name: (*name).to_owned(),
            description: (*description).to_owned(),
            argument_hint: String::new(),
            aliases: Vec::new(),
            builtin: true,
        })
        .collect();
    for command in cli {
        if command.name.is_empty() || entries.iter().any(|entry| entry.name == command.name) {
            continue;
        }
        entries.push(CommandEntry {
            name: command.name.clone(),
            description: command.description.clone(),
            argument_hint: command.argument_hint.clone(),
            aliases: command.aliases.clone(),
            builtin: command.builtin,
        });
    }
    entries
}

/// The command being typed: the text after a leading `/`, until the first space.
pub fn slash_query(text: &str) -> Option<&str> {
    let query = text.strip_prefix('/')?;
    (!query.contains(char::is_whitespace)).then_some(query)
}

/// The entries matching `query`, best first: exact names, then name prefixes, alias prefixes,
/// names containing it and descriptions containing it; builtin commands before the others.
pub fn filter_commands(entries: &[CommandEntry], query: &str) -> Vec<usize> {
    let query = query.to_lowercase();
    let mut matches: Vec<(u8, bool, &str, usize)> = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let name = entry.name.to_lowercase();
            let rank = if query.is_empty() || name == query {
                0
            } else if name.starts_with(&query) {
                1
            } else if entry
                .aliases
                .iter()
                .any(|alias| alias.to_lowercase().starts_with(&query))
            {
                2
            } else if name.contains(&query) {
                3
            } else if entry.description.to_lowercase().contains(&query) {
                4
            } else {
                return None;
            };
            Some((rank, !entry.builtin, entry.name.as_str(), index))
        })
        .collect();
    if query.is_empty() {
        // Without a query the app's commands keep their curated order on top.
        matches.sort_by_key(|&(rank, not_builtin, _, index)| (rank, not_builtin, index));
    } else {
        matches.sort_by(|a, b| (a.0, a.1, a.2).cmp(&(b.0, b.1, b.2)));
    }
    matches.into_iter().map(|(_, _, _, index)| index).collect()
}

/// A typed `/command args` split into its name and arguments.
pub fn parse_command(text: &str) -> Option<(&str, &str)> {
    let rest = text.trim().strip_prefix('/')?;
    let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    (!name.is_empty()).then(|| (name, args.trim()))
}

// --- @ mentions ---------------------------------------------------------------------------

/// The file mention being typed at the end of `text`: the byte offset of its `@` and the query
/// after it.
pub fn mention_query(text: &str) -> Option<(usize, &str)> {
    let at = text.rfind('@')?;
    let query = &text[at + 1..];
    if query.contains(char::is_whitespace) || query.contains('"') {
        return None;
    }
    let starts_word = text[..at]
        .chars()
        .next_back()
        .is_none_or(char::is_whitespace);
    starts_word.then_some((at, query))
}

/// `text` with the mention at `at` completed to `path`; paths with spaces are quoted.
pub fn complete_mention(text: &str, at: usize, path: &str) -> String {
    let mention = if path.contains(char::is_whitespace) {
        format!("@\"{path}\"")
    } else {
        format!("@{path}")
    };
    format!("{}{mention} ", &text[..at])
}

// --- quoted mentions ----------------------------------------------------------------------

/// The start of `text` on one line, for a chip.
pub fn preview(text: &str, max_chars: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max_chars {
        return flat;
    }
    let mut cut: String = flat.chars().take(max_chars).collect();
    cut = cut.trim_end().to_owned();
    cut.push('…');
    cut
}

// --- model and effort ---------------------------------------------------------------------

/// The effort levels when Claude doesn't list a model's own.
pub const DEFAULT_EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// "claude-opus-5-5" → "Opus 5.5", "claude-haiku-4-5-20251001" → "Haiku 4.5".
pub fn model_label(model_id: &str) -> Option<String> {
    let id = model_id.trim().strip_prefix("claude-")?;
    let id = id.split('[').next().unwrap_or(id);
    let mut parts = id.split('-');
    let family = parts.next().filter(|family| !family.is_empty())?;
    let mut family_chars = family.chars();
    let family = family_chars
        .next()
        .map(|first| first.to_uppercase().chain(family_chars).collect::<String>())?;
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

pub fn effort_label(effort: &str) -> String {
    match effort {
        "low" => "Baixo".to_owned(),
        "medium" => "Médio".to_owned(),
        "high" => "Alto".to_owned(),
        "xhigh" => "Extra alto".to_owned(),
        "max" => "Máximo".to_owned(),
        "auto" => "Automático".to_owned(),
        other => other.to_owned(),
    }
}

/// The model Claude runs: the one picked in this chat (a `set_model` value), else the one it
/// reported.
pub fn current_model<'a>(
    models: &'a [ModelInfo],
    chosen: Option<&str>,
    reported: Option<&str>,
) -> Option<&'a ModelInfo> {
    if let Some(chosen) = chosen {
        return models.iter().find(|model| model.value == chosen);
    }
    let Some(reported) = reported else {
        return models.iter().find(|model| model.value == "default");
    };
    models
        .iter()
        .find(|model| model.value != "default" && model.resolved_model.as_deref() == Some(reported))
        .or_else(|| models.iter().find(|model| model.value == reported))
}

/// The name the model chip shows.
pub fn model_display_name(
    models: &[ModelInfo],
    chosen: Option<&str>,
    reported: Option<&str>,
) -> String {
    if let Some(model) = current_model(models, chosen, reported) {
        if model.value == "default" {
            if let Some(label) = model.resolved_model.as_deref().and_then(model_label) {
                return label;
            }
        } else if !model.display_name.is_empty() {
            return model.display_name.clone();
        }
    }
    chosen
        .and_then(model_label)
        .or_else(|| {
            chosen.map(|value| {
                let mut chars = value.chars();
                chars
                    .next()
                    .map(|first| first.to_uppercase().chain(chars).collect())
                    .unwrap_or_default()
            })
        })
        .or_else(|| reported.and_then(model_label))
        .unwrap_or_else(|| "Claude".to_owned())
}

/// The effort levels of a model; empty when it has none.
pub fn effort_levels(model: Option<&ModelInfo>) -> Vec<String> {
    match model {
        Some(model) if !model.supported_effort_levels.is_empty() => {
            model.supported_effort_levels.clone()
        }
        Some(model) if !model.supports_effort => Vec::new(),
        Some(_) | None => DEFAULT_EFFORTS
            .iter()
            .map(|&level| level.to_owned())
            .collect(),
    }
}

/// Where the slider's knob sits for the level at `index` of `count`, from 0 to 1.
pub fn effort_position(index: usize, count: usize) -> f32 {
    if count <= 1 {
        return 0.;
    }
    index as f32 / (count - 1) as f32
}

/// The level a model runs at when no effort was picked: "high" when it has it, else the middle
/// one.
pub fn default_effort_index(levels: &[String]) -> Option<usize> {
    if levels.is_empty() {
        return None;
    }
    levels
        .iter()
        .position(|level| level == "high")
        .or(Some(levels.len() / 2))
}

// --- context ------------------------------------------------------------------------------

/// How full the context window is, from 0 to 1: the latest prompt's size while it's known, else
/// the last context report.
pub fn context_fraction(
    usage: Option<&ContextUsage>,
    live_tokens: Option<u64>,
    window: Option<u64>,
) -> Option<f32> {
    let window = window.or_else(|| usage.and_then(ContextUsage::window_tokens));
    if let (Some(live), Some(window)) = (live_tokens, window)
        && window > 0
    {
        return Some((live as f32 / window as f32).clamp(0., 1.));
    }
    let usage = usage?;
    if let Some(percentage) = usage.percentage {
        return Some((percentage as f32 / 100.).clamp(0., 1.));
    }
    let total = usage.total_tokens?;
    let window = window.filter(|&window| window > 0)?;
    Some((total as f32 / window as f32).clamp(0., 1.))
}

/// `12.3k`, `1M`.
pub fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        let millions = tokens as f64 / 1_000_000.;
        if (millions - millions.round()).abs() < 0.05 {
            format!("{}M", millions.round())
        } else {
            format!("{millions:.1}M")
        }
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.)
    } else {
        tokens.to_string()
    }
}

// --- todos --------------------------------------------------------------------------------

/// Completed and total tasks.
pub fn todo_progress(todos: &[TodoItem]) -> (usize, usize) {
    let done = todos
        .iter()
        .filter(|todo| todo.status == TodoStatus::Completed)
        .count();
    (done, todos.len())
}

/// The task in progress, in its present-tense form when Claude gave one.
pub fn current_todo(todos: &[TodoItem]) -> Option<&str> {
    todos
        .iter()
        .find(|todo| todo.status == TodoStatus::InProgress)
        .map(|todo| todo.active_form.as_deref().unwrap_or(&todo.content))
}

// --- permission requests ------------------------------------------------------------------

/// What a permission card previews about the tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionPreview {
    Command(String),
    File(DiffStats),
    Path(String),
    Text(String),
    None,
}

pub fn permission_preview(tool_name: &str, input: &Value) -> PermissionPreview {
    let text = |key: &str| {
        input
            .get(key)
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    match tool_name {
        "Bash" | "BashOutput" | "PowerShell" => text("command")
            .map(PermissionPreview::Command)
            .unwrap_or(PermissionPreview::None),
        "Edit" | "MultiEdit" | "Write" => {
            let tool = ToolCall {
                id: String::new(),
                name: tool_name.to_owned(),
                input: input.clone(),
                partial_input: String::new(),
                status: ToolStatus::AwaitingApproval,
                result: None,
                parent_tool_use_id: None,
                started_at: None,
                finished_at: None,
            };
            presentation::edit_stats(&tool)
                .map(PermissionPreview::File)
                .unwrap_or(PermissionPreview::None)
        }
        "Read" | "NotebookEdit" | "NotebookRead" => text("file_path")
            .or_else(|| text("notebook_path"))
            .map(PermissionPreview::Path)
            .unwrap_or(PermissionPreview::None),
        "WebFetch" => text("url")
            .map(PermissionPreview::Path)
            .unwrap_or(PermissionPreview::None),
        "WebSearch" => text("query")
            .map(PermissionPreview::Text)
            .unwrap_or(PermissionPreview::None),
        "Glob" | "Grep" => text("pattern")
            .map(PermissionPreview::Command)
            .unwrap_or(PermissionPreview::None),
        _ => match input {
            Value::Object(map) if !map.is_empty() => {
                PermissionPreview::Command(serde_json::to_string_pretty(input).unwrap_or_default())
            }
            _ => PermissionPreview::None,
        },
    }
}

/// The tool's name for people: `mcp__server__tool` → `tool (server)`.
pub fn tool_display_name(tool_name: &str, display_name: Option<&str>) -> String {
    if let Some(name) = display_name.filter(|name| !name.is_empty() && *name != tool_name) {
        return name.to_owned();
    }
    if let Some(rest) = tool_name.strip_prefix("mcp__")
        && let Some((server, tool)) = rest.split_once("__")
    {
        return format!("{tool} ({server})");
    }
    tool_name.to_owned()
}

/// The label of the "always allow" button for one of Claude's permission suggestions.
pub fn suggestion_label(suggestion: &Value) -> String {
    let in_session = suggestion.get("destination").and_then(Value::as_str) == Some("session");
    let scope = if in_session { " nesta sessão" } else { "" };
    match suggestion.get("type").and_then(Value::as_str) {
        Some("addRules") | Some("replaceRules") => {
            let rule = suggestion
                .get("rules")
                .and_then(Value::as_array)
                .and_then(|rules| rules.first());
            let content = rule
                .and_then(|rule| rule.get("ruleContent"))
                .and_then(Value::as_str)
                .filter(|content| !content.is_empty());
            let tool = rule
                .and_then(|rule| rule.get("toolName"))
                .and_then(Value::as_str);
            match (content, tool) {
                (Some(content), _) => match content.strip_prefix("domain:") {
                    Some(domain) => format!("Sempre permitir {domain}{scope}"),
                    None => format!("Sempre permitir “{}”{scope}", preview(content, 32)),
                },
                (None, Some(tool)) => format!("Sempre permitir {tool}{scope}"),
                (None, None) => format!("Sempre permitir{scope}"),
            }
        }
        Some("setMode") => match suggestion.get("mode").and_then(Value::as_str) {
            Some("acceptEdits") => format!("Aceitar edições{scope}"),
            mode => {
                let label = PERMISSION_MODES[permission_mode_index(mode)].label;
                format!("Mudar para {label}{scope}")
            }
        },
        Some("addDirectories") => {
            let directory = suggestion
                .get("directories")
                .and_then(Value::as_array)
                .and_then(|directories| directories.first())
                .and_then(Value::as_str)
                .and_then(|path| std::path::Path::new(path).file_name()?.to_str());
            match directory {
                Some(directory) => format!("Sempre permitir em {directory}{scope}"),
                None => format!("Sempre permitir nesta pasta{scope}"),
            }
        }
        _ => format!("Sempre permitir{scope}"),
    }
}

/// The answer a question gets: its chosen labels joined by ", ", or the typed text.
pub fn question_answer(labels: &[&str], typed: &str) -> Option<String> {
    let typed = typed.trim();
    if !typed.is_empty() {
        return Some(typed.to_owned());
    }
    (!labels.is_empty()).then(|| labels.join(", "))
}

// --- artifacts ----------------------------------------------------------------------------

/// The composer's placeholder while the next message asks for an artifact of `kind`.
pub fn artifact_placeholder(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Document => "O que este documento deve conter?",
        ArtifactKind::Presentation => "O que esta apresentação deve mostrar?",
        ArtifactKind::Design => "O que este design deve mostrar?",
        ArtifactKind::Dashboard => "O que este painel deve mostrar?",
        ArtifactKind::Motion => "O que esta animação deve mostrar?",
        ArtifactKind::Other => "O que este artefato deve mostrar?",
    }
}

/// What Claude is told before a message that asks for an artifact of `kind`.
pub fn artifact_instructions(kind: ArtifactKind) -> String {
    let detail = match kind {
        ArtifactKind::Document => "a well-formatted, readable document",
        ArtifactKind::Presentation => "a slide deck navigable with the arrow keys",
        ArtifactKind::Design => "a polished visual design or UI mock",
        ArtifactKind::Dashboard => "an interactive dashboard with charts",
        ArtifactKind::Motion => "an animation (motion graphics) that plays on its own",
        ArtifactKind::Other => "an interactive page",
    };
    format!(
        "Build what I ask below as an artifact of kind \"{}\": {detail}, as a single \
         self-contained HTML page created with the create_artifact tool.",
        kind.as_str()
    )
}
