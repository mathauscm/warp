//! One native chat's connection to Claude Code: the `claude` process driven over stream-json, the
//! conversation it builds, and the requests the UI answers. Views observe it through
//! [`ClaudeChatSessionEvent`]s.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use claude_chat::artifacts::{self, ArtifactContext, ArtifactStore};
use claude_chat::history::{self, LoadedTranscript};
use claude_chat::process::{self, ClaudeProcess, SpawnConfig};
use claude_chat::protocol::{
    self, ControlRequestBody, IncomingControlRequest, IncomingRequest, OutgoingImage, StdoutMessage,
};
use claude_chat::reducer::{
    ControlReply, Conversation, ConversationChange, ConversationStatus, ReplyPayload,
    RequestDecision,
};
use claude_chat::timeline::{ImageAttachment, PendingRequestKind, TimelineItem};
use serde_json::{Value, json};
use warpui::r#async::Timer;
use warpui::{Entity, ModelContext, SingletonEntity};

use crate::terminal::local_shell::LocalShellState;

/// How often MCP servers are polled while some are still connecting.
const MCP_POLL_INTERVAL: Duration = Duration::from_secs(3);
/// MCP servers usually connect within a few seconds; stop polling after this many tries.
const MCP_POLL_ATTEMPTS: usize = 10;
const TITLE_MAX_CHARS: usize = 60;
/// Appended to Claude Code's system prompt. Kept neutral so Claude behaves as it does in a
/// terminal; the artifacts tools describe themselves. It must not be empty: without an appended
/// prompt, a headless CLI introduces itself as a generic "Claude agent" instead of Claude Code.
const APPEND_SYSTEM_PROMPT: &str =
    "You are running inside Picolas Code, a desktop app with a native chat UI for Claude Code.";

/// Where the native chat keeps its artifacts.
pub fn artifacts_root() -> PathBuf {
    warp_core::paths::data_dir().join("artifacts")
}

/// `~/.claude`, where Claude Code keeps its transcripts.
fn claude_home() -> Option<PathBuf> {
    static HOME: OnceLock<Option<PathBuf>> = OnceLock::new();
    HOME.get_or_init(|| dirs::home_dir().map(|home| home.join(".claude")))
        .clone()
}

/// The top-level `effortLevel` in Claude Code's settings for `cwd`: local, then project, then user
/// settings, the way the CLI layers them.
fn configured_effort(cwd: &Path) -> Option<String> {
    let project = cwd.join(".claude");
    [
        Some(project.join("settings.local.json")),
        Some(project.join("settings.json")),
        claude_home().map(|home| home.join("settings.json")),
    ]
    .into_iter()
    .flatten()
    .find_map(|path| {
        let settings: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
        settings.get("effortLevel")?.as_str().map(str::to_owned)
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessState {
    /// Looking up `claude` and starting it.
    Starting,
    Running,
    /// The process ended; `error` explains why when it wasn't asked to.
    Exited {
        error: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClaudeChatSessionEvent {
    Changed(Vec<ConversationChange>),
    ProcessStateChanged,
    TitleChanged,
    /// The saved conversation finished loading (it may have been empty).
    HistoryLoaded,
    /// The `session_id` a new chat got, so its pane can restore it.
    SessionIdAssigned,
    FileSuggestions {
        query: String,
        paths: Vec<String>,
    },
    /// The CLI answered taking the queued message `uuid` back; `cancelled` is false when Claude
    /// had already taken it in.
    QueuedMessageCancelled {
        uuid: String,
        cancelled: bool,
    },
    /// A request the UI made failed, with a message to show.
    Error(String),
}

/// How the user answered a permission prompt.
#[derive(Debug, Clone, PartialEq)]
pub enum PermissionAnswer {
    AllowOnce,
    /// Allow, and echo one of the CLI's suggestions so it isn't asked again.
    AllowAlways {
        suggestion: Value,
    },
    Deny {
        message: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlanAnswer {
    Approve { accept_edits: bool },
    KeepPlanning { feedback: String },
}

pub struct ClaudeChatSession {
    cwd: PathBuf,
    session_id: String,
    /// Whether `session_id` names a saved conversation to resume, rather than a new one.
    resume: bool,
    /// Whether a message was sent since this chat opened, which saves its conversation.
    has_sent: bool,
    title: Option<String>,
    conversation: Conversation,
    tracker: claude_chat::reducer::RequestTracker,
    process: Option<ClaudeProcess>,
    state: ProcessState,
    /// Lines written before the process started, sent once it's up.
    queued_lines: Vec<String>,
    history_loaded: bool,
    model_override: Option<String>,
    effort_override: Option<String>,
    artifacts: ArtifactStore,
}

impl ClaudeChatSession {
    /// Opens the conversation `session_id` in `cwd`, or a new one.
    pub fn new(cwd: PathBuf, session_id: Option<String>, ctx: &mut ModelContext<Self>) -> Self {
        let transcript = session_id.as_deref().and_then(|session_id| {
            let home = claude_home()?;
            let path = history::transcript_path(&home, &cwd, session_id);
            if path.exists() {
                Some(path)
            } else {
                history::find_transcript(&home, session_id)
            }
        });
        let resume = transcript.is_some();
        let session_id = session_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let mut session = Self {
            cwd,
            session_id: session_id.clone(),
            resume,
            has_sent: false,
            title: None,
            conversation: Conversation::new(Some(session_id)),
            tracker: Default::default(),
            process: None,
            state: ProcessState::Starting,
            queued_lines: Vec::new(),
            history_loaded: transcript.is_none(),
            model_override: None,
            effort_override: None,
            artifacts: ArtifactStore::new(artifacts_root()),
        };
        if let Some(path) = transcript {
            session.load_history(path, ctx);
        }
        session.start(ctx);
        session
    }

    pub fn conversation(&self) -> &Conversation {
        &self.conversation
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Whether the conversation has anything saved yet; new chats only get a transcript with their
    /// first message.
    pub fn has_transcript(&self) -> bool {
        self.resume || self.has_sent
    }

    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    pub fn process_state(&self) -> &ProcessState {
        &self.state
    }

    pub fn is_history_loaded(&self) -> bool {
        self.history_loaded
    }

    pub fn artifacts(&self) -> &ArtifactStore {
        &self.artifacts
    }

    // --- process lifecycle ------------------------------------------------------------------

    fn start(&mut self, ctx: &mut ModelContext<Self>) {
        self.state = ProcessState::Starting;
        ctx.emit(ClaudeChatSessionEvent::ProcessStateChanged);
        let path_future = LocalShellState::handle(ctx)
            .update(ctx, |shell, ctx| shell.get_interactive_path_env_var(ctx));
        ctx.spawn(path_future, |me, path_env, ctx| {
            me.spawn_process(path_env, ctx)
        });
    }

    fn spawn_process(&mut self, path_env: Option<String>, ctx: &mut ModelContext<Self>) {
        let path_env = path_env.or_else(|| std::env::var("PATH").ok());
        let Some(claude_path) = process::find_claude(path_env.as_deref()) else {
            self.fail_to_start(
                "O Claude Code não foi encontrado. Instale com `npm install -g \
                 @anthropic-ai/claude-code` e faça login com `claude`."
                    .to_owned(),
                ctx,
            );
            return;
        };
        let mut config = SpawnConfig::new(claude_path, self.cwd.clone());
        config.path_env = path_env;
        config.model = self.model_override.clone();
        config.effort = self.effort_override.clone();
        if self.resume {
            config.resume = Some(self.session_id.clone());
        } else {
            config.session_id = Some(self.session_id.clone());
        }
        let (process, driver) = match process::spawn(&config) {
            Ok(spawned) => spawned,
            Err(err) => {
                self.fail_to_start(
                    format!("Não foi possível iniciar o Claude Code: {err}"),
                    ctx,
                );
                return;
            }
        };
        ctx.spawn_stream_local(
            process.messages(),
            |me, message, ctx| me.on_message(message, ctx),
            |_, _| {},
        );
        ctx.spawn(driver, |me, status, ctx| me.on_process_exit(status, ctx));
        self.process = Some(process);
        self.state = ProcessState::Running;
        ctx.emit(ClaudeChatSessionEvent::ProcessStateChanged);

        self.request(
            ControlRequestBody::Initialize {
                sdk_mcp_servers: vec![artifacts::SERVER_NAME.to_owned()],
                append_system_prompt: Some(APPEND_SYSTEM_PROMPT.to_owned()),
            },
            ctx,
        );
        for line in std::mem::take(&mut self.queued_lines) {
            self.write(line);
        }
        self.request(ControlRequestBody::McpStatus, ctx);
        self.refresh_usage(ctx);
        self.refresh_context_usage(ctx);
        self.poll_mcp_status(MCP_POLL_ATTEMPTS, ctx);
    }

    fn fail_to_start(&mut self, error: String, ctx: &mut ModelContext<Self>) {
        log::warn!("Native chat: {error}");
        self.state = ProcessState::Exited {
            error: Some(error.clone()),
        };
        let changes = self
            .conversation
            .mark_exited(Some(error), SystemTime::now());
        ctx.emit(ClaudeChatSessionEvent::ProcessStateChanged);
        ctx.emit(ClaudeChatSessionEvent::Changed(changes));
    }

    fn on_process_exit(
        &mut self,
        status: std::io::Result<std::process::ExitStatus>,
        ctx: &mut ModelContext<Self>,
    ) {
        let error = match status {
            Ok(status) if status.success() => None,
            Ok(status) => Some(format!("O Claude Code terminou ({status}).")),
            Err(err) => Some(format!("O Claude Code terminou com erro: {err}")),
        };
        self.process = None;
        self.state = ProcessState::Exited {
            error: error.clone(),
        };
        let changes = self.conversation.mark_exited(error, SystemTime::now());
        ctx.emit(ClaudeChatSessionEvent::ProcessStateChanged);
        ctx.emit(ClaudeChatSessionEvent::Changed(changes));
    }

    /// Starts Claude again after it exited, resuming the same conversation.
    pub fn restart(&mut self, ctx: &mut ModelContext<Self>) {
        if self.process.is_some() {
            return;
        }
        self.resume = self.has_transcript();
        self.conversation = Conversation::new(Some(self.session_id.clone()));
        self.history_loaded = !self.resume;
        if self.resume
            && let Some(home) = claude_home()
        {
            self.load_history(
                history::transcript_path(&home, &self.cwd, &self.session_id),
                ctx,
            );
        }
        ctx.emit(ClaudeChatSessionEvent::Changed(Vec::new()));
        self.start(ctx);
    }

    /// Moves a chat that hasn't started its conversation to another folder.
    pub fn change_cwd(&mut self, cwd: PathBuf, ctx: &mut ModelContext<Self>) {
        if self.has_transcript() || cwd == self.cwd {
            return;
        }
        self.cwd = cwd;
        self.stop();
        self.conversation = Conversation::new(Some(self.session_id.clone()));
        ctx.emit(ClaudeChatSessionEvent::Changed(Vec::new()));
        self.start(ctx);
    }

    /// Ends the process gracefully: Claude exits when its stdin closes.
    fn stop(&mut self) {
        if let Some(process) = self.process.take() {
            process.close_stdin();
        }
    }

    fn load_history(&mut self, path: PathBuf, ctx: &mut ModelContext<Self>) {
        self.history_loaded = false;
        ctx.spawn(
            async move { history::load_transcript(&path) },
            |me, loaded, ctx| me.on_history_loaded(loaded, ctx),
        );
    }

    fn on_history_loaded(
        &mut self,
        loaded: anyhow::Result<LoadedTranscript>,
        ctx: &mut ModelContext<Self>,
    ) {
        self.history_loaded = true;
        match loaded {
            Ok(transcript) => {
                if transcript.title.is_some() {
                    self.title = transcript.title.clone();
                    ctx.emit(ClaudeChatSessionEvent::TitleChanged);
                }
                let changes = self.conversation.load_history(transcript.items);
                ctx.emit(ClaudeChatSessionEvent::Changed(changes));
            }
            Err(err) => {
                log::warn!("Couldn't load the Claude transcript: {err:#}");
                ctx.emit(ClaudeChatSessionEvent::Error(
                    "Não foi possível carregar o histórico desta conversa.".to_owned(),
                ));
            }
        }
        ctx.emit(ClaudeChatSessionEvent::HistoryLoaded);
    }

    fn poll_mcp_status(&mut self, attempts_left: usize, ctx: &mut ModelContext<Self>) {
        if attempts_left == 0 {
            return;
        }
        ctx.spawn(Timer::after(MCP_POLL_INTERVAL), move |me, _, ctx| {
            if me.process.is_none() {
                return;
            }
            let connecting = me.conversation.mcp_servers().is_empty()
                || me
                    .conversation
                    .mcp_servers()
                    .iter()
                    .any(|server| server.status == "pending");
            if connecting {
                me.request(ControlRequestBody::McpStatus, ctx);
                me.poll_mcp_status(attempts_left - 1, ctx);
            }
        });
    }

    // --- IO ---------------------------------------------------------------------------------

    fn write(&mut self, line: String) {
        match &self.process {
            Some(process) if self.state == ProcessState::Running => {
                if !process.send_line(line) {
                    log::warn!("Native chat: Claude's stdin is closed");
                }
            }
            _ => self.queued_lines.push(line),
        }
    }

    fn request(&mut self, request: ControlRequestBody, _ctx: &mut ModelContext<Self>) {
        let (_, line) = self.tracker.issue(request);
        self.write(line);
    }

    fn apply_changes(&mut self, changes: Vec<ConversationChange>, ctx: &mut ModelContext<Self>) {
        if !changes.is_empty() {
            ctx.emit(ClaudeChatSessionEvent::Changed(changes));
        }
    }

    fn on_message(&mut self, message: StdoutMessage, ctx: &mut ModelContext<Self>) {
        match message {
            StdoutMessage::ControlRequest(request) => self.on_control_request(request, ctx),
            StdoutMessage::ControlResponse(response) => {
                if let Some(reply) = self.tracker.resolve(response) {
                    self.on_control_reply(reply, ctx);
                }
            }
            message => {
                let finished_turn = matches!(message, StdoutMessage::Result(_));
                let queued = self.conversation.queued_messages().len();
                let changes = self.conversation.apply(message, SystemTime::now());
                if changes.is_empty() && self.conversation.queued_messages().len() != queued {
                    // The queue isn't a change of its own; an empty batch still has views re-read it.
                    ctx.emit(ClaudeChatSessionEvent::Changed(changes));
                } else {
                    self.apply_changes(changes, ctx);
                }
                if finished_turn {
                    self.on_turn_finished(ctx);
                }
            }
        }
    }

    fn on_turn_finished(&mut self, ctx: &mut ModelContext<Self>) {
        self.refresh_context_usage(ctx);
        self.refresh_usage(ctx);
        if self.title.is_none()
            && let Some(title) = self.first_prompt_title()
        {
            self.title = Some(title);
            ctx.emit(ClaudeChatSessionEvent::TitleChanged);
        }
    }

    fn first_prompt_title(&self) -> Option<String> {
        self.conversation
            .items()
            .iter()
            .find_map(|item| match item {
                TimelineItem::User(message) if message.command.is_none() => {
                    let line = message.text.lines().find(|line| !line.trim().is_empty())?;
                    let mut title: String = line.trim().chars().take(TITLE_MAX_CHARS).collect();
                    if line.trim().chars().count() > TITLE_MAX_CHARS {
                        title.push('…');
                    }
                    Some(title)
                }
                _ => None,
            })
    }

    fn on_control_request(
        &mut self,
        request: IncomingControlRequest,
        ctx: &mut ModelContext<Self>,
    ) {
        match &request.request {
            IncomingRequest::McpMessage {
                server_name,
                message,
            } if server_name == artifacts::SERVER_NAME => {
                let context = ArtifactContext {
                    session_id: Some(self.session_id.clone()),
                    cwd: Some(self.cwd.clone()),
                };
                let response = artifacts::handle_mcp_message(&self.artifacts, &context, message)
                    .unwrap_or_else(|| json!({ "jsonrpc": "2.0", "result": {}, "id": 0 }));
                self.write(protocol::control_success_line(
                    &request.request_id,
                    protocol::mcp_message_response(response),
                ));
            }
            IncomingRequest::McpMessage { server_name, .. } => {
                self.write(protocol::control_error_line(
                    &request.request_id,
                    &format!("Unknown SDK MCP server: {server_name}"),
                ));
            }
            IncomingRequest::Elicitation(_) => {
                self.write(protocol::control_success_line(
                    &request.request_id,
                    json!({ "action": "decline" }),
                ));
            }
            IncomingRequest::HookCallback { .. } => {
                self.write(protocol::control_success_line(
                    &request.request_id,
                    json!({}),
                ));
            }
            IncomingRequest::CanUseTool(_) => {
                let changes = self.conversation.apply(
                    StdoutMessage::ControlRequest(request.clone()),
                    SystemTime::now(),
                );
                self.apply_changes(changes, ctx);
            }
            IncomingRequest::Other { subtype, .. } => {
                self.write(protocol::control_error_line(
                    &request.request_id,
                    &format!("Unsupported control request subtype: {subtype}"),
                ));
            }
        }
    }

    fn on_control_reply(&mut self, reply: ControlReply, ctx: &mut ModelContext<Self>) {
        let changes = self.conversation.apply_control_reply(&reply);
        self.apply_changes(changes, ctx);
        match &reply.outcome {
            Ok(ReplyPayload::McpAuthenticate(response)) => {
                if let Some(url) = &response.auth_url {
                    ctx.open_url(url);
                }
                if let ControlRequestBody::McpAuthenticate { server_name } = &reply.request {
                    self.request(
                        ControlRequestBody::McpReconnect {
                            server_name: server_name.clone(),
                        },
                        ctx,
                    );
                    self.poll_mcp_status(MCP_POLL_ATTEMPTS * 4, ctx);
                }
            }
            Ok(ReplyPayload::CancelAsyncMessage(response)) => {
                if let ControlRequestBody::CancelAsyncMessage { message_uuid } = &reply.request {
                    if response.cancelled {
                        let changes = self
                            .conversation
                            .withdraw_user_message(message_uuid, SystemTime::now());
                        self.apply_changes(changes, ctx);
                    }
                    ctx.emit(ClaudeChatSessionEvent::QueuedMessageCancelled {
                        uuid: message_uuid.clone(),
                        cancelled: response.cancelled,
                    });
                }
            }
            Ok(ReplyPayload::FileSuggestions(response)) => {
                if let ControlRequestBody::FileSuggestions { query } = &reply.request {
                    ctx.emit(ClaudeChatSessionEvent::FileSuggestions {
                        query: query.clone(),
                        paths: response.paths(),
                    });
                }
            }
            Ok(_) => {
                if matches!(
                    reply.request,
                    ControlRequestBody::McpToggle { .. } | ControlRequestBody::McpReconnect { .. }
                ) {
                    self.request(ControlRequestBody::McpStatus, ctx);
                }
            }
            Err(error) => {
                log::warn!("Native chat: {} failed: {error}", reply.request.subtype());
                if let ControlRequestBody::CancelAsyncMessage { message_uuid } = &reply.request {
                    ctx.emit(ClaudeChatSessionEvent::QueuedMessageCancelled {
                        uuid: message_uuid.clone(),
                        cancelled: false,
                    });
                }
                if !matches!(
                    reply.request,
                    ControlRequestBody::GetUsage { .. }
                        | ControlRequestBody::GetContextUsage { .. }
                ) {
                    ctx.emit(ClaudeChatSessionEvent::Error(error.clone()));
                }
            }
        }
    }

    // --- actions ----------------------------------------------------------------------------

    /// Sends a message (or a slash command typed as text) to Claude. Returns its uuid.
    pub fn send_message(
        &mut self,
        text: String,
        images: Vec<OutgoingImage>,
        ctx: &mut ModelContext<Self>,
    ) -> String {
        if self.process.is_none() && matches!(self.state, ProcessState::Exited { .. }) {
            self.restart(ctx);
        }
        let uuid = uuid::Uuid::new_v4().to_string();
        let attachments = images
            .iter()
            .map(|image| ImageAttachment {
                media_type: image.media_type.clone(),
                path: None,
                data: Some(image.data_base64.clone()),
            })
            .collect();
        let changes =
            self.conversation
                .push_local_user_message(&uuid, &text, attachments, SystemTime::now());
        self.write(protocol::user_message_line(&uuid, &text, &images));
        self.apply_changes(changes, ctx);
        if !self.has_sent {
            self.has_sent = true;
            ctx.emit(ClaudeChatSessionEvent::SessionIdAssigned);
        }
        uuid
    }

    /// Asks the CLI to drop the queued message `uuid`; see
    /// [`ClaudeChatSessionEvent::QueuedMessageCancelled`].
    pub fn cancel_queued_message(&mut self, uuid: String, ctx: &mut ModelContext<Self>) {
        self.request(
            ControlRequestBody::CancelAsyncMessage { message_uuid: uuid },
            ctx,
        );
    }

    /// Has Claude take the queued message `uuid` in now, making way for it in the running turn.
    pub fn send_queued_message_now(&mut self, uuid: String, ctx: &mut ModelContext<Self>) {
        self.request(
            ControlRequestBody::SendNow {
                message_uuid: uuid,
                send_now: true,
            },
            ctx,
        );
    }

    /// Runs a slash command on the user's behalf without showing it in the conversation.
    fn send_hidden_command(&mut self, command: String, ctx: &mut ModelContext<Self>) {
        let uuid = uuid::Uuid::new_v4().to_string();
        let changes = self.conversation.hide_replies_to(&uuid);
        self.apply_changes(changes, ctx);
        self.write(protocol::user_message_line(&uuid, &command, &[]));
    }

    pub fn interrupt(&mut self, ctx: &mut ModelContext<Self>) {
        if matches!(
            self.conversation.status(),
            ConversationStatus::Working { .. } | ConversationStatus::AwaitingUser
        ) {
            self.request(ControlRequestBody::Interrupt, ctx);
        }
    }

    fn pending_request(&self, request_id: &str) -> Option<&claude_chat::timeline::PendingRequest> {
        self.conversation
            .pending_requests()
            .iter()
            .find(|request| request.request_id == request_id)
    }

    fn answer(
        &mut self,
        request_id: &str,
        response: Value,
        decision: RequestDecision,
        ctx: &mut ModelContext<Self>,
    ) {
        self.write(protocol::control_success_line(request_id, response));
        let changes = self.conversation.resolve_request(request_id, decision);
        self.apply_changes(changes, ctx);
    }

    pub fn answer_permission(
        &mut self,
        request_id: &str,
        answer: PermissionAnswer,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(request) = self.pending_request(request_id) else {
            return;
        };
        let PendingRequestKind::Permission { input, .. } = &request.kind else {
            return;
        };
        let tool_use_id = request.tool_use_id.clone();
        let input = input.clone();
        let (response, decision) = match answer {
            PermissionAnswer::AllowOnce => (
                protocol::permission_allow(input, None, tool_use_id.as_deref()),
                RequestDecision::Allowed,
            ),
            PermissionAnswer::AllowAlways { suggestion } => (
                protocol::permission_allow(input, Some(vec![suggestion]), tool_use_id.as_deref()),
                RequestDecision::Allowed,
            ),
            PermissionAnswer::Deny { message } => (
                protocol::permission_deny(
                    message
                        .as_deref()
                        .unwrap_or("The user declined this action."),
                    false,
                ),
                RequestDecision::Denied,
            ),
        };
        self.answer(request_id, response, decision, ctx);
    }

    /// Answers an AskUserQuestion prompt; `answers` pairs each question's text with the chosen
    /// labels joined by ", " (or the user's own text). An empty list dismisses it.
    pub fn answer_question(
        &mut self,
        request_id: &str,
        answers: Vec<(String, String)>,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(request) = self.pending_request(request_id) else {
            return;
        };
        let PendingRequestKind::Question { input, .. } = &request.kind else {
            return;
        };
        if answers.is_empty() {
            let response = protocol::permission_deny("The user dismissed the question.", false);
            self.answer(request_id, response, RequestDecision::Denied, ctx);
            return;
        }
        let response = protocol::question_answers(input, &answers, request.tool_use_id.as_deref());
        self.answer(request_id, response, RequestDecision::Allowed, ctx);
    }

    pub fn answer_plan(
        &mut self,
        request_id: &str,
        answer: PlanAnswer,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(request) = self.pending_request(request_id) else {
            return;
        };
        let PendingRequestKind::PlanApproval { input, .. } = &request.kind else {
            return;
        };
        let (response, decision) = match answer {
            PlanAnswer::Approve { accept_edits } => (
                protocol::plan_approve(input.clone(), accept_edits, request.tool_use_id.as_deref()),
                RequestDecision::Allowed,
            ),
            PlanAnswer::KeepPlanning { feedback } => {
                (protocol::plan_reject(&feedback), RequestDecision::Denied)
            }
        };
        self.answer(request_id, response, decision, ctx);
    }

    /// `None` goes back to the default model.
    pub fn set_model(&mut self, model: Option<String>, ctx: &mut ModelContext<Self>) {
        self.model_override = model.clone();
        self.request(ControlRequestBody::SetModel { model }, ctx);
    }

    pub fn set_effort(&mut self, effort: String, ctx: &mut ModelContext<Self>) {
        self.effort_override = Some(effort.clone());
        self.send_hidden_command(format!("/effort {effort}"), ctx);
    }

    /// Goes back to the effort configured in Claude Code's settings. `/effort auto` would drop to
    /// the API default instead, below what the user configured.
    pub fn reset_effort(&mut self, ctx: &mut ModelContext<Self>) {
        match configured_effort(&self.cwd) {
            Some(effort) => self.set_effort(effort, ctx),
            None => self.effort_override = None,
        }
    }

    /// The effort the user picked in this session, which the CLI doesn't always report back.
    pub fn effort(&self) -> Option<&str> {
        self.effort_override
            .as_deref()
            .or(self.conversation.session().effort.as_deref())
    }

    pub fn set_permission_mode(&mut self, mode: &str, ctx: &mut ModelContext<Self>) {
        self.request(
            ControlRequestBody::SetPermissionMode {
                mode: mode.to_owned(),
            },
            ctx,
        );
    }

    pub fn set_remote_control(&mut self, enabled: bool, ctx: &mut ModelContext<Self>) {
        let name = self.title.clone();
        self.request(ControlRequestBody::RemoteControl { enabled, name }, ctx);
    }

    pub fn refresh_mcp_status(&mut self, ctx: &mut ModelContext<Self>) {
        self.request(ControlRequestBody::McpStatus, ctx);
    }

    /// Opens the server's sign-in page in the browser, then reconnects it.
    pub fn authenticate_mcp_server(&mut self, server_name: String, ctx: &mut ModelContext<Self>) {
        self.request(ControlRequestBody::McpAuthenticate { server_name }, ctx);
    }

    pub fn set_mcp_server_enabled(
        &mut self,
        server_name: String,
        enabled: bool,
        ctx: &mut ModelContext<Self>,
    ) {
        self.request(
            ControlRequestBody::McpToggle {
                server_name,
                enabled,
            },
            ctx,
        );
    }

    pub fn reconnect_mcp_server(&mut self, server_name: String, ctx: &mut ModelContext<Self>) {
        self.request(ControlRequestBody::McpReconnect { server_name }, ctx);
    }

    pub fn refresh_context_usage(&mut self, ctx: &mut ModelContext<Self>) {
        self.request(
            ControlRequestBody::GetContextUsage {
                detail: Some("summary".to_owned()),
            },
            ctx,
        );
    }

    pub fn refresh_usage(&mut self, ctx: &mut ModelContext<Self>) {
        self.request(
            ControlRequestBody::GetUsage {
                skip_behaviors: Some(true),
            },
            ctx,
        );
    }

    /// Summarizes the conversation so far to free up context.
    pub fn compact(&mut self, ctx: &mut ModelContext<Self>) {
        self.send_message("/compact".to_owned(), Vec::new(), ctx);
    }

    pub fn request_file_suggestions(&mut self, query: String, ctx: &mut ModelContext<Self>) {
        self.request(ControlRequestBody::FileSuggestions { query }, ctx);
    }
}

impl Drop for ClaudeChatSession {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Entity for ClaudeChatSession {
    type Event = ClaudeChatSessionEvent;
}

// --- composer ---
impl ClaudeChatSession {
    /// The model picked in this chat (a `set_model` value like `opus`), which the CLI only
    /// reports again when it restarts.
    pub fn chosen_model(&self) -> Option<&str> {
        self.model_override.as_deref()
    }

    /// Sends `instructions` followed by `text`, while the conversation shows only `text` (until
    /// it's reloaded from the transcript, which has both).
    pub fn send_message_with_instructions(
        &mut self,
        instructions: &str,
        text: String,
        images: Vec<OutgoingImage>,
        ctx: &mut ModelContext<Self>,
    ) -> String {
        if self.process.is_none() && matches!(self.state, ProcessState::Exited { .. }) {
            self.restart(ctx);
        }
        let uuid = uuid::Uuid::new_v4().to_string();
        let attachments = images
            .iter()
            .map(|image| ImageAttachment {
                media_type: image.media_type.clone(),
                path: None,
                data: Some(image.data_base64.clone()),
            })
            .collect();
        let changes =
            self.conversation
                .push_local_user_message(&uuid, &text, attachments, SystemTime::now());
        let sent = format!("{instructions}\n\n{text}");
        self.write(protocol::user_message_line(&uuid, &sent, &images));
        self.apply_changes(changes, ctx);
        if !self.has_sent {
            self.has_sent = true;
            ctx.emit(ClaudeChatSessionEvent::SessionIdAssigned);
        }
        uuid
    }
}

// --- renaming ---------------------------------------------------------------------------------

impl ClaudeChatSession {
    /// Names the conversation, the way Claude Code's `/rename` does, so Threads and `claude
    /// --resume` show it too.
    pub fn rename(&mut self, title: String, ctx: &mut ModelContext<Self>) {
        if self.has_transcript()
            && let Some(home) = claude_home()
        {
            let path = history::transcript_path(&home, &self.cwd, &self.session_id);
            match crate::claude_threads::rename_transcript(&path, &self.session_id, &title) {
                Ok(title) => self.title = Some(title),
                Err(err) => {
                    ctx.emit(ClaudeChatSessionEvent::Error(err));
                    return;
                }
            }
        } else {
            self.title = Some(title);
        }
        ctx.emit(ClaudeChatSessionEvent::TitleChanged);
    }
}
