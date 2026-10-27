//! The open native chats by conversation, so the Threads panel can show what each is doing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use claude_chat::reducer::ConversationStatus;
use warpui::{AppContext, Entity, ModelContext, ModelHandle, SingletonEntity, WeakModelHandle};

use super::session::ClaudeChatSession;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeChatStatus {
    Idle,
    Working,
    /// Claude waits for the user to approve or answer something.
    AwaitingUser,
}

/// An open native chat, as the Threads panel lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenChat {
    pub session_id: String,
    pub cwd: PathBuf,
    pub title: Option<String>,
    pub has_transcript: bool,
}

#[derive(Default)]
pub struct NativeChatRegistry {
    /// Weak, so a chat drops out once its pane is closed.
    sessions: HashMap<String, WeakModelHandle<ClaudeChatSession>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeChatRegistryEvent {
    Changed,
}

impl NativeChatRegistry {
    pub fn register(
        &mut self,
        session_id: &str,
        session: &ModelHandle<ClaudeChatSession>,
        ctx: &mut ModelContext<Self>,
    ) {
        self.sessions
            .insert(session_id.to_owned(), session.downgrade());
        ctx.emit(NativeChatRegistryEvent::Changed);
    }

    /// Tells observers that a chat's status changed.
    pub fn notify_changed(&mut self, ctx: &mut ModelContext<Self>) {
        ctx.emit(NativeChatRegistryEvent::Changed);
    }

    /// Every open chat: its conversation, folder, title and whether it has a transcript yet.
    pub fn open_chats(&self, app: &AppContext) -> Vec<OpenChat> {
        self.sessions
            .iter()
            .filter_map(|(session_id, session)| {
                let session = session.upgrade(app)?;
                let session = session.as_ref(app);
                Some(OpenChat {
                    session_id: session_id.clone(),
                    cwd: session.cwd().to_path_buf(),
                    title: session.title().map(str::to_owned),
                    has_transcript: session.has_transcript(),
                })
            })
            .collect()
    }

    /// The open chat in `cwd` that hasn't been sent a message, to reuse instead of opening another.
    pub fn unused_chat_in(&self, cwd: &Path, app: &AppContext) -> Option<String> {
        self.open_chats(app)
            .into_iter()
            .find(|chat| !chat.has_transcript && chat.cwd == cwd)
            .map(|chat| chat.session_id)
    }

    /// What the open chat showing `session_id` is doing, if one is open.
    pub fn status(&self, session_id: &str, app: &AppContext) -> Option<NativeChatStatus> {
        let session = self.sessions.get(session_id)?.upgrade(app)?;
        Some(match session.as_ref(app).conversation().status() {
            ConversationStatus::Working { .. } => NativeChatStatus::Working,
            ConversationStatus::AwaitingUser => NativeChatStatus::AwaitingUser,
            ConversationStatus::Starting
            | ConversationStatus::Idle
            | ConversationStatus::Exited => NativeChatStatus::Idle,
        })
    }
}

impl Entity for NativeChatRegistry {
    type Event = NativeChatRegistryEvent;
}

impl SingletonEntity for NativeChatRegistry {}
