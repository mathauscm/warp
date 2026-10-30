//! Drives the OpenAI Codex CLI headlessly through `codex app-server`: JSON-RPC 2.0 over stdio,
//! one message per line, without the `jsonrpc` field. [`CodexClient`] translates what the app
//! server reports into the Claude Code `stream-json` messages [`crate::reducer`] already models,
//! so a Codex conversation renders with the same timeline, permission cards and turn summaries.
//!
//! Like [`crate::process`], this module has no executor: [`spawn`] returns the process and a
//! driver future to run, and [`CodexClient`] is a pure state machine the caller feeds with the
//! process's messages and whose [`CodexEvent`]s it carries out.

use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use command::r#async::Command;
use serde_json::{Value, json};

use crate::process::{self, LineProcess, ProcessDriver};
use crate::protocol::{INTERRUPT_MARKER, InitializeResponse, ModelInfo, StdoutMessage};

/// What to tell the user when the CLI isn't installed.
pub const INSTALL_HINT: &str = "npm i -g @openai/codex";
const CLIENT_NAME: &str = "picolas";
const CLIENT_TITLE: &str = "Picolas Code";
/// The prefix of the permission request ids made up for the app server's approval requests.
const APPROVAL_PREFIX: &str = "codex-";
/// The suggestion a permission card offers to stop asking for the rest of the thread. Its shape
/// is the one the card already labels ("Sempre permitir … nesta sessão").
const SESSION_DECISION: &str = "acceptForSession";

/// `$CODEX_HOME`, else `~/.codex`, where the CLI keeps its sessions and model cache.
pub fn codex_home() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
}

/// Looks for the `codex` executable in `path_env` (a `PATH`-style list, typically from the user's
/// login shell), then in the usual npm and Homebrew install locations.
pub fn find_codex(path_env: Option<&str>) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    find_codex_in(path_env, home.as_deref())
}

/// [`find_codex`] with an explicit home directory.
pub fn find_codex_in(path_env: Option<&str>, home: Option<&Path>) -> Option<PathBuf> {
    process::find_executable_in("codex", path_env, home)
}

// ---------------------------------------------------------------------------------------------
// process
// ---------------------------------------------------------------------------------------------

/// A running `codex app-server`, whose stdout lines are parsed as JSON.
pub type CodexProcess = LineProcess<Value>;

#[derive(Debug, Clone, PartialEq)]
pub struct SpawnConfig {
    pub codex_path: PathBuf,
    pub cwd: PathBuf,
    /// The `PATH` for the CLI and the commands it runs, usually the login shell's.
    pub path_env: Option<String>,
}

impl SpawnConfig {
    pub fn new(codex_path: impl Into<PathBuf>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            codex_path: codex_path.into(),
            cwd: cwd.into(),
            path_env: None,
        }
    }

    pub fn args(&self) -> Vec<String> {
        vec!["app-server".to_owned()]
    }

    /// Inherited variables to drop: they change how the CLI's Node launcher behaves.
    pub fn env_remove() -> &'static [&'static str] {
        &["NODE_OPTIONS", "DEBUG", "CLAUDECODE"]
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.codex_path);
        command.args(self.args()).current_dir(&self.cwd);
        for name in Self::env_remove() {
            command.env_remove(name);
        }
        if let Some(path) = &self.path_env {
            command.env(OsString::from("PATH"), OsString::from(path));
        }
        command
    }
}

/// Starts the app server. The returned driver must be spawned on an executor.
pub fn spawn(config: &SpawnConfig) -> io::Result<(CodexProcess, ProcessDriver)> {
    log::info!(
        "Spawning codex in {} with args {:?}",
        config.cwd.display(),
        config.args()
    );
    process::spawn_lines(config.command(), "codex", |line| {
        serde_json::from_str(line).map_err(|error| error.to_string())
    })
}

// ---------------------------------------------------------------------------------------------
// models
// ---------------------------------------------------------------------------------------------

/// A model the Codex CLI offers.
#[derive(Debug, Clone, PartialEq)]
pub struct CodexModel {
    /// What to pass as `model`, like `gpt-5-codex`.
    pub id: String,
    pub display_name: String,
    pub description: String,
    /// Reasoning efforts, like `low`, `medium` and `high`.
    pub efforts: Vec<String>,
    pub default_effort: Option<String>,
    /// The model the CLI picks when none is given.
    pub is_default: bool,
}

impl CodexModel {
    /// The entry the model picker lists.
    pub fn model_info(&self) -> ModelInfo {
        ModelInfo {
            value: self.id.clone(),
            resolved_model: Some(self.id.clone()),
            display_name: self.display_name.clone(),
            description: self.description.clone(),
            supports_effort: !self.efforts.is_empty(),
            supported_effort_levels: self.efforts.clone(),
            ..ModelInfo::default()
        }
    }
}

/// The visible models of a `model/list` result.
pub fn parse_model_list(result: &Value) -> Vec<CodexModel> {
    result
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|model| {
            !model
                .get("hidden")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .filter_map(|model| {
            let id = str_of(model, "model").or_else(|| str_of(model, "id"))?;
            Some(CodexModel {
                display_name: str_of(model, "displayName").unwrap_or_else(|| id.clone()),
                description: str_of(model, "description").unwrap_or_default(),
                efforts: model
                    .get("supportedReasoningEfforts")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|effort| str_of(effort, "reasoningEffort"))
                    .collect(),
                default_effort: str_of(model, "defaultReasoningEffort"),
                is_default: model
                    .get("isDefault")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                id,
            })
        })
        .collect()
}

/// The models listed in the CLI's own cache (`models_cache.json` in `codex_home`), so the picker
/// can offer Codex models before any Codex session runs. Empty when there's no cache.
pub fn read_models_cache(codex_home: &Path) -> Vec<CodexModel> {
    let Ok(text) = std::fs::read_to_string(codex_home.join("models_cache.json")) else {
        return Vec::new();
    };
    let Ok(cache) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    let mut models: Vec<(i64, CodexModel)> = cache
        .get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|model| str_of(model, "visibility").as_deref() == Some("list"))
        .filter_map(|model| {
            let id = str_of(model, "slug")?;
            let priority = model
                .get("priority")
                .and_then(Value::as_i64)
                .unwrap_or(i64::MAX);
            Some((
                priority,
                CodexModel {
                    display_name: str_of(model, "display_name").unwrap_or_else(|| id.clone()),
                    description: str_of(model, "description").unwrap_or_default(),
                    efforts: model
                        .get("supported_reasoning_levels")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(|level| str_of(level, "effort"))
                        .collect(),
                    default_effort: str_of(model, "default_reasoning_level"),
                    is_default: false,
                    id,
                },
            ))
        })
        .collect();
    models.sort_by_key(|(priority, _)| *priority);
    let mut models: Vec<CodexModel> = models.into_iter().map(|(_, model)| model).collect();
    if let Some(first) = models.first_mut() {
        first.is_default = true;
    }
    models
}

// ---------------------------------------------------------------------------------------------
// permissions
// ---------------------------------------------------------------------------------------------

/// The app server's approval policy and sandbox for one of the chat's permission modes
/// (`default`, `acceptEdits`, `plan`, `auto` or `bypassPermissions`). `writable_roots` are folders
/// besides the working directory that Codex may write to.
pub fn turn_policy(permission_mode: &str, writable_roots: &[PathBuf]) -> (Value, Value) {
    let workspace_write = || {
        let mut sandbox = json!({ "type": "workspaceWrite" });
        if !writable_roots.is_empty() {
            sandbox["writableRoots"] = writable_roots
                .iter()
                .map(|root| Value::String(root.to_string_lossy().into_owned()))
                .collect();
        }
        sandbox
    };
    match permission_mode {
        "plan" => (json!("on-request"), json!({ "type": "readOnly" })),
        "acceptEdits" | "auto" => (json!("on-request"), workspace_write()),
        "bypassPermissions" => (json!("never"), json!({ "type": "dangerFullAccess" })),
        _ => (json!("untrusted"), workspace_write()),
    }
}

/// The `sandbox` mode `thread/start` takes for a permission mode.
fn sandbox_mode(permission_mode: &str) -> &'static str {
    match permission_mode {
        "plan" => "read-only",
        "bypassPermissions" => "danger-full-access",
        _ => "workspace-write",
    }
}

// ---------------------------------------------------------------------------------------------
// client
// ---------------------------------------------------------------------------------------------

/// How a Codex chat starts.
#[derive(Debug, Clone, PartialEq)]
pub struct CodexConfig {
    pub cwd: PathBuf,
    /// More folders Codex may write to besides `cwd`.
    pub extra_dirs: Vec<PathBuf>,
    /// Continue this saved thread instead of starting a new one.
    pub resume_thread_id: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// One of the chat's permission modes; see [`turn_policy`].
    pub permission_mode: String,
    /// Added to Codex's instructions as a developer message.
    pub developer_instructions: Option<String>,
    pub client_version: String,
}

impl CodexConfig {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            extra_dirs: Vec::new(),
            resume_thread_id: None,
            model: None,
            effort: None,
            permission_mode: "default".to_owned(),
            developer_instructions: None,
            client_version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }
}

/// What the caller has to carry out after feeding the client.
#[derive(Debug, Clone, PartialEq)]
pub enum CodexEvent {
    /// A line to write to the app server's stdin.
    Write(String),
    /// A Claude-shaped message for the conversation reducer.
    Message(StdoutMessage),
    /// The thread started or resumed; its id names the conversation from now on.
    ThreadStarted {
        thread_id: String,
    },
    Models(Vec<CodexModel>),
    /// The latest request's size and the model's context window.
    ContextTokens {
        tokens: u64,
        window: Option<u64>,
    },
    /// Codex named the thread.
    TitleChanged(String),
    /// Paths matching an `@` mention's query, relative to the folder they're in.
    FileSuggestions {
        query: String,
        paths: Vec<String>,
    },
    /// Something failed that the user should hear about.
    Error(String),
}

#[derive(Debug, Clone, PartialEq)]
enum PendingRequest {
    Initialize,
    ModelList,
    ThreadStart,
    ThreadResume,
    TurnStart {
        user_uuid: String,
    },
    TurnInterrupt,
    FileSearch {
        query: String,
    },
    /// Requests whose reply only matters when it's an error.
    Other {
        method: &'static str,
    },
}

#[derive(Debug, Clone, PartialEq)]
struct QueuedInput {
    user_uuid: String,
    input: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq)]
struct ActiveTurn {
    user_uuid: String,
    turn_id: Option<String>,
    /// Set by an `error` notification the app server won't retry, for the turn's result.
    error: Option<String>,
    interrupt_requested: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamKind {
    Text,
    Thinking,
}

#[derive(Debug, Clone, PartialEq)]
struct OpenStream {
    item_id: String,
    kind: StreamKind,
}

/// One tool call shown for a Codex item; a patch touching several files shows one per file.
#[derive(Debug, Clone, PartialEq)]
struct ItemTool {
    id: String,
    name: String,
    input: Value,
}

/// An approval request of the app server, waiting for the user.
#[derive(Debug, Clone, PartialEq)]
struct PendingApproval {
    rpc_id: Value,
}

/// Translates between the chat and one `codex app-server` process; see the module docs.
pub struct CodexClient {
    config: CodexConfig,
    next_id: i64,
    pending: HashMap<i64, PendingRequest>,
    thread_id: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    permission_mode: String,
    turn: Option<ActiveTurn>,
    queued: VecDeque<QueuedInput>,
    approvals: HashMap<String, PendingApproval>,
    open_stream: Option<OpenStream>,
    /// Reasoning items whose summary streamed, so their raw text isn't shown twice.
    summarized_reasoning: Vec<String>,
    item_tools: HashMap<String, Vec<ItemTool>>,
    context_window: Option<u64>,
    plan_updates: u64,
    /// The models `model/list` reported, for their default efforts.
    models: Vec<CodexModel>,
}

impl CodexClient {
    pub fn new(config: CodexConfig) -> Self {
        Self {
            model: config.model.clone(),
            effort: config.effort.clone(),
            permission_mode: config.permission_mode.clone(),
            config,
            next_id: 0,
            pending: HashMap::new(),
            thread_id: None,
            turn: None,
            queued: VecDeque::new(),
            approvals: HashMap::new(),
            open_stream: None,
            summarized_reasoning: Vec::new(),
            item_tools: HashMap::new(),
            context_window: None,
            plan_updates: 0,
            models: Vec::new(),
        }
    }

    pub fn thread_id(&self) -> Option<&str> {
        self.thread_id.as_deref()
    }

    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    pub fn effort(&self) -> Option<&str> {
        self.effort.as_deref()
    }

    pub fn permission_mode(&self) -> &str {
        &self.permission_mode
    }

    /// Whether a turn runs or waits to start.
    pub fn is_busy(&self) -> bool {
        self.turn.is_some() || !self.queued.is_empty()
    }

    /// The handshake, written first thing once the process is up.
    pub fn start(&mut self) -> Vec<CodexEvent> {
        let params = json!({
            "clientInfo": {
                "name": CLIENT_NAME,
                "title": CLIENT_TITLE,
                "version": self.config.client_version,
            },
        });
        vec![self.request(PendingRequest::Initialize, "initialize", params)]
    }

    /// Sends a user message: starts a turn with it, or queues it until the thread is ready and
    /// the running turn ended. `images` are `(media type, base64 data)` pairs.
    pub fn send_message(
        &mut self,
        user_uuid: &str,
        text: &str,
        images: &[(String, String)],
    ) -> Vec<CodexEvent> {
        let mut input: Vec<Value> = images
            .iter()
            .map(|(media_type, data)| {
                json!({ "type": "image", "url": format!("data:{media_type};base64,{data}") })
            })
            .collect();
        if !text.is_empty() || input.is_empty() {
            input.push(json!({ "type": "text", "text": text, "text_elements": [] }));
        }
        self.queued.push_back(QueuedInput {
            user_uuid: user_uuid.to_owned(),
            input,
        });
        self.start_next_turn()
    }

    /// Stops the running turn. Queued messages still run afterwards.
    pub fn interrupt(&mut self) -> Vec<CodexEvent> {
        let Some(thread_id) = self.thread_id.clone() else {
            return Vec::new();
        };
        let Some(turn) = &mut self.turn else {
            return Vec::new();
        };
        turn.interrupt_requested = true;
        let Some(turn_id) = turn.turn_id.clone() else {
            return Vec::new();
        };
        vec![self.request(
            PendingRequest::TurnInterrupt,
            "turn/interrupt",
            json!({ "threadId": thread_id, "turnId": turn_id }),
        )]
    }

    /// Answers the approval request `request_id` (a `can_use_tool` request id this client made
    /// up) with `accept`, `acceptForSession`, `decline` or `cancel`.
    pub fn answer_approval(&mut self, request_id: &str, decision: &str) -> Vec<CodexEvent> {
        let Some(approval) = self.approvals.remove(request_id) else {
            return Vec::new();
        };
        vec![CodexEvent::Write(response_line(
            &approval.rpc_id,
            json!({ "decision": decision }),
        ))]
    }

    /// The decision that also stops asking for the rest of the thread, as echoed by the card's
    /// suggestion.
    pub fn suggestion_decision(suggestion: &Value) -> Option<&'static str> {
        (suggestion.get("codexDecision").and_then(Value::as_str) == Some(SESSION_DECISION))
            .then_some(SESSION_DECISION)
    }

    /// Looks for files matching `query` in the chat's folders.
    pub fn file_suggestions(&mut self, query: &str) -> Vec<CodexEvent> {
        let roots: Vec<String> = std::iter::once(&self.config.cwd)
            .chain(&self.config.extra_dirs)
            .map(|root| root.to_string_lossy().into_owned())
            .collect();
        vec![self.request(
            PendingRequest::FileSearch {
                query: query.to_owned(),
            },
            "fuzzyFileSearch",
            json!({ "query": query, "roots": roots }),
        )]
    }

    /// Summarizes the thread so far to free up context.
    pub fn compact(&mut self) -> Vec<CodexEvent> {
        let Some(thread_id) = self.thread_id.clone() else {
            return Vec::new();
        };
        vec![self.request(
            PendingRequest::Other {
                method: "thread/compact/start",
            },
            "thread/compact/start",
            json!({ "threadId": thread_id }),
        )]
    }

    /// Names the thread, the way Codex's own UI does.
    pub fn rename(&mut self, name: &str) -> Vec<CodexEvent> {
        let Some(thread_id) = self.thread_id.clone() else {
            return Vec::new();
        };
        vec![self.request(
            PendingRequest::Other {
                method: "thread/name/set",
            },
            "thread/name/set",
            json!({ "threadId": thread_id, "name": name }),
        )]
    }

    /// Picks the model of the next turns; `None` goes back to the CLI's default.
    pub fn set_model(&mut self, model: Option<String>) -> Vec<CodexEvent> {
        self.model = model;
        self.init_message().into_iter().collect()
    }

    pub fn set_effort(&mut self, effort: Option<String>) {
        self.effort = effort;
    }

    /// Switches the approval policy and sandbox of the next turns.
    pub fn set_permission_mode(&mut self, mode: &str) -> Vec<CodexEvent> {
        self.permission_mode = mode.to_owned();
        vec![message(json!({
            "type": "system",
            "subtype": "status",
            "status": null,
            "permissionMode": mode,
        }))]
    }

    /// Handles one message from the app server.
    pub fn handle(&mut self, message: Value) -> Vec<CodexEvent> {
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_owned);
        match (method, message.get("id").cloned()) {
            (Some(method), Some(id)) => self.on_server_request(&method, id, &message),
            (Some(method), None) => {
                self.on_notification(&method, message.get("params").unwrap_or(&Value::Null))
            }
            (None, Some(id)) => self.on_response(&id, &message),
            (None, None) => Vec::new(),
        }
    }

    // --- requests -----------------------------------------------------------------------------

    fn request(&mut self, kind: PendingRequest, method: &str, params: Value) -> CodexEvent {
        self.next_id += 1;
        self.pending.insert(self.next_id, kind);
        CodexEvent::Write(
            json!({ "id": self.next_id, "method": method, "params": params }).to_string(),
        )
    }

    fn start_thread(&mut self) -> CodexEvent {
        let mut params = json!({
            "cwd": self.config.cwd.to_string_lossy(),
            "approvalPolicy": turn_policy(&self.permission_mode, &self.config.extra_dirs).0,
            "sandbox": sandbox_mode(&self.permission_mode),
        });
        if let Some(model) = &self.model {
            params["model"] = json!(model);
        }
        if let Some(instructions) = &self.config.developer_instructions {
            params["developerInstructions"] = json!(instructions);
        }
        match self.config.resume_thread_id.clone() {
            Some(thread_id) => {
                params["threadId"] = json!(thread_id);
                // The history comes from the session file; see `crate::codex_history`.
                params["excludeTurns"] = json!(true);
                self.request(PendingRequest::ThreadResume, "thread/resume", params)
            }
            None => self.request(PendingRequest::ThreadStart, "thread/start", params),
        }
    }

    fn start_next_turn(&mut self) -> Vec<CodexEvent> {
        if self.turn.is_some() {
            return Vec::new();
        }
        let Some(thread_id) = self.thread_id.clone() else {
            return Vec::new();
        };
        let Some(next) = self.queued.pop_front() else {
            return Vec::new();
        };
        let (approval_policy, sandbox_policy) =
            turn_policy(&self.permission_mode, &self.config.extra_dirs);
        let mut params = json!({
            "threadId": thread_id,
            "input": next.input,
            "approvalPolicy": approval_policy,
            "sandboxPolicy": sandbox_policy,
            "summary": "auto",
        });
        if let Some(model) = &self.model {
            params["model"] = json!(model);
        }
        if let Some(effort) = &self.effort {
            params["effort"] = json!(effort);
        }
        self.turn = Some(ActiveTurn {
            user_uuid: next.user_uuid.clone(),
            turn_id: None,
            error: None,
            interrupt_requested: false,
        });
        vec![self.request(
            PendingRequest::TurnStart {
                user_uuid: next.user_uuid,
            },
            "turn/start",
            params,
        )]
    }

    /// The effort turns run with when none was picked: the current model's default.
    fn default_effort(&self) -> Option<String> {
        let model = self.model.as_deref();
        self.models
            .iter()
            .find(|candidate| Some(candidate.id.as_str()) == model)
            .or_else(|| self.models.iter().find(|candidate| candidate.is_default))
            .and_then(|candidate| candidate.default_effort.clone())
    }

    fn init_message(&self) -> Option<CodexEvent> {
        let thread_id = self.thread_id.as_ref()?;
        Some(message(json!({
            "type": "system",
            "subtype": "init",
            "session_id": thread_id,
            "cwd": self.config.cwd.to_string_lossy(),
            "model": self.model,
            "effort": self.effort.clone().or_else(|| self.default_effort()),
            "permissionMode": self.permission_mode,
            "tools": [],
            "slash_commands": [],
        })))
    }

    // --- responses ----------------------------------------------------------------------------

    fn on_response(&mut self, id: &Value, message: &Value) -> Vec<CodexEvent> {
        let Some(kind) = id.as_i64().and_then(|id| self.pending.remove(&id)) else {
            return Vec::new();
        };
        let result = match (message.get("result"), message.get("error")) {
            (_, Some(error)) => Err(error
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| error.to_string())),
            (Some(result), None) => Ok(result),
            (None, None) => Ok(&Value::Null),
        };
        let mut events = Vec::new();
        match (kind, result) {
            (PendingRequest::Initialize, Ok(_)) => {
                events.push(CodexEvent::Write(
                    json!({ "method": "initialized" }).to_string(),
                ));
                events.push(self.request(PendingRequest::ModelList, "model/list", json!({})));
                events.push(self.start_thread());
            }
            (PendingRequest::Initialize, Err(error)) => {
                events.push(CodexEvent::Error(format!(
                    "Não foi possível iniciar o Codex: {error}"
                )));
            }
            (PendingRequest::ModelList, Ok(result)) => {
                self.models = parse_model_list(result);
                events.push(CodexEvent::Models(self.models.clone()));
                events.extend(self.init_message());
            }
            (PendingRequest::ModelList, Err(error)) => {
                log::warn!("Codex model/list failed: {error}");
            }
            (PendingRequest::ThreadStart | PendingRequest::ThreadResume, Ok(result)) => {
                events.extend(self.on_thread_ready(result));
            }
            (PendingRequest::ThreadResume, Err(error)) => {
                log::warn!("Codex thread/resume failed: {error}");
                events.push(CodexEvent::Error(format!(
                    "Não foi possível retomar a conversa do Codex: {error}. Começando uma nova."
                )));
                self.config.resume_thread_id = None;
                events.push(self.start_thread());
            }
            (PendingRequest::ThreadStart, Err(error)) => {
                events.push(CodexEvent::Error(format!(
                    "Não foi possível iniciar a conversa do Codex: {error}"
                )));
            }
            (PendingRequest::TurnStart { user_uuid }, Ok(result)) => {
                if let Some(turn) = self
                    .turn
                    .as_mut()
                    .filter(|turn| turn.user_uuid == user_uuid)
                    && turn.turn_id.is_none()
                {
                    turn.turn_id = result.get("turn").and_then(|turn| str_of(turn, "id"));
                    if turn.interrupt_requested {
                        events.extend(self.interrupt());
                    }
                }
            }
            (PendingRequest::TurnStart { user_uuid }, Err(error)) => {
                if self
                    .turn
                    .as_ref()
                    .is_some_and(|turn| turn.user_uuid == user_uuid)
                {
                    self.turn = None;
                }
                events.push(self.result_message(&user_uuid, "failed", None, Some(error)));
                events.extend(self.start_next_turn());
            }
            (PendingRequest::TurnInterrupt, result) => {
                if let Err(error) = result {
                    log::warn!("Codex turn/interrupt failed: {error}");
                }
            }
            (PendingRequest::FileSearch { query }, Ok(result)) => {
                let paths = result
                    .get("files")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|file| str_of(file, "path"))
                    .collect();
                events.push(CodexEvent::FileSuggestions { query, paths });
            }
            (PendingRequest::FileSearch { query }, Err(error)) => {
                log::warn!("Codex fuzzyFileSearch failed: {error}");
                events.push(CodexEvent::FileSuggestions {
                    query,
                    paths: Vec::new(),
                });
            }
            (PendingRequest::Other { method }, result) => {
                if let Err(error) = result {
                    log::warn!("Codex {method} failed: {error}");
                    events.push(CodexEvent::Error(error));
                }
            }
        }
        events
    }

    fn on_thread_ready(&mut self, result: &Value) -> Vec<CodexEvent> {
        let Some(thread) = result.get("thread") else {
            return vec![CodexEvent::Error(
                "O Codex não informou a conversa iniciada.".to_owned(),
            )];
        };
        let Some(thread_id) = str_of(thread, "id") else {
            return Vec::new();
        };
        self.thread_id = Some(thread_id.clone());
        if self.model.is_none() {
            self.model = str_of(result, "model").or_else(|| str_of(thread, "model"));
        }
        if self.effort.is_none() {
            self.effort = str_of(result, "reasoningEffort");
        }
        let mut events = vec![CodexEvent::ThreadStarted { thread_id }];
        if let Some(name) = str_of(thread, "name").filter(|name| !name.trim().is_empty()) {
            events.push(CodexEvent::TitleChanged(name));
        }
        events.extend(self.init_message());
        events.extend(self.start_next_turn());
        events
    }

    // --- server requests ----------------------------------------------------------------------

    fn on_server_request(&mut self, method: &str, id: Value, message: &Value) -> Vec<CodexEvent> {
        let params = message.get("params").unwrap_or(&Value::Null);
        match method {
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                self.request_approval(method, id, params)
            }
            "item/permissions/requestApproval" => vec![CodexEvent::Write(response_line(
                &id,
                json!({ "permissions": {}, "scope": "turn" }),
            ))],
            "mcpServer/elicitation/request" => vec![CodexEvent::Write(response_line(
                &id,
                json!({ "action": "decline" }),
            ))],
            "execCommandApproval" | "applyPatchApproval" => vec![CodexEvent::Write(response_line(
                &id,
                json!({ "decision": "denied" }),
            ))],
            _ => {
                log::info!("Declining unsupported Codex request {method}");
                vec![CodexEvent::Write(
                    json!({
                        "id": id,
                        "error": { "code": -32601, "message": format!("{method} is not supported") },
                    })
                    .to_string(),
                )]
            }
        }
    }

    fn request_approval(&mut self, method: &str, id: Value, params: &Value) -> Vec<CodexEvent> {
        let request_id = format!("{APPROVAL_PREFIX}{}", rpc_id_text(&id));
        let item_id = str_of(params, "itemId").unwrap_or_default();
        let tools = self.item_tools.get(&item_id);
        // The card shows a command as the command it runs, whatever the timeline calls it.
        let (tool_name, input, tool_use_id, rule) =
            if method == "item/commandExecution/requestApproval" {
                let command = str_of(params, "command")
                    .map(|command| unwrap_shell(&command))
                    .or_else(|| {
                        tools
                            .and_then(|tools| tools.first())
                            .and_then(|tool| str_of(&tool.input, "command"))
                    })
                    .unwrap_or_default();
                (
                    "Bash".to_owned(),
                    json!({ "command": command }),
                    item_id.clone(),
                    json!({ "toolName": "Bash", "ruleContent": command }),
                )
            } else {
                let tool = tools.and_then(|tools| tools.first());
                (
                    tool.map(|tool| tool.name.clone())
                        .unwrap_or_else(|| "Edit".to_owned()),
                    tool.map(|tool| tool.input.clone())
                        .unwrap_or_else(|| json!({})),
                    tool.map(|tool| tool.id.clone())
                        .unwrap_or_else(|| item_id.clone()),
                    json!({ "toolName": "edições" }),
                )
            };
        let mut description = str_of(params, "reason");
        if let Some(host) = params
            .get("networkApprovalContext")
            .and_then(|context| str_of(context, "host"))
        {
            description = Some(format!("Acesso à rede: {host}"));
        }
        self.approvals
            .insert(request_id.clone(), PendingApproval { rpc_id: id });
        vec![message(json!({
            "type": "control_request",
            "request_id": request_id,
            "request": {
                "subtype": "can_use_tool",
                "tool_name": tool_name,
                "input": input,
                "description": description,
                "tool_use_id": tool_use_id,
                "permission_suggestions": [{
                    "type": "addRules",
                    "destination": "session",
                    "behavior": "allow",
                    "rules": [rule],
                    "codexDecision": SESSION_DECISION,
                }],
            },
        }))]
    }

    // --- notifications ------------------------------------------------------------------------

    fn on_notification(&mut self, method: &str, params: &Value) -> Vec<CodexEvent> {
        match method {
            "turn/started" => {
                if let Some(turn) = &mut self.turn
                    && turn.turn_id.is_none()
                {
                    turn.turn_id = params.get("turn").and_then(|turn| str_of(turn, "id"));
                }
                Vec::new()
            }
            "turn/completed" => self.on_turn_completed(params),
            "item/started" => params
                .get("item")
                .map(|item| self.on_item_started(item))
                .unwrap_or_default(),
            "item/completed" => params
                .get("item")
                .map(|item| self.on_item_completed(item))
                .unwrap_or_default(),
            "item/agentMessage/delta" => {
                self.stream_delta(params, StreamKind::Text, "text_delta", "text")
            }
            "item/reasoning/summaryTextDelta" => {
                if let Some(item_id) = str_of(params, "itemId")
                    && !self.summarized_reasoning.contains(&item_id)
                {
                    self.summarized_reasoning.push(item_id);
                }
                self.stream_delta(params, StreamKind::Thinking, "thinking_delta", "thinking")
            }
            "item/reasoning/summaryPartAdded" => {
                let is_open = self.open_stream.as_ref().is_some_and(|stream| {
                    Some(&stream.item_id) == str_of(params, "itemId").as_ref()
                });
                if is_open {
                    let mut params = params.clone();
                    params["delta"] = json!("\n\n");
                    self.stream_delta(&params, StreamKind::Thinking, "thinking_delta", "thinking")
                } else {
                    Vec::new()
                }
            }
            "item/reasoning/textDelta" => {
                let summarized = str_of(params, "itemId")
                    .is_some_and(|item_id| self.summarized_reasoning.contains(&item_id));
                if summarized {
                    Vec::new()
                } else {
                    self.stream_delta(params, StreamKind::Thinking, "thinking_delta", "thinking")
                }
            }
            "turn/plan/updated" => self.on_plan_updated(params),
            "thread/tokenUsage/updated" => {
                let usage = params.get("tokenUsage").unwrap_or(&Value::Null);
                if let Some(window) = usage.get("modelContextWindow").and_then(Value::as_u64) {
                    self.context_window = Some(window);
                }
                let last = usage.get("last").unwrap_or(&Value::Null);
                let tokens = ["inputTokens", "outputTokens"]
                    .iter()
                    .filter_map(|key| last.get(*key).and_then(Value::as_u64))
                    .sum();
                vec![CodexEvent::ContextTokens {
                    tokens,
                    window: self.context_window,
                }]
            }
            "thread/name/updated" => str_of(params, "threadName")
                .or_else(|| str_of(params, "name"))
                .filter(|name| !name.trim().is_empty())
                .map(|name| vec![CodexEvent::TitleChanged(name)])
                .unwrap_or_default(),
            "thread/compacted" => vec![message(json!({
                "type": "system",
                "subtype": "compact_boundary",
                "compact_metadata": { "trigger": "auto" },
            }))],
            "error" => {
                let error_message = params
                    .get("error")
                    .and_then(|error| str_of(error, "message"))
                    .unwrap_or_else(|| "Erro desconhecido do Codex".to_owned());
                let will_retry = params
                    .get("willRetry")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if will_retry {
                    log::info!("Codex will retry after: {error_message}");
                } else if let Some(turn) = &mut self.turn {
                    turn.error = Some(error_message);
                }
                Vec::new()
            }
            "serverRequest/resolved" => {
                let request_id = params
                    .get("requestId")
                    .map(|id| format!("{APPROVAL_PREFIX}{}", rpc_id_text(id)));
                match request_id.and_then(|id| self.approvals.remove_entry(&id)) {
                    Some((request_id, _)) => vec![message(json!({
                        "type": "control_cancel_request",
                        "request_id": request_id,
                    }))],
                    None => Vec::new(),
                }
            }
            "warning" | "configWarning" | "deprecationNotice" => {
                log::info!("Codex {method}: {params}");
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn on_turn_completed(&mut self, params: &Value) -> Vec<CodexEvent> {
        let turn_value = params.get("turn").unwrap_or(&Value::Null);
        let mut events = self.close_stream();
        let Some(turn) = self.turn.take() else {
            return events;
        };
        let status = str_of(turn_value, "status").unwrap_or_else(|| "completed".to_owned());
        let error = turn_value
            .get("error")
            .and_then(|error| str_of(error, "message"))
            .or(turn.error);
        if status == "interrupted" {
            events.push(message(json!({
                "type": "user",
                "message": { "content": [{ "type": "text", "text": format!("{INTERRUPT_MARKER}]") }] },
            })));
        }
        let duration = turn_value.get("durationMs").and_then(Value::as_u64);
        events.push(self.result_message(&turn.user_uuid, &status, duration, error));
        // Approvals the turn left unanswered can't be answered anymore.
        for request_id in self.approvals.keys().cloned().collect::<Vec<_>>() {
            events.push(message(json!({
                "type": "control_cancel_request",
                "request_id": request_id,
            })));
        }
        self.approvals.clear();
        events.extend(self.start_next_turn());
        events
    }

    fn result_message(
        &self,
        user_uuid: &str,
        status: &str,
        duration_ms: Option<u64>,
        error: Option<String>,
    ) -> CodexEvent {
        let is_error = status == "failed";
        let mut result = json!({
            "type": "result",
            "subtype": if is_error { "error_during_execution" } else { "success" },
            "is_error": is_error,
            "duration_ms": duration_ms,
            "user_message_uuid": user_uuid,
            "session_id": self.thread_id,
            "errors": error.into_iter().filter(|_| is_error).collect::<Vec<_>>(),
        });
        if status == "interrupted" {
            result["terminal_reason"] = json!("aborted_streaming");
        }
        if let (Some(model), Some(window)) = (&self.model, self.context_window) {
            result["modelUsage"] = json!({ model.clone(): { "contextWindow": window } });
        }
        message(result)
    }

    fn on_plan_updated(&mut self, params: &Value) -> Vec<CodexEvent> {
        let todos: Vec<Value> = params
            .get("plan")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|step| {
                let content = str_of(step, "step")?;
                let status = match str_of(step, "status").as_deref() {
                    Some("completed") => "completed",
                    Some("inProgress" | "in_progress") => "in_progress",
                    _ => "pending",
                };
                Some(json!({ "content": content, "status": status, "activeForm": content }))
            })
            .collect();
        self.plan_updates += 1;
        let id = format!("codex-plan-{}", self.plan_updates);
        vec![
            self.assistant_message(
                &id,
                vec![json!({
                    "type": "tool_use",
                    "id": id,
                    "name": "TodoWrite",
                    "input": { "todos": todos },
                })],
            ),
            tool_result_message(&id, "", false, None, None),
        ]
    }

    // --- items --------------------------------------------------------------------------------

    fn on_item_started(&mut self, item: &Value) -> Vec<CodexEvent> {
        let Some(item_id) = str_of(item, "id") else {
            return Vec::new();
        };
        match str_of(item, "type").as_deref() {
            Some("agentMessage") => self.open(&item_id, StreamKind::Text),
            Some("commandExecution") => {
                let tool = command_tool(&item_id, item);
                self.start_tools(&item_id, vec![tool])
            }
            Some("fileChange") => {
                let tools = file_change_tools(&item_id, item);
                self.start_tools(&item_id, tools)
            }
            Some("mcpToolCall") => {
                let name = format!(
                    "mcp__{}__{}",
                    str_of(item, "server").unwrap_or_default(),
                    str_of(item, "tool").unwrap_or_default()
                );
                let input = item.get("arguments").cloned().unwrap_or(Value::Null);
                self.start_tools(
                    &item_id,
                    vec![ItemTool {
                        id: item_id.clone(),
                        name,
                        input,
                    }],
                )
            }
            Some("dynamicToolCall") => {
                let name = str_of(item, "tool").unwrap_or_else(|| "tool".to_owned());
                let input = item.get("arguments").cloned().unwrap_or(Value::Null);
                self.start_tools(
                    &item_id,
                    vec![ItemTool {
                        id: item_id.clone(),
                        name,
                        input,
                    }],
                )
            }
            Some("webSearch") => {
                let query = str_of(item, "query").unwrap_or_default();
                self.start_tools(
                    &item_id,
                    vec![ItemTool {
                        id: item_id.clone(),
                        name: "WebSearch".to_owned(),
                        input: json!({ "query": query }),
                    }],
                )
            }
            Some("imageView") => {
                let path = str_of(item, "path").unwrap_or_default();
                self.start_tools(
                    &item_id,
                    vec![ItemTool {
                        id: item_id.clone(),
                        name: "Read".to_owned(),
                        input: json!({ "file_path": path }),
                    }],
                )
            }
            _ => Vec::new(),
        }
    }

    fn on_item_completed(&mut self, item: &Value) -> Vec<CodexEvent> {
        let Some(item_id) = str_of(item, "id") else {
            return Vec::new();
        };
        match str_of(item, "type").as_deref() {
            Some("agentMessage") => {
                let text = str_of(item, "text").unwrap_or_default();
                let was_open = self.is_open(&item_id);
                let mut events = if was_open {
                    self.close_stream()
                } else {
                    Vec::new()
                };
                if was_open || !text.trim().is_empty() {
                    events.push(self.assistant_message(
                        &item_id,
                        vec![json!({ "type": "text", "text": text })],
                    ));
                }
                events
            }
            Some("reasoning") => {
                let summary = item
                    .get("summary")
                    .and_then(Value::as_array)
                    .map(|parts| {
                        parts
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join("\n\n")
                    })
                    .unwrap_or_default();
                let was_open = self.is_open(&item_id);
                let mut events = if was_open {
                    self.close_stream()
                } else {
                    Vec::new()
                };
                self.summarized_reasoning.retain(|id| *id != item_id);
                if was_open || !summary.trim().is_empty() {
                    events.push(self.assistant_message(
                        &item_id,
                        vec![json!({ "type": "thinking", "thinking": summary })],
                    ));
                }
                events
            }
            Some("commandExecution") => {
                self.ensure_tools(&item_id, || vec![command_tool(&item_id, item)]);
                let output = str_of(item, "aggregatedOutput").unwrap_or_default();
                let status = str_of(item, "status").unwrap_or_default();
                let exit_code = item.get("exitCode").and_then(Value::as_i64);
                let declined = status == "declined";
                let is_error =
                    declined || status == "failed" || exit_code.is_some_and(|code| code != 0);
                let text = match (declined, output.is_empty(), exit_code) {
                    (true, _, _) => "O usuário recusou este comando.".to_owned(),
                    (false, true, Some(code)) if code != 0 => format!("Exit code {code}"),
                    _ => output,
                };
                self.finish_tools(&item_id, &text, is_error, declined, |_| None)
            }
            Some("fileChange") => {
                self.ensure_tools(&item_id, || file_change_tools(&item_id, item));
                let status = str_of(item, "status").unwrap_or_default();
                let declined = status == "declined";
                let is_error = declined || status == "failed";
                let text = match status.as_str() {
                    "declined" => "O usuário recusou esta edição.",
                    "failed" => "Não foi possível aplicar a edição.",
                    _ => "",
                };
                let changes: Vec<Value> = item
                    .get("changes")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                self.finish_tools(&item_id, text, is_error, declined, |index| {
                    let change = changes.get(index)?;
                    let path = str_of(change, "path")?;
                    let diff = str_of(change, "diff").unwrap_or_default();
                    let kind = change
                        .get("kind")
                        .and_then(|kind| str_of(kind, "type"))
                        .unwrap_or_default();
                    let hunks = match kind.as_str() {
                        "add" => return None,
                        "delete" => vec![json!({
                            "lines": diff.lines().map(|line| format!("-{line}")).collect::<Vec<_>>(),
                        })],
                        _ => unified_diff_hunks(&diff),
                    };
                    (!hunks.is_empty())
                        .then(|| json!({ "filePath": path, "structuredPatch": hunks }))
                })
            }
            Some("mcpToolCall") => {
                let error = item.get("error").and_then(|error| str_of(error, "message"));
                let text = error.clone().unwrap_or_else(|| {
                    item.get("result")
                        .and_then(|result| result.get("content"))
                        .map(content_text)
                        .unwrap_or_default()
                });
                self.finish_tools(&item_id, &text, error.is_some(), false, |_| None)
            }
            Some("dynamicToolCall") => {
                let failed = item.get("success").and_then(Value::as_bool) == Some(false);
                let text = item
                    .get("contentItems")
                    .map(content_text)
                    .unwrap_or_default();
                self.finish_tools(&item_id, &text, failed, false, |_| None)
            }
            Some("webSearch" | "imageView") => {
                self.finish_tools(&item_id, "", false, false, |_| None)
            }
            Some("contextCompaction") => vec![message(json!({
                "type": "system",
                "subtype": "compact_boundary",
                "compact_metadata": { "trigger": "auto" },
            }))],
            _ => Vec::new(),
        }
    }

    fn start_tools(&mut self, item_id: &str, tools: Vec<ItemTool>) -> Vec<CodexEvent> {
        if tools.is_empty() {
            return Vec::new();
        }
        let blocks = tools
            .iter()
            .map(|tool| {
                json!({ "type": "tool_use", "id": tool.id, "name": tool.name, "input": tool.input })
            })
            .collect();
        self.item_tools.insert(item_id.to_owned(), tools);
        vec![self.assistant_message(item_id, blocks)]
    }

    /// Records an item's tools when its start was missed, so its result still has calls to land on.
    fn ensure_tools(&mut self, item_id: &str, tools: impl FnOnce() -> Vec<ItemTool>) {
        if !self.item_tools.contains_key(item_id) {
            self.item_tools.insert(item_id.to_owned(), tools());
        }
    }

    /// The results of an item's tool calls; `structured` gives each call's structured output by
    /// its position.
    fn finish_tools(
        &mut self,
        item_id: &str,
        text: &str,
        is_error: bool,
        declined: bool,
        structured: impl Fn(usize) -> Option<Value>,
    ) -> Vec<CodexEvent> {
        let Some(tools) = self.item_tools.remove(item_id) else {
            return Vec::new();
        };
        let mut events = Vec::new();
        // The start may have been missed, in which case the reducer has no call to update.
        events.push(self.assistant_message(
            item_id,
            tools
                .iter()
                .map(|tool| {
                    json!({ "type": "tool_use", "id": tool.id, "name": tool.name, "input": tool.input })
                })
                .collect(),
        ));
        for (index, tool) in tools.iter().enumerate() {
            events.push(tool_result_message(
                &tool.id,
                text,
                is_error,
                structured(index),
                declined.then_some("declined"),
            ));
        }
        events
    }

    fn assistant_message(&self, message_id: &str, content: Vec<Value>) -> CodexEvent {
        message(json!({
            "type": "assistant",
            "message": { "id": message_id, "model": self.model, "content": content },
            "session_id": self.thread_id,
        }))
    }

    // --- streaming ----------------------------------------------------------------------------

    fn is_open(&self, item_id: &str) -> bool {
        self.open_stream
            .as_ref()
            .is_some_and(|stream| stream.item_id == item_id)
    }

    /// Starts streaming the item's text or thinking, closing whatever streamed before.
    fn open(&mut self, item_id: &str, kind: StreamKind) -> Vec<CodexEvent> {
        if self.is_open(item_id) {
            return Vec::new();
        }
        let mut events = self.close_stream();
        let block = match kind {
            StreamKind::Text => json!({ "type": "text", "text": "" }),
            StreamKind::Thinking => json!({ "type": "thinking", "thinking": "" }),
        };
        events.push(stream_event(
            json!({ "type": "message_start", "message": { "id": item_id, "model": self.model } }),
        ));
        events.push(stream_event(
            json!({ "type": "content_block_start", "index": 0, "content_block": block }),
        ));
        self.open_stream = Some(OpenStream {
            item_id: item_id.to_owned(),
            kind,
        });
        events
    }

    fn stream_delta(
        &mut self,
        params: &Value,
        kind: StreamKind,
        delta_type: &str,
        key: &str,
    ) -> Vec<CodexEvent> {
        let (Some(item_id), Some(delta)) = (str_of(params, "itemId"), str_of(params, "delta"))
        else {
            return Vec::new();
        };
        let mut events = Vec::new();
        let is_open_as_kind = self
            .open_stream
            .as_ref()
            .is_some_and(|stream| stream.item_id == item_id && stream.kind == kind);
        if !is_open_as_kind {
            events.extend(self.open(&item_id, kind));
        }
        if !delta.is_empty() {
            events.push(stream_event(json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": { "type": delta_type, key: delta },
            })));
        }
        events
    }

    fn close_stream(&mut self) -> Vec<CodexEvent> {
        if self.open_stream.take().is_none() {
            return Vec::new();
        }
        vec![
            stream_event(json!({ "type": "content_block_stop", "index": 0 })),
            stream_event(json!({ "type": "message_stop" })),
        ]
    }

    /// The models a session reports through the reducer, as an `initialize` reply would.
    pub fn initialize_response(models: &[CodexModel], permission_mode: &str) -> InitializeResponse {
        InitializeResponse {
            models: models.iter().map(CodexModel::model_info).collect(),
            current_permission_mode: Some(permission_mode.to_owned()),
            ..InitializeResponse::default()
        }
    }
}

// ---------------------------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------------------------

fn str_of(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn message(value: Value) -> CodexEvent {
    CodexEvent::Message(StdoutMessage::from_value(value))
}

fn stream_event(event: Value) -> CodexEvent {
    message(json!({ "type": "stream_event", "event": event }))
}

fn tool_result_message(
    tool_use_id: &str,
    text: &str,
    is_error: bool,
    structured: Option<Value>,
    non_execution_kind: Option<&str>,
) -> CodexEvent {
    let mut frame = json!({
        "type": "user",
        "message": {
            "content": [{
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": text,
                "is_error": is_error,
            }],
        },
    });
    if let Some(structured) = structured {
        frame["tool_use_result"] = structured;
    }
    if let Some(kind) = non_execution_kind {
        frame["tool_result_meta"] = json!([{ "id": tool_use_id, "non_execution_kind": kind }]);
    }
    message(frame)
}

fn response_line(id: &Value, result: Value) -> String {
    json!({ "id": id, "result": result }).to_string()
}

fn rpc_id_text(id: &Value) -> String {
    match id {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// The text parts of MCP-style content (`[{ "type": "text", "text": … }]`).
fn content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| part.as_str().map(str::to_owned))
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// The command a shell wrapper like `/bin/zsh -lc 'ls -la'` runs.
pub fn unwrap_shell(command: &str) -> String {
    let trimmed = command.trim();
    let mut parts = trimmed.splitn(3, ' ');
    let (Some(shell), Some(flag), Some(script)) = (parts.next(), parts.next(), parts.next()) else {
        return trimmed.to_owned();
    };
    let shell_name = shell.rsplit('/').next().unwrap_or(shell);
    let is_shell = matches!(shell_name, "sh" | "bash" | "zsh" | "dash" | "fish");
    let is_command_flag = flag.starts_with('-') && flag.ends_with('c');
    if !is_shell || !is_command_flag {
        return trimmed.to_owned();
    }
    let script = script.trim();
    if let Some(inner) = script
        .strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
    {
        return inner.replace("'\\''", "'");
    }
    if let Some(inner) = script
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    {
        return inner.replace("\\\"", "\"");
    }
    script.to_owned()
}

/// A command item as the tool the timeline knows best: a single read, listing or search as Read,
/// LS or Grep, anything else as Bash.
fn command_tool(item_id: &str, item: &Value) -> ItemTool {
    let command = unwrap_shell(&str_of(item, "command").unwrap_or_default());
    let actions = item
        .get("commandActions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let (name, input) = match actions.as_slice() {
        [action] => match str_of(action, "type").as_deref() {
            Some("read") if str_of(action, "path").is_some() => (
                "Read",
                json!({ "file_path": str_of(action, "path"), "command": command }),
            ),
            Some("listFiles") => (
                "LS",
                json!({
                    "path": str_of(action, "path").or_else(|| str_of(item, "cwd")),
                    "command": command,
                }),
            ),
            Some("search") => (
                "Grep",
                json!({
                    "pattern": str_of(action, "query").unwrap_or_default(),
                    "path": str_of(action, "path"),
                    "command": command,
                }),
            ),
            _ => ("Bash", json!({ "command": command })),
        },
        _ => ("Bash", json!({ "command": command })),
    };
    ItemTool {
        id: item_id.to_owned(),
        name: name.to_owned(),
        input,
    }
}

/// A patch item as one Write (new file) or Edit call per file it touches.
fn file_change_tools(item_id: &str, item: &Value) -> Vec<ItemTool> {
    let changes = item
        .get("changes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let single = changes.len() == 1;
    changes
        .iter()
        .enumerate()
        .filter_map(|(index, change)| {
            let path = str_of(change, "path")?;
            let kind = change
                .get("kind")
                .and_then(|kind| str_of(kind, "type"))
                .unwrap_or_default();
            let id = if single {
                item_id.to_owned()
            } else {
                format!("{item_id}#{index}")
            };
            let (name, input) = if kind == "add" {
                (
                    "Write",
                    json!({
                        "file_path": path,
                        "content": str_of(change, "diff").unwrap_or_default(),
                    }),
                )
            } else {
                ("Edit", json!({ "file_path": path }))
            };
            Some(ItemTool {
                id,
                name: name.to_owned(),
                input,
            })
        })
        .collect()
}

/// The hunks of a unified diff, shaped like Claude Code's `structuredPatch` (`{ "lines": [...] }`
/// with `+`, `-` or ` ` prefixed lines).
pub fn unified_diff_hunks(diff: &str) -> Vec<Value> {
    let mut hunks: Vec<Vec<String>> = Vec::new();
    for line in diff.lines() {
        if line.starts_with("@@") {
            hunks.push(Vec::new());
            continue;
        }
        if line.starts_with("diff --git")
            || line.starts_with("index ")
            || line.starts_with("--- ")
            || line.starts_with("+++ ")
            || line.starts_with("\\ No newline")
        {
            continue;
        }
        if hunks.is_empty() {
            hunks.push(Vec::new());
        }
        if let Some(hunk) = hunks.last_mut() {
            hunk.push(line.to_owned());
        }
    }
    hunks
        .into_iter()
        .filter(|lines| !lines.is_empty())
        .map(|lines| json!({ "lines": lines }))
        .collect()
}

#[cfg(test)]
#[path = "codex_tests.rs"]
mod tests;
