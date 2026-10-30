//! The open native chats by conversation, so the Threads panel can show what each is doing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use claude_chat::reducer::ConversationStatus;
use warpui::{
    AppContext, Entity, EntityId, ModelContext, ModelHandle, SingletonEntity, WeakModelHandle,
};

use super::session::ClaudeChatSession;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeChatStatus {
    Idle,
    Working,
    /// Claude waits for the user to approve or answer something.
    AwaitingUser,
}

impl NativeChatStatus {
    pub fn of(status: &ConversationStatus) -> Self {
        match status {
            ConversationStatus::Working { .. } => Self::Working,
            ConversationStatus::AwaitingUser => Self::AwaitingUser,
            ConversationStatus::Starting
            | ConversationStatus::Idle
            | ConversationStatus::Exited => Self::Idle,
        }
    }
}

/// How much a chat pane holds, which decides whether it can be swapped for another conversation
/// or closed without asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatActivity {
    /// Never sent a message and has nothing typed: closing it loses nothing.
    Unused,
    /// A conversation saved to its transcript, waiting for the next message.
    Idle,
    /// Text or images typed in the composer that weren't sent.
    Drafting,
    /// Claude is working or waiting for the user's answer.
    Busy,
}

impl ChatActivity {
    pub fn new(has_transcript: bool, has_draft: bool, status: NativeChatStatus) -> Self {
        // Without a message sent, nothing the CLI reports (a hidden `/effort` command, say) holds
        // anything worth keeping.
        if !has_transcript && !has_draft {
            return Self::Unused;
        }
        match status {
            NativeChatStatus::Working | NativeChatStatus::AwaitingUser => Self::Busy,
            NativeChatStatus::Idle if has_draft => Self::Drafting,
            NativeChatStatus::Idle if has_transcript => Self::Idle,
            NativeChatStatus::Idle => Self::Unused,
        }
    }

    /// Whether the pane can show another conversation instead without losing anything: the
    /// conversation shown is saved and nothing is running or typed.
    pub fn can_be_replaced(self) -> bool {
        match self {
            Self::Unused | Self::Idle => true,
            Self::Drafting | Self::Busy => false,
        }
    }
}

/// An open native chat, as the Threads panel lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenChat {
    pub provider: super::ChatProvider,
    pub session_id: String,
    pub cwd: PathBuf,
    pub title: Option<String>,
    pub has_transcript: bool,
    /// The Threads panel workspace the chat is in.
    pub workspace_id: Option<String>,
}

struct RegisteredSession {
    id: EntityId,
    session: WeakModelHandle<ClaudeChatSession>,
}

#[derive(Default)]
pub struct NativeChatRegistry {
    /// Chats shown in a pane. Weak, so a chat dropped without unregistering still drops out.
    sessions: HashMap<String, RegisteredSession>,
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
        // A Codex chat's id changes once its thread starts; drop the entry under the old one.
        self.sessions
            .retain(|id, registered| id == session_id || registered.id != session.id());
        self.sessions.insert(
            session_id.to_owned(),
            RegisteredSession {
                id: session.id(),
                session: session.downgrade(),
            },
        );
        ctx.emit(NativeChatRegistryEvent::Changed);
    }

    /// Drops the chat once its pane is closed (even while it waits to be reopened with undo).
    pub fn unregister(
        &mut self,
        session_id: &str,
        session: &ModelHandle<ClaudeChatSession>,
        ctx: &mut ModelContext<Self>,
    ) {
        if self
            .sessions
            .get(session_id)
            .is_some_and(|registered| registered.id == session.id())
        {
            self.sessions.remove(session_id);
            ctx.emit(NativeChatRegistryEvent::Changed);
        }
    }

    /// Tells observers that a chat's status changed.
    pub fn notify_changed(&mut self, ctx: &mut ModelContext<Self>) {
        ctx.emit(NativeChatRegistryEvent::Changed);
    }

    fn session(
        &self,
        session_id: &str,
        app: &AppContext,
    ) -> Option<ModelHandle<ClaudeChatSession>> {
        self.sessions.get(session_id)?.session.upgrade(app)
    }

    /// Every open chat: its conversation, folder, title and whether it has a transcript yet.
    pub fn open_chats(&self, app: &AppContext) -> Vec<OpenChat> {
        self.sessions
            .iter()
            .filter_map(|(session_id, registered)| {
                let session = registered.session.upgrade(app)?;
                let session = session.as_ref(app);
                Some(OpenChat {
                    provider: session.provider(),
                    session_id: session_id.clone(),
                    cwd: session.cwd().to_path_buf(),
                    title: session.title().map(str::to_owned),
                    has_transcript: session.has_transcript(),
                    workspace_id: session.workspace().map(|workspace| workspace.id.clone()),
                })
            })
            .collect()
    }

    /// The open chat in `cwd`, outside every workspace, that hasn't been sent a message, to reuse
    /// instead of opening another.
    pub fn unused_chat_in(&self, cwd: &Path, app: &AppContext) -> Option<String> {
        self.open_chats(app)
            .into_iter()
            .find(|chat| !chat.has_transcript && chat.cwd == cwd && chat.workspace_id.is_none())
            .map(|chat| chat.session_id)
    }

    /// [`Self::unused_chat_in`] for the workspace `workspace_id`.
    pub fn unused_chat_in_workspace(&self, workspace_id: &str, app: &AppContext) -> Option<String> {
        self.open_chats(app)
            .into_iter()
            .find(|chat| !chat.has_transcript && chat.workspace_id.as_deref() == Some(workspace_id))
            .map(|chat| chat.session_id)
    }

    /// Whether `session_id` is open in a chat that never sent a message.
    pub fn is_unsent(&self, session_id: &str, app: &AppContext) -> bool {
        self.session(session_id, app)
            .is_some_and(|session| !session.as_ref(app).has_transcript())
    }

    /// What the open chat showing `session_id` is doing, if one is open.
    pub fn status(&self, session_id: &str, app: &AppContext) -> Option<NativeChatStatus> {
        let session = self.session(session_id, app)?;
        Some(NativeChatStatus::of(
            &session.as_ref(app).conversation().status(),
        ))
    }
}

impl Entity for NativeChatRegistry {
    type Event = NativeChatRegistryEvent;
}

impl SingletonEntity for NativeChatRegistry {}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
