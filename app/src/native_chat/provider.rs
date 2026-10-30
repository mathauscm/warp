//! Which CLI a native chat drives: Claude Code (the default) or OpenAI's Codex, and the settings
//! that pick them.

use std::path::PathBuf;
use std::sync::Mutex;

use claude_chat::codex::{self, CodexModel};
use claude_chat::protocol::ModelInfo;
use serde::{Deserialize, Serialize};
use settings::macros::define_settings_group;
use settings::{SupportedPlatforms, SyncToCloud};

/// The CLI behind a chat. Chats keep the provider they started with.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
#[serde(rename_all = "snake_case")]
#[schemars(
    description = "The CLI new native chats use.",
    rename_all = "snake_case"
)]
pub enum ChatProvider {
    /// Claude Code.
    #[default]
    Claude,
    /// OpenAI's Codex CLI.
    Codex,
}

impl ChatProvider {
    /// In the order the pickers list them.
    pub const ALL: [Self; 2] = [Self::Claude, Self::Codex];

    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
        }
    }

    /// How the provider is persisted with a chat pane.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }
}

define_settings_group!(ChatProviderSettings, settings: [
    default_provider: DefaultChatProvider {
        type: ChatProvider,
        default: ChatProvider::Claude,
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "chat.providers.default_provider",
        description: "The CLI new native chats use: Claude Code or OpenAI's Codex.",
    },
    codex_default_model: CodexDefaultModel {
        type: String,
        default: String::new(),
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "chat.providers.codex_default_model",
        description: "The model new Codex chats use. Empty uses the Codex CLI's own default.",
    },
]);

/// The Codex models seen last: from a running Codex chat, else from the CLI's cache.
static CODEX_MODELS: Mutex<Vec<CodexModel>> = Mutex::new(Vec::new());

/// The models Codex offers, so the picker can list them in any chat.
pub fn codex_models() -> Vec<CodexModel> {
    let mut models = CODEX_MODELS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if models.is_empty()
        && let Some(home) = codex::codex_home()
    {
        *models = codex::read_models_cache(&home);
    }
    models.clone()
}

/// Remembers the models a Codex chat reported.
pub fn remember_codex_models(models: &[CodexModel]) {
    if models.is_empty() {
        return;
    }
    *CODEX_MODELS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = models.to_vec();
}

/// Claude Code's models as a Claude chat last reported them.
static CLAUDE_MODELS: Mutex<Vec<ModelInfo>> = Mutex::new(Vec::new());

/// The models Claude Code offers, for the picker of a Codex chat. Empty until a Claude chat
/// reported them.
pub fn claude_models() -> Vec<ModelInfo> {
    CLAUDE_MODELS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// Remembers the models a Claude chat reported.
pub fn remember_claude_models(models: &[ModelInfo]) {
    if models.is_empty() {
        return;
    }
    *CLAUDE_MODELS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = models.to_vec();
}

/// Where the Codex CLI is installed, looking in `PATH` and the usual npm and Homebrew folders.
pub fn codex_cli_path() -> Option<PathBuf> {
    find_codex(std::env::var("PATH").ok().as_deref())
}

/// [`codex::find_codex`], except that `PICOLAS_CODEX_PATH=<executable>` points at a Codex CLI
/// installed elsewhere, like a throwaway install for trying a change. Debug builds only.
pub fn find_codex(path_env: Option<&str>) -> Option<PathBuf> {
    if cfg!(debug_assertions)
        && let Some(path) = std::env::var_os("PICOLAS_CODEX_PATH").map(PathBuf::from)
        && path.is_file()
    {
        return Some(path);
    }
    codex::find_codex(path_env)
}

#[cfg(test)]
#[path = "provider_tests.rs"]
mod tests;
