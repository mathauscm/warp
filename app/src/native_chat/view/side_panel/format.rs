//! Pure helpers that turn the conversation's numbers and names into the side panel's text.

use std::path::Path;
use std::time::Duration;

use claude_chat::protocol::{ContextCategory, ContextUsage};

const FREE_SPACE_CATEGORY: &str = "Free space";
const DEFERRED_SUFFIX: &str = " (deferred)";

/// Token counts the way Claude shows them: `850`, `78.4k`, `120k`, `1M`, `1.5M`.
pub(in crate::native_chat::view) fn format_tokens(tokens: u64) -> String {
    if tokens < 1_000 {
        return tokens.to_string();
    }
    let thousands = rounded_for_display(tokens as f64 / 1_000.);
    if thousands < 1_000. {
        return format!("{}k", trim_decimal(thousands));
    }
    let millions = rounded_for_display(tokens as f64 / 1_000_000.);
    format!("{}M", trim_decimal(millions))
}

/// One decimal below 100, none above, so labels stay short.
fn rounded_for_display(value: f64) -> f64 {
    if value >= 100. {
        value.round()
    } else {
        (value * 10.).round() / 10.
    }
}

fn trim_decimal(value: f64) -> String {
    let text = format!("{value:.1}");
    text.strip_suffix(".0").map(str::to_owned).unwrap_or(text)
}

/// A whole percentage, with `<1%` for a sliver that would otherwise read as empty.
pub(in crate::native_chat::view) fn format_percent(percent: f64) -> String {
    if percent <= 0. {
        "0%".to_owned()
    } else if percent < 1. {
        "<1%".to_owned()
    } else {
        format!("{}%", percent.round().min(100.) as u64)
    }
}

/// A share of the context window: one decimal below 10%, whole above.
pub(super) fn format_share(percent: f64) -> String {
    if percent <= 0. {
        "0%".to_owned()
    } else if percent < 0.1 {
        "<0.1%".to_owned()
    } else if percent < 10. {
        format!("{}%", trim_decimal((percent * 10.).round() / 10.))
    } else {
        format!("{}%", percent.round() as u64)
    }
}

/// When a usage window starts over, like `reinicia em 2h 13m`.
pub(in crate::native_chat::view) fn format_reset(remaining: Duration) -> String {
    let minutes = remaining.as_secs().div_ceil(60);
    if minutes == 0 {
        return "reinicia em instantes".to_owned();
    }
    let days = minutes / (24 * 60);
    let hours = minutes % (24 * 60) / 60;
    let minutes = minutes % 60;
    let when = match (days, hours, minutes) {
        (0, 0, minutes) => format!("{minutes}m"),
        (0, hours, 0) => format!("{hours}h"),
        (0, hours, minutes) => format!("{hours}h {minutes}m"),
        (days, 0, _) => format!("{days}d"),
        (days, hours, _) => format!("{days}d {hours}h"),
    };
    format!("reinicia em {when}")
}

/// Dollars in the Brazilian format, like `US$ 0,03`.
pub(in crate::native_chat::view) fn format_cost(usd: f64) -> String {
    let text = if usd < 0.01 {
        "<0,01".to_owned()
    } else {
        format!("{usd:.2}").replace('.', ",")
    };
    format!("US$ {text}")
}

/// A folder with the home directory written as `~`.
pub(super) fn abbreviate_home(path: &Path, home: Option<&Path>) -> String {
    if let Some(home) = home
        && let Ok(rest) = path.strip_prefix(home)
    {
        return if rest.as_os_str().is_empty() {
            "~".to_owned()
        } else {
            format!("~/{}", rest.display())
        };
    }
    path.display().to_string()
}

/// An MCP server's name without the CLI's namespacing: `plugin:linear:linear` is `Linear`,
/// `claude.ai Gmail` is `Gmail`.
pub(super) fn display_server_name(name: &str) -> String {
    let name = name.strip_prefix("claude.ai ").unwrap_or(name);
    let name = match name.strip_prefix("plugin:") {
        Some(rest) => rest.rsplit(':').next().unwrap_or(rest),
        None => name,
    };
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_lowercase() => first.to_uppercase().chain(chars).collect(),
        Some(_) => name.to_owned(),
        None => "Servidor".to_owned(),
    }
}

/// The letter on a connector's avatar.
pub(super) fn avatar_letter(display_name: &str) -> String {
    display_name
        .chars()
        .find(|char| char.is_alphanumeric())
        .map(|char| char.to_uppercase().collect())
        .unwrap_or_else(|| "?".to_owned())
}

/// An MCP tool's own name, without the `mcp__<server>__` prefix.
pub(super) fn display_tool_name(name: &str) -> &str {
    match name.strip_prefix("mcp__") {
        Some(rest) => rest.split_once("__").map_or(rest, |(_, tool)| tool),
        None => name,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ServerState {
    Connected,
    Pending,
    NeedsAuth,
    Failed,
    Disabled,
}

impl ServerState {
    pub(super) fn from_status(status: &str) -> Self {
        match status {
            "connected" => Self::Connected,
            "needs-auth" => Self::NeedsAuth,
            "failed" => Self::Failed,
            "disabled" => Self::Disabled,
            _ => Self::Pending,
        }
    }

    /// Working servers first; within a rank the CLI's order holds, so rows don't jump while
    /// servers connect.
    pub(super) fn rank(self) -> u8 {
        match self {
            Self::Connected | Self::Pending => 0,
            Self::NeedsAuth => 1,
            Self::Failed => 2,
            Self::Disabled => 3,
        }
    }
}

/// What a context category is, which picks its color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CategoryTone {
    SystemPrompt,
    SystemTools,
    McpTools,
    McpInstructions,
    Agents,
    Memory,
    Skills,
    Messages,
    AutocompactBuffer,
    Free,
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct ContextRow {
    pub label: String,
    pub tokens: u64,
    /// Share of the context window, when its size is known.
    pub percent: Option<f64>,
    pub tone: CategoryTone,
    /// Loaded only when used, so it doesn't count toward the fill.
    pub is_deferred: bool,
}

/// How full the context window is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(in crate::native_chat::view) struct ContextFill {
    pub used: u64,
    pub window: Option<u64>,
    pub percent: Option<f64>,
}

/// The context fill from the last breakdown, or from the latest API call before there is one.
pub(in crate::native_chat::view) fn context_fill(
    usage: Option<&ContextUsage>,
    live_tokens: Option<u64>,
    live_window: Option<u64>,
) -> Option<ContextFill> {
    let (used, window, reported_percent) = match usage {
        Some(usage) if usage.total_tokens.is_some() => (
            usage.total_tokens?,
            usage.window_tokens().or(live_window),
            usage.percentage,
        ),
        _ => (live_tokens?, live_window, None),
    };
    let percent = match window {
        Some(window) if window > 0 => Some(used as f64 * 100. / window as f64),
        _ => reported_percent,
    };
    Some(ContextFill {
        used,
        window,
        percent,
    })
}

/// The breakdown's rows, with free space last.
pub(super) fn context_rows(usage: &ContextUsage) -> Vec<ContextRow> {
    let window = usage.window_tokens().filter(|window| *window > 0);
    let mut rows: Vec<ContextRow> = usage
        .categories
        .iter()
        .map(|category| context_row(category, window))
        .collect();
    rows.sort_by_key(|row| row.tone == CategoryTone::Free);
    rows
}

fn context_row(category: &ContextCategory, window: Option<u64>) -> ContextRow {
    let is_free = category.kind.as_deref() == Some("free") || category.name == FREE_SPACE_CATEGORY;
    let is_deferred = category.is_deferred
        || category.kind.as_deref() == Some("deferred")
        || category.name.ends_with(DEFERRED_SUFFIX);
    let base_name = category
        .name
        .strip_suffix(DEFERRED_SUFFIX)
        .unwrap_or(&category.name);
    let tone = if is_free {
        CategoryTone::Free
    } else {
        category_tone(base_name)
    };
    let label = if is_deferred {
        deferred_label(tone, base_name)
    } else {
        category_label(base_name)
    };
    let tokens = category.tokens.unwrap_or_default();
    ContextRow {
        label,
        tokens,
        percent: window.map(|window| tokens as f64 * 100. / window as f64),
        tone,
        is_deferred,
    }
}

fn category_tone(name: &str) -> CategoryTone {
    match name {
        "System prompt" => CategoryTone::SystemPrompt,
        "System tools" => CategoryTone::SystemTools,
        "MCP tools" => CategoryTone::McpTools,
        "MCP server instructions" => CategoryTone::McpInstructions,
        "Custom agents" => CategoryTone::Agents,
        "Memory files" => CategoryTone::Memory,
        "Skills" => CategoryTone::Skills,
        "Messages" => CategoryTone::Messages,
        "Autocompact buffer" => CategoryTone::AutocompactBuffer,
        FREE_SPACE_CATEGORY => CategoryTone::Free,
        _ => CategoryTone::Other,
    }
}

/// A category's name in Portuguese; unknown ones keep the CLI's name.
pub(super) fn category_label(name: &str) -> String {
    match name {
        "System prompt" => "Prompt do sistema",
        "System tools" => "Ferramentas do sistema",
        "MCP tools" => "Ferramentas MCP",
        "MCP server instructions" => "Instruções MCP",
        "Custom agents" => "Agentes personalizados",
        "Memory files" => "Arquivos de memória",
        "Skills" => "Skills",
        "Messages" => "Mensagens",
        "Autocompact buffer" => "Reserva de compactação",
        FREE_SPACE_CATEGORY => "Livre",
        other => other,
    }
    .to_owned()
}

/// Deferred categories repeat a loaded one's name, so they get a short name of their own.
fn deferred_label(tone: CategoryTone, name: &str) -> String {
    match tone {
        CategoryTone::SystemTools => "Sistema sob demanda".to_owned(),
        CategoryTone::McpTools => "MCP sob demanda".to_owned(),
        CategoryTone::SystemPrompt
        | CategoryTone::McpInstructions
        | CategoryTone::Agents
        | CategoryTone::Memory
        | CategoryTone::Skills
        | CategoryTone::Messages
        | CategoryTone::AutocompactBuffer
        | CategoryTone::Free
        | CategoryTone::Other => format!("{} sob demanda", category_label(name)),
    }
}
