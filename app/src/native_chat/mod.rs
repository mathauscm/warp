//! Picolas Code's native Claude chat: Claude Code driven headlessly (see the `claude_chat` crate)
//! behind a chat UI like the Claude desktop app, plus the local artifacts it creates.

mod artifacts;
pub mod artifacts_view;
pub mod design;
pub mod registry;
pub mod session;
pub mod view;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The Threads panel workspace a chat belongs to (see `crate::claude_threads::ThreadWorkspace`),
/// as of when the chat opened.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatWorkspace {
    pub id: String,
    pub name: String,
    /// The workspace's folders besides the one Claude runs in, which Claude gets access to.
    #[serde(default)]
    pub extra_dirs: Vec<PathBuf>,
}

impl ChatWorkspace {
    /// The line appended to Claude Code's system prompt, so Claude knows the folders belong
    /// together.
    pub fn system_prompt(&self, cwd: &Path) -> String {
        let projects = std::iter::once(format!("{} (the working directory)", cwd.display()))
            .chain(self.extra_dirs.iter().map(|dir| dir.display().to_string()))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "This chat is in the workspace \"{}\", which spans these projects: {projects}.",
            self.name
        )
    }
}

/// What a chat pane restores from: the conversation and the folder Claude runs in.
#[derive(Clone, Debug, PartialEq)]
pub struct ClaudeChatPaneSnapshot {
    /// `None` until a new chat's first message creates its conversation.
    pub session_id: Option<String>,
    pub cwd: PathBuf,
    /// The workspace whose other folders Claude gets access to, if the chat is in one.
    pub workspace: Option<ChatWorkspace>,
    /// Text to start the composer with, like an artifact's starter prompt. Not persisted.
    pub draft: Option<String>,
    /// The kind of artifact (`ArtifactKind::as_str`) the first message asks for, from a gallery
    /// tile. Not persisted.
    pub artifact_kind: Option<String>,
}

impl ClaudeChatPaneSnapshot {
    /// A new chat in `cwd`.
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            session_id: None,
            cwd,
            workspace: None,
            draft: None,
            artifact_kind: None,
        }
    }

    /// A chat on the saved conversation `session_id`.
    pub fn resume(cwd: PathBuf, session_id: String) -> Self {
        Self {
            session_id: Some(session_id),
            cwd,
            workspace: None,
            draft: None,
            artifact_kind: None,
        }
    }
}

pub use view::{ClaudeChatView, ClaudeChatViewEvent};

/// For working on the chat: `PICOLAS_OPEN_CHAT=<folder>[#<session id>]` opens that chat when the
/// first window opens, `PICOLAS_CHAT_DRAFT=<text>` starts its composer with text,
/// `PICOLAS_CHAT_ARTIFACT_KIND=motion` starts it as a new artifact of that kind,
/// `PICOLAS_CHAT_SEND=1` sends that text right away (with `PICOLAS_CHAT_MODEL=haiku`, say),
/// `PICOLAS_CHAT_PANEL=1` opens its side panel and `PICOLAS_WINDOW_WIDTH=<px>` sizes the window.
/// `PICOLAS_OPEN_ARTIFACTS=1` opens the artifacts gallery (see
/// [`take_dev_open_artifacts_action`]), or `=<artifact id>` that artifact. Debug builds only.
pub fn take_dev_open_chat_action() -> Option<crate::workspace::WorkspaceAction> {
    use std::sync::atomic::{AtomicBool, Ordering};

    static TAKEN: AtomicBool = AtomicBool::new(false);
    if !cfg!(debug_assertions) || TAKEN.swap(true, Ordering::Relaxed) {
        return None;
    }
    let spec = std::env::var("PICOLAS_OPEN_CHAT").ok()?;
    let (cwd, session_id) = match spec.split_once('#') {
        Some((cwd, session_id)) => (cwd.to_owned(), Some(session_id.to_owned())),
        None => (spec, None),
    };
    Some(crate::workspace::WorkspaceAction::OpenClaudeChat {
        cwd: PathBuf::from(cwd),
        session_id,
        draft: std::env::var("PICOLAS_CHAT_DRAFT").ok(),
        artifact_kind: std::env::var("PICOLAS_CHAT_ARTIFACT_KIND").ok(),
        group: None,
    })
}

/// Id of the workspace `PICOLAS_SEED_WORKSPACE=<name>=<folder>,<folder>...` lists in the Threads
/// panel.
pub const DEV_WORKSPACE_ID: &str = "picolas-dev-workspace";

/// The `PICOLAS_SEED_WORKSPACE` workspace, for a chat opened in its first folder, so
/// `PICOLAS_OPEN_CHAT` can open a workspace chat. Debug builds only.
pub fn dev_workspace_for(cwd: &Path) -> Option<ChatWorkspace> {
    if !cfg!(debug_assertions) {
        return None;
    }
    let spec = std::env::var("PICOLAS_SEED_WORKSPACE").ok()?;
    let (name, dirs) = spec.split_once('=')?;
    let mut dirs = dirs
        .split(',')
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from);
    (dirs.next()? == cwd).then(|| ChatWorkspace {
        id: DEV_WORKSPACE_ID.to_owned(),
        name: name.to_owned(),
        extra_dirs: dirs.collect(),
    })
}

/// `PICOLAS_OPEN_MORE_CHATS=<folder>[#<session id>];...` opens those chats one after another, the
/// way the Threads panel would, once the first window's restored tabs settle. Debug builds only.
pub fn take_dev_open_more_chats_actions() -> Vec<crate::workspace::WorkspaceAction> {
    use std::sync::atomic::{AtomicBool, Ordering};

    static TAKEN: AtomicBool = AtomicBool::new(false);
    if !cfg!(debug_assertions) || TAKEN.swap(true, Ordering::Relaxed) {
        return Vec::new();
    }
    let Ok(specs) = std::env::var("PICOLAS_OPEN_MORE_CHATS") else {
        return Vec::new();
    };
    specs
        .split(';')
        .filter(|spec| !spec.is_empty())
        .map(|spec| {
            let (cwd, session_id) = match spec.split_once('#') {
                Some((cwd, session_id)) => (cwd, Some(session_id.to_owned())),
                None => (spec, None),
            };
            crate::workspace::WorkspaceAction::OpenClaudeChat {
                cwd: PathBuf::from(cwd),
                session_id,
                draft: None,
                artifact_kind: None,
                group: None,
            }
        })
        .collect()
}

/// `PICOLAS_OPEN_SETTINGS=<page slug>` (like `Appearance` or `Features`) opens Settings on that
/// page when the first window opens. Debug builds only.
pub fn take_dev_open_settings_action() -> Option<crate::workspace::WorkspaceAction> {
    use std::sync::atomic::{AtomicBool, Ordering};

    static TAKEN: AtomicBool = AtomicBool::new(false);
    if !cfg!(debug_assertions) || TAKEN.swap(true, Ordering::Relaxed) {
        return None;
    }
    let slug = std::env::var("PICOLAS_OPEN_SETTINGS").ok()?;
    let section = crate::settings_view::SettingsSection::from_slug(&slug)?;
    Some(crate::workspace::WorkspaceAction::ShowSettingsPage(section))
}

/// The `PICOLAS_OPEN_ARTIFACTS` part of [`take_dev_open_chat_action`].
pub fn take_dev_open_artifacts_action() -> Option<crate::workspace::WorkspaceAction> {
    use std::sync::atomic::{AtomicBool, Ordering};

    static TAKEN: AtomicBool = AtomicBool::new(false);
    if !cfg!(debug_assertions) || TAKEN.swap(true, Ordering::Relaxed) {
        return None;
    }
    let spec = std::env::var("PICOLAS_OPEN_ARTIFACTS").ok()?;
    Some(crate::workspace::WorkspaceAction::OpenArtifacts {
        artifact_id: (spec != "1").then_some(spec),
    })
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
