//! Reads the Codex CLI's saved sessions (`$CODEX_HOME/sessions/YYYY/MM/DD/rollout-*-<id>.jsonl`)
//! into the same timeline a live chat builds, and lists them for the Threads panel.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Context as _;
use serde_json::{Value, json};

use crate::codex::{unified_diff_hunks, unwrap_shell};
use crate::history::{LoadedTranscript, parse_timestamp};
use crate::timeline::{
    AssistantText, Notice, NoticeKind, Thinking, TimelineItem, ToolCall, ToolResult, ToolStatus,
    TurnSummary, UserMessage,
};

/// How many lines of a session [`list_sessions`] reads looking for its first prompt.
const SUMMARY_SCAN_LINES: usize = 400;
const TITLE_MAX_CHARS: usize = 60;

/// A saved Codex session, as the Threads panel lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct CodexSessionSummary {
    pub id: String,
    pub path: PathBuf,
    pub cwd: Option<PathBuf>,
    /// The name Codex gave the thread, else its first prompt.
    pub title: Option<String>,
    pub created_at: Option<SystemTime>,
    pub updated_at: SystemTime,
}

/// Every session file under `codex_home`, newest first.
pub fn session_files(codex_home: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_jsonl(&codex_home.join("sessions"), 0, &mut files);
    files.sort_by(|left, right| right.file_name().cmp(&left.file_name()));
    files
}

fn collect_jsonl(dir: &Path, depth: usize, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            if depth < 4 {
                collect_jsonl(&path, depth + 1, files);
            }
        } else if path
            .extension()
            .is_some_and(|extension| extension == "jsonl")
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("rollout-"))
        {
            files.push(path);
        }
    }
}

/// The session file of the thread `thread_id`.
pub fn find_session(codex_home: &Path, thread_id: &str) -> Option<PathBuf> {
    let suffix = format!("-{thread_id}.jsonl");
    session_files(codex_home).into_iter().find(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(&suffix))
    })
}

/// The names Codex gave its threads (`session_index.jsonl`), by thread id. Later lines win.
pub fn thread_names(codex_home: &Path) -> HashMap<String, String> {
    let Ok(file) = File::open(codex_home.join("session_index.jsonl")) else {
        return HashMap::new();
    };
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| {
            let entry: Value = serde_json::from_str(&line).ok()?;
            let id = entry.get("id")?.as_str()?.to_owned();
            let name = entry.get("thread_name")?.as_str()?.trim().to_owned();
            (!name.is_empty()).then_some((id, name))
        })
        .collect()
}

/// The sessions under `codex_home`, newest first.
pub fn list_sessions(codex_home: &Path) -> Vec<CodexSessionSummary> {
    let names = thread_names(codex_home);
    session_files(codex_home)
        .into_iter()
        .filter_map(|path| summarize_session(&path, &names))
        .collect()
}

/// What the session file at `path` says about itself; `names` are [`thread_names`].
pub fn summarize_session(
    path: &Path,
    names: &HashMap<String, String>,
) -> Option<CodexSessionSummary> {
    let updated_at = std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()?;
    let file = File::open(path).ok()?;
    let mut id = None;
    let mut cwd = None;
    let mut created_at = None;
    let mut first_prompt = None;
    for line in BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .take(SUMMARY_SCAN_LINES)
    {
        let Ok(entry) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let payload = entry.get("payload").unwrap_or(&Value::Null);
        match entry.get("type").and_then(Value::as_str) {
            Some("session_meta") => {
                id = id.or_else(|| str_of(payload, "id"));
                cwd = cwd.or_else(|| str_of(payload, "cwd").map(PathBuf::from));
                created_at = created_at.or_else(|| {
                    str_of(payload, "timestamp").and_then(|text| parse_timestamp(&text))
                });
            }
            Some("event_msg") if first_prompt.is_none() => {
                first_prompt = user_text(payload).filter(|text| !text.trim().is_empty());
            }
            _ => {}
        }
        if id.is_some() && first_prompt.is_some() {
            break;
        }
    }
    let id = id.or_else(|| id_from_file_name(path))?;
    let title = names
        .get(&id)
        .cloned()
        .or_else(|| first_prompt.as_deref().map(title_from_prompt));
    Some(CodexSessionSummary {
        id,
        path: path.to_owned(),
        cwd,
        title,
        created_at,
        updated_at,
    })
}

/// The thread id at the end of a `rollout-<timestamp>-<id>.jsonl` name.
fn id_from_file_name(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    // Ids are UUIDs: the last five dash-separated groups.
    let parts: Vec<&str> = stem.rsplitn(6, '-').collect();
    if parts.len() < 6 {
        return None;
    }
    let mut id: Vec<&str> = parts[..5].to_vec();
    id.reverse();
    Some(id.join("-"))
}

fn title_from_prompt(prompt: &str) -> String {
    let line = prompt
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default()
        .trim();
    let mut title: String = line.chars().take(TITLE_MAX_CHARS).collect();
    if line.chars().count() > TITLE_MAX_CHARS {
        title.push('…');
    }
    title
}

/// The prompt a user message event carries, in the current (`item_completed`) or legacy
/// (`user_message`) shape.
fn user_text(payload: &Value) -> Option<String> {
    match payload.get("type").and_then(Value::as_str)? {
        "item_completed" => {
            let item = payload.get("item")?;
            (item.get("type").and_then(Value::as_str)? == "UserMessage")
                .then(|| content_text(item.get("content")?))?
        }
        "user_message" => str_of(payload, "message"),
        _ => None,
    }
}

/// Loads a session file into the timeline, with its title (from `session_index.jsonl` next to
/// `sessions/` when Codex named the thread).
pub fn load_session(path: &Path) -> anyhow::Result<LoadedTranscript> {
    let file =
        File::open(path).with_context(|| format!("Couldn't open {}", path.to_string_lossy()))?;
    let entries: Vec<Value> = BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str(&line).ok())
        .collect();
    let mut loaded = build_transcript(&entries);
    let codex_home = path
        .ancestors()
        .find(|dir| dir.file_name().is_some_and(|name| name == "sessions"))
        .and_then(Path::parent);
    if let (Some(home), Some(id)) = (codex_home, loaded.session_id.as_ref())
        && let Some(name) = thread_names(home).remove(id)
    {
        loaded.title = Some(name);
    }
    Ok(loaded)
}

/// The timeline of a session's lines.
pub fn build_transcript(entries: &[Value]) -> LoadedTranscript {
    let has_items = entries.iter().any(|entry| {
        entry.get("type").and_then(Value::as_str) == Some("event_msg")
            && entry
                .get("payload")
                .and_then(|payload| payload.get("type"))
                .and_then(Value::as_str)
                == Some("item_completed")
    });
    let mut loaded = LoadedTranscript::default();
    let mut builder = Builder::default();
    for entry in entries {
        let payload = entry.get("payload").unwrap_or(&Value::Null);
        let timestamp = entry
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_timestamp);
        match entry.get("type").and_then(Value::as_str) {
            Some("session_meta") => {
                loaded.session_id = loaded.session_id.take().or_else(|| str_of(payload, "id"));
                loaded.cwd = loaded.cwd.take().or_else(|| str_of(payload, "cwd"));
            }
            Some("turn_context") => {
                if let Some(model) = str_of(payload, "model") {
                    loaded.model = Some(model);
                }
            }
            Some("event_msg") => builder.event(payload, has_items, timestamp),
            _ => {}
        }
    }
    loaded.title = builder.first_prompt.as_deref().map(title_from_prompt);
    loaded.items = builder.items;
    loaded
}

#[derive(Default)]
struct Builder {
    items: Vec<TimelineItem>,
    first_prompt: Option<String>,
    next_id: usize,
}

impl Builder {
    fn id(&mut self, prefix: &str) -> String {
        self.next_id += 1;
        format!("codex-{prefix}-{}", self.next_id)
    }

    fn event(&mut self, payload: &Value, has_items: bool, timestamp: Option<SystemTime>) {
        let Some(kind) = payload.get("type").and_then(Value::as_str) else {
            return;
        };
        match kind {
            "item_completed" => {
                if let Some(item) = payload.get("item") {
                    self.item(item, timestamp);
                }
            }
            "task_complete" => {
                let id = str_of(payload, "turn_id").unwrap_or_else(|| self.id("turn"));
                self.items.push(TimelineItem::TurnEnd(TurnSummary {
                    id: format!("{id}:end"),
                    duration_ms: payload.get("duration_ms").and_then(Value::as_u64),
                    cost_usd: None,
                    is_error: false,
                    subtype: "success".to_owned(),
                }));
            }
            "turn_aborted" => {
                let id = self.id("interrupted");
                self.items.push(TimelineItem::Notice(Notice {
                    id,
                    kind: NoticeKind::Interrupted,
                }));
            }
            "user_message" if !has_items => {
                if let Some(text) = str_of(payload, "message") {
                    self.user(text, timestamp);
                }
            }
            "agent_message" if !has_items => {
                if let Some(text) = str_of(payload, "message") {
                    let id = self.id("assistant");
                    self.assistant(id, text, timestamp);
                }
            }
            "agent_reasoning" if !has_items => {
                if let Some(text) = str_of(payload, "text") {
                    let id = self.id("thinking");
                    self.thinking(id, text);
                }
            }
            "exec_command_end" if !has_items => {
                let id = str_of(payload, "call_id").unwrap_or_else(|| self.id("exec"));
                let command = command_text(payload.get("command"));
                let output = str_of(payload, "aggregated_output")
                    .or_else(|| str_of(payload, "stdout"))
                    .unwrap_or_default();
                let exit_code = payload.get("exit_code").and_then(Value::as_i64);
                let parsed = payload.get("parsed_cmd").cloned().unwrap_or(Value::Null);
                self.command(id, &command, &parsed, output, exit_code, "completed");
            }
            "patch_apply_end" if !has_items => {
                let id = str_of(payload, "call_id").unwrap_or_else(|| self.id("patch"));
                let success = payload
                    .get("success")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                let status = if success { "completed" } else { "failed" };
                self.file_change(id, payload.get("changes"), status);
            }
            _ => {}
        }
    }

    fn item(&mut self, item: &Value, timestamp: Option<SystemTime>) {
        let id = str_of(item, "id");
        match item.get("type").and_then(Value::as_str) {
            Some("UserMessage") => {
                if let Some(text) = item.get("content").and_then(content_text) {
                    self.user(text, timestamp);
                }
            }
            Some("AgentMessage") => {
                if let Some(text) = item.get("content").and_then(content_text) {
                    let id = id.unwrap_or_else(|| self.id("assistant"));
                    self.assistant(id, text, timestamp);
                }
            }
            Some("Reasoning") => {
                let text = ["summary_text", "summary", "raw_content", "content"]
                    .iter()
                    .find_map(|key| item.get(*key).and_then(content_text))
                    .unwrap_or_default();
                if !text.trim().is_empty() {
                    let id = id.unwrap_or_else(|| self.id("thinking"));
                    self.thinking(id, text);
                }
            }
            Some("CommandExecution") => {
                let id = id.unwrap_or_else(|| self.id("exec"));
                let command = command_text(item.get("command"));
                let output = str_of(item, "aggregated_output").unwrap_or_default();
                let exit_code = item.get("exit_code").and_then(Value::as_i64);
                let parsed = item.get("parsed_cmd").cloned().unwrap_or(Value::Null);
                let status = str_of(item, "status").unwrap_or_else(|| "completed".to_owned());
                self.command(id, &command, &parsed, output, exit_code, &status);
            }
            Some("FileChange") => {
                let id = id.unwrap_or_else(|| self.id("patch"));
                let status = str_of(item, "status").unwrap_or_else(|| "completed".to_owned());
                self.file_change(id, item.get("changes"), &status);
            }
            Some("WebSearch") => {
                let id = id.unwrap_or_else(|| self.id("search"));
                let query = str_of(item, "query").unwrap_or_default();
                self.tool(
                    id,
                    "WebSearch",
                    json!({ "query": query }),
                    String::new(),
                    false,
                );
            }
            _ => {}
        }
    }

    fn user(&mut self, text: String, timestamp: Option<SystemTime>) {
        let text = text.trim_end().to_owned();
        if text.is_empty() {
            return;
        }
        if self.first_prompt.is_none() {
            self.first_prompt = Some(text.clone());
        }
        let id = self.id("user");
        self.items.push(TimelineItem::User(UserMessage {
            id,
            text,
            images: Vec::new(),
            command: None,
            timestamp,
        }));
    }

    fn assistant(&mut self, id: String, markdown: String, timestamp: Option<SystemTime>) {
        if markdown.trim().is_empty() {
            return;
        }
        self.items.push(TimelineItem::Assistant(AssistantText {
            id,
            markdown,
            is_streaming: false,
            parent_tool_use_id: None,
            timestamp,
        }));
    }

    fn thinking(&mut self, id: String, text: String) {
        self.items.push(TimelineItem::Thinking(Thinking {
            id,
            text,
            is_streaming: false,
            duration_ms: None,
            parent_tool_use_id: None,
        }));
    }

    fn command(
        &mut self,
        id: String,
        command: &str,
        parsed: &Value,
        output: String,
        exit_code: Option<i64>,
        status: &str,
    ) {
        let actions = parsed.as_array().cloned().unwrap_or_default();
        let (name, input) = match actions.as_slice() {
            [action] => {
                let path = str_of(action, "path");
                match action.get("type").and_then(Value::as_str) {
                    Some("read") if path.is_some() => {
                        ("Read", json!({ "file_path": path, "command": command }))
                    }
                    Some("list_files" | "listFiles") => {
                        ("LS", json!({ "path": path, "command": command }))
                    }
                    Some("search") => (
                        "Grep",
                        json!({
                            "pattern": str_of(action, "query").unwrap_or_default(),
                            "path": path,
                            "command": command,
                        }),
                    ),
                    _ => ("Bash", json!({ "command": command })),
                }
            }
            _ => ("Bash", json!({ "command": command })),
        };
        let is_error =
            status == "failed" || status == "declined" || exit_code.is_some_and(|code| code != 0);
        self.tool(id, name, input, output, is_error);
        if status == "declined"
            && let Some(TimelineItem::Tool(tool)) = self.items.last_mut()
        {
            tool.status = ToolStatus::Denied;
        }
    }

    /// A patch's files, given as an array of `{ path, kind, diff }` or a map of path to change.
    fn file_change(&mut self, id: String, changes: Option<&Value>, status: &str) {
        let changes: Vec<(String, Value)> = match changes {
            Some(Value::Array(changes)) => changes
                .iter()
                .filter_map(|change| Some((str_of(change, "path")?, change.clone())))
                .collect(),
            Some(Value::Object(changes)) => changes
                .iter()
                .map(|(path, change)| (path.clone(), change.clone()))
                .collect(),
            _ => Vec::new(),
        };
        let single = changes.len() == 1;
        let is_error = status == "failed" || status == "declined";
        for (index, (path, change)) in changes.into_iter().enumerate() {
            let tool_id = if single {
                id.clone()
            } else {
                format!("{id}#{index}")
            };
            let kind = change
                .get("kind")
                .and_then(|kind| kind.get("type").or(Some(kind)))
                .and_then(Value::as_str)
                .or_else(|| change.get("type").and_then(Value::as_str))
                .or_else(|| {
                    change
                        .as_object()
                        .and_then(|object| object.keys().next().map(String::as_str))
                })
                .unwrap_or("update")
                .to_owned();
            let diff = ["diff", "unified_diff", "content"]
                .iter()
                .find_map(|key| {
                    change
                        .get(*key)
                        .or_else(|| change.get(&kind).and_then(|inner| inner.get(*key)))
                        .and_then(Value::as_str)
                })
                .unwrap_or_default()
                .to_owned();
            let (name, input, structured) = if kind == "add" {
                ("Write", json!({ "file_path": path, "content": diff }), None)
            } else {
                let hunks = unified_diff_hunks(&diff);
                (
                    "Edit",
                    json!({ "file_path": path }),
                    (!hunks.is_empty())
                        .then(|| json!({ "filePath": path, "structuredPatch": hunks })),
                )
            };
            self.tool(tool_id, name, input, String::new(), is_error);
            if let Some(TimelineItem::Tool(tool)) = self.items.last_mut()
                && let Some(result) = &mut tool.result
            {
                result.structured = structured;
            }
        }
    }

    fn tool(&mut self, id: String, name: &str, input: Value, text: String, is_error: bool) {
        self.items.push(TimelineItem::Tool(ToolCall {
            id,
            name: name.to_owned(),
            input,
            partial_input: String::new(),
            status: if is_error {
                ToolStatus::Failed
            } else {
                ToolStatus::Done
            },
            result: Some(ToolResult {
                text,
                is_error,
                structured: None,
            }),
            parent_tool_use_id: None,
            started_at: None,
            finished_at: None,
        }));
    }
}

fn str_of(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// A command given as an argv array (usually a shell wrapper) or a string.
fn command_text(command: Option<&Value>) -> String {
    match command {
        Some(Value::Array(parts)) => {
            let parts: Vec<&str> = parts.iter().filter_map(Value::as_str).collect();
            match parts.as_slice() {
                [shell, flag, script] if flag.starts_with('-') && flag.ends_with('c') => {
                    let name = shell.rsplit('/').next().unwrap_or(shell);
                    if matches!(name, "sh" | "bash" | "zsh" | "dash" | "fish") {
                        (*script).to_owned()
                    } else {
                        parts.join(" ")
                    }
                }
                _ => parts.join(" "),
            }
        }
        Some(Value::String(command)) => unwrap_shell(command),
        _ => String::new(),
    }
}

/// The text of content given as a string, an array of `{ text }` parts or of strings.
fn content_text(content: &Value) -> Option<String> {
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => {
            let texts: Vec<String> = parts
                .iter()
                .filter_map(|part| match part {
                    Value::String(text) => Some(text.clone()),
                    part => str_of(part, "text"),
                })
                .collect();
            (!texts.is_empty()).then(|| texts.join("\n\n"))
        }
        _ => None,
    }
}

#[cfg(test)]
#[path = "codex_history_tests.rs"]
mod tests;
