//! The live state of one Claude session, built purely from the CLI's stdout messages: the timeline,
//! the requests waiting for the user, and everything the chat chrome shows (model, permission mode,
//! slash commands, MCP servers, context and rate-limit usage, todos, subagent tasks).
//!
//! No IO happens here. The session feeds every parsed [`StdoutMessage`] to
//! [`Conversation::apply`] and repaints what the returned [`ConversationChange`]s name.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::protocol::{
    AccountInfo, AssistantMessage, CanUseToolRequest, CancelAsyncMessageResponse, CommandLifecycle,
    CompactBoundary, ContentBlock, ContentDelta, ContextUsage, ControlRequestBody, ControlResponse,
    FileSuggestionsResponse, INTERRUPT_MARKER, IncomingRequest, InitializeResponse,
    InterruptResponse, McpAuthenticateResponse, McpServerStatus, McpStatusResponse, MessageContent,
    ModelInfo, RateLimitEvent, RemoteControlResponse, ResultMessage, RewindFilesResponse,
    SlashCommand, StdoutMessage, StreamEvent, StreamEventMessage, SystemInit, SystemMessage,
    SystemStatus, UsageResponse, UsageWindow, UserFrame, control_request_line,
    local_command_stdout,
};
use crate::timeline::{
    AssistantText, ImageAttachment, Notice, NoticeKind, PendingRequest, PendingRequestKind,
    Question, QuestionOption, SlashCommandInvocation, Thinking, TimelineItem, TodoItem, TodoStatus,
    ToolCall, ToolResult, ToolStatus, TurnSummary, UserMessage,
};

pub const ASK_USER_QUESTION_TOOL: &str = "AskUserQuestion";
pub const EXIT_PLAN_MODE_TOOL: &str = "ExitPlanMode";
const TODO_WRITE_TOOL: &str = "TodoWrite";
const TASK_CREATE_TOOL: &str = "TaskCreate";
const TASK_UPDATE_TOOL: &str = "TaskUpdate";
const TASK_LIST_TOOL: &str = "TaskList";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationStatus {
    /// The process is up but hasn't answered `initialize` yet.
    Starting,
    Idle,
    Working {
        since: SystemTime,
    },
    /// A permission prompt, question or plan waits for the user.
    AwaitingUser,
    Exited,
}

/// What an [`Conversation::apply`] call changed, so views only refresh what they show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversationChange {
    ItemInserted {
        index: usize,
        id: String,
    },
    ItemUpdated {
        index: usize,
        id: String,
    },
    /// Items after `index` moved up by one.
    ItemRemoved {
        index: usize,
        id: String,
    },
    PendingRequests,
    Status,
    /// Session id, cwd, model, effort, permission mode, tools or CLI version.
    Session,
    Commands,
    Models,
    Account,
    McpServers,
    /// The context window meter (`context_usage` or `live_context_tokens`).
    ContextUsage,
    RateLimits,
    Cost,
    Todos,
    Tasks,
    RemoteControl,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionInfo {
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    /// The resolved model id, like `claude-opus-5-5`.
    pub model: Option<String>,
    /// Only known when the CLI reports it (`system/init`, or the reply to `/effort`).
    pub effort: Option<String>,
    /// `default`, `acceptEdits`, `plan`, `auto`, `dontAsk` or `bypassPermissions`.
    pub permission_mode: Option<String>,
    pub claude_code_version: Option<String>,
    pub tools: Vec<String>,
    pub agents: Vec<String>,
    pub output_style: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RemoteControlState {
    pub available: bool,
    pub enabled: bool,
    /// The bridge's state, like `ready`, once enabled.
    pub bridge_state: Option<String>,
    pub session_url: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RateLimits {
    pub five_hour: Option<RateLimitWindow>,
    pub seven_day: Option<RateLimitWindow>,
    /// `allowed`, `allowed_warning`, `rejected`, ...
    pub status: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RateLimitWindow {
    /// Between 0 and 100.
    pub utilization_percent: f64,
    pub resets_at: Option<SystemTime>,
}

/// A subagent or background task the CLI runs.
#[derive(Debug, Clone, PartialEq)]
pub struct BackgroundTask {
    pub task_id: String,
    /// The `Agent`/`Task` tool call that started it.
    pub tool_use_id: Option<String>,
    pub description: String,
    pub subagent_type: Option<String>,
    pub task_type: Option<String>,
    /// `running`, `completed`, `failed`, `stopped`, `killed`, ...
    pub status: String,
    pub summary: Option<String>,
    pub is_backgrounded: bool,
    pub started_at: SystemTime,
    pub finished_at: Option<SystemTime>,
}

impl BackgroundTask {
    pub fn is_running(&self) -> bool {
        matches!(self.status.as_str(), "running" | "pending")
    }
}

/// How the user answered a pending request, for [`Conversation::resolve_request`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestDecision {
    Allowed,
    Denied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Text,
    Thinking,
    Tool,
}

#[derive(Debug)]
struct StreamedBlock {
    index: usize,
    kind: BlockKind,
    item_id: String,
    started_at: SystemTime,
    committed: bool,
}

#[derive(Debug, Default)]
struct StreamedMessage {
    blocks: Vec<StreamedBlock>,
    /// Blocks that arrived as `assistant` frames without having streamed.
    unstreamed_count: usize,
}

#[derive(Default)]
struct Changes(Vec<ConversationChange>);

impl Changes {
    fn push(&mut self, change: ConversationChange) {
        if let ConversationChange::ItemUpdated { id, .. } = &change
            && self.0.iter().any(|existing| {
                matches!(existing, ConversationChange::ItemInserted { id: inserted, .. } if inserted == id)
            })
        {
            return;
        }
        if !self.0.contains(&change) {
            self.0.push(change);
        }
    }
}

/// A Claude conversation as a pure state machine.
pub struct Conversation {
    items: Vec<TimelineItem>,
    index_by_id: HashMap<String, usize>,
    pending_requests: Vec<PendingRequest>,
    session: SessionInfo,
    commands: Vec<SlashCommand>,
    terminal_commands: HashSet<String>,
    models: Vec<ModelInfo>,
    account: Option<AccountInfo>,
    mcp_servers: Vec<McpServerStatus>,
    context_usage: Option<ContextUsage>,
    live_context_tokens: Option<u64>,
    context_window: Option<u64>,
    rate_limits: RateLimits,
    total_cost_usd: f64,
    todos: Vec<TodoItem>,
    tasks: Vec<BackgroundTask>,
    remote_control: RemoteControlState,

    initialized: bool,
    exited: Option<Option<String>>,
    compacting: bool,
    turn_running: bool,
    /// Our user messages that haven't finished their turn yet.
    outstanding: Vec<String>,
    working_since: Option<SystemTime>,
    hidden: HashSet<String>,
    /// The user message the CLI is processing (`command_lifecycle` `started`).
    current_command: Option<String>,
    /// Messages sent while Claude was busy that it hasn't taken in yet, oldest first. They join
    /// the timeline when it does, so they show up where Claude read them.
    queued: Vec<UserMessage>,
    /// Whether the CLI reports `command_lifecycle` frames, which tell when it takes in a message.
    reports_lifecycle: bool,
    interrupted_this_turn: bool,
    turn_cost_baseline: f64,

    /// Streamed message ids per subagent (`None` for the main thread).
    current_message: HashMap<Option<String>, String>,
    messages: HashMap<String, StreamedMessage>,
    next_local_id: u64,
}

impl Conversation {
    pub fn new(session_id: Option<String>) -> Self {
        Self {
            items: Vec::new(),
            index_by_id: HashMap::new(),
            pending_requests: Vec::new(),
            session: SessionInfo {
                session_id,
                ..SessionInfo::default()
            },
            commands: Vec::new(),
            terminal_commands: HashSet::new(),
            models: Vec::new(),
            account: None,
            mcp_servers: Vec::new(),
            context_usage: None,
            live_context_tokens: None,
            context_window: None,
            rate_limits: RateLimits::default(),
            total_cost_usd: 0.0,
            todos: Vec::new(),
            tasks: Vec::new(),
            remote_control: RemoteControlState::default(),
            initialized: false,
            exited: None,
            compacting: false,
            turn_running: false,
            outstanding: Vec::new(),
            working_since: None,
            hidden: HashSet::new(),
            current_command: None,
            queued: Vec::new(),
            reports_lifecycle: false,
            interrupted_this_turn: false,
            turn_cost_baseline: 0.0,
            current_message: HashMap::new(),
            messages: HashMap::new(),
            next_local_id: 0,
        }
    }

    // --- accessors ----------------------------------------------------------------------------

    pub fn items(&self) -> &[TimelineItem] {
        &self.items
    }

    pub fn item(&self, id: &str) -> Option<&TimelineItem> {
        self.index_by_id.get(id).map(|index| &self.items[*index])
    }

    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.index_by_id.get(id).copied()
    }

    pub fn pending_requests(&self) -> &[PendingRequest] {
        &self.pending_requests
    }

    /// Messages Claude hasn't taken in yet, oldest first; see [`Self::tracks_queue`].
    pub fn queued_messages(&self) -> &[UserMessage] {
        &self.queued
    }

    pub fn is_queued(&self, uuid: &str) -> bool {
        self.queued.iter().any(|message| message.id == uuid)
    }

    /// Whether the CLI says when it takes each message in, so [`Self::queued_messages`] is exact.
    /// Without it, messages sent while Claude is busy go straight into the timeline.
    pub fn tracks_queue(&self) -> bool {
        self.reports_lifecycle
    }

    pub fn status(&self) -> ConversationStatus {
        if self.exited.is_some() {
            ConversationStatus::Exited
        } else if !self.pending_requests.is_empty() {
            ConversationStatus::AwaitingUser
        } else if let Some(since) = self.working_since {
            ConversationStatus::Working { since }
        } else if !self.initialized {
            ConversationStatus::Starting
        } else {
            ConversationStatus::Idle
        }
    }

    /// Why the process exited, when it did with an error.
    pub fn exit_error(&self) -> Option<&str> {
        self.exited.as_ref().and_then(|error| error.as_deref())
    }

    pub fn is_compacting(&self) -> bool {
        self.compacting
    }

    pub fn session(&self) -> &SessionInfo {
        &self.session
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session.session_id.as_deref()
    }

    /// The slash commands a GUI can run (terminal-only ones are left out).
    pub fn slash_commands(&self) -> impl Iterator<Item = &SlashCommand> {
        self.commands
            .iter()
            .filter(|command| !self.terminal_commands.contains(&command.name))
    }

    pub fn models(&self) -> &[ModelInfo] {
        &self.models
    }

    pub fn account(&self) -> Option<&AccountInfo> {
        self.account.as_ref()
    }

    pub fn mcp_servers(&self) -> &[McpServerStatus] {
        &self.mcp_servers
    }

    /// The last `get_context_usage` (or `/context`) report.
    pub fn context_usage(&self) -> Option<&ContextUsage> {
        self.context_usage.as_ref()
    }

    /// The prompt size of the latest main-thread API call, which tracks the context fill between
    /// `get_context_usage` reports.
    pub fn live_context_tokens(&self) -> Option<u64> {
        self.live_context_tokens
    }

    /// The model's context window, as reported by the last `result`.
    pub fn context_window(&self) -> Option<u64> {
        self.context_window
    }

    pub fn rate_limits(&self) -> &RateLimits {
        &self.rate_limits
    }

    /// The cost of this process so far.
    pub fn total_cost_usd(&self) -> f64 {
        self.total_cost_usd
    }

    pub fn todos(&self) -> &[TodoItem] {
        &self.todos
    }

    pub fn tasks(&self) -> &[BackgroundTask] {
        &self.tasks
    }

    pub fn remote_control(&self) -> &RemoteControlState {
        &self.remote_control
    }

    // --- inputs from the UI -------------------------------------------------------------------

    /// Seeds the timeline with a saved transcript before live messages arrive.
    pub fn load_history(&mut self, items: Vec<TimelineItem>) -> Vec<ConversationChange> {
        let mut changes = Changes::default();
        for item in items {
            self.insert_item(item, &mut changes);
        }
        changes.0
    }

    /// Shows the message the user just sent, keyed by the `uuid` written to the CLI so its replies
    /// can be matched to it.
    pub fn push_local_user_message(
        &mut self,
        uuid: &str,
        text: &str,
        images: Vec<ImageAttachment>,
        now: SystemTime,
    ) -> Vec<ConversationChange> {
        let mut changes = Changes::default();
        let status = self.status();
        if !self.index_by_id.contains_key(uuid) && !self.is_queued(uuid) {
            let message = UserMessage {
                id: uuid.to_owned(),
                text: text.to_owned(),
                images,
                command: self.parse_command(text),
                timestamp: Some(now),
            };
            let is_busy = matches!(
                status,
                ConversationStatus::Working { .. } | ConversationStatus::AwaitingUser
            );
            if self.reports_lifecycle && is_busy {
                self.queued.push(message);
            } else {
                self.insert_item(TimelineItem::User(message), &mut changes);
            }
        }
        if !self
            .outstanding
            .iter()
            .any(|outstanding| outstanding == uuid)
        {
            self.outstanding.push(uuid.to_owned());
        }
        self.finish(status, now, &mut changes);
        changes.0
    }

    /// Keeps the user message `uuid` and the CLI's replies to it out of the timeline, for commands
    /// the UI sends on the user's behalf (like `/effort high`).
    pub fn hide_replies_to(&mut self, uuid: &str) -> Vec<ConversationChange> {
        let mut changes = Changes::default();
        self.hidden.insert(uuid.to_owned());
        self.outstanding.retain(|outstanding| outstanding != uuid);
        self.remove_item(uuid, &mut changes);
        changes.0
    }

    /// Takes back the queued message `uuid` the CLI dropped before Claude got to it.
    pub fn withdraw_user_message(
        &mut self,
        uuid: &str,
        now: SystemTime,
    ) -> Vec<ConversationChange> {
        let mut changes = Changes::default();
        let status = self.status();
        self.hidden.insert(uuid.to_owned());
        self.outstanding.retain(|outstanding| outstanding != uuid);
        self.queued.retain(|message| message.id != uuid);
        self.remove_item(uuid, &mut changes);
        self.finish(status, now, &mut changes);
        changes.0
    }

    /// Drops a pending request the user answered, updating its tool call.
    pub fn resolve_request(
        &mut self,
        request_id: &str,
        decision: RequestDecision,
    ) -> Vec<ConversationChange> {
        let mut changes = Changes::default();
        let Some(position) = self
            .pending_requests
            .iter()
            .position(|request| request.request_id == request_id)
        else {
            return changes.0;
        };
        let request = self.pending_requests.remove(position);
        changes.push(ConversationChange::PendingRequests);
        changes.push(ConversationChange::Status);
        if let Some(tool_use_id) = &request.tool_use_id {
            let status = match decision {
                RequestDecision::Allowed => ToolStatus::Running,
                RequestDecision::Denied => ToolStatus::Denied,
            };
            self.with_tool(tool_use_id, &mut changes, |tool| {
                if tool.status == ToolStatus::AwaitingApproval {
                    tool.status = status;
                }
            });
        }
        changes.0
    }

    /// The process ended; anything still in flight is interrupted.
    pub fn mark_exited(
        &mut self,
        error: Option<String>,
        now: SystemTime,
    ) -> Vec<ConversationChange> {
        let mut changes = Changes::default();
        let status = self.status();
        self.exited = Some(error);
        self.finish_turn_items(true, now, &mut changes);
        if !self.pending_requests.is_empty() {
            self.pending_requests.clear();
            changes.push(ConversationChange::PendingRequests);
        }
        self.turn_running = false;
        self.outstanding.clear();
        // Claude never read them, but their text shouldn't vanish with the process.
        for message in std::mem::take(&mut self.queued) {
            self.insert_item(TimelineItem::User(message), &mut changes);
        }
        self.compacting = false;
        self.finish(status, now, &mut changes);
        changes.0
    }

    pub fn apply_initialize_response(
        &mut self,
        response: &InitializeResponse,
    ) -> Vec<ConversationChange> {
        let mut changes = Changes::default();
        let status = self.status();
        self.initialized = true;
        if !response.commands.is_empty() {
            self.commands = response.commands.clone();
            changes.push(ConversationChange::Commands);
        }
        if !response.models.is_empty() {
            self.models = response.models.clone();
            changes.push(ConversationChange::Models);
        }
        if response.account.is_some() {
            self.account = response.account.clone();
            changes.push(ConversationChange::Account);
        }
        if let Some(mode) = &response.current_permission_mode {
            self.set_permission_mode(mode, &mut changes);
        }
        if self.remote_control.available != response.remote_control_available {
            self.remote_control.available = response.remote_control_available;
            changes.push(ConversationChange::RemoteControl);
        }
        if let Some(style) = &response.output_style
            && self.session.output_style.as_ref() != Some(style)
        {
            self.session.output_style = Some(style.clone());
            changes.push(ConversationChange::Session);
        }
        self.finish(
            status,
            self.working_since.unwrap_or(UNIX_EPOCH),
            &mut changes,
        );
        changes.0
    }

    pub fn apply_mcp_status(&mut self, response: &McpStatusResponse) -> Vec<ConversationChange> {
        if self.mcp_servers == response.mcp_servers {
            return Vec::new();
        }
        self.mcp_servers = response.mcp_servers.clone();
        vec![ConversationChange::McpServers]
    }

    /// Records the latest request's size and the model's context window, for backends that report
    /// them outside Claude's stream events.
    pub fn apply_context_tokens(
        &mut self,
        tokens: u64,
        window: Option<u64>,
    ) -> Vec<ConversationChange> {
        self.live_context_tokens = Some(tokens);
        if window.is_some() {
            self.context_window = window;
        }
        vec![ConversationChange::ContextUsage]
    }

    pub fn apply_context_usage(&mut self, usage: ContextUsage) -> Vec<ConversationChange> {
        if let Some(window) = usage.window_tokens() {
            self.context_window = Some(window);
        }
        self.context_usage = Some(usage);
        vec![ConversationChange::ContextUsage]
    }

    pub fn apply_usage(&mut self, usage: &UsageResponse) -> Vec<ConversationChange> {
        let mut changes = Changes::default();
        if let Some(limits) = &usage.rate_limits {
            let window = |window: &Option<UsageWindow>| {
                window.as_ref().and_then(|window| {
                    Some(RateLimitWindow {
                        utilization_percent: window.utilization?,
                        resets_at: window.resets_at.as_deref().and_then(parse_rfc3339),
                    })
                })
            };
            let five_hour = window(&limits.five_hour);
            let seven_day = window(&limits.seven_day);
            if five_hour.is_some() || seven_day.is_some() {
                self.rate_limits.five_hour = five_hour.or(self.rate_limits.five_hour.take());
                self.rate_limits.seven_day = seven_day.or(self.rate_limits.seven_day.take());
                changes.push(ConversationChange::RateLimits);
            }
        }
        if let Some(cost) = usage
            .session
            .as_ref()
            .and_then(|session| session.total_cost_usd)
            && cost > self.total_cost_usd
        {
            self.total_cost_usd = cost;
            changes.push(ConversationChange::Cost);
        }
        changes.0
    }

    pub fn apply_remote_control(
        &mut self,
        enabled: bool,
        response: Option<&RemoteControlResponse>,
    ) -> Vec<ConversationChange> {
        self.remote_control.enabled = enabled;
        if enabled {
            if let Some(url) = response.and_then(|response| response.session_url.clone()) {
                self.remote_control.session_url = Some(url);
            }
        } else {
            self.remote_control.session_url = None;
            self.remote_control.bridge_state = None;
        }
        vec![ConversationChange::RemoteControl]
    }

    /// Applies what a reply to one of our control requests tells about the session.
    pub fn apply_control_reply(&mut self, reply: &ControlReply) -> Vec<ConversationChange> {
        let Ok(payload) = &reply.outcome else {
            return Vec::new();
        };
        match payload {
            ReplyPayload::Initialize(response) => self.apply_initialize_response(response),
            ReplyPayload::McpStatus(response) => self.apply_mcp_status(response),
            ReplyPayload::ContextUsage(usage) => self.apply_context_usage((**usage).clone()),
            ReplyPayload::Usage(usage) => self.apply_usage(usage),
            ReplyPayload::RemoteControl(response) => {
                let enabled = match &reply.request {
                    ControlRequestBody::RemoteControl { enabled, .. } => *enabled,
                    _ => true,
                };
                self.apply_remote_control(enabled, Some(response))
            }
            ReplyPayload::PermissionMode(mode) => {
                let mut changes = Changes::default();
                self.set_permission_mode(mode, &mut changes);
                changes.0
            }
            ReplyPayload::McpAuthenticate(_)
            | ReplyPayload::RewindFiles(_)
            | ReplyPayload::FileSuggestions(_)
            | ReplyPayload::Interrupt(_)
            | ReplyPayload::CancelAsyncMessage(_)
            | ReplyPayload::Ack
            | ReplyPayload::Raw(_) => Vec::new(),
        }
    }

    // --- stdout -------------------------------------------------------------------------------

    pub fn apply(&mut self, message: StdoutMessage, now: SystemTime) -> Vec<ConversationChange> {
        let mut changes = Changes::default();
        let status = self.status();
        match message {
            StdoutMessage::System(system) => self.apply_system(system, now, &mut changes),
            StdoutMessage::StreamEvent(event) => self.apply_stream_event(event, now, &mut changes),
            StdoutMessage::Assistant(assistant) => {
                self.apply_assistant(*assistant, now, &mut changes)
            }
            StdoutMessage::User(user) => self.apply_user(*user, now, &mut changes),
            StdoutMessage::Result(result) => self.apply_result(*result, now, &mut changes),
            StdoutMessage::RateLimit(event) => self.apply_rate_limit(event, &mut changes),
            StdoutMessage::CommandLifecycle(lifecycle) => {
                self.apply_lifecycle(lifecycle, &mut changes)
            }
            StdoutMessage::ControlRequest(request) => {
                if let IncomingRequest::CanUseTool(can_use_tool) = request.request {
                    self.add_permission_request(request.request_id, *can_use_tool, &mut changes);
                }
            }
            StdoutMessage::ControlCancelRequest { request_id } => {
                if let Some(position) = self
                    .pending_requests
                    .iter()
                    .position(|request| request.request_id == request_id)
                {
                    let request = self.pending_requests.remove(position);
                    changes.push(ConversationChange::PendingRequests);
                    if let Some(tool_use_id) = &request.tool_use_id {
                        self.with_tool(tool_use_id, &mut changes, |tool| {
                            if tool.status == ToolStatus::AwaitingApproval {
                                tool.status = ToolStatus::Running;
                            }
                        });
                    }
                }
            }
            StdoutMessage::ControlResponse(_)
            | StdoutMessage::KeepAlive
            | StdoutMessage::Unknown(_) => {}
        }
        self.finish(status, now, &mut changes);
        changes.0
    }

    fn apply_system(&mut self, system: SystemMessage, now: SystemTime, changes: &mut Changes) {
        match system {
            SystemMessage::Init(init) => self.apply_init(*init, changes),
            SystemMessage::Status(status) => self.apply_status(status, changes),
            SystemMessage::CompactBoundary(boundary) => {
                self.apply_compact_boundary(boundary, changes)
            }
            SystemMessage::CommandsChanged { commands } => {
                if self.commands != commands {
                    self.commands = commands;
                    changes.push(ConversationChange::Commands);
                }
            }
            SystemMessage::TaskStarted(started) => {
                let task = BackgroundTask {
                    task_id: started.task_id.clone(),
                    tool_use_id: started.tool_use_id,
                    description: started.description,
                    subagent_type: started.subagent_type,
                    task_type: started.task_type,
                    status: "running".to_owned(),
                    summary: None,
                    is_backgrounded: started.is_backgrounded,
                    started_at: now,
                    finished_at: None,
                };
                match self
                    .tasks
                    .iter_mut()
                    .find(|task| task.task_id == started.task_id)
                {
                    Some(existing) => *existing = task,
                    None => self.tasks.push(task),
                }
                changes.push(ConversationChange::Tasks);
            }
            SystemMessage::TaskUpdated(updated) => {
                if let Some(task) = self.task_mut(&updated.task_id) {
                    if let Some(status) = updated.patch.status {
                        task.status = status;
                    }
                    if let Some(description) = updated.patch.description {
                        task.description = description;
                    }
                    if let Some(error) = updated.patch.error {
                        task.summary = Some(error);
                    }
                    if let Some(end_time) = updated.patch.end_time {
                        task.finished_at =
                            Some(UNIX_EPOCH + Duration::from_millis(end_time.max(0) as u64));
                    }
                    changes.push(ConversationChange::Tasks);
                }
            }
            SystemMessage::TaskProgress(progress) => {
                if let Some(task) = self.task_mut(&progress.task_id)
                    && progress.summary.is_some()
                {
                    task.summary = progress.summary;
                    changes.push(ConversationChange::Tasks);
                }
            }
            SystemMessage::TaskNotification(notification) => {
                if let Some(task) = self.task_mut(&notification.task_id) {
                    task.status = notification.status;
                    task.summary = notification.summary.or(task.summary.take());
                    task.finished_at.get_or_insert(now);
                    changes.push(ConversationChange::Tasks);
                }
            }
            SystemMessage::BackgroundTasksChanged { tasks } => {
                let live: HashSet<&str> = tasks.iter().map(|task| task.task_id.as_str()).collect();
                for task in &mut self.tasks {
                    if task.is_backgrounded
                        && task.is_running()
                        && !live.contains(task.task_id.as_str())
                    {
                        task.status = "completed".to_owned();
                        task.finished_at.get_or_insert(now);
                    }
                }
                for info in tasks.iter().filter(|info| !info.ambient) {
                    match self
                        .tasks
                        .iter_mut()
                        .find(|task| task.task_id == info.task_id)
                    {
                        Some(task) => task.is_backgrounded = true,
                        None => self.tasks.push(BackgroundTask {
                            task_id: info.task_id.clone(),
                            tool_use_id: None,
                            description: info.description.clone(),
                            subagent_type: info.subagent_type.clone(),
                            task_type: Some(info.task_type.clone()),
                            status: "running".to_owned(),
                            summary: None,
                            is_backgrounded: true,
                            started_at: now,
                            finished_at: None,
                        }),
                    }
                }
                changes.push(ConversationChange::Tasks);
            }
            SystemMessage::BridgeState { state } => {
                self.remote_control.bridge_state = Some(state);
                changes.push(ConversationChange::RemoteControl);
            }
            SystemMessage::PermissionDenied(denied) => {
                self.with_tool(&denied.tool_use_id, changes, |tool| {
                    tool.status = ToolStatus::Denied;
                });
            }
            SystemMessage::LocalCommandOutput { content } => {
                if !self.attach_command_output(&content, changes) && !content.trim().is_empty() {
                    self.push_notice(NoticeKind::Info(content), changes);
                }
            }
            SystemMessage::HookStarted(_)
            | SystemMessage::HookResponse(_)
            | SystemMessage::ThinkingTokens(_)
            | SystemMessage::ControlRequestProgress { .. }
            | SystemMessage::SessionStateChanged { .. }
            | SystemMessage::ApiRetry(_)
            | SystemMessage::Informational { .. }
            | SystemMessage::Other { .. } => {}
        }
    }

    fn apply_init(&mut self, init: SystemInit, changes: &mut Changes) {
        self.initialized = true;
        let mut session = self.session.clone();
        if init.session_id.is_some() {
            session.session_id = init.session_id;
        }
        if init.cwd.is_some() {
            session.cwd = init.cwd;
        }
        if init.model.is_some() {
            session.model = init.model;
        }
        if init.effort.is_some() {
            session.effort = init.effort;
        }
        if init.permission_mode.is_some() {
            session.permission_mode = init.permission_mode;
        }
        if init.claude_code_version.is_some() {
            session.claude_code_version = init.claude_code_version;
        }
        if init.output_style.is_some() {
            session.output_style = init.output_style;
        }
        session.tools = init.tools;
        session.agents = init.agents;
        if session != self.session {
            self.session = session;
            changes.push(ConversationChange::Session);
        }

        let terminal_commands: HashSet<String> = init.terminal_slash_commands.into_iter().collect();
        let mut by_name: HashMap<String, SlashCommand> = self
            .commands
            .drain(..)
            .map(|command| (command.name.clone(), command))
            .collect();
        let previous_names: Vec<String> = by_name.keys().cloned().collect();
        let mut commands: Vec<SlashCommand> = init
            .slash_commands
            .into_iter()
            .map(|name| {
                by_name.remove(&name).unwrap_or_else(|| SlashCommand {
                    name,
                    ..SlashCommand::default()
                })
            })
            .collect();
        // Full command objects the init list doesn't name (aliases, builtins) stay available.
        commands.extend(by_name.into_values().filter(|command| command.builtin));
        let mut new_names: Vec<String> = commands
            .iter()
            .map(|command| command.name.clone())
            .collect();
        let mut old_names = previous_names;
        new_names.sort();
        old_names.sort();
        if new_names != old_names || terminal_commands != self.terminal_commands {
            changes.push(ConversationChange::Commands);
        }
        self.commands = commands;
        self.terminal_commands = terminal_commands;

        if !init.mcp_servers.is_empty() {
            let mut servers = Vec::with_capacity(init.mcp_servers.len());
            for server in init.mcp_servers {
                match self
                    .mcp_servers
                    .iter()
                    .find(|existing| existing.name == server.name)
                {
                    Some(existing) => {
                        let mut merged = existing.clone();
                        merged.status = server.status;
                        if server.source.is_some() {
                            merged.source = server.source;
                        }
                        servers.push(merged);
                    }
                    None => servers.push(server),
                }
            }
            if servers != self.mcp_servers {
                self.mcp_servers = servers;
                changes.push(ConversationChange::McpServers);
            }
        }
    }

    fn apply_status(&mut self, status: SystemStatus, changes: &mut Changes) {
        if let Some(mode) = &status.permission_mode {
            self.set_permission_mode(mode, changes);
        }
        match status.status.as_deref() {
            Some("compacting") => {
                self.compacting = true;
                changes.push(ConversationChange::Status);
            }
            Some("requesting") => {
                if self.compacting {
                    self.compacting = false;
                    changes.push(ConversationChange::Status);
                }
                self.turn_running = true;
            }
            _ => {
                if self.compacting {
                    self.compacting = false;
                    changes.push(ConversationChange::Status);
                }
            }
        }
        if status.compact_result.as_deref() == Some("failed") {
            let error = status
                .compact_error
                .unwrap_or_else(|| "Compaction failed".to_owned());
            self.push_notice(NoticeKind::Error(error), changes);
        }
    }

    fn apply_compact_boundary(&mut self, boundary: CompactBoundary, changes: &mut Changes) {
        let kind = NoticeKind::Compacted {
            pre_tokens: boundary.compact_metadata.pre_tokens,
            post_tokens: boundary.compact_metadata.post_tokens,
        };
        match boundary.uuid {
            Some(id) if !self.index_by_id.contains_key(&id) => {
                self.insert_item(TimelineItem::Notice(Notice { id, kind }), changes)
            }
            _ => self.push_notice(kind, changes),
        }
        if let Some(post_tokens) = boundary.compact_metadata.post_tokens {
            self.live_context_tokens = Some(post_tokens);
            changes.push(ConversationChange::ContextUsage);
        }
    }

    fn apply_stream_event(
        &mut self,
        message: StreamEventMessage,
        now: SystemTime,
        changes: &mut Changes,
    ) {
        let parent = message.parent_tool_use_id;
        match message.event {
            StreamEvent::MessageStart { message: start } => {
                if parent.is_none()
                    && let Some(usage) = &start.usage
                {
                    self.live_context_tokens = Some(usage.context_tokens());
                    changes.push(ConversationChange::ContextUsage);
                }
                self.messages.entry(start.id.clone()).or_default();
                self.current_message.insert(parent, start.id);
            }
            StreamEvent::ContentBlockStart {
                index,
                content_block,
            } => {
                let Some(message_id) = self.current_message.get(&parent).cloned() else {
                    return;
                };
                let block_id = format!("{message_id}:{index}");
                let (kind, item_id) = match content_block {
                    ContentBlock::Text { text } => {
                        self.insert_item(
                            TimelineItem::Assistant(AssistantText {
                                id: block_id.clone(),
                                markdown: text,
                                is_streaming: true,
                                parent_tool_use_id: parent.clone(),
                                timestamp: Some(now),
                            }),
                            changes,
                        );
                        (BlockKind::Text, block_id)
                    }
                    ContentBlock::Thinking { thinking } => {
                        self.insert_item(
                            TimelineItem::Thinking(Thinking {
                                id: block_id.clone(),
                                text: thinking,
                                is_streaming: true,
                                duration_ms: None,
                                parent_tool_use_id: parent.clone(),
                            }),
                            changes,
                        );
                        (BlockKind::Thinking, block_id)
                    }
                    ContentBlock::ToolUse { id, name, .. } => {
                        if !self.index_by_id.contains_key(&id) {
                            self.insert_item(
                                TimelineItem::Tool(ToolCall {
                                    id: id.clone(),
                                    name,
                                    input: Value::Null,
                                    partial_input: String::new(),
                                    status: ToolStatus::Streaming,
                                    result: None,
                                    parent_tool_use_id: parent.clone(),
                                    started_at: Some(now),
                                    finished_at: None,
                                }),
                                changes,
                            );
                        }
                        (BlockKind::Tool, id)
                    }
                    ContentBlock::RedactedThinking
                    | ContentBlock::ToolResult(_)
                    | ContentBlock::Image(_)
                    | ContentBlock::Other(_) => return,
                };
                self.messages
                    .entry(message_id)
                    .or_default()
                    .blocks
                    .push(StreamedBlock {
                        index,
                        kind,
                        item_id,
                        started_at: now,
                        committed: false,
                    });
            }
            StreamEvent::ContentBlockDelta { index, delta } => {
                let Some(block) = self.streamed_block(&parent, index) else {
                    return;
                };
                let item_id = block.item_id.clone();
                match delta {
                    ContentDelta::Text(text) if !text.is_empty() => {
                        self.with_item(&item_id, changes, |item| {
                            if let TimelineItem::Assistant(assistant) = item {
                                assistant.markdown.push_str(&text);
                            }
                        });
                    }
                    ContentDelta::Thinking(text) if !text.is_empty() => {
                        self.with_item(&item_id, changes, |item| {
                            if let TimelineItem::Thinking(thinking) = item {
                                thinking.text.push_str(&text);
                            }
                        });
                    }
                    ContentDelta::InputJson(json) if !json.is_empty() => {
                        self.with_tool(&item_id, changes, |tool| {
                            if tool.status == ToolStatus::Streaming {
                                tool.partial_input.push_str(&json);
                            }
                        });
                    }
                    ContentDelta::Text(_)
                    | ContentDelta::Thinking(_)
                    | ContentDelta::InputJson(_)
                    | ContentDelta::Signature
                    | ContentDelta::Unknown(_) => {}
                }
            }
            StreamEvent::ContentBlockStop { index } => {
                let Some(block) = self.streamed_block(&parent, index) else {
                    return;
                };
                let item_id = block.item_id.clone();
                let started_at = block.started_at;
                self.with_item(&item_id, changes, |item| match item {
                    TimelineItem::Assistant(assistant) => assistant.is_streaming = false,
                    TimelineItem::Thinking(thinking) => {
                        thinking.is_streaming = false;
                        if thinking.duration_ms.is_none() {
                            thinking.duration_ms = Some(elapsed_ms(started_at, now));
                        }
                    }
                    TimelineItem::Tool(tool) => {
                        if tool.status == ToolStatus::Streaming {
                            tool.input =
                                serde_json::from_str(&tool.partial_input).unwrap_or(Value::Null);
                            tool.partial_input.clear();
                            tool.status = ToolStatus::Running;
                        }
                    }
                    TimelineItem::User(_) | TimelineItem::Notice(_) | TimelineItem::TurnEnd(_) => {}
                });
            }
            StreamEvent::MessageDelta { .. } => {}
            StreamEvent::MessageStop => {
                self.current_message.remove(&parent);
            }
            StreamEvent::Unknown(_) => {}
        }
    }

    fn streamed_block(&self, parent: &Option<String>, index: usize) -> Option<&StreamedBlock> {
        let message_id = self.current_message.get(parent)?;
        self.messages
            .get(message_id)?
            .blocks
            .iter()
            .rev()
            .find(|block| block.index == index)
    }

    fn apply_assistant(
        &mut self,
        assistant: AssistantMessage,
        now: SystemTime,
        changes: &mut Changes,
    ) {
        if assistant.is_synthetic() || assistant.local_command_source.is_some() {
            self.apply_local_command_reply(assistant, changes);
            return;
        }
        if let Some(error) = &assistant.error {
            let text = MessageContent::Blocks(assistant.message.content).text();
            let message = if text.trim().is_empty() {
                error.clone()
            } else {
                text
            };
            self.push_notice(NoticeKind::Error(message), changes);
            return;
        }

        let message_id = assistant.message.id.clone();
        let parent = assistant.parent_tool_use_id.clone();
        for block in assistant.message.content {
            match block {
                ContentBlock::Text { text } => {
                    match self.claim_streamed_block(&message_id, BlockKind::Text) {
                        Some(item_id) => {
                            self.with_item(&item_id, changes, |item| {
                                if let TimelineItem::Assistant(existing) = item {
                                    if !text.is_empty() {
                                        existing.markdown = text;
                                    }
                                    existing.is_streaming = false;
                                }
                            });
                        }
                        None => {
                            let id = self.unstreamed_block_id(&message_id);
                            self.insert_item(
                                TimelineItem::Assistant(AssistantText {
                                    id,
                                    markdown: text,
                                    is_streaming: false,
                                    parent_tool_use_id: parent.clone(),
                                    timestamp: Some(now),
                                }),
                                changes,
                            );
                        }
                    }
                }
                ContentBlock::Thinking { thinking } => {
                    let duration = assistant.thinking_duration_ms;
                    match self.claim_streamed_block(&message_id, BlockKind::Thinking) {
                        Some(item_id) => {
                            self.with_item(&item_id, changes, |item| {
                                if let TimelineItem::Thinking(existing) = item {
                                    if !thinking.is_empty() {
                                        existing.text = thinking;
                                    }
                                    existing.is_streaming = false;
                                    if duration.is_some() {
                                        existing.duration_ms = duration;
                                    }
                                }
                            });
                        }
                        None => {
                            let id = self.unstreamed_block_id(&message_id);
                            self.insert_item(
                                TimelineItem::Thinking(Thinking {
                                    id,
                                    text: thinking,
                                    is_streaming: false,
                                    duration_ms: duration,
                                    parent_tool_use_id: parent.clone(),
                                }),
                                changes,
                            );
                        }
                    }
                }
                ContentBlock::ToolUse { id, name, input } => {
                    self.claim_streamed_tool(&message_id, &id);
                    if parent.is_none() {
                        self.apply_todo_input(&name, &input, changes);
                    }
                    let updated = self.with_tool(&id, changes, |tool| {
                        tool.input = input.clone();
                        tool.partial_input.clear();
                        if tool.status == ToolStatus::Streaming {
                            tool.status = ToolStatus::Running;
                        }
                    });
                    if !updated {
                        self.insert_item(
                            TimelineItem::Tool(ToolCall {
                                id,
                                name,
                                input,
                                partial_input: String::new(),
                                status: ToolStatus::Running,
                                result: None,
                                parent_tool_use_id: parent.clone(),
                                started_at: Some(now),
                                finished_at: None,
                            }),
                            changes,
                        );
                    }
                }
                ContentBlock::RedactedThinking
                | ContentBlock::ToolResult(_)
                | ContentBlock::Image(_)
                | ContentBlock::Other(_) => {}
            }
        }
    }

    fn claim_streamed_block(&mut self, message_id: &str, kind: BlockKind) -> Option<String> {
        let block = self
            .messages
            .get_mut(message_id)?
            .blocks
            .iter_mut()
            .filter(|block| block.kind == kind && !block.committed)
            .min_by_key(|block| block.index)?;
        block.committed = true;
        Some(block.item_id.clone())
    }

    fn claim_streamed_tool(&mut self, message_id: &str, tool_use_id: &str) {
        if let Some(block) = self.messages.get_mut(message_id).and_then(|message| {
            message
                .blocks
                .iter_mut()
                .find(|block| block.item_id == tool_use_id)
        }) {
            block.committed = true;
        }
    }

    fn unstreamed_block_id(&mut self, message_id: &str) -> String {
        if message_id.is_empty() {
            return self.next_local_id("assistant");
        }
        let message = self.messages.entry(message_id.to_owned()).or_default();
        let id = format!("{message_id}:u{}", message.unstreamed_count);
        message.unstreamed_count += 1;
        id
    }

    /// A reply the CLI produced itself, like the output of `/context` or `/effort`.
    fn apply_local_command_reply(&mut self, assistant: AssistantMessage, changes: &mut Changes) {
        let text = MessageContent::Blocks(assistant.message.content).text();
        if let Some(run) = &assistant.local_command_run
            && run.command == "effort"
            && let Some(level) = text
                .strip_prefix("Set effort level to ")
                .and_then(|rest| rest.split([' ', ':', '(']).next())
            && !level.is_empty()
            && self.session.effort.as_deref() != Some(level)
        {
            self.session.effort = Some(level.to_owned());
            changes.push(ConversationChange::Session);
        }
        if let Some(usage) = assistant.context_usage {
            if let Some(window) = usage.window_tokens() {
                self.context_window = Some(window);
            }
            self.context_usage = Some(usage);
            changes.push(ConversationChange::ContextUsage);
        }
        if let Some(report) = &assistant.usage_report {
            for change in self.apply_usage(report) {
                changes.push(change);
            }
        }
        if self.is_hidden_command() {
            return;
        }
        if !self.attach_command_output(&text, changes) && !text.trim().is_empty() {
            self.push_notice(NoticeKind::Info(text), changes);
        }
    }

    fn is_hidden_command(&self) -> bool {
        self.current_command
            .as_ref()
            .is_some_and(|uuid| self.hidden.contains(uuid))
    }

    /// Puts a local command's output on the user's command chip. Returns false when there is no
    /// command item waiting for output.
    fn attach_command_output(&mut self, output: &str, changes: &mut Changes) -> bool {
        // Without `command_lifecycle` frames, the CLI runs queued messages in order.
        let target = self
            .current_command
            .iter()
            .chain(self.outstanding.iter())
            .find(|uuid| self.is_command_awaiting_output(uuid))
            .cloned();
        let Some(target) = target else {
            return false;
        };
        let output = output.to_owned();
        self.with_item(&target, changes, |item| {
            if let TimelineItem::User(user) = item {
                // A `/name` we didn't know as a command, which the CLI answered as one.
                if user.command.is_none() {
                    user.command = parse_command_syntax(&user.text);
                }
                if let Some(command) = &mut user.command {
                    command.output = Some(output);
                }
            }
        })
    }

    fn is_command_awaiting_output(&self, uuid: &str) -> bool {
        match self.item(uuid) {
            Some(TimelineItem::User(user)) => match &user.command {
                Some(command) => command.output.is_none(),
                None => parse_command_syntax(&user.text).is_some(),
            },
            _ => false,
        }
    }

    fn apply_user(&mut self, user: UserFrame, now: SystemTime, changes: &mut Changes) {
        if user
            .uuid
            .as_ref()
            .is_some_and(|uuid| self.hidden.contains(uuid))
        {
            return;
        }

        let blocks = user.message.content.blocks();
        let tool_results: Vec<_> = blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolResult(result) => Some(result),
                _ => None,
            })
            .collect();
        if !tool_results.is_empty() {
            let single = tool_results.len() == 1;
            for result in tool_results {
                let denied_by_rule = user
                    .tool_result_meta
                    .iter()
                    .any(|meta| meta.id == result.tool_use_id && meta.non_execution_kind.is_some());
                let text = result.text();
                let is_error = result.is_error;
                let structured = if single {
                    user.tool_use_result.clone()
                } else {
                    None
                };
                let mut finished_name = None;
                self.with_tool(&result.tool_use_id, changes, |tool| {
                    tool.status = if denied_by_rule || tool.status == ToolStatus::Denied {
                        ToolStatus::Denied
                    } else if is_error && text.starts_with(INTERRUPT_MARKER) {
                        ToolStatus::Interrupted
                    } else if is_error {
                        ToolStatus::Failed
                    } else {
                        ToolStatus::Done
                    };
                    if tool.input.is_null() && !tool.partial_input.is_empty() {
                        tool.input =
                            serde_json::from_str(&tool.partial_input).unwrap_or(Value::Null);
                        tool.partial_input.clear();
                    }
                    tool.result = Some(ToolResult {
                        text: text.clone(),
                        is_error,
                        structured: structured.clone(),
                    });
                    tool.finished_at = Some(now);
                    if tool.parent_tool_use_id.is_none() && !is_error {
                        finished_name = Some((tool.name.clone(), tool.input.clone()));
                    }
                });
                if let Some((name, input)) = finished_name
                    && let Some(structured) = &structured
                {
                    self.apply_todo_result(&name, &input, structured, changes);
                }
            }
            return;
        }

        // A subagent's prompt; the Agent tool call already shows it.
        if user.parent_tool_use_id.is_some() {
            return;
        }

        let text = user.message.content.text();
        if text.trim_start().starts_with(INTERRUPT_MARKER) {
            self.interrupted_this_turn = true;
            self.finish_turn_items(true, now, changes);
            self.push_notice(NoticeKind::Interrupted, changes);
            return;
        }
        if let Some(output) = local_command_stdout(&text) {
            if self.is_hidden_command() {
                return;
            }
            if output.starts_with("Set model to ") {
                return;
            }
            if !self.attach_command_output(output, changes) && !output.is_empty() {
                self.push_notice(NoticeKind::Info(output.to_owned()), changes);
            }
            return;
        }
        if user.is_synthetic {
            self.push_notice(NoticeKind::CompactSummary(text), changes);
            return;
        }
        if let Some(uuid) = &user.uuid
            && self.is_queued(uuid)
        {
            self.take_in_queued(uuid, changes);
            return;
        }
        let id = match user.uuid {
            Some(uuid) if self.index_by_id.contains_key(&uuid) => return,
            Some(uuid) => uuid,
            None if user.is_replay => return,
            None => self.next_local_id("user"),
        };
        let images = blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Image(image) => Some(ImageAttachment {
                    media_type: image.media_type.clone().unwrap_or_default(),
                    path: None,
                    data: image.data.clone(),
                }),
                _ => None,
            })
            .collect();
        let text = match &user.message.content {
            MessageContent::Text(text) => text.clone(),
            MessageContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        };
        let command = self.parse_command(&text);
        self.insert_item(
            TimelineItem::User(UserMessage {
                id,
                text,
                images,
                command,
                timestamp: Some(now),
            }),
            changes,
        );
    }

    fn apply_result(&mut self, result: ResultMessage, now: SystemTime, changes: &mut Changes) {
        self.initialized = true;
        let uuids: Vec<&String> = result
            .user_message_uuids
            .iter()
            .chain(result.user_message_uuid.iter())
            .collect();
        let hidden = (!uuids.is_empty() && uuids.iter().all(|uuid| self.hidden.contains(*uuid)))
            || (uuids.is_empty() && self.is_hidden_command());

        if uuids.is_empty() {
            if !self.outstanding.is_empty() {
                self.outstanding.remove(0);
            }
        } else {
            self.outstanding
                .retain(|outstanding| !uuids.contains(&outstanding));
        }
        self.turn_running = false;

        let interrupted = self.interrupted_this_turn
            || result
                .terminal_reason
                .as_deref()
                .is_some_and(|reason| reason.starts_with("aborted"));
        self.finish_turn_items(interrupted, now, changes);

        for denial in &result.permission_denials {
            self.with_tool(&denial.tool_use_id, changes, |tool| {
                if tool.status != ToolStatus::Denied {
                    tool.status = ToolStatus::Denied;
                }
            });
        }

        let window = result
            .model_usage
            .iter()
            .filter(|(model, _)| {
                self.session
                    .model
                    .as_deref()
                    .is_none_or(|current| current == model.as_str())
            })
            .filter_map(|(_, usage)| usage.context_window)
            .max()
            .or_else(|| {
                result
                    .model_usage
                    .values()
                    .filter_map(|usage| usage.context_window)
                    .max()
            });
        if window.is_some() && window != self.context_window {
            self.context_window = window;
            changes.push(ConversationChange::ContextUsage);
        }

        let previous_cost = self.turn_cost_baseline;
        if let Some(total) = result.total_cost_usd {
            if total != self.total_cost_usd {
                self.total_cost_usd = total;
                changes.push(ConversationChange::Cost);
            }
            self.turn_cost_baseline = total;
        }

        if !hidden {
            if result.is_error && !interrupted {
                let message = result
                    .errors
                    .iter()
                    .filter_map(|error| match error {
                        Value::String(text) => Some(text.clone()),
                        Value::Null => None,
                        other => Some(other.to_string()),
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let message = if !message.is_empty() {
                    message
                } else {
                    result
                        .result
                        .clone()
                        .filter(|text| !text.is_empty())
                        .unwrap_or_else(|| result.subtype.clone())
                };
                self.push_notice(NoticeKind::Error(message), changes);
            }
            let id = match result.uuid {
                Some(uuid) if !self.index_by_id.contains_key(&uuid) => uuid,
                _ => self.next_local_id("turn-end"),
            };
            let cost = result
                .total_cost_usd
                .map(|total| (total - previous_cost).max(0.0));
            self.insert_item(
                TimelineItem::TurnEnd(TurnSummary {
                    id,
                    duration_ms: result.duration_ms,
                    cost_usd: cost,
                    is_error: result.is_error,
                    subtype: result.subtype,
                }),
                changes,
            );
        }

        self.interrupted_this_turn = false;
        self.messages.clear();
        self.current_message.clear();
    }

    fn apply_rate_limit(&mut self, event: RateLimitEvent, changes: &mut Changes) {
        let info = event.rate_limit_info;
        let window = |name: &str| {
            info.unified_windows
                .get(name)
                .and_then(Option::as_ref)
                .and_then(|window| {
                    Some(RateLimitWindow {
                        utilization_percent: window.utilization? * 100.0,
                        resets_at: window.resets_at.map(unix_seconds),
                    })
                })
        };
        let limits = RateLimits {
            five_hour: window("five_hour").or(self.rate_limits.five_hour.clone()),
            seven_day: window("seven_day").or(self.rate_limits.seven_day.clone()),
            status: info.status.clone().or(self.rate_limits.status.clone()),
        };
        if limits != self.rate_limits {
            self.rate_limits = limits;
            changes.push(ConversationChange::RateLimits);
        }
    }

    fn apply_lifecycle(&mut self, lifecycle: CommandLifecycle, changes: &mut Changes) {
        self.reports_lifecycle = true;
        let uuid = lifecycle.command_uuid;
        match lifecycle.state.as_str() {
            "started" => {
                self.take_in_queued(&uuid, changes);
                if !self.hidden.contains(&uuid) {
                    self.turn_running = true;
                }
                self.current_command = Some(uuid);
            }
            "completed" | "cancelled" | "discarded" | "refused" => {
                // A terminal state can come without `started`; only `completed` means Claude got it.
                if lifecycle.state == "completed" {
                    self.take_in_queued(&uuid, changes);
                } else {
                    self.queued.retain(|message| message.id != uuid);
                }
                self.outstanding.retain(|outstanding| *outstanding != uuid);
                if self.current_command.as_ref() == Some(&uuid) {
                    self.current_command = None;
                }
            }
            _ => {}
        }
    }

    /// Moves the queued message `uuid` into the timeline, now that Claude is reading it.
    fn take_in_queued(&mut self, uuid: &str, changes: &mut Changes) {
        if let Some(position) = self.queued.iter().position(|message| message.id == uuid) {
            let message = self.queued.remove(position);
            self.insert_item(TimelineItem::User(message), changes);
        }
    }

    fn add_permission_request(
        &mut self,
        request_id: String,
        request: CanUseToolRequest,
        changes: &mut Changes,
    ) {
        if self
            .pending_requests
            .iter()
            .any(|pending| pending.request_id == request_id)
        {
            return;
        }
        let kind = match request.tool_name.as_str() {
            ASK_USER_QUESTION_TOOL => PendingRequestKind::Question {
                questions: parse_questions(&request.input),
                input: request.input,
            },
            EXIT_PLAN_MODE_TOOL => PendingRequestKind::PlanApproval {
                plan: request
                    .input
                    .get("plan")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                plan_file_path: request
                    .input
                    .get("planFilePath")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                input: request.input,
            },
            _ => PendingRequestKind::Permission {
                tool_name: request.tool_name,
                display_name: request.display_name,
                input: request.input,
                description: request.title.or(request.description),
                blocked_path: request.blocked_path,
                suggestions: if request.suppress_always_allow_rule {
                    Vec::new()
                } else {
                    request.permission_suggestions
                },
            },
        };
        if let Some(tool_use_id) = &request.tool_use_id {
            self.with_tool(tool_use_id, changes, |tool| {
                if matches!(tool.status, ToolStatus::Streaming | ToolStatus::Running) {
                    tool.status = ToolStatus::AwaitingApproval;
                }
            });
        }
        self.pending_requests.push(PendingRequest {
            request_id,
            tool_use_id: request.tool_use_id,
            kind,
        });
        changes.push(ConversationChange::PendingRequests);
    }

    // --- todos --------------------------------------------------------------------------------

    fn apply_todo_input(&mut self, tool_name: &str, input: &Value, changes: &mut Changes) {
        match tool_name {
            TODO_WRITE_TOOL => {
                let Some(todos) = input.get("todos").and_then(Value::as_array) else {
                    return;
                };
                self.todos = todos
                    .iter()
                    .filter_map(|todo| {
                        Some(TodoItem {
                            id: None,
                            content: todo.get("content")?.as_str()?.to_owned(),
                            active_form: todo
                                .get("activeForm")
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                            status: parse_todo_status(todo.get("status")),
                        })
                    })
                    .collect();
                changes.push(ConversationChange::Todos);
            }
            TASK_UPDATE_TOOL => {
                let Some(task_id) = input.get("taskId").and_then(Value::as_str) else {
                    return;
                };
                let Some(position) = self
                    .todos
                    .iter()
                    .position(|todo| todo.id.as_deref() == Some(task_id))
                else {
                    return;
                };
                if input.get("status").and_then(Value::as_str) == Some("deleted") {
                    self.todos.remove(position);
                } else {
                    let todo = &mut self.todos[position];
                    if let Some(subject) = input.get("subject").and_then(Value::as_str) {
                        todo.content = subject.to_owned();
                    }
                    if let Some(active_form) = input.get("activeForm").and_then(Value::as_str) {
                        todo.active_form = Some(active_form.to_owned());
                    }
                    if input.get("status").is_some() {
                        todo.status = parse_todo_status(input.get("status"));
                    }
                }
                changes.push(ConversationChange::Todos);
            }
            _ => {}
        }
    }

    fn apply_todo_result(
        &mut self,
        tool_name: &str,
        input: &Value,
        structured: &Value,
        changes: &mut Changes,
    ) {
        match tool_name {
            TASK_CREATE_TOOL => {
                let task = structured.get("task");
                let id = task
                    .and_then(|task| task.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let content = task
                    .and_then(|task| task.get("subject"))
                    .or_else(|| input.get("subject"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                if id.is_some() && self.todos.iter().any(|todo| todo.id == id) {
                    return;
                }
                self.todos.push(TodoItem {
                    id,
                    content,
                    active_form: input
                        .get("activeForm")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    status: TodoStatus::Pending,
                });
                changes.push(ConversationChange::Todos);
            }
            TASK_LIST_TOOL => {
                let Some(tasks) = structured.get("tasks").and_then(Value::as_array) else {
                    return;
                };
                let previous = std::mem::take(&mut self.todos);
                self.todos = tasks
                    .iter()
                    .filter_map(|task| {
                        let id = task.get("id")?.as_str()?.to_owned();
                        let active_form = previous
                            .iter()
                            .find(|todo| todo.id.as_deref() == Some(id.as_str()))
                            .and_then(|todo| todo.active_form.clone());
                        Some(TodoItem {
                            content: task
                                .get("subject")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            id: Some(id),
                            active_form,
                            status: parse_todo_status(task.get("status")),
                        })
                    })
                    .collect();
                changes.push(ConversationChange::Todos);
            }
            _ => {}
        }
    }

    // --- helpers ------------------------------------------------------------------------------

    /// A `/name args` message, when `name` is a known command (or any name before the command
    /// list is known).
    fn parse_command(&self, text: &str) -> Option<SlashCommandInvocation> {
        let command = parse_command_syntax(text)?;
        let known = self.commands.is_empty()
            || self
                .commands
                .iter()
                .any(|known| known.name == command.name || known.aliases.contains(&command.name));
        known.then_some(command)
    }

    fn set_permission_mode(&mut self, mode: &str, changes: &mut Changes) {
        if self.session.permission_mode.as_deref() != Some(mode) {
            self.session.permission_mode = Some(mode.to_owned());
            changes.push(ConversationChange::Session);
        }
    }

    fn task_mut(&mut self, task_id: &str) -> Option<&mut BackgroundTask> {
        self.tasks.iter_mut().find(|task| task.task_id == task_id)
    }

    /// Stops streaming indicators of the current turn; with `interrupted`, tool calls still in
    /// flight become interrupted.
    fn finish_turn_items(&mut self, interrupted: bool, now: SystemTime, changes: &mut Changes) {
        for index in (0..self.items.len()).rev() {
            let item = &mut self.items[index];
            let changed = match item {
                TimelineItem::User(_) => break,
                TimelineItem::Assistant(assistant) if assistant.is_streaming => {
                    assistant.is_streaming = false;
                    true
                }
                TimelineItem::Thinking(thinking) if thinking.is_streaming => {
                    thinking.is_streaming = false;
                    true
                }
                TimelineItem::Tool(tool)
                    if interrupted
                        && matches!(
                            tool.status,
                            ToolStatus::Streaming
                                | ToolStatus::Running
                                | ToolStatus::AwaitingApproval
                        ) =>
                {
                    tool.status = ToolStatus::Interrupted;
                    tool.finished_at.get_or_insert(now);
                    true
                }
                TimelineItem::Assistant(_)
                | TimelineItem::Thinking(_)
                | TimelineItem::Tool(_)
                | TimelineItem::Notice(_)
                | TimelineItem::TurnEnd(_) => false,
            };
            if changed {
                let id = item.id().to_owned();
                changes.push(ConversationChange::ItemUpdated { index, id });
            }
        }
    }

    fn finish(
        &mut self,
        previous_status: ConversationStatus,
        now: SystemTime,
        changes: &mut Changes,
    ) {
        let working = self.exited.is_none()
            && (self.turn_running || !self.outstanding.is_empty() || self.compacting);
        if working {
            self.working_since.get_or_insert(now);
        } else {
            self.working_since = None;
        }
        if self.status() != previous_status {
            changes.push(ConversationChange::Status);
        }
    }

    fn next_local_id(&mut self, prefix: &str) -> String {
        self.next_local_id += 1;
        format!("local-{prefix}-{}", self.next_local_id)
    }

    fn push_notice(&mut self, kind: NoticeKind, changes: &mut Changes) {
        let id = self.next_local_id("notice");
        self.insert_item(TimelineItem::Notice(Notice { id, kind }), changes);
    }

    fn insert_item(&mut self, item: TimelineItem, changes: &mut Changes) {
        let id = item.id().to_owned();
        if let Some(&index) = self.index_by_id.get(&id) {
            self.items[index] = item;
            changes.push(ConversationChange::ItemUpdated { index, id });
            return;
        }
        let index = self.items.len();
        self.items.push(item);
        self.index_by_id.insert(id.clone(), index);
        changes.push(ConversationChange::ItemInserted { index, id });
    }

    fn remove_item(&mut self, id: &str, changes: &mut Changes) {
        let Some(index) = self.index_by_id.remove(id) else {
            return;
        };
        self.items.remove(index);
        for (position, item) in self.items.iter().enumerate().skip(index) {
            self.index_by_id.insert(item.id().to_owned(), position);
        }
        changes.push(ConversationChange::ItemRemoved {
            index,
            id: id.to_owned(),
        });
    }

    fn with_item(
        &mut self,
        id: &str,
        changes: &mut Changes,
        update: impl FnOnce(&mut TimelineItem),
    ) -> bool {
        let Some(&index) = self.index_by_id.get(id) else {
            return false;
        };
        update(&mut self.items[index]);
        changes.push(ConversationChange::ItemUpdated {
            index,
            id: id.to_owned(),
        });
        true
    }

    fn with_tool(
        &mut self,
        id: &str,
        changes: &mut Changes,
        update: impl FnOnce(&mut ToolCall),
    ) -> bool {
        let Some(&index) = self.index_by_id.get(id) else {
            return false;
        };
        let TimelineItem::Tool(tool) = &mut self.items[index] else {
            return false;
        };
        update(tool);
        changes.push(ConversationChange::ItemUpdated {
            index,
            id: id.to_owned(),
        });
        true
    }
}

fn parse_command_syntax(text: &str) -> Option<SlashCommandInvocation> {
    let rest = text.strip_prefix('/')?;
    let name_len = rest
        .find(|character: char| character.is_whitespace())
        .unwrap_or(rest.len());
    let name = &rest[..name_len];
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_alphanumeric() || matches!(character, '-' | '_' | ':'));
    valid.then(|| SlashCommandInvocation {
        name: name.to_owned(),
        args: rest[name_len..].trim().to_owned(),
        output: None,
    })
}

fn parse_questions(input: &Value) -> Vec<Question> {
    let Some(questions) = input.get("questions").and_then(Value::as_array) else {
        return Vec::new();
    };
    questions
        .iter()
        .filter_map(|question| {
            Some(Question {
                question: question.get("question")?.as_str()?.to_owned(),
                header: question
                    .get("header")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                multi_select: question
                    .get("multiSelect")
                    .and_then(Value::as_bool)
                    .unwrap_or_default(),
                options: question
                    .get("options")
                    .and_then(Value::as_array)
                    .map(|options| {
                        options
                            .iter()
                            .filter_map(|option| {
                                Some(QuestionOption {
                                    label: option.get("label")?.as_str()?.to_owned(),
                                    description: option
                                        .get("description")
                                        .and_then(Value::as_str)
                                        .map(str::to_owned),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            })
        })
        .collect()
}

fn parse_todo_status(status: Option<&Value>) -> TodoStatus {
    match status.and_then(Value::as_str) {
        Some("in_progress") => TodoStatus::InProgress,
        Some("completed") => TodoStatus::Completed,
        _ => TodoStatus::Pending,
    }
}

fn elapsed_ms(since: SystemTime, now: SystemTime) -> u64 {
    now.duration_since(since)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default()
}

fn unix_seconds(seconds: i64) -> SystemTime {
    if seconds >= 0 {
        UNIX_EPOCH + Duration::from_secs(seconds as u64)
    } else {
        UNIX_EPOCH
    }
}

/// Parses an RFC 3339 timestamp like `2026-10-26T22:20:00.097901+00:00`.
pub fn parse_rfc3339(text: &str) -> Option<SystemTime> {
    let parsed = chrono::DateTime::parse_from_rfc3339(text).ok()?;
    let millis = parsed.timestamp_millis();
    (millis >= 0).then(|| UNIX_EPOCH + Duration::from_millis(millis as u64))
}

// ---------------------------------------------------------------------------------------------
// Correlating our own control requests
// ---------------------------------------------------------------------------------------------

/// The decoded reply to one of our control requests.
#[derive(Debug, Clone, PartialEq)]
pub struct ControlReply {
    pub request_id: String,
    /// The request this answers.
    pub request: ControlRequestBody,
    pub outcome: Result<ReplyPayload, String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReplyPayload {
    Initialize(Box<InitializeResponse>),
    McpStatus(McpStatusResponse),
    McpAuthenticate(McpAuthenticateResponse),
    ContextUsage(Box<ContextUsage>),
    Usage(UsageResponse),
    RemoteControl(RemoteControlResponse),
    RewindFiles(RewindFilesResponse),
    FileSuggestions(FileSuggestionsResponse),
    Interrupt(InterruptResponse),
    CancelAsyncMessage(CancelAsyncMessageResponse),
    /// The mode `set_permission_mode` switched to.
    PermissionMode(String),
    /// A success without a payload we model.
    Ack,
    /// A payload whose shape didn't match what the request should return.
    Raw(Value),
}

/// Issues ids for our control requests and matches the CLI's replies back to them.
pub struct RequestTracker {
    prefix: String,
    next_id: u64,
    in_flight: HashMap<String, ControlRequestBody>,
}

impl Default for RequestTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl RequestTracker {
    pub fn new() -> Self {
        let prefix = uuid::Uuid::new_v4().simple().to_string()[..8].to_owned();
        Self {
            prefix,
            next_id: 0,
            in_flight: HashMap::new(),
        }
    }

    /// Returns the request id and the stdin line to write.
    pub fn issue(&mut self, request: ControlRequestBody) -> (String, String) {
        self.next_id += 1;
        let request_id = format!("req_{}_{}", self.prefix, self.next_id);
        let line = control_request_line(&request_id, &request);
        self.in_flight.insert(request_id.clone(), request);
        (request_id, line)
    }

    /// Records a request whose line was built elsewhere, so its reply can still be matched.
    pub fn track(&mut self, request_id: String, request: ControlRequestBody) {
        self.in_flight.insert(request_id, request);
    }

    pub fn is_in_flight(&self, request_id: &str) -> bool {
        self.in_flight.contains_key(request_id)
    }

    /// Matches a reply to the request it answers. `None` for replies to requests we didn't issue.
    pub fn resolve(&mut self, response: ControlResponse) -> Option<ControlReply> {
        let request = self.in_flight.remove(&response.request_id)?;
        let outcome = response.outcome.map(|value| decode_reply(&request, value));
        Some(ControlReply {
            request_id: response.request_id,
            request,
            outcome,
        })
    }
}

fn decode_reply(request: &ControlRequestBody, value: Value) -> ReplyPayload {
    fn typed<T: serde::de::DeserializeOwned>(
        value: Value,
        wrap: impl FnOnce(T) -> ReplyPayload,
    ) -> ReplyPayload {
        match T::deserialize(&value) {
            Ok(typed) => wrap(typed),
            Err(_) => ReplyPayload::Raw(value),
        }
    }

    match request {
        ControlRequestBody::Initialize { .. } => typed(value, |response| {
            ReplyPayload::Initialize(Box::new(response))
        }),
        ControlRequestBody::McpStatus => typed(value, ReplyPayload::McpStatus),
        ControlRequestBody::McpAuthenticate { .. } => typed(value, ReplyPayload::McpAuthenticate),
        ControlRequestBody::GetContextUsage { .. } => {
            typed(value, |usage| ReplyPayload::ContextUsage(Box::new(usage)))
        }
        ControlRequestBody::GetUsage { .. } => typed(value, ReplyPayload::Usage),
        ControlRequestBody::RemoteControl { .. } => {
            if value.is_null() {
                ReplyPayload::RemoteControl(RemoteControlResponse::default())
            } else {
                typed(value, ReplyPayload::RemoteControl)
            }
        }
        ControlRequestBody::RewindFiles { .. } => typed(value, ReplyPayload::RewindFiles),
        ControlRequestBody::FileSuggestions { .. } => typed(value, ReplyPayload::FileSuggestions),
        ControlRequestBody::CancelAsyncMessage { .. } => {
            typed(value, ReplyPayload::CancelAsyncMessage)
        }
        ControlRequestBody::Interrupt | ControlRequestBody::SendNow { .. } => {
            if value.is_null() {
                ReplyPayload::Interrupt(InterruptResponse::default())
            } else {
                typed(value, ReplyPayload::Interrupt)
            }
        }
        ControlRequestBody::SetPermissionMode { mode } => match value.get("mode") {
            Some(Value::String(mode)) => ReplyPayload::PermissionMode(mode.clone()),
            _ => ReplyPayload::PermissionMode(mode.clone()),
        },
        ControlRequestBody::SetModel { .. }
        | ControlRequestBody::SetMaxThinkingTokens { .. }
        | ControlRequestBody::McpOauthCallbackUrl { .. }
        | ControlRequestBody::McpClearAuth { .. }
        | ControlRequestBody::McpToggle { .. }
        | ControlRequestBody::McpReconnect { .. } => ReplyPayload::Ack,
    }
}

#[cfg(test)]
#[path = "reducer_tests.rs"]
mod tests;
