//! Wire types for the Claude Code CLI's `stream-json` stdio protocol: what the CLI prints on stdout,
//! the control requests it sends and answers, and builders for every line we write to its stdin.
//!
//! Parsing never fails on a well-formed JSON object: message types, subtypes and content blocks we
//! don't know (or whose shape changed in a newer CLI) come back as `Unknown`/`Other` variants that
//! keep the raw JSON.

use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value, json};

fn nullable<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// Accepts integers, floats (TypeScript numbers) and null.
fn lenient_u64<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u64>, D::Error> {
    Ok(match Option::<Value>::deserialize(deserializer)? {
        Some(Value::Number(number)) => number.as_u64().or_else(|| {
            number
                .as_f64()
                .filter(|value| *value >= 0.0)
                .map(|value| value.round() as u64)
        }),
        _ => None,
    })
}

fn lenient_i64<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<i64>, D::Error> {
    Ok(match Option::<Value>::deserialize(deserializer)? {
        Some(Value::Number(number)) => number
            .as_i64()
            .or_else(|| number.as_f64().map(|value| value.round() as i64)),
        _ => None,
    })
}

fn lenient_f64<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<f64>, D::Error> {
    Ok(match Option::<Value>::deserialize(deserializer)? {
        Some(Value::Number(number)) => number.as_f64(),
        _ => None,
    })
}

/// Accepts any JSON value, keeping it only when it is a string.
fn lenient_string<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    Ok(match Option::<Value>::deserialize(deserializer)? {
        Some(Value::String(text)) => Some(text),
        _ => None,
    })
}

/// Never fails: a value of an unexpected shape becomes `None`, so one odd field can't drop a whole
/// message.
fn lenient<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let value = Value::deserialize(deserializer)?;
    Ok(T::deserialize(&value).ok())
}

fn lenient_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned + Default,
{
    Ok(lenient(deserializer)?.unwrap_or_default())
}

fn parse<T: DeserializeOwned>(value: &Value) -> Result<T, serde_json::Error> {
    T::deserialize(value)
}

fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

// ---------------------------------------------------------------------------------------------
// stdout messages
// ---------------------------------------------------------------------------------------------

/// One line the CLI printed on stdout.
#[derive(Debug, Clone, PartialEq)]
pub enum StdoutMessage {
    System(SystemMessage),
    StreamEvent(StreamEventMessage),
    Assistant(Box<AssistantMessage>),
    User(Box<UserFrame>),
    Result(Box<ResultMessage>),
    RateLimit(RateLimitEvent),
    CommandLifecycle(CommandLifecycle),
    ControlRequest(IncomingControlRequest),
    ControlResponse(ControlResponse),
    ControlCancelRequest { request_id: String },
    KeepAlive,
    Unknown(Value),
}

impl StdoutMessage {
    /// Parses one NDJSON line. Only fails when the line is not JSON.
    pub fn parse_line(line: &str) -> Result<StdoutMessage, serde_json::Error> {
        serde_json::from_str::<Value>(line).map(StdoutMessage::from_value)
    }

    pub fn from_value(value: Value) -> StdoutMessage {
        let Some(kind) = str_field(&value, "type") else {
            return StdoutMessage::Unknown(value);
        };
        let parsed = match kind {
            "system" => {
                return SystemMessage::from_value(value)
                    .map_or_else(StdoutMessage::Unknown, StdoutMessage::System);
            }
            "stream_event" => parse(&value).map(StdoutMessage::StreamEvent),
            "assistant" => parse(&value).map(|message| StdoutMessage::Assistant(Box::new(message))),
            "user" => parse(&value).map(|message| StdoutMessage::User(Box::new(message))),
            "result" => parse(&value).map(|message| StdoutMessage::Result(Box::new(message))),
            "rate_limit_event" => parse(&value).map(StdoutMessage::RateLimit),
            "command_lifecycle" => parse(&value).map(StdoutMessage::CommandLifecycle),
            "control_request" => parse(&value).map(StdoutMessage::ControlRequest),
            "control_response" => parse(&value).map(StdoutMessage::ControlResponse),
            "control_cancel_request" => match str_field(&value, "request_id") {
                Some(request_id) => Ok(StdoutMessage::ControlCancelRequest {
                    request_id: request_id.to_owned(),
                }),
                None => return StdoutMessage::Unknown(value),
            },
            "keep_alive" => Ok(StdoutMessage::KeepAlive),
            _ => return StdoutMessage::Unknown(value),
        };
        match parsed {
            Ok(message) => message,
            Err(error) => {
                log::warn!("Claude stdout message of type {kind} has an unexpected shape: {error}");
                StdoutMessage::Unknown(value)
            }
        }
    }
}

// --- system -----------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum SystemMessage {
    Init(Box<SystemInit>),
    Status(SystemStatus),
    CompactBoundary(CompactBoundary),
    CommandsChanged {
        commands: Vec<SlashCommand>,
    },
    TaskStarted(TaskStarted),
    TaskUpdated(TaskUpdated),
    TaskProgress(TaskProgress),
    TaskNotification(TaskNotification),
    BackgroundTasksChanged {
        tasks: Vec<BackgroundTaskInfo>,
    },
    BridgeState {
        state: String,
    },
    HookStarted(HookFrame),
    HookResponse(HookFrame),
    ThinkingTokens(ThinkingTokens),
    ControlRequestProgress {
        request_id: String,
        status: Option<String>,
    },
    SessionStateChanged {
        state: String,
    },
    ApiRetry(ApiRetry),
    PermissionDenied(PermissionDeniedFrame),
    LocalCommandOutput {
        content: String,
    },
    Informational {
        content: String,
        level: Option<String>,
    },
    /// A subtype we don't model.
    Other {
        subtype: String,
        raw: Value,
    },
}

impl SystemMessage {
    /// Returns the value back when a known subtype has an unexpected shape.
    fn from_value(value: Value) -> Result<SystemMessage, Value> {
        let subtype = str_field(&value, "subtype").unwrap_or_default().to_owned();

        #[derive(Deserialize)]
        struct Commands {
            #[serde(default, deserialize_with = "nullable")]
            commands: Vec<SlashCommand>,
        }
        #[derive(Deserialize)]
        struct Tasks {
            #[serde(default, deserialize_with = "nullable")]
            tasks: Vec<BackgroundTaskInfo>,
        }
        #[derive(Deserialize)]
        struct State {
            #[serde(default, deserialize_with = "nullable")]
            state: String,
        }
        #[derive(Deserialize)]
        struct Progress {
            request_id: String,
            #[serde(default, deserialize_with = "lenient_string")]
            status: Option<String>,
        }
        #[derive(Deserialize)]
        struct Content {
            #[serde(default, deserialize_with = "nullable")]
            content: String,
            #[serde(default, deserialize_with = "lenient_string")]
            level: Option<String>,
        }

        let parsed = match subtype.as_str() {
            "init" => parse(&value).map(|init| SystemMessage::Init(Box::new(init))),
            "status" => parse(&value).map(SystemMessage::Status),
            "compact_boundary" => parse(&value).map(SystemMessage::CompactBoundary),
            "commands_changed" => {
                parse::<Commands>(&value).map(|commands| SystemMessage::CommandsChanged {
                    commands: commands.commands,
                })
            }
            "task_started" => parse(&value).map(SystemMessage::TaskStarted),
            "task_updated" => parse(&value).map(SystemMessage::TaskUpdated),
            "task_progress" => parse(&value).map(SystemMessage::TaskProgress),
            "task_notification" => parse(&value).map(SystemMessage::TaskNotification),
            "background_tasks_changed" => parse::<Tasks>(&value)
                .map(|tasks| SystemMessage::BackgroundTasksChanged { tasks: tasks.tasks }),
            "bridge_state" => parse::<State>(&value)
                .map(|state| SystemMessage::BridgeState { state: state.state }),
            "hook_started" => parse(&value).map(SystemMessage::HookStarted),
            "hook_response" => parse(&value).map(SystemMessage::HookResponse),
            "thinking_tokens" => parse(&value).map(SystemMessage::ThinkingTokens),
            "control_request_progress" => {
                parse::<Progress>(&value).map(|progress| SystemMessage::ControlRequestProgress {
                    request_id: progress.request_id,
                    status: progress.status,
                })
            }
            "session_state_changed" => parse::<State>(&value)
                .map(|state| SystemMessage::SessionStateChanged { state: state.state }),
            "api_retry" => parse(&value).map(SystemMessage::ApiRetry),
            "permission_denied" => parse(&value).map(SystemMessage::PermissionDenied),
            "local_command_output" => {
                parse::<Content>(&value).map(|content| SystemMessage::LocalCommandOutput {
                    content: content.content,
                })
            }
            "informational" => {
                parse::<Content>(&value).map(|content| SystemMessage::Informational {
                    content: content.content,
                    level: content.level,
                })
            }
            _ => {
                return Ok(SystemMessage::Other {
                    subtype,
                    raw: value,
                });
            }
        };
        parsed.map_err(|error| {
            log::warn!("Claude system/{subtype} message has an unexpected shape: {error}");
            value
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct SystemInit {
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub model: Option<String>,
    #[serde(rename = "permissionMode")]
    pub permission_mode: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub effort: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub tools: Vec<String>,
    #[serde(deserialize_with = "lenient_default")]
    pub mcp_servers: Vec<McpServerStatus>,
    #[serde(deserialize_with = "lenient_default")]
    pub slash_commands: Vec<String>,
    /// Commands that only make sense in the interactive terminal UI.
    #[serde(deserialize_with = "nullable")]
    pub terminal_slash_commands: Vec<String>,
    #[serde(deserialize_with = "nullable")]
    pub agents: Vec<String>,
    #[serde(deserialize_with = "nullable")]
    pub skills: Vec<String>,
    pub claude_code_version: Option<String>,
    pub output_style: Option<String>,
    #[serde(rename = "apiKeySource")]
    pub api_key_source: Option<String>,
    pub fast_mode_state: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct SystemStatus {
    /// `requesting`, `compacting`, or null when idle.
    #[serde(deserialize_with = "lenient_string")]
    pub status: Option<String>,
    #[serde(rename = "permissionMode", deserialize_with = "lenient_string")]
    pub permission_mode: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub compact_result: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub compact_error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct CompactBoundary {
    pub uuid: Option<String>,
    pub compact_metadata: CompactMetadata,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct CompactMetadata {
    #[serde(deserialize_with = "lenient_string")]
    pub trigger: Option<String>,
    #[serde(deserialize_with = "lenient_u64")]
    pub pre_tokens: Option<u64>,
    #[serde(deserialize_with = "lenient_u64")]
    pub post_tokens: Option<u64>,
    #[serde(deserialize_with = "lenient_u64")]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct TaskStarted {
    pub task_id: String,
    pub tool_use_id: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub description: String,
    pub subagent_type: Option<String>,
    pub task_type: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub is_backgrounded: bool,
    pub prompt: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub skip_transcript: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct TaskUpdated {
    pub task_id: String,
    pub patch: TaskPatch,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct TaskPatch {
    #[serde(deserialize_with = "lenient_string")]
    pub status: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub description: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub error: Option<String>,
    #[serde(deserialize_with = "lenient_i64")]
    pub end_time: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct TaskProgress {
    pub task_id: String,
    pub tool_use_id: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub summary: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub last_tool_name: Option<String>,
    pub usage: Option<TaskUsage>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct TaskNotification {
    pub task_id: String,
    pub tool_use_id: Option<String>,
    /// `completed`, `failed` or `stopped`.
    #[serde(deserialize_with = "nullable")]
    pub status: String,
    #[serde(deserialize_with = "lenient_string")]
    pub summary: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub output_file: Option<String>,
    pub usage: Option<TaskUsage>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct TaskUsage {
    #[serde(deserialize_with = "lenient_u64")]
    pub total_tokens: Option<u64>,
    #[serde(deserialize_with = "lenient_u64")]
    pub tool_uses: Option<u64>,
    #[serde(deserialize_with = "lenient_u64")]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct BackgroundTaskInfo {
    pub task_id: String,
    #[serde(deserialize_with = "nullable")]
    pub task_type: String,
    #[serde(deserialize_with = "nullable")]
    pub description: String,
    pub subagent_type: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub ambient: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct HookFrame {
    pub hook_id: Option<String>,
    pub hook_name: Option<String>,
    pub hook_event: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub output: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub stderr: Option<String>,
    #[serde(deserialize_with = "lenient_i64")]
    pub exit_code: Option<i64>,
    #[serde(deserialize_with = "lenient_string")]
    pub outcome: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct ThinkingTokens {
    #[serde(deserialize_with = "lenient_u64")]
    pub estimated_tokens: Option<u64>,
    #[serde(deserialize_with = "lenient_u64")]
    pub estimated_tokens_delta: Option<u64>,
    pub user_message_uuid: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct ApiRetry {
    #[serde(deserialize_with = "lenient_u64")]
    pub attempt: Option<u64>,
    #[serde(deserialize_with = "lenient_u64")]
    pub max_retries: Option<u64>,
    #[serde(deserialize_with = "lenient_u64")]
    pub retry_delay_ms: Option<u64>,
    #[serde(deserialize_with = "lenient_string")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct PermissionDeniedFrame {
    #[serde(deserialize_with = "nullable")]
    pub tool_name: String,
    #[serde(deserialize_with = "nullable")]
    pub tool_use_id: String,
    #[serde(deserialize_with = "lenient_string")]
    pub message: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub decision_reason: Option<String>,
}

// --- stream events ----------------------------------------------------------------------------

/// A raw Anthropic streaming event, sent with `--include-partial-messages`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct StreamEventMessage {
    pub event: StreamEvent,
    #[serde(default)]
    pub parent_tool_use_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub user_message_uuid: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    MessageStart {
        message: StreamMessageStart,
    },
    ContentBlockStart {
        index: usize,
        content_block: ContentBlock,
    },
    ContentBlockDelta {
        index: usize,
        delta: ContentDelta,
    },
    ContentBlockStop {
        index: usize,
    },
    MessageDelta {
        stop_reason: Option<String>,
        usage: Option<Usage>,
    },
    MessageStop,
    Unknown(Value),
}

impl<'de> Deserialize<'de> for StreamEvent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(StreamEvent::from_value(Value::deserialize(deserializer)?))
    }
}

impl StreamEvent {
    fn from_value(value: Value) -> StreamEvent {
        #[derive(Deserialize)]
        struct Start {
            message: StreamMessageStart,
        }
        #[derive(Deserialize)]
        struct BlockStart {
            index: usize,
            content_block: ContentBlock,
        }
        #[derive(Deserialize)]
        struct BlockDelta {
            index: usize,
            delta: ContentDelta,
        }
        #[derive(Deserialize)]
        struct BlockStop {
            index: usize,
        }
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct MessageDeltaBody {
            #[serde(deserialize_with = "lenient_string")]
            stop_reason: Option<String>,
        }
        #[derive(Deserialize)]
        struct MessageDelta {
            #[serde(default, deserialize_with = "lenient")]
            delta: Option<MessageDeltaBody>,
            #[serde(default, deserialize_with = "lenient")]
            usage: Option<Usage>,
        }

        let parsed = match str_field(&value, "type").unwrap_or_default() {
            "message_start" => parse::<Start>(&value).map(|start| StreamEvent::MessageStart {
                message: start.message,
            }),
            "content_block_start" => {
                parse::<BlockStart>(&value).map(|start| StreamEvent::ContentBlockStart {
                    index: start.index,
                    content_block: start.content_block,
                })
            }
            "content_block_delta" => {
                parse::<BlockDelta>(&value).map(|delta| StreamEvent::ContentBlockDelta {
                    index: delta.index,
                    delta: delta.delta,
                })
            }
            "content_block_stop" => parse::<BlockStop>(&value)
                .map(|stop| StreamEvent::ContentBlockStop { index: stop.index }),
            "message_delta" => {
                parse::<MessageDelta>(&value).map(|delta| StreamEvent::MessageDelta {
                    stop_reason: delta.delta.and_then(|body| body.stop_reason),
                    usage: delta.usage,
                })
            }
            "message_stop" => Ok(StreamEvent::MessageStop),
            _ => return StreamEvent::Unknown(value),
        };
        parsed.unwrap_or(StreamEvent::Unknown(value))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct StreamMessageStart {
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    pub model: Option<String>,
    #[serde(deserialize_with = "lenient")]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ContentDelta {
    Text(String),
    Thinking(String),
    InputJson(String),
    Signature,
    Unknown(Value),
}

impl<'de> Deserialize<'de> for ContentDelta {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let text = |key: &str| str_field(&value, key).unwrap_or_default().to_owned();
        Ok(match str_field(&value, "type").unwrap_or_default() {
            "text_delta" => ContentDelta::Text(text("text")),
            "thinking_delta" => ContentDelta::Thinking(text("thinking")),
            "input_json_delta" => ContentDelta::InputJson(text("partial_json")),
            "signature_delta" => ContentDelta::Signature,
            _ => ContentDelta::Unknown(value),
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Usage {
    #[serde(deserialize_with = "lenient_u64")]
    pub input_tokens: Option<u64>,
    #[serde(deserialize_with = "lenient_u64")]
    pub output_tokens: Option<u64>,
    #[serde(deserialize_with = "lenient_u64")]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(deserialize_with = "lenient_u64")]
    pub cache_read_input_tokens: Option<u64>,
}

impl Usage {
    /// Tokens of the prompt that was sent, which is how full the context window is.
    pub fn context_tokens(&self) -> u64 {
        self.input_tokens.unwrap_or_default()
            + self.cache_creation_input_tokens.unwrap_or_default()
            + self.cache_read_input_tokens.unwrap_or_default()
    }
}

// --- content blocks ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
    },
    RedactedThinking,
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult(ToolResultBlock),
    Image(ImageBlock),
    Other(Value),
}

impl<'de> Deserialize<'de> for ContentBlock {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(ContentBlock::from_value(Value::deserialize(deserializer)?))
    }
}

impl ContentBlock {
    fn from_value(value: Value) -> ContentBlock {
        #[derive(Deserialize)]
        struct ToolUse {
            id: String,
            name: String,
            #[serde(default)]
            input: Value,
        }

        let text = |key: &str| str_field(&value, key).unwrap_or_default().to_owned();
        match str_field(&value, "type").unwrap_or_default() {
            "text" => ContentBlock::Text { text: text("text") },
            "thinking" => ContentBlock::Thinking {
                thinking: text("thinking"),
            },
            "redacted_thinking" => ContentBlock::RedactedThinking,
            "tool_use" => match parse::<ToolUse>(&value) {
                Ok(tool) => ContentBlock::ToolUse {
                    id: tool.id,
                    name: tool.name,
                    input: tool.input,
                },
                Err(_) => ContentBlock::Other(value),
            },
            "tool_result" => match parse::<ToolResultBlock>(&value) {
                Ok(result) => ContentBlock::ToolResult(result),
                Err(_) => ContentBlock::Other(value),
            },
            "image" => {
                let source = value.get("source");
                let source_field = |key: &str| {
                    source
                        .and_then(|source| str_field(source, key))
                        .map(str::to_owned)
                };
                ContentBlock::Image(ImageBlock {
                    media_type: source_field("media_type"),
                    data: source_field("data"),
                })
            }
            _ => ContentBlock::Other(value),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct ToolResultBlock {
    pub tool_use_id: String,
    pub content: MessageContent,
    #[serde(deserialize_with = "nullable")]
    pub is_error: bool,
}

impl ToolResultBlock {
    /// The text Claude received, with non-text blocks shown as placeholders.
    pub fn text(&self) -> String {
        self.content.text()
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImageBlock {
    pub media_type: Option<String>,
    /// Base64 image data.
    pub data: Option<String>,
}

/// A message's `content`, which the CLI sends either as a plain string or as blocks.
#[derive(Debug, Clone, PartialEq)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

impl Default for MessageContent {
    fn default() -> Self {
        MessageContent::Blocks(Vec::new())
    }
}

impl<'de> Deserialize<'de> for MessageContent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match Value::deserialize(deserializer)? {
            Value::String(text) => MessageContent::Text(text),
            Value::Array(blocks) => {
                MessageContent::Blocks(blocks.into_iter().map(ContentBlock::from_value).collect())
            }
            Value::Null => MessageContent::default(),
            other => MessageContent::Blocks(vec![ContentBlock::Other(other)]),
        })
    }
}

impl MessageContent {
    pub fn blocks(&self) -> &[ContentBlock] {
        match self {
            MessageContent::Text(_) => &[],
            MessageContent::Blocks(blocks) => blocks,
        }
    }

    /// The text parts joined with newlines; images and other blocks become placeholders.
    pub fn text(&self) -> String {
        match self {
            MessageContent::Text(text) => text.clone(),
            MessageContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.clone()),
                    ContentBlock::Image(_) => Some("[image]".to_owned()),
                    ContentBlock::ToolResult(result) => Some(result.text()),
                    ContentBlock::Other(value) => match str_field(value, "type") {
                        Some("tool_reference") => str_field(value, "tool_name").map(str::to_owned),
                        _ => None,
                    },
                    ContentBlock::Thinking { .. }
                    | ContentBlock::RedactedThinking
                    | ContentBlock::ToolUse { .. } => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

// --- assistant / user / result ----------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct ApiMessage {
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    pub model: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub content: Vec<ContentBlock>,
    #[serde(deserialize_with = "lenient_string")]
    pub stop_reason: Option<String>,
    #[serde(deserialize_with = "lenient")]
    pub usage: Option<Usage>,
}

/// The model id the CLI puts on replies it produced itself, like a local slash command's output.
pub const SYNTHETIC_MODEL: &str = "<synthetic>";

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct AssistantMessage {
    pub message: ApiMessage,
    pub parent_tool_use_id: Option<String>,
    pub session_id: Option<String>,
    pub uuid: Option<String>,
    pub timestamp: Option<String>,
    pub user_message_uuid: Option<String>,
    /// Set when the turn failed, like `rate_limit` or `authentication_failed`.
    #[serde(deserialize_with = "lenient_string")]
    pub error: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub aborted: bool,
    #[serde(deserialize_with = "lenient_u64")]
    pub thinking_duration_ms: Option<u64>,
    #[serde(deserialize_with = "lenient_string")]
    pub local_command_source: Option<String>,
    #[serde(deserialize_with = "lenient")]
    pub local_command_run: Option<LocalCommandRun>,
    /// The structured twin of the `/context` report.
    #[serde(deserialize_with = "lenient")]
    pub context_usage: Option<ContextUsage>,
    /// The structured twin of the `/usage` report.
    #[serde(deserialize_with = "lenient")]
    pub usage_report: Option<UsageResponse>,
}

impl AssistantMessage {
    pub fn is_synthetic(&self) -> bool {
        self.message.model.as_deref() == Some(SYNTHETIC_MODEL)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct LocalCommandRun {
    #[serde(deserialize_with = "nullable")]
    pub command: String,
    #[serde(deserialize_with = "nullable")]
    pub args: String,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct UserFrameMessage {
    pub content: MessageContent,
}

/// A `user` message the CLI printed: tool results, replays, markers and synthetic summaries.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct UserFrame {
    pub message: UserFrameMessage,
    pub parent_tool_use_id: Option<String>,
    pub session_id: Option<String>,
    pub uuid: Option<String>,
    pub timestamp: Option<String>,
    #[serde(rename = "isReplay", deserialize_with = "nullable")]
    pub is_replay: bool,
    #[serde(rename = "isSynthetic", deserialize_with = "nullable")]
    pub is_synthetic: bool,
    /// The tool's structured output; its shape depends on the tool.
    pub tool_use_result: Option<Value>,
    #[serde(deserialize_with = "lenient_default")]
    pub tool_result_meta: Vec<ToolResultMeta>,
    pub subagent_type: Option<String>,
    pub task_description: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct ToolResultMeta {
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    /// `permission-rule` when the tool was denied instead of run.
    #[serde(deserialize_with = "lenient_string")]
    pub non_execution_kind: Option<String>,
}

/// The text of the user frame the CLI emits when a turn is interrupted.
pub const INTERRUPT_MARKER: &str = "[Request interrupted by user";
pub const LOCAL_COMMAND_STDOUT_OPEN: &str = "<local-command-stdout>";
pub const LOCAL_COMMAND_STDOUT_CLOSE: &str = "</local-command-stdout>";

/// Returns the text inside `<local-command-stdout>` tags, if `text` is such a block.
pub fn local_command_stdout(text: &str) -> Option<&str> {
    let inner = text.trim().strip_prefix(LOCAL_COMMAND_STDOUT_OPEN)?;
    Some(
        inner
            .strip_suffix(LOCAL_COMMAND_STDOUT_CLOSE)
            .unwrap_or(inner)
            .trim(),
    )
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct ResultMessage {
    /// `success`, `error_during_execution`, `error_max_turns`, ...
    #[serde(deserialize_with = "nullable")]
    pub subtype: String,
    #[serde(deserialize_with = "nullable")]
    pub is_error: bool,
    #[serde(deserialize_with = "lenient_string")]
    pub result: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub stop_reason: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub terminal_reason: Option<String>,
    #[serde(deserialize_with = "lenient_u64")]
    pub num_turns: Option<u64>,
    #[serde(deserialize_with = "lenient_u64")]
    pub duration_ms: Option<u64>,
    #[serde(deserialize_with = "lenient_u64")]
    pub duration_api_ms: Option<u64>,
    /// Cumulative for the whole process, not just this turn.
    #[serde(deserialize_with = "lenient_f64")]
    pub total_cost_usd: Option<f64>,
    #[serde(deserialize_with = "lenient")]
    pub usage: Option<Usage>,
    #[serde(rename = "modelUsage", deserialize_with = "lenient_default")]
    pub model_usage: BTreeMap<String, ModelUsage>,
    #[serde(deserialize_with = "lenient_default")]
    pub permission_denials: Vec<PermissionDenial>,
    #[serde(deserialize_with = "lenient_default")]
    pub errors: Vec<Value>,
    pub session_id: Option<String>,
    pub uuid: Option<String>,
    pub user_message_uuid: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub user_message_uuids: Vec<String>,
    /// The local slash command that produced this result, like `context`.
    #[serde(deserialize_with = "lenient_string")]
    pub local_command: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ModelUsage {
    #[serde(deserialize_with = "lenient_u64")]
    pub input_tokens: Option<u64>,
    #[serde(deserialize_with = "lenient_u64")]
    pub output_tokens: Option<u64>,
    #[serde(rename = "costUSD", deserialize_with = "lenient_f64")]
    pub cost_usd: Option<f64>,
    #[serde(deserialize_with = "lenient_u64")]
    pub context_window: Option<u64>,
    #[serde(deserialize_with = "lenient_u64")]
    pub max_output_tokens: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct PermissionDenial {
    #[serde(deserialize_with = "nullable")]
    pub tool_name: String,
    #[serde(deserialize_with = "nullable")]
    pub tool_use_id: String,
    pub tool_input: Value,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct RateLimitEvent {
    pub rate_limit_info: RateLimitInfo,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RateLimitInfo {
    #[serde(deserialize_with = "lenient_string")]
    pub status: Option<String>,
    #[serde(deserialize_with = "lenient_i64")]
    pub resets_at: Option<i64>,
    #[serde(deserialize_with = "lenient_string")]
    pub rate_limit_type: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub unified_windows: BTreeMap<String, Option<UnifiedWindow>>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UnifiedWindow {
    /// A fraction between 0 and 1.
    #[serde(deserialize_with = "lenient_f64")]
    pub utilization: Option<f64>,
    /// Unix seconds.
    #[serde(deserialize_with = "lenient_i64")]
    pub resets_at: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct CommandLifecycle {
    /// The uuid of the user message this is about.
    #[serde(deserialize_with = "nullable")]
    pub command_uuid: String,
    /// `queued`, `started` (Claude took it in, maybe into the running turn), then `completed`,
    /// `cancelled`, `discarded` or `refused`.
    #[serde(deserialize_with = "nullable")]
    pub state: String,
}

// --- control requests from the CLI ------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct IncomingControlRequest {
    pub request_id: String,
    pub request: IncomingRequest,
}

#[derive(Debug, Clone, PartialEq)]
pub enum IncomingRequest {
    CanUseTool(Box<CanUseToolRequest>),
    /// A JSON-RPC message for an in-process MCP server we registered in `initialize`.
    McpMessage {
        server_name: String,
        message: Value,
    },
    Elicitation(ElicitationRequest),
    HookCallback {
        callback_id: String,
        input: Value,
        tool_use_id: Option<String>,
    },
    Other {
        subtype: String,
        raw: Value,
    },
}

impl<'de> Deserialize<'de> for IncomingRequest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(IncomingRequest::from_value(Value::deserialize(
            deserializer,
        )?))
    }
}

impl IncomingRequest {
    fn from_value(value: Value) -> IncomingRequest {
        #[derive(Deserialize)]
        struct McpMessage {
            server_name: String,
            #[serde(default)]
            message: Value,
        }
        #[derive(Deserialize)]
        struct HookCallback {
            callback_id: String,
            #[serde(default)]
            input: Value,
            #[serde(default)]
            tool_use_id: Option<String>,
        }

        let subtype = str_field(&value, "subtype").unwrap_or_default().to_owned();
        let parsed = match subtype.as_str() {
            "can_use_tool" => {
                parse(&value).map(|request| IncomingRequest::CanUseTool(Box::new(request)))
            }
            "mcp_message" => {
                parse::<McpMessage>(&value).map(|request| IncomingRequest::McpMessage {
                    server_name: request.server_name,
                    message: request.message,
                })
            }
            "elicitation" => parse(&value).map(IncomingRequest::Elicitation),
            "hook_callback" => {
                parse::<HookCallback>(&value).map(|request| IncomingRequest::HookCallback {
                    callback_id: request.callback_id,
                    input: request.input,
                    tool_use_id: request.tool_use_id,
                })
            }
            _ => {
                return IncomingRequest::Other {
                    subtype,
                    raw: value,
                };
            }
        };
        parsed.unwrap_or(IncomingRequest::Other {
            subtype,
            raw: value,
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct CanUseToolRequest {
    pub tool_name: String,
    pub display_name: Option<String>,
    pub input: Value,
    #[serde(deserialize_with = "lenient_string")]
    pub title: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub description: Option<String>,
    /// `PermissionUpdate`s to echo in `updatedPermissions` for "don't ask again".
    #[serde(deserialize_with = "nullable")]
    pub permission_suggestions: Vec<Value>,
    #[serde(deserialize_with = "lenient_string")]
    pub blocked_path: Option<String>,
    pub tool_use_id: Option<String>,
    /// Set for tools that need the user's own input (AskUserQuestion, ExitPlanMode) rather than
    /// a yes/no.
    #[serde(deserialize_with = "nullable")]
    pub requires_user_interaction: bool,
    #[serde(deserialize_with = "lenient_string")]
    pub decision_reason: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub suppress_always_allow_rule: bool,
    /// Set when a subagent asked.
    pub agent_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct ElicitationRequest {
    #[serde(deserialize_with = "nullable")]
    pub mcp_server_name: String,
    #[serde(deserialize_with = "nullable")]
    pub message: String,
    /// `form` or `url`.
    #[serde(deserialize_with = "lenient_string")]
    pub mode: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub url: Option<String>,
    pub elicitation_id: Option<String>,
    pub requested_schema: Option<Value>,
    #[serde(deserialize_with = "lenient_string")]
    pub title: Option<String>,
}

// --- control responses to our requests --------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct ControlResponse {
    pub request_id: String,
    /// The success payload (null when the CLI sent none) or the error message.
    pub outcome: Result<Value, String>,
}

impl<'de> Deserialize<'de> for ControlResponse {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Envelope {
            response: Inner,
        }
        #[derive(Deserialize)]
        struct Inner {
            #[serde(default)]
            subtype: String,
            request_id: String,
            #[serde(default)]
            response: Value,
            #[serde(default)]
            error: Value,
        }

        let envelope = Envelope::deserialize(deserializer)?;
        let inner = envelope.response;
        let outcome = if inner.subtype == "error" {
            Err(match inner.error {
                Value::String(error) => error,
                Value::Null => "unknown error".to_owned(),
                other => other.to_string(),
            })
        } else {
            Ok(inner.response)
        };
        Ok(ControlResponse {
            request_id: inner.request_id,
            outcome,
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct InitializeResponse {
    #[serde(deserialize_with = "nullable")]
    pub commands: Vec<SlashCommand>,
    #[serde(deserialize_with = "nullable")]
    pub agents: Vec<AgentInfo>,
    #[serde(deserialize_with = "nullable")]
    pub models: Vec<ModelInfo>,
    pub account: Option<AccountInfo>,
    #[serde(deserialize_with = "lenient_u64")]
    pub pid: Option<u64>,
    #[serde(deserialize_with = "lenient_string")]
    pub current_permission_mode: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub remote_control_available: bool,
    #[serde(deserialize_with = "lenient_string")]
    pub output_style: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub available_output_styles: Vec<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub fast_mode_state: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub session_state: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SlashCommand {
    /// Without the leading `/`.
    #[serde(deserialize_with = "nullable")]
    pub name: String,
    #[serde(deserialize_with = "nullable")]
    pub description: String,
    #[serde(deserialize_with = "nullable")]
    pub argument_hint: String,
    #[serde(deserialize_with = "nullable")]
    pub aliases: Vec<String>,
    /// Claude Code's own commands, as opposed to skills and user/plugin commands.
    #[serde(deserialize_with = "nullable")]
    pub builtin: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct AgentInfo {
    #[serde(deserialize_with = "nullable")]
    pub name: String,
    #[serde(deserialize_with = "nullable")]
    pub description: String,
    #[serde(deserialize_with = "lenient_string")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ModelInfo {
    /// What to pass to `set_model` / `--model`, like `opus` or a full model id.
    #[serde(deserialize_with = "nullable")]
    pub value: String,
    #[serde(deserialize_with = "lenient_string")]
    pub resolved_model: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub display_name: String,
    #[serde(deserialize_with = "nullable")]
    pub description: String,
    #[serde(deserialize_with = "nullable")]
    pub supports_effort: bool,
    #[serde(deserialize_with = "nullable")]
    pub supported_effort_levels: Vec<String>,
    #[serde(deserialize_with = "nullable")]
    pub supports_adaptive_thinking: bool,
    #[serde(deserialize_with = "nullable")]
    pub supports_fast_mode: bool,
    #[serde(deserialize_with = "nullable")]
    pub supports_auto_mode: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AccountInfo {
    #[serde(deserialize_with = "lenient_string")]
    pub email: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub organization: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub subscription_type: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub api_provider: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct McpStatusResponse {
    #[serde(rename = "mcpServers", deserialize_with = "nullable")]
    pub mcp_servers: Vec<McpServerStatus>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct McpServerStatus {
    #[serde(deserialize_with = "nullable")]
    pub name: String,
    /// `connected`, `failed`, `needs-auth`, `pending` or `disabled`.
    #[serde(deserialize_with = "nullable")]
    pub status: String,
    pub server_info: Option<Value>,
    #[serde(deserialize_with = "lenient_string")]
    pub error: Option<String>,
    /// Includes `type` (`stdio`, `http`, `sse`, `claudeai-proxy`, `sdk`) and the URL or command.
    pub config: Option<Value>,
    #[serde(deserialize_with = "lenient_string")]
    pub scope: Option<String>,
    /// `user`, `project`, `plugin`, `claudeai`, `sdk`, ...
    #[serde(deserialize_with = "lenient_string")]
    pub source: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub tools: Vec<McpToolInfo>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct McpToolInfo {
    #[serde(deserialize_with = "nullable")]
    pub name: String,
    #[serde(deserialize_with = "lenient_string")]
    pub description: Option<String>,
    pub annotations: Option<Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct McpAuthenticateResponse {
    /// Open this in the browser.
    #[serde(deserialize_with = "lenient_string")]
    pub auth_url: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub requires_user_action: bool,
    /// When true the CLI listens on `localhost:callback_port` for the OAuth redirect.
    #[serde(deserialize_with = "nullable")]
    pub callback_expected: bool,
    #[serde(deserialize_with = "lenient_u64")]
    pub callback_port: Option<u64>,
}

/// How the context window is used (`get_context_usage`, or the `/context` command's structured
/// twin, whose keys are snake_case).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ContextUsage {
    #[serde(deserialize_with = "nullable")]
    pub categories: Vec<ContextCategory>,
    #[serde(alias = "total_tokens", deserialize_with = "lenient_u64")]
    pub total_tokens: Option<u64>,
    #[serde(deserialize_with = "lenient_u64")]
    pub max_tokens: Option<u64>,
    #[serde(alias = "raw_max_tokens", deserialize_with = "lenient_u64")]
    pub raw_max_tokens: Option<u64>,
    #[serde(deserialize_with = "lenient_f64")]
    pub percentage: Option<f64>,
    #[serde(deserialize_with = "lenient_u64")]
    pub auto_compact_threshold: Option<u64>,
    #[serde(deserialize_with = "nullable")]
    pub is_auto_compact_enabled: bool,
    #[serde(deserialize_with = "lenient_string")]
    pub model: Option<String>,
    #[serde(alias = "memory_files", deserialize_with = "nullable")]
    pub memory_files: Vec<MemoryFile>,
    #[serde(alias = "mcp_tools", deserialize_with = "nullable")]
    pub mcp_tools: Vec<ContextMcpTool>,
}

impl ContextUsage {
    /// The window size: `maxTokens` when the CLI sent it, else the raw maximum.
    pub fn window_tokens(&self) -> Option<u64> {
        self.max_tokens.or(self.raw_max_tokens)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ContextCategory {
    #[serde(deserialize_with = "nullable")]
    pub name: String,
    #[serde(deserialize_with = "lenient_u64")]
    pub tokens: Option<u64>,
    /// `used`, `deferred` or `free`.
    #[serde(deserialize_with = "lenient_string")]
    pub kind: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub is_deferred: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct MemoryFile {
    #[serde(deserialize_with = "nullable")]
    pub path: String,
    /// `User`, `Project`, ...
    #[serde(rename = "type", deserialize_with = "lenient_string")]
    pub kind: Option<String>,
    #[serde(deserialize_with = "lenient_u64")]
    pub tokens: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ContextMcpTool {
    #[serde(deserialize_with = "nullable")]
    pub name: String,
    #[serde(alias = "server_name", deserialize_with = "lenient_string")]
    pub server_name: Option<String>,
    #[serde(deserialize_with = "lenient_u64")]
    pub tokens: Option<u64>,
    #[serde(alias = "is_loaded", deserialize_with = "nullable")]
    pub is_loaded: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct UsageResponse {
    pub session: Option<UsageSession>,
    #[serde(deserialize_with = "lenient_string")]
    pub subscription_type: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub rate_limits_available: bool,
    pub rate_limits: Option<UsageRateLimits>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct UsageSession {
    #[serde(deserialize_with = "lenient_f64")]
    pub total_cost_usd: Option<f64>,
    #[serde(deserialize_with = "lenient_u64")]
    pub total_duration_ms: Option<u64>,
    #[serde(deserialize_with = "lenient_u64")]
    pub total_lines_added: Option<u64>,
    #[serde(deserialize_with = "lenient_u64")]
    pub total_lines_removed: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct UsageRateLimits {
    pub five_hour: Option<UsageWindow>,
    pub seven_day: Option<UsageWindow>,
    pub seven_day_opus: Option<UsageWindow>,
    pub seven_day_sonnet: Option<UsageWindow>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct UsageWindow {
    /// A percentage between 0 and 100.
    #[serde(deserialize_with = "lenient_f64")]
    pub utilization: Option<f64>,
    /// RFC 3339.
    #[serde(deserialize_with = "lenient_string")]
    pub resets_at: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct RemoteControlResponse {
    /// The claude.ai page that drives this session.
    #[serde(deserialize_with = "lenient_string")]
    pub session_url: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub connect_url: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub bridge_session_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RewindFilesResponse {
    #[serde(deserialize_with = "nullable")]
    pub can_rewind: bool,
    #[serde(deserialize_with = "lenient_string")]
    pub error: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub files_changed: Vec<String>,
    #[serde(deserialize_with = "lenient_u64")]
    pub insertions: Option<u64>,
    #[serde(deserialize_with = "lenient_u64")]
    pub deletions: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct FileSuggestionsResponse {
    #[serde(deserialize_with = "nullable")]
    pub suggestions: Vec<Value>,
    #[serde(deserialize_with = "lenient_string")]
    pub cwd: Option<String>,
}

impl FileSuggestionsResponse {
    /// The suggested paths, whether the CLI sent plain strings or objects.
    pub fn paths(&self) -> Vec<String> {
        self.suggestions
            .iter()
            .filter_map(|suggestion| match suggestion {
                Value::String(path) => Some(path.clone()),
                Value::Object(_) => ["path", "displayText", "name"]
                    .iter()
                    .find_map(|key| str_field(suggestion, key))
                    .map(str::to_owned),
                _ => None,
            })
            .collect()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct CancelAsyncMessageResponse {
    /// False when the message wasn't queued anymore: Claude already took it in.
    pub cancelled: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct InterruptResponse {
    #[serde(deserialize_with = "nullable")]
    pub still_queued: Vec<String>,
}

// ---------------------------------------------------------------------------------------------
// stdin lines
// ---------------------------------------------------------------------------------------------

/// An image to attach to a user message.
#[derive(Debug, Clone, PartialEq)]
pub struct OutgoingImage {
    pub media_type: String,
    pub data_base64: String,
}

/// A user message. `uuid` becomes the transcript id of the message and is echoed back as
/// `user_message_uuid` / `command_lifecycle.command_uuid`.
pub fn user_message_line(uuid: &str, text: &str, images: &[OutgoingImage]) -> String {
    let mut content: Vec<Value> = images
        .iter()
        .map(|image| {
            json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": image.media_type,
                    "data": image.data_base64,
                },
            })
        })
        .collect();
    if !text.is_empty() || content.is_empty() {
        content.push(json!({ "type": "text", "text": text }));
    }
    json!({
        "type": "user",
        "session_id": "",
        "parent_tool_use_id": null,
        "uuid": uuid,
        "message": { "role": "user", "content": content },
    })
    .to_string()
}

/// A control request we send to the CLI.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub enum ControlRequestBody {
    Initialize {
        /// Names of in-process MCP servers we host; the CLI forwards their traffic to us as
        /// `mcp_message` requests.
        #[serde(rename = "sdkMcpServers", skip_serializing_if = "Vec::is_empty")]
        sdk_mcp_servers: Vec<String>,
        #[serde(rename = "appendSystemPrompt", skip_serializing_if = "Option::is_none")]
        append_system_prompt: Option<String>,
    },
    Interrupt,
    /// `None` resets to the default model.
    SetModel {
        model: Option<String>,
    },
    SetPermissionMode {
        mode: String,
    },
    SetMaxThinkingTokens {
        max_thinking_tokens: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        thinking_display: Option<String>,
    },
    McpStatus,
    McpAuthenticate {
        #[serde(rename = "serverName")]
        server_name: String,
    },
    McpOauthCallbackUrl {
        #[serde(rename = "serverName")]
        server_name: String,
        #[serde(rename = "callbackUrl")]
        callback_url: String,
    },
    McpClearAuth {
        #[serde(rename = "serverName")]
        server_name: String,
    },
    McpToggle {
        #[serde(rename = "serverName")]
        server_name: String,
        enabled: bool,
    },
    McpReconnect {
        #[serde(rename = "serverName")]
        server_name: String,
    },
    GetContextUsage {
        /// `summary` is cheap; `full` (the default) counts tokens through the API.
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    GetUsage {
        #[serde(skip_serializing_if = "Option::is_none")]
        skip_behaviors: Option<bool>,
    },
    RemoteControl {
        enabled: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    RewindFiles {
        user_message_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        dry_run: Option<bool>,
    },
    FileSuggestions {
        query: String,
    },
    /// Takes a message the user sent while Claude was busy back out of the CLI's queue.
    CancelAsyncMessage {
        message_uuid: String,
    },
    /// Has Claude take the queued message in now instead of after the running turn.
    #[serde(rename = "interrupt")]
    SendNow {
        message_uuid: String,
        send_now: bool,
    },
}

impl ControlRequestBody {
    pub fn subtype(&self) -> &'static str {
        match self {
            ControlRequestBody::Initialize { .. } => "initialize",
            ControlRequestBody::Interrupt => "interrupt",
            ControlRequestBody::SetModel { .. } => "set_model",
            ControlRequestBody::SetPermissionMode { .. } => "set_permission_mode",
            ControlRequestBody::SetMaxThinkingTokens { .. } => "set_max_thinking_tokens",
            ControlRequestBody::McpStatus => "mcp_status",
            ControlRequestBody::McpAuthenticate { .. } => "mcp_authenticate",
            ControlRequestBody::McpOauthCallbackUrl { .. } => "mcp_oauth_callback_url",
            ControlRequestBody::McpClearAuth { .. } => "mcp_clear_auth",
            ControlRequestBody::McpToggle { .. } => "mcp_toggle",
            ControlRequestBody::McpReconnect { .. } => "mcp_reconnect",
            ControlRequestBody::GetContextUsage { .. } => "get_context_usage",
            ControlRequestBody::GetUsage { .. } => "get_usage",
            ControlRequestBody::RemoteControl { .. } => "remote_control",
            ControlRequestBody::RewindFiles { .. } => "rewind_files",
            ControlRequestBody::FileSuggestions { .. } => "file_suggestions",
            ControlRequestBody::CancelAsyncMessage { .. } => "cancel_async_message",
            ControlRequestBody::SendNow { .. } => "interrupt",
        }
    }
}

pub fn control_request_line(request_id: &str, request: &ControlRequestBody) -> String {
    json!({
        "type": "control_request",
        "request_id": request_id,
        "request": request,
    })
    .to_string()
}

/// Answers a control request from the CLI. `response` is omitted when null.
pub fn control_success_line(request_id: &str, response: Value) -> String {
    let mut inner = Map::new();
    inner.insert("subtype".to_owned(), json!("success"));
    inner.insert("request_id".to_owned(), json!(request_id));
    if !response.is_null() {
        inner.insert("response".to_owned(), response);
    }
    json!({ "type": "control_response", "response": inner }).to_string()
}

/// Rejects a control request from the CLI, like one whose subtype we don't support.
pub fn control_error_line(request_id: &str, error: &str) -> String {
    json!({
        "type": "control_response",
        "response": { "subtype": "error", "request_id": request_id, "error": error },
    })
    .to_string()
}

/// The `can_use_tool` reply that lets the tool run with `updated_input` (the original input, or
/// the user's edit of it). `updated_permissions` are echoed `permission_suggestions` for "don't
/// ask again".
pub fn permission_allow(
    updated_input: Value,
    updated_permissions: Option<Vec<Value>>,
    tool_use_id: Option<&str>,
) -> Value {
    let mut response = Map::new();
    response.insert("behavior".to_owned(), json!("allow"));
    response.insert("updatedInput".to_owned(), updated_input);
    if let Some(updated_permissions) = updated_permissions {
        response.insert(
            "updatedPermissions".to_owned(),
            Value::Array(updated_permissions),
        );
    }
    if let Some(tool_use_id) = tool_use_id {
        response.insert("toolUseID".to_owned(), json!(tool_use_id));
    }
    Value::Object(response)
}

/// The `can_use_tool` reply that refuses the tool; Claude sees `message` as the tool's error.
/// `interrupt` also stops the turn.
pub fn permission_deny(message: &str, interrupt: bool) -> Value {
    json!({ "behavior": "deny", "message": message, "interrupt": interrupt })
}

/// Answers an AskUserQuestion request. `answers` maps each question's text to the chosen label;
/// several labels of a multi-select question are joined with `", "`, and free text is passed as
/// is.
pub fn question_answers(
    input: &Value,
    answers: &[(String, String)],
    tool_use_id: Option<&str>,
) -> Value {
    let mut updated_input = match input {
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    };
    let answers: Map<String, Value> = answers
        .iter()
        .map(|(question, answer)| (question.clone(), json!(answer)))
        .collect();
    updated_input.insert("answers".to_owned(), Value::Object(answers));
    permission_allow(Value::Object(updated_input), None, tool_use_id)
}

/// Approves an ExitPlanMode request. With `accept_edits` the session also switches to
/// auto-accepting edits.
pub fn plan_approve(input: Value, accept_edits: bool, tool_use_id: Option<&str>) -> Value {
    let updated_permissions = accept_edits.then(|| {
        vec![json!({ "type": "setMode", "mode": "acceptEdits", "destination": "session" })]
    });
    permission_allow(input, updated_permissions, tool_use_id)
}

/// Keeps Claude planning, with the user's feedback as the reason.
pub fn plan_reject(feedback: &str) -> Value {
    permission_deny(feedback, false)
}

/// The reply to an `mcp_message` request for an in-process MCP server.
pub fn mcp_message_response(mcp_response: Value) -> Value {
    json!({ "mcp_response": mcp_response })
}

/// Permission modes the CLI understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionMode {
    Default,
    AcceptEdits,
    Plan,
    Auto,
    DontAsk,
    BypassPermissions,
}

impl PermissionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            PermissionMode::Default => "default",
            PermissionMode::AcceptEdits => "acceptEdits",
            PermissionMode::Plan => "plan",
            PermissionMode::Auto => "auto",
            PermissionMode::DontAsk => "dontAsk",
            PermissionMode::BypassPermissions => "bypassPermissions",
        }
    }

    pub fn parse(mode: &str) -> Option<PermissionMode> {
        [
            PermissionMode::Default,
            PermissionMode::AcceptEdits,
            PermissionMode::Plan,
            PermissionMode::Auto,
            PermissionMode::DontAsk,
            PermissionMode::BypassPermissions,
        ]
        .into_iter()
        .find(|candidate| candidate.as_str() == mode)
    }
}

#[cfg(test)]
#[path = "protocol_tests.rs"]
mod tests;
