use serde::{Deserialize, Serialize};
use settings::macros::define_settings_group;
use settings::{RespectUserSyncSetting, Setting, SupportedPlatforms, SyncToCloud};
use warpui::AppContext;
use warpui::platform::SystemTheme;

use crate::themes::theme::{RespectSystemTheme, SelectedSystemThemes, ThemeKind};

// Settings group for themes related settings.
// Note that we store just the information needed to derive the current
// theme state, which boils down to:
// ThemeKind: the theme to use when the system theme is off.
// UseSystemTheme: whether to respect the system theme.
// SelectedSystemThemes: the themes to use when the system theme is on.
define_settings_group!(ThemeSettings, settings: [
    theme_kind: Theme {
        type: ThemeKind,
        // Note that for new users, we now override this default value in SettingsInitializer
        // to set the default theme to Phenomenon.
        default: ThemeKind::default(),
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "appearance.themes.theme",
        max_table_depth: 0,
        description: "The color theme.",
    },
    use_system_theme: UseSystemTheme {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        surface: settings::SettingSurfaces::GUI,
        private: false,
        storage_key: "SystemTheme",
        toml_path: "appearance.themes.system_theme",
        description: "Whether to match the system light/dark theme.",
    },
    selected_system_themes: SystemThemes {
        type: SelectedSystemThemes,
        default: SelectedSystemThemes::default(),
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        surface: settings::SettingSurfaces::GUI,
        private: false,
        storage_key: "SelectedSystemThemes",
        toml_path: "appearance.themes.selected_system_themes",
        max_table_depth: 0,
        description: "The themes to use for system light and dark modes.",
    },
    claude_theme: ClaudeThemeSetting {
        type: ClaudeTheme,
        default: ClaudeTheme::default(),
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "appearance.themes.claude_theme",
        description: "The colors of panes running Claude Code and of the native Claude chat.",
    },
    claude_chat_accent: ClaudeChatAccentSetting {
        type: ClaudeChatAccent,
        default: ClaudeChatAccent::default(),
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "appearance.themes.claude_chat_accent",
        description: "The highlight color of the native Claude chat.",
    },
]);

/// The colors a pane takes while Claude Code runs in it.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
#[schemars(
    description = "The colors of panes running Claude Code and of the native Claude chat.",
    rename_all = "snake_case"
)]
pub enum ClaudeTheme {
    /// The Claude desktop app's warm dark palette.
    ClaudeDark,
    /// The Claude desktop app's light palette.
    ClaudeLight,
    /// The Claude palette matching the system's light or dark mode.
    System,
    /// The Warp theme, like any other pane; the native chat derives its palette from it.
    #[default]
    Warp,
}

impl ClaudeTheme {
    /// In the order the pickers list them.
    pub const ALL: [Self; 4] = [
        Self::Warp,
        Self::ClaudeDark,
        Self::ClaudeLight,
        Self::System,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::ClaudeDark => "Claude escuro",
            Self::ClaudeLight => "Claude claro",
            Self::System => "Sistema",
            Self::Warp => "Seguir o tema do Warp",
        }
    }
}

/// The highlight color of the native Claude chat: buttons, selection, focus.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
#[schemars(
    description = "The highlight color of the native Claude chat.",
    rename_all = "snake_case"
)]
pub enum ClaudeChatAccent {
    #[default]
    Blue,
    Purple,
    Green,
    Orange,
    Pink,
    /// The Warp theme's accent color.
    Theme,
}

impl ClaudeChatAccent {
    /// In the order the pickers list them.
    pub const ALL: [Self; 6] = [
        Self::Blue,
        Self::Purple,
        Self::Green,
        Self::Orange,
        Self::Pink,
        Self::Theme,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Blue => "Azul",
            Self::Purple => "Roxo",
            Self::Green => "Verde",
            Self::Orange => "Laranja",
            Self::Pink => "Rosa",
            Self::Theme => "Do tema",
        }
    }
}

impl Theme {
    fn current_value_is_syncable(&self) -> bool {
        self.value().is_custom_theme_reference_syncable()
    }
}

impl SystemThemes {
    fn current_value_is_syncable(&self) -> bool {
        let selected = self.value();
        selected.light.is_custom_theme_reference_syncable()
            && selected.dark.is_custom_theme_reference_syncable()
    }
}

/// Returns a derived value for whether to respect the system theme based on
/// the current theme settings.
pub fn respect_system_theme(theme_settings: &ThemeSettings) -> RespectSystemTheme {
    if *theme_settings.use_system_theme.value() {
        RespectSystemTheme::On(theme_settings.selected_system_themes.value().clone())
    } else {
        RespectSystemTheme::Off
    }
}

/// Returns the current theme kind based on the theme settings and the system theme.
pub fn derived_theme_kind(theme_settings: &ThemeSettings, system_theme: SystemTheme) -> ThemeKind {
    let respect_system_theme = respect_system_theme(theme_settings);
    match respect_system_theme {
        RespectSystemTheme::On(selected_system_themes) => match system_theme {
            SystemTheme::Light => selected_system_themes.light.clone(),
            SystemTheme::Dark => selected_system_themes.dark.clone(),
        },
        RespectSystemTheme::Off => theme_settings.theme_kind.value().clone(),
    }
}

/// Return the current theme kind based on the theme settings and active app context.
pub fn active_theme_kind(theme_settings: &ThemeSettings, app: &AppContext) -> ThemeKind {
    derived_theme_kind(theme_settings, app.system_theme())
}

#[cfg(test)]
#[path = "theme_tests.rs"]
mod tests;
