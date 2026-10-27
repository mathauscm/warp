//! The normalized conversation model a chat UI renders. Live sessions (see `reducer`) and saved
//! transcripts (see `history`) both produce it, so a resumed thread looks the same as a live one.

use std::time::SystemTime;

use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum TimelineItem {
    User(UserMessage),
    Assistant(AssistantText),
    Thinking(Thinking),
    Tool(ToolCall),
    Notice(Notice),
    TurnEnd(TurnSummary),
}

impl TimelineItem {
    /// Stable across updates of the same item, so views can keep per-item state (expanded,
    /// hovered, code editors) while it streams.
    pub fn id(&self) -> &str {
        match self {
            TimelineItem::User(message) => &message.id,
            TimelineItem::Assistant(text) => &text.id,
            TimelineItem::Thinking(thinking) => &thinking.id,
            TimelineItem::Tool(tool) => &tool.id,
            TimelineItem::Notice(notice) => &notice.id,
            TimelineItem::TurnEnd(summary) => &summary.id,
        }
    }

    /// The subagent tool call this item belongs to, if it was produced inside one.
    pub fn parent_tool_use_id(&self) -> Option<&str> {
        match self {
            TimelineItem::Assistant(text) => text.parent_tool_use_id.as_deref(),
            TimelineItem::Thinking(thinking) => thinking.parent_tool_use_id.as_deref(),
            TimelineItem::Tool(tool) => tool.parent_tool_use_id.as_deref(),
            TimelineItem::User(_) | TimelineItem::Notice(_) | TimelineItem::TurnEnd(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct UserMessage {
    /// The message uuid, which is also its id in the transcript and for `rewind_files`.
    pub id: String,
    pub text: String,
    pub images: Vec<ImageAttachment>,
    /// Set when the message is a slash command, shown as a command chip instead of prose.
    pub command: Option<SlashCommandInvocation>,
    pub timestamp: Option<SystemTime>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImageAttachment {
    pub media_type: String,
    /// Where the image can be read from, when it was saved to disk.
    pub path: Option<String>,
    /// Base64 data, for images that only exist in the message.
    pub data: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SlashCommandInvocation {
    /// Without the leading `/`.
    pub name: String,
    pub args: String,
    /// What a local command printed, like `/context`'s report.
    pub output: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AssistantText {
    pub id: String,
    pub markdown: String,
    pub is_streaming: bool,
    pub parent_tool_use_id: Option<String>,
    pub timestamp: Option<SystemTime>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Thinking {
    pub id: String,
    pub text: String,
    pub is_streaming: bool,
    pub duration_ms: Option<u64>,
    pub parent_tool_use_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    /// The `tool_use` id.
    pub id: String,
    /// The tool's name as Claude calls it, like `Bash`, `Edit` or `mcp__server__tool`.
    pub name: String,
    pub input: Value,
    /// The input JSON received so far while it streams; `input` is filled once it is complete.
    pub partial_input: String,
    pub status: ToolStatus,
    pub result: Option<ToolResult>,
    pub parent_tool_use_id: Option<String>,
    pub started_at: Option<SystemTime>,
    pub finished_at: Option<SystemTime>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    /// Claude is still writing the call.
    Streaming,
    /// Waiting for the user to allow it.
    AwaitingApproval,
    Running,
    Done,
    Failed,
    Denied,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolResult {
    /// The text Claude received.
    pub text: String,
    pub is_error: bool,
    /// The tool's structured output (`tool_use_result` / `toolUseResult`), when there is one.
    pub structured: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub id: String,
    pub kind: NoticeKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum NoticeKind {
    Compacted {
        pre_tokens: Option<u64>,
        post_tokens: Option<u64>,
    },
    /// The compaction summary that replaced the earlier conversation.
    CompactSummary(String),
    Interrupted,
    Error(String),
    /// Something to show as a muted line, like a hook's or a local command's output.
    Info(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TurnSummary {
    pub id: String,
    pub duration_ms: Option<u64>,
    pub cost_usd: Option<f64>,
    pub is_error: bool,
    /// The `result` subtype, like `success` or `error_during_execution`.
    pub subtype: String,
}

/// A request from Claude that waits for the user. These are not timeline items: they sit above the
/// composer until answered.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingRequest {
    /// The control request id to answer.
    pub request_id: String,
    pub tool_use_id: Option<String>,
    pub kind: PendingRequestKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PendingRequestKind {
    Permission {
        tool_name: String,
        display_name: Option<String>,
        input: Value,
        description: Option<String>,
        blocked_path: Option<String>,
        /// Echoed back in `updatedPermissions` to allow for the rest of the session or for good.
        suggestions: Vec<Value>,
    },
    Question {
        input: Value,
        questions: Vec<Question>,
    },
    PlanApproval {
        input: Value,
        plan: String,
        plan_file_path: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Question {
    pub question: String,
    pub header: Option<String>,
    pub multi_select: bool,
    pub options: Vec<QuestionOption>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QuestionOption {
    pub label: String,
    pub description: Option<String>,
}

/// One entry of Claude's task list (TodoWrite, or the TaskCreate/TaskUpdate tools).
#[derive(Debug, Clone, PartialEq)]
pub struct TodoItem {
    pub id: Option<String>,
    pub content: String,
    pub active_form: Option<String>,
    pub status: TodoStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}
