//! Reads Claude Code's saved sessions (`~/.claude/projects/<dir>/<session-id>.jsonl`) into timeline
//! items. The reconstruction follows the Agent SDK's `getSessionMessages`: relink compactions, pick
//! the leaf, walk `parentUuid` back to the root, and re-attach parallel tool calls.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::timeline::{
    AssistantText, ImageAttachment, Notice, NoticeKind, SlashCommandInvocation, Thinking,
    TimelineItem, TodoItem, TodoStatus, ToolCall, ToolResult, ToolStatus, TurnSummary, UserMessage,
};

/// Longest encoded project directory name before the CLI shortens it and appends a hash.
const MAX_ENCODED_DIR_LEN: usize = 200;

/// The command name used for bash-mode input (`!cmd` typed in the CLI).
pub const BASH_MODE_COMMAND: &str = "!";

const INTERRUPTED_MARKER: &str = "[Request interrupted by user";
const USER_REJECTED_PREFIX: &str = "The user doesn't want to proceed with this tool use";

/// The directory name the CLI stores a project's sessions under: every character that is not an
/// ASCII letter or digit becomes `-`. `cwd` should already be canonical (on macOS `/tmp` is
/// `/private/tmp`).
pub fn encoded_project_dir(cwd: &Path) -> String {
    let path = cwd.to_string_lossy();
    // The CLI works on UTF-16 code units, so a character outside the BMP becomes two dashes.
    let units: Vec<u16> = path.encode_utf16().collect();
    let sanitized: String = units
        .iter()
        .map(|&unit| match u8::try_from(unit) {
            Ok(byte) if byte.is_ascii_alphanumeric() => char::from(byte),
            _ => '-',
        })
        .collect();
    if sanitized.len() <= MAX_ENCODED_DIR_LEN {
        return sanitized;
    }
    let hash = units.iter().fold(0i32, |hash, &unit| {
        hash.wrapping_mul(31).wrapping_add(i32::from(unit))
    });
    format!(
        "{}-{}",
        &sanitized[..MAX_ENCODED_DIR_LEN],
        to_base36(i64::from(hash).unsigned_abs())
    )
}

fn to_base36(mut value: u64) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if value == 0 {
        return "0".to_owned();
    }
    let mut digits = Vec::new();
    while value > 0 {
        digits.push(DIGITS[(value % 36) as usize]);
        value /= 36;
    }
    digits.reverse();
    String::from_utf8(digits).unwrap_or_default()
}

/// Where the CLI writes the transcript of `session_id` started in `cwd`. `claude_home` is the
/// `~/.claude` directory.
pub fn transcript_path(claude_home: &Path, cwd: &Path, session_id: &str) -> PathBuf {
    let cwd = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    claude_home
        .join("projects")
        .join(encoded_project_dir(&cwd))
        .join(format!("{session_id}.jsonl"))
}

/// Looks for `session_id`'s transcript in every project directory, for when the cwd is unknown or
/// was encoded differently. Picks the most recently modified match.
pub fn find_transcript(claude_home: &Path, session_id: &str) -> Option<PathBuf> {
    let file_name = format!("{session_id}.jsonl");
    std::fs::read_dir(claude_home.join("projects"))
        .ok()?
        .filter_map(Result::ok)
        .map(|dir| dir.path().join(&file_name))
        .filter_map(|path| {
            let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
            Some((modified, path))
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoadOptions {
    /// Also show the conversation from before each compaction. The CLI (and the SDK) only keep
    /// what came after the last one, but people expect to scroll back through it.
    pub include_pre_compaction: bool,
    /// Load `<session-id>/subagents/agent-*.jsonl` and nest their items under the tool call that
    /// started them.
    pub include_subagents: bool,
    /// Keep only the most recent turns, about this many items, for very long sessions.
    pub max_items: Option<usize>,
}

impl Default for LoadOptions {
    fn default() -> Self {
        Self {
            include_pre_compaction: true,
            include_subagents: true,
            max_items: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LoadedTranscript {
    pub items: Vec<TimelineItem>,
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    /// The custom title, else the AI title, else the legacy summary, else the first prompt.
    pub title: Option<String>,
    pub git_branch: Option<String>,
    /// The model of the last assistant message.
    pub model: Option<String>,
    pub last_effort: Option<String>,
    /// Claude's task list as of the last TodoWrite / TaskCreate / TaskUpdate call.
    pub todos: Vec<TodoItem>,
    /// The uuid of the last user message, the target for `rewind_files`.
    pub last_user_message_uuid: Option<String>,
    /// Whether older items were left out because of [`LoadOptions::max_items`].
    pub truncated: bool,
    /// Lines that were not valid JSON or not shaped like a transcript entry.
    pub skipped_lines: usize,
}

pub fn load_transcript(path: &Path) -> anyhow::Result<LoadedTranscript> {
    load_transcript_with(path, &LoadOptions::default())
}

/// Like [`load_transcript`], keeping only the last turns that add up to about `max_items` items.
pub fn load_transcript_tail(path: &Path, max_items: usize) -> anyhow::Result<LoadedTranscript> {
    load_transcript_with(
        path,
        &LoadOptions {
            max_items: Some(max_items),
            ..LoadOptions::default()
        },
    )
}

pub fn load_transcript_with(
    path: &Path,
    options: &LoadOptions,
) -> anyhow::Result<LoadedTranscript> {
    let mut file = read_transcript_file(path)?;
    let ordered = visible_entries(&file, options.include_pre_compaction, false);
    let mut converter = Converter::new(None);
    for index in ordered {
        converter.convert(&mut file.entries[index]);
    }
    converter.finish();
    let TranscriptFile {
        meta,
        skipped_lines,
        ..
    } = file;

    let Converter {
        mut items,
        model,
        last_effort,
        todos,
        last_user_uuid,
        agent_tool_ids,
        ..
    } = converter;
    if options.include_subagents {
        let subagents = load_subagents(path, agent_tool_ids);
        if !subagents.is_empty() {
            items = nest_subagent_items(items, subagents);
        }
    }

    let first_prompt = items.iter().find_map(|item| match item {
        TimelineItem::User(message) if message.command.is_none() => {
            first_line_title(&crate::inline::display_text(&message.text))
        }
        _ => None,
    });
    let truncated = options
        .max_items
        .is_some_and(|max_items| keep_tail(&mut items, max_items));
    let session_id = meta.session_id.or_else(|| {
        path.file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
    });

    Ok(LoadedTranscript {
        items,
        session_id,
        cwd: meta.cwd,
        title: meta
            .custom_title
            .or(meta.ai_title)
            .or(meta.summary)
            .or(first_prompt),
        git_branch: meta.git_branch,
        model,
        last_effort,
        todos,
        last_user_message_uuid: last_user_uuid,
        truncated,
        skipped_lines,
    })
}

fn first_line_title(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    Some(line.chars().take(200).collect())
}

/// Drops whole turns from the front until at most `max_items` remain (the last turn is always
/// kept). Returns whether anything was dropped.
fn keep_tail(items: &mut Vec<TimelineItem>, max_items: usize) -> bool {
    if items.len() <= max_items {
        return false;
    }
    let earliest = items.len() - max_items;
    let turn_start = items
        .iter()
        .enumerate()
        .skip(earliest)
        .find(|(_, item)| matches!(item, TimelineItem::User(_)))
        .map(|(index, _)| index)
        .or_else(|| {
            items
                .iter()
                .rposition(|item| matches!(item, TimelineItem::User(_)))
        });
    match turn_start {
        Some(start) if start > 0 => {
            items.drain(..start);
            true
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------------------------
// Reading

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawEntry {
    #[serde(rename = "type", deserialize_with = "lenient_string")]
    kind: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    subtype: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    uuid: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    parent_uuid: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    logical_parent_uuid: Option<String>,
    #[serde(deserialize_with = "lenient_bool")]
    is_sidechain: bool,
    #[serde(deserialize_with = "lenient_bool")]
    is_meta: bool,
    #[serde(deserialize_with = "lenient_string")]
    team_name: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    agent_id: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    timestamp: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    session_id: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    cwd: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    git_branch: Option<String>,
    message: Option<Value>,
    content: Option<Value>,
    tool_use_result: Option<Value>,
    #[serde(deserialize_with = "lenient_string")]
    tool_denial_kind: Option<String>,
    #[serde(
        rename = "sourceToolAssistantUUID",
        deserialize_with = "lenient_string"
    )]
    source_tool_assistant_uuid: Option<String>,
    #[serde(deserialize_with = "lenient_bool")]
    is_compact_summary: bool,
    #[serde(deserialize_with = "lenient_bool")]
    is_api_error_message: bool,
    origin: Option<Value>,
    #[serde(deserialize_with = "lenient_attachment")]
    attachment: Option<RawAttachment>,
    compact_metadata: Option<Value>,
    duration_ms: Option<Value>,
    per_turn_effort: Option<Value>,
    #[serde(deserialize_with = "lenient_string")]
    ai_title: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    custom_title: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    summary: Option<String>,
}

/// Only `queued_command` attachments are rendered; everything else in them is skipped unparsed.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawAttachment {
    #[serde(rename = "type", deserialize_with = "lenient_string")]
    kind: Option<String>,
    prompt: Option<Value>,
    #[serde(rename = "source_uuid", deserialize_with = "lenient_string")]
    source_uuid: Option<String>,
    #[serde(deserialize_with = "lenient_bool")]
    is_meta: bool,
    origin: Option<Value>,
}

fn lenient_string<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    Ok(match Option::<Value>::deserialize(deserializer)? {
        Some(Value::String(value)) => Some(value),
        _ => None,
    })
}

fn lenient_bool<'de, D: Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    Ok(matches!(
        Option::<Value>::deserialize(deserializer)?,
        Some(Value::Bool(true))
    ))
}

fn lenient_attachment<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<RawAttachment>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum MaybeAttachment {
        Attachment(RawAttachment),
        Other(serde::de::IgnoredAny),
    }
    Ok(
        match Option::<MaybeAttachment>::deserialize(deserializer)? {
            Some(MaybeAttachment::Attachment(attachment)) => Some(attachment),
            Some(MaybeAttachment::Other(_)) | None => None,
        },
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    User,
    Assistant,
    System,
    Attachment,
    Progress,
}

impl EntryKind {
    fn parse(kind: &str) -> Option<Self> {
        match kind {
            "user" => Some(Self::User),
            "assistant" => Some(Self::Assistant),
            "system" => Some(Self::System),
            "attachment" => Some(Self::Attachment),
            "progress" => Some(Self::Progress),
            _ => None,
        }
    }

    fn is_message(self) -> bool {
        matches!(self, Self::User | Self::Assistant)
    }
}

#[derive(Debug)]
struct Entry {
    kind: EntryKind,
    uuid: String,
    /// As written, before compaction relinking.
    parent: Option<usize>,
    logical_parent: Option<usize>,
    /// Line of the last occurrence of this uuid, which the SDK uses to rank leaves.
    last_line: usize,
    raw: RawEntry,
}

impl Entry {
    fn message_id(&self) -> Option<&str> {
        if self.kind != EntryKind::Assistant {
            return None;
        }
        self.raw.message.as_ref()?.get("id")?.as_str()
    }

    fn content_blocks(&self) -> &[Value] {
        self.raw
            .message
            .as_ref()
            .and_then(|message| message.get("content"))
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    fn block_ids<'a>(&'a self, block_type: &'a str, key: &'a str) -> impl Iterator<Item = &'a str> {
        self.content_blocks().iter().filter_map(move |block| {
            if block.get("type")?.as_str()? != block_type {
                return None;
            }
            block.get(key)?.as_str()
        })
    }

    fn tool_use_ids(&self) -> impl Iterator<Item = &str> {
        self.block_ids("tool_use", "id")
    }

    fn tool_result_ids(&self) -> impl Iterator<Item = &str> {
        self.block_ids("tool_result", "tool_use_id")
    }

    fn is_tool_result(&self, parent: Option<usize>) -> bool {
        self.kind == EntryKind::User && parent.is_some() && self.tool_result_ids().next().is_some()
    }

    fn is_team(&self) -> bool {
        self.raw
            .team_name
            .as_deref()
            .is_some_and(|name| !name.is_empty())
    }

    fn attachment_kind(&self) -> Option<&str> {
        self.raw.attachment.as_ref()?.kind.as_deref()
    }

    fn is_queued_command(&self) -> bool {
        self.kind == EntryKind::Attachment && self.attachment_kind() == Some("queued_command")
    }

    fn timestamp(&self) -> Option<SystemTime> {
        parse_timestamp(self.raw.timestamp.as_deref()?)
    }
}

#[derive(Debug, Default)]
struct FileMeta {
    session_id: Option<String>,
    cwd: Option<String>,
    git_branch: Option<String>,
    ai_title: Option<String>,
    custom_title: Option<String>,
    summary: Option<String>,
}

#[derive(Debug, Default)]
struct TranscriptFile {
    entries: Vec<Entry>,
    by_uuid: HashMap<String, usize>,
    meta: FileMeta,
    skipped_lines: usize,
}

impl TranscriptFile {
    fn index_of(&self, uuid: Option<&str>) -> Option<usize> {
        self.by_uuid.get(uuid?).copied()
    }
}

/// Entry types that never take part in the conversation, skipped before parsing because some
/// (file history snapshots) are large.
const SKIPPED_ENTRY_TYPES: &[&str] = &[
    "file-history-snapshot",
    "file-history-delta",
    "queue-operation",
    "atis-latch",
    "attribution-snapshot",
    "cost-state",
    "pr-link",
    "bridge-session",
    "frame-link",
    "mode",
    "permission-mode",
    "api-request",
    "api-request-blob",
    "api-request-shape",
];

fn is_skipped_line(line: &[u8]) -> bool {
    const PREFIX: &[u8] = b"{\"type\":\"";
    let Some(rest) = line.strip_prefix(PREFIX) else {
        return false;
    };
    let Some(end) = rest.iter().position(|&byte| byte == b'"') else {
        return false;
    };
    SKIPPED_ENTRY_TYPES
        .iter()
        .any(|kind| kind.as_bytes() == &rest[..end])
}

fn read_transcript_file(path: &Path) -> anyhow::Result<TranscriptFile> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut reader = BufReader::with_capacity(1 << 16, file);
    let mut transcript = TranscriptFile::default();
    let mut parent_uuids: Vec<(Option<String>, Option<String>)> = Vec::new();
    let mut line = Vec::new();
    let mut line_number = 0;
    loop {
        line.clear();
        let read = reader
            .read_until(b'\n', &mut line)
            .with_context(|| format!("reading {}", path.display()))?;
        if read == 0 {
            break;
        }
        line_number += 1;
        let trimmed = line.trim_ascii();
        if trimmed.is_empty() || is_skipped_line(trimmed) {
            continue;
        }
        let Ok(raw) = serde_json::from_slice::<RawEntry>(trimmed) else {
            transcript.skipped_lines += 1;
            continue;
        };
        absorb_meta(&mut transcript.meta, &raw);
        let Some(kind) = raw.kind.as_deref().and_then(EntryKind::parse) else {
            continue;
        };
        let Some(uuid) = raw.uuid.clone() else {
            continue;
        };
        let parents = (raw.parent_uuid.clone(), raw.logical_parent_uuid.clone());
        match transcript.by_uuid.get(&uuid) {
            Some(&existing) => {
                let entry = &mut transcript.entries[existing];
                entry.kind = kind;
                entry.last_line = line_number;
                entry.raw = raw;
                parent_uuids[existing] = parents;
            }
            None => {
                transcript
                    .by_uuid
                    .insert(uuid.clone(), transcript.entries.len());
                transcript.entries.push(Entry {
                    kind,
                    uuid,
                    parent: None,
                    logical_parent: None,
                    last_line: line_number,
                    raw,
                });
                parent_uuids.push(parents);
            }
        }
    }
    for (index, (parent, logical_parent)) in parent_uuids.into_iter().enumerate() {
        let parent = transcript.index_of(parent.as_deref());
        let logical_parent = transcript.index_of(logical_parent.as_deref());
        let entry = &mut transcript.entries[index];
        entry.parent = parent;
        entry.logical_parent = logical_parent;
    }
    Ok(transcript)
}

fn absorb_meta(meta: &mut FileMeta, raw: &RawEntry) {
    match raw.kind.as_deref() {
        Some("ai-title") => {
            if let Some(title) = non_empty(raw.ai_title.as_deref()) {
                meta.ai_title = Some(title.to_owned());
            }
        }
        Some("custom-title") => {
            if let Some(title) = non_empty(raw.custom_title.as_deref()) {
                meta.custom_title = Some(title.to_owned());
            }
        }
        Some("summary") => {
            if let Some(summary) = non_empty(raw.summary.as_deref()) {
                meta.summary = Some(summary.to_owned());
            }
        }
        _ => {}
    }
    if raw.is_sidechain {
        return;
    }
    if meta.session_id.is_none() {
        meta.session_id.clone_from(&raw.session_id);
    }
    if raw.cwd.is_some() {
        meta.cwd.clone_from(&raw.cwd);
    }
    if raw.git_branch.is_some() {
        meta.git_branch.clone_from(&raw.git_branch);
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

// ---------------------------------------------------------------------------------------------
// Reconstruction

fn is_compact_boundary(entry: &Entry) -> bool {
    entry.kind == EntryKind::System && entry.raw.subtype.as_deref() == Some("compact_boundary")
}

/// The entries to render, in chronological order.
fn visible_entries(
    file: &TranscriptFile,
    include_pre_compaction: bool,
    is_subagent: bool,
) -> Vec<usize> {
    let entries = &file.entries;
    let parents = relinked_parents(file);
    let Some(leaf) = pick_leaf(entries, &parents, is_subagent) else {
        return Vec::new();
    };

    let mut selected = vec![false; entries.len()];
    let mut chain = Vec::new();
    let mut current = Some(leaf);
    while let Some(index) = current {
        if selected[index] {
            break;
        }
        selected[index] = true;
        chain.push(index);
        current = parents[index];
    }
    if include_pre_compaction {
        extend_before_compactions(entries, &parents, &mut selected, &mut chain);
    }
    reattach_siblings(file, &parents, &chain, &mut selected);
    for index in trailing_queued_commands(entries, &parents, leaf, &selected) {
        selected[index] = true;
    }

    // Entries are stored in first-occurrence order, which is chronological.
    (0..entries.len())
        .filter(|&index| selected[index])
        .filter(|&index| !is_redundant_queued_command(file, index, &selected))
        .collect()
}

/// A queued command whose prompt was later written as its own user entry.
fn is_redundant_queued_command(file: &TranscriptFile, index: usize, selected: &[bool]) -> bool {
    let entry = &file.entries[index];
    if !entry.is_queued_command() {
        return false;
    }
    let source = entry
        .raw
        .attachment
        .as_ref()
        .and_then(|attachment| attachment.source_uuid.as_deref());
    match file.index_of(source) {
        Some(source) => source != index && selected[source],
        None => false,
    }
}

/// Applies each compaction's `preservedMessages` / `preservedSegment`, which moves the messages
/// the compaction kept verbatim after its summary.
fn relinked_parents(file: &TranscriptFile) -> Vec<Option<usize>> {
    let entries = &file.entries;
    let mut parents: Vec<Option<usize>> = entries.iter().map(|entry| entry.parent).collect();
    let uuid_at = |value: &Value, key: &str| file.index_of(value.get(key).and_then(Value::as_str));
    for boundary in entries.iter().filter(|entry| is_compact_boundary(entry)) {
        let Some(metadata) = boundary.raw.compact_metadata.as_ref() else {
            continue;
        };
        if let Some(preserved) = metadata.get("preservedMessages") {
            let uuids: Option<Vec<usize>> = preserved
                .get("uuids")
                .and_then(Value::as_array)
                .and_then(|uuids| {
                    uuids
                        .iter()
                        .map(|uuid| file.index_of(uuid.as_str()))
                        .collect()
                });
            let Some(uuids) = uuids.filter(|uuids| !uuids.is_empty()) else {
                continue;
            };
            let anchor = uuid_at(preserved, "anchorUuid");
            let mut previous = anchor;
            for &index in &uuids {
                parents[index] = previous;
                previous = Some(index);
            }
            let (first, last) = (uuids[0], uuids[uuids.len() - 1]);
            if let Some(anchor) = anchor {
                for (index, parent) in parents.iter_mut().enumerate() {
                    if *parent == Some(anchor) && index != first {
                        *parent = Some(last);
                    }
                }
            }
        } else if let Some(segment) = metadata.get("preservedSegment") {
            let head = uuid_at(segment, "headUuid");
            let anchor = uuid_at(segment, "anchorUuid");
            let tail = uuid_at(segment, "tailUuid");
            if let Some(head) = head {
                parents[head] = anchor;
            }
            if let Some(anchor) = anchor {
                for (index, parent) in parents.iter_mut().enumerate() {
                    if *parent == Some(anchor) && Some(index) != head {
                        *parent = tail;
                    }
                }
            }
        }
    }
    parents
}

/// The conversation's current end: the latest entry nobody continues from, walked up to the
/// nearest user or assistant message. Older leaves are abandoned branches (rewinds, edits).
fn pick_leaf(entries: &[Entry], parents: &[Option<usize>], allow_sidechain: bool) -> Option<usize> {
    let is_candidate = |entry: &Entry| {
        (allow_sidechain || !entry.raw.is_sidechain)
            && !entry.is_team()
            && entry.kind != EntryKind::Progress
            && entry.attachment_kind() != Some("fork_briefing")
    };
    let mut has_child = vec![false; entries.len()];
    let mut has_candidate_child = vec![false; entries.len()];
    for (entry, parent) in entries.iter().zip(parents) {
        if let Some(parent) = *parent {
            has_child[parent] = true;
            if is_candidate(entry) {
                has_candidate_child[parent] = true;
            }
        }
    }

    let mut candidates: Vec<usize> = (0..entries.len())
        .filter(|&index| is_candidate(&entries[index]) && !has_candidate_child[index])
        .collect();
    candidates.sort_unstable_by_key(|&index| std::cmp::Reverse(entries[index].last_line));
    let mut dead = vec![false; entries.len()];
    let mut walked_in_round = vec![usize::MAX; entries.len()];
    for (round, &candidate) in candidates.iter().enumerate() {
        let mut walked = Vec::new();
        let mut current = Some(candidate);
        while let Some(index) = current {
            if dead[index] || walked_in_round[index] == round {
                break;
            }
            if entries[index].kind.is_message() {
                return Some(index);
            }
            walked_in_round[index] = round;
            walked.push(index);
            current = parents[index];
        }
        for index in walked {
            dead[index] = true;
        }
    }

    let mut found = Vec::new();
    for leaf in (0..entries.len()).filter(|&index| !has_child[index]) {
        let mut seen = HashSet::new();
        let mut current = Some(leaf);
        while let Some(index) = current {
            if !seen.insert(index) {
                break;
            }
            if entries[index].kind.is_message() {
                found.push(index);
                break;
            }
            current = parents[index];
        }
    }
    let latest = |indices: &mut dyn Iterator<Item = usize>| {
        indices.max_by_key(|&index| entries[index].last_line)
    };
    latest(&mut found.iter().copied().filter(|&index| {
        let entry = &entries[index];
        !entry.raw.is_sidechain && !entry.is_team() && !entry.raw.is_meta
    }))
    .or_else(|| latest(&mut found.iter().copied()))
}

/// Continues the history past compaction boundaries (whose `parentUuid` is null) through their
/// `logicalParentUuid`, following the parents as written.
fn extend_before_compactions(
    entries: &[Entry],
    parents: &[Option<usize>],
    selected: &mut [bool],
    chain: &mut Vec<usize>,
) {
    let mut visited = vec![false; entries.len()];
    let Some(mut root) = chain.last().copied() else {
        return;
    };
    loop {
        let entry = &entries[root];
        if !is_compact_boundary(entry) || parents[root].is_some() {
            return;
        }
        let Some(logical_parent) = entry.logical_parent else {
            return;
        };
        let mut current = Some(logical_parent);
        let mut last = None;
        while let Some(index) = current {
            if visited[index] {
                break;
            }
            visited[index] = true;
            if !selected[index] {
                selected[index] = true;
                chain.push(index);
            }
            last = Some(index);
            current = entries[index].parent;
        }
        match last {
            Some(last) if last != root => root = last,
            _ => return,
        }
    }
}

/// Parallel tool calls can branch: each `tool_use` of one API message is its own entry, and the
/// leaf walk only passes through one of them. Adds the sibling entries of every assistant message
/// on the chain and the tool results that answer them (the SDK's `dxe`).
fn reattach_siblings(
    file: &TranscriptFile,
    parents: &[Option<usize>],
    chain: &[usize],
    selected: &mut [bool],
) {
    let entries = &file.entries;
    let chain_assistants: Vec<usize> = chain
        .iter()
        .copied()
        .filter(|&index| entries[index].kind == EntryKind::Assistant)
        .collect();
    if chain_assistants.is_empty() {
        return;
    }

    let mut by_message: HashMap<&str, Vec<usize>> = HashMap::new();
    // `None` when tool_use ids collide across different messages.
    let mut tool_owner: HashMap<&str, Option<usize>> = HashMap::new();
    let mut result_entries = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        if let Some(message_id) = entry.message_id() {
            by_message.entry(message_id).or_default().push(index);
            for tool_id in entry.tool_use_ids() {
                let owner = match tool_owner.get(tool_id) {
                    None => Some(index),
                    Some(Some(previous)) if entries[*previous].message_id() == Some(message_id) => {
                        Some(index)
                    }
                    Some(_) => None,
                };
                tool_owner.insert(tool_id, owner);
            }
        } else if entry.is_tool_result(parents[index]) {
            result_entries.push(index);
        }
    }

    let same_side = |a: &Entry, b: &Entry| {
        a.raw.is_sidechain == b.raw.is_sidechain && a.raw.agent_id == b.raw.agent_id
    };
    let mut attached: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut attached_pairs = HashSet::new();
    let mut attach = |key: usize, result: usize| {
        if attached_pairs.insert((key, result)) {
            attached.entry(key).or_default().push(result);
        }
    };
    for &result in &result_entries {
        let entry = &entries[result];
        let parent = parents[result];
        if let Some(parent) = parent {
            attach(parent, result);
        }
        let parent_uuid = parent.map(|parent| entries[parent].uuid.as_str());
        let source = entry.raw.source_tool_assistant_uuid.as_deref();
        if source.is_some()
            && source != parent_uuid
            && let Some(source) = file.index_of(source)
            && same_side(entry, &entries[source])
        {
            attach(source, result);
        }
        for tool_id in entry.tool_result_ids() {
            if let Some(Some(owner)) = tool_owner.get(tool_id)
                && same_side(entry, &entries[*owner])
            {
                attach(*owner, result);
            }
        }
    }

    let answered: HashSet<&str> = chain
        .iter()
        .flat_map(|&index| entries[index].tool_result_ids())
        .collect();
    let mut processed = HashSet::new();
    for &assistant in &chain_assistants {
        let Some(message_id) = entries[assistant].message_id() else {
            continue;
        };
        if !processed.insert(message_id) {
            continue;
        }
        let siblings = by_message
            .get(message_id)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let sibling_set: HashSet<usize> = siblings.iter().copied().collect();
        let missing: Vec<usize> = siblings
            .iter()
            .copied()
            .filter(|&index| !selected[index])
            .collect();
        let mut results = Vec::new();
        let mut other_results = Vec::new();
        let mut seen = HashSet::new();
        for sibling in siblings {
            for &result in attached.get(sibling).map(Vec::as_slice).unwrap_or_default() {
                if selected[result] || !seen.insert(result) {
                    continue;
                }
                if parents[result].is_some_and(|parent| sibling_set.contains(&parent)) {
                    results.push(result);
                } else {
                    other_results.push(result);
                }
            }
        }
        if !other_results.is_empty() {
            let mut known: HashSet<&str> = answered.clone();
            for &result in &results {
                known.extend(entries[result].tool_result_ids());
            }
            other_results.sort_unstable();
            for result in other_results {
                let answers_sibling = entries[result].tool_result_ids().any(|tool_id| {
                    !known.contains(tool_id)
                        && matches!(tool_owner.get(tool_id), Some(Some(owner)) if sibling_set.contains(owner))
                });
                if answers_sibling {
                    known.extend(entries[result].tool_result_ids());
                    results.push(result);
                }
            }
        }
        for index in missing.into_iter().chain(results) {
            selected[index] = true;
        }
    }
}

/// Messages the user queued while Claude was working on the last turn, which hang off the leaf
/// through attachments and system entries (the SDK's `_xe`), plus the last turn's duration.
fn trailing_queued_commands(
    entries: &[Entry],
    parents: &[Option<usize>],
    leaf: usize,
    selected: &[bool],
) -> Vec<usize> {
    let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
    for (index, entry) in entries.iter().enumerate() {
        if let Some(parent) = parents[index]
            && !entry.kind.is_message()
        {
            children.entry(parent).or_default().push(index);
        }
    }
    // Queued commands that lead to an abandoned message were answered on another branch.
    let mut blocked = vec![false; entries.len()];
    for (index, entry) in entries.iter().enumerate() {
        if !entry.kind.is_message() || entry.raw.is_sidechain || entry.is_team() || selected[index]
        {
            continue;
        }
        let mut current = parents[index];
        while let Some(ancestor) = current {
            if selected[ancestor] || blocked[ancestor] {
                break;
            }
            blocked[ancestor] = true;
            current = parents[ancestor];
        }
    }

    let mut visited = vec![false; entries.len()];
    let mut found = Vec::new();
    let mut stack = vec![leaf];
    while let Some(index) = stack.pop() {
        if index != leaf {
            if selected[index] || visited[index] {
                continue;
            }
            visited[index] = true;
            let entry = &entries[index];
            let is_turn_duration = entry.kind == EntryKind::System
                && entry.raw.subtype.as_deref() == Some("turn_duration");
            if (entry.is_queued_command() && !blocked[index]) || is_turn_duration {
                found.push(index);
            }
        }
        if let Some(children) = children.get(&index) {
            let mut children = children.clone();
            children.sort_by(|&a, &b| entries[a].raw.timestamp.cmp(&entries[b].raw.timestamp));
            stack.extend(children.into_iter().rev());
        }
    }
    found
}

// ---------------------------------------------------------------------------------------------
// Conversion into timeline items

#[derive(Debug, Default)]
struct Turn {
    started_at: Option<SystemTime>,
    last_activity_at: Option<SystemTime>,
    /// The uuid the turn's `TurnEnd` id derives from.
    anchor: Option<String>,
    /// Whether Claude did anything, so a `TurnEnd` is due.
    active: bool,
    interrupted: bool,
    failed: bool,
    duration_ms: Option<u64>,
    running_tools: Vec<usize>,
}

struct Converter {
    /// Set when converting a subagent's transcript: the tool call that started it.
    subagent_parent: Option<String>,
    items: Vec<TimelineItem>,
    tool_index: HashMap<String, usize>,
    /// The message id of the last item when it is assistant text, so the following text block of
    /// the same message merges into it.
    open_text_message: Option<String>,
    turn: Turn,
    model: Option<String>,
    last_effort: Option<String>,
    last_user_uuid: Option<String>,
    todos: Vec<TodoItem>,
    /// TaskCreate calls waiting for their result to learn the task id, by tool_use id.
    pending_task_creates: HashMap<String, String>,
    /// Agent/Task tool calls by the `agentId` in their result, to find subagent transcripts.
    agent_tool_ids: HashMap<String, String>,
}

impl Converter {
    fn new(subagent_parent: Option<String>) -> Self {
        Self {
            subagent_parent,
            items: Vec::new(),
            tool_index: HashMap::new(),
            open_text_message: None,
            turn: Turn::default(),
            model: None,
            last_effort: None,
            last_user_uuid: None,
            todos: Vec::new(),
            pending_task_creates: HashMap::new(),
            agent_tool_ids: HashMap::new(),
        }
    }

    fn is_subagent(&self) -> bool {
        self.subagent_parent.is_some()
    }

    fn push(&mut self, item: TimelineItem) {
        self.open_text_message = None;
        self.items.push(item);
    }

    fn push_notice(&mut self, id: &str, kind: NoticeKind) {
        self.push(TimelineItem::Notice(Notice {
            id: id.to_owned(),
            kind,
        }));
    }

    fn is_visible(&self, entry: &Entry) -> bool {
        if entry.raw.is_meta && !is_channel_origin(entry.raw.origin.as_ref()) {
            return false;
        }
        !entry.is_team() && (!entry.raw.is_sidechain || self.is_subagent())
    }

    fn convert(&mut self, entry: &mut Entry) {
        match entry.kind {
            EntryKind::Assistant => {
                if self.is_visible(entry) {
                    self.assistant(entry);
                }
            }
            EntryKind::User => {
                if entry.raw.is_meta {
                    self.meta_user(entry);
                }
                if self.is_visible(entry) {
                    self.user(entry);
                }
            }
            EntryKind::System => {
                if !entry.raw.is_sidechain || self.is_subagent() {
                    self.system(entry);
                }
            }
            EntryKind::Attachment => {
                if entry.is_queued_command() {
                    self.queued_command(entry);
                }
            }
            EntryKind::Progress => {}
        }
    }

    fn touch(&mut self, entry: &Entry) {
        let timestamp = entry.timestamp();
        let turn = &mut self.turn;
        turn.active = true;
        if turn.started_at.is_none() {
            turn.started_at = timestamp;
        }
        if timestamp.is_some() {
            turn.last_activity_at = timestamp;
        }
        turn.anchor = Some(entry.uuid.clone());
    }

    fn begin_turn(&mut self, entry: &Entry) {
        self.close_turn();
        self.turn = Turn {
            started_at: entry.timestamp(),
            anchor: Some(entry.uuid.clone()),
            ..Turn::default()
        };
    }

    fn close_turn(&mut self) {
        let turn = std::mem::take(&mut self.turn);
        if !turn.active || self.is_subagent() {
            return;
        }
        for index in turn.running_tools {
            if let Some(TimelineItem::Tool(tool)) = self.items.get_mut(index)
                && tool.status == ToolStatus::Running
            {
                tool.status = ToolStatus::Interrupted;
            }
        }
        let duration_ms = turn.duration_ms.or_else(|| {
            let elapsed = turn
                .last_activity_at?
                .duration_since(turn.started_at?)
                .ok()?;
            u64::try_from(elapsed.as_millis()).ok()
        });
        let is_error = turn.interrupted || turn.failed;
        let anchor = turn.anchor.unwrap_or_default();
        self.push(TimelineItem::TurnEnd(TurnSummary {
            id: format!("{anchor}:turn-end"),
            duration_ms,
            cost_usd: None,
            is_error,
            subtype: if is_error {
                "error_during_execution"
            } else {
                "success"
            }
            .to_owned(),
        }));
    }

    /// Ends the last turn at the end of the file, unless a tool is still waiting for its result
    /// (the session may still be running).
    fn finish(&mut self) {
        if self.turn.running_tools.is_empty() {
            self.close_turn();
        }
    }

    fn assistant(&mut self, entry: &mut Entry) {
        let Some(mut message) = entry.raw.message.take() else {
            return;
        };
        if let Some(model) = message.get("model").and_then(Value::as_str)
            && model != "<synthetic>"
            && !self.is_subagent()
        {
            self.model = Some(model.to_owned());
        }
        if let Some(effort) = entry.raw.per_turn_effort.as_ref().and_then(Value::as_str)
            && !self.is_subagent()
        {
            self.last_effort = Some(effort.to_owned());
        }
        if entry.raw.is_api_error_message {
            let text = content_text(message.get("content"));
            self.push_notice(&entry.uuid, NoticeKind::Error(text));
            self.turn.failed = true;
            self.touch(entry);
            return;
        }

        let message_id = message
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or(&entry.uuid)
            .to_owned();
        let timestamp = entry.timestamp();
        let blocks = match message.get_mut("content").map(Value::take) {
            Some(Value::Array(blocks)) => blocks,
            Some(Value::String(text)) => vec![serde_json::json!({"type": "text", "text": text})],
            _ => Vec::new(),
        };
        let multiple = blocks.len() > 1;
        for (block_index, mut block) in blocks.into_iter().enumerate() {
            let id = if multiple {
                format!("{}:{block_index}", entry.uuid)
            } else {
                entry.uuid.clone()
            };
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    let text = take_string(&mut block, "text");
                    if !text.trim().is_empty() {
                        self.assistant_text(id, &message_id, text, timestamp);
                    }
                }
                Some("thinking") => {
                    let text = take_string(&mut block, "thinking");
                    if !text.trim().is_empty() {
                        self.push(TimelineItem::Thinking(Thinking {
                            id,
                            text,
                            is_streaming: false,
                            duration_ms: None,
                            parent_tool_use_id: self.subagent_parent.clone(),
                        }));
                    }
                }
                Some("tool_use") => self.tool_use(&mut block, timestamp, ToolStatus::Running),
                Some("server_tool_use") => self.tool_use(&mut block, timestamp, ToolStatus::Done),
                _ => {}
            }
        }
        self.touch(entry);
    }

    fn assistant_text(
        &mut self,
        id: String,
        message_id: &str,
        text: String,
        timestamp: Option<SystemTime>,
    ) {
        if self.open_text_message.as_deref() == Some(message_id)
            && let Some(TimelineItem::Assistant(previous)) = self.items.last_mut()
        {
            previous.markdown.push_str("\n\n");
            previous.markdown.push_str(&text);
            return;
        }
        self.push(TimelineItem::Assistant(AssistantText {
            id,
            markdown: text,
            is_streaming: false,
            parent_tool_use_id: self.subagent_parent.clone(),
            timestamp,
        }));
        self.open_text_message = Some(message_id.to_owned());
    }

    fn tool_use(&mut self, block: &mut Value, timestamp: Option<SystemTime>, status: ToolStatus) {
        let id = take_string(block, "id");
        if id.is_empty() || self.tool_index.contains_key(&id) {
            return;
        }
        let name = take_string(block, "name");
        let input = block
            .get_mut("input")
            .map(Value::take)
            .unwrap_or(Value::Null);
        self.track_todos(&id, &name, &input);
        let index = self.items.len();
        self.tool_index.insert(id.clone(), index);
        if status == ToolStatus::Running {
            self.turn.running_tools.push(index);
        }
        self.push(TimelineItem::Tool(ToolCall {
            id,
            name,
            input,
            partial_input: String::new(),
            status,
            result: None,
            parent_tool_use_id: self.subagent_parent.clone(),
            started_at: timestamp,
            finished_at: (status == ToolStatus::Done).then_some(timestamp).flatten(),
        }));
    }

    fn user(&mut self, entry: &mut Entry) {
        let Some(mut message) = entry.raw.message.take() else {
            return;
        };
        let denied_ids = denied_tool_ids(&message);
        match message.get_mut("content").map(Value::take) {
            Some(Value::String(text)) => self.user_text(entry, &text, Vec::new(), false),
            Some(Value::Array(blocks)) => {
                let result_count = blocks
                    .iter()
                    .filter(|block| block_type(block) == Some("tool_result"))
                    .count();
                let mut texts = Vec::new();
                let mut images = Vec::new();
                for mut block in blocks {
                    match block_type(&block) {
                        Some("tool_result") => {
                            self.tool_result(entry, &mut block, result_count == 1, &denied_ids);
                        }
                        Some("text") => texts.push(take_string(&mut block, "text")),
                        Some("image") => images.extend(image_attachment(&block)),
                        _ => {}
                    }
                }
                let text = texts.join("\n\n");
                if result_count > 0 {
                    if text.trim_start().starts_with(INTERRUPTED_MARKER) {
                        self.interrupted(entry);
                    }
                    return;
                }
                if !text.trim().is_empty() || !images.is_empty() {
                    self.user_text(entry, &text, images, false);
                }
            }
            _ => {}
        }
    }

    fn tool_result(
        &mut self,
        entry: &mut Entry,
        block: &mut Value,
        owns_structured_result: bool,
        denied_ids: &HashSet<String>,
    ) {
        let tool_use_id = take_string(block, "tool_use_id");
        let Some(&index) = self.tool_index.get(&tool_use_id) else {
            return;
        };
        let text = content_text(block.get("content"));
        let is_error = block.get("is_error").and_then(Value::as_bool) == Some(true);
        let denied = (is_error && entry.raw.tool_denial_kind.is_some())
            || denied_ids.contains(&tool_use_id)
            || text.starts_with(USER_REJECTED_PREFIX);
        let status = if denied {
            ToolStatus::Denied
        } else if text.trim_start().starts_with(INTERRUPTED_MARKER) {
            ToolStatus::Interrupted
        } else if is_error {
            ToolStatus::Failed
        } else {
            ToolStatus::Done
        };
        let structured = if owns_structured_result {
            entry.raw.tool_use_result.take()
        } else {
            None
        };
        if let Some(agent_id) = structured
            .as_ref()
            .and_then(|structured| structured.get("agentId"))
            .and_then(Value::as_str)
        {
            self.agent_tool_ids
                .insert(agent_id.to_owned(), tool_use_id.clone());
        }
        if let Some(content) = self.pending_task_creates.remove(&tool_use_id) {
            let task_id = structured
                .as_ref()
                .and_then(|structured| structured.pointer("/task/id"))
                .and_then(json_id)
                .or_else(|| task_id_from_text(&text));
            if let Some(todo) = self
                .todos
                .iter_mut()
                .find(|todo| todo.id.is_none() && todo.content == content)
            {
                todo.id = task_id;
            }
        }
        if let Some(TimelineItem::Tool(tool)) = self.items.get_mut(index) {
            tool.status = status;
            tool.finished_at = entry.timestamp();
            tool.result = Some(ToolResult {
                text,
                is_error,
                structured,
            });
        }
        self.turn.running_tools.retain(|&running| running != index);
        self.touch(entry);
    }

    fn user_text(
        &mut self,
        entry: &Entry,
        text: &str,
        images: Vec<ImageAttachment>,
        is_queued: bool,
    ) {
        if self.is_subagent() {
            // A subagent's prompt is the input of the tool call it is nested under.
            return;
        }
        let trimmed = text.trim();
        if entry.raw.is_compact_summary {
            self.push_notice(&entry.uuid, NoticeKind::CompactSummary(trimmed.to_owned()));
            return;
        }
        if trimmed.starts_with(INTERRUPTED_MARKER) {
            self.interrupted(entry);
            return;
        }
        if let Some(command) = parse_slash_command(trimmed) {
            self.begin_turn(entry);
            let text = format!("/{} {}", command.name, command.args)
                .trim_end()
                .to_owned();
            self.push_user(entry, text, Vec::new(), Some(command));
            return;
        }
        if let Some(output) = local_command_output(trimmed) {
            self.attach_command_output(entry, output);
            return;
        }
        if let Some(command) = extract_tag(trimmed, "bash-input") {
            self.begin_turn(entry);
            let command = command.trim().to_owned();
            self.push_user(
                entry,
                format!("{BASH_MODE_COMMAND}{command}"),
                Vec::new(),
                Some(SlashCommandInvocation {
                    name: BASH_MODE_COMMAND.to_owned(),
                    args: command,
                    output: None,
                }),
            );
            return;
        }
        if trimmed.starts_with("<bash-stdout>") || trimmed.starts_with("<bash-stderr>") {
            let output = ["bash-stdout", "bash-stderr"]
                .iter()
                .filter_map(|tag| extract_tag(trimmed, tag))
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            self.attach_command_output(entry, clean_output(&output));
            return;
        }
        if trimmed.starts_with("<task-notification>") {
            let summary = extract_tag(trimmed, "summary")
                .unwrap_or(trimmed)
                .trim()
                .to_owned();
            self.begin_turn(entry);
            self.push_notice(&entry.uuid, NoticeKind::Info(summary));
            return;
        }
        let cleaned = strip_context_tags(trimmed);
        if cleaned.is_empty() && images.is_empty() {
            return;
        }
        if !is_queued {
            self.begin_turn(entry);
        }
        self.push_user(entry, cleaned, images, None);
    }

    fn push_user(
        &mut self,
        entry: &Entry,
        text: String,
        images: Vec<ImageAttachment>,
        command: Option<SlashCommandInvocation>,
    ) {
        let id = entry
            .raw
            .attachment
            .as_ref()
            .and_then(|attachment| attachment.source_uuid.clone())
            .unwrap_or_else(|| entry.uuid.clone());
        if command.is_none() {
            self.last_user_uuid = Some(id.clone());
        }
        self.push(TimelineItem::User(UserMessage {
            id,
            text,
            images,
            command,
            timestamp: entry.timestamp(),
        }));
    }

    fn attach_command_output(&mut self, entry: &Entry, output: String) {
        if let Some(TimelineItem::User(UserMessage {
            command: Some(command),
            ..
        })) = self.items.last_mut()
            && command.output.is_none()
        {
            command.output = (!output.is_empty()).then_some(output);
            return;
        }
        if !output.is_empty() {
            self.push_notice(&entry.uuid, NoticeKind::Info(output));
        }
    }

    fn interrupted(&mut self, entry: &Entry) {
        self.push_notice(&entry.uuid, NoticeKind::Interrupted);
        self.turn.interrupted = true;
        self.touch(entry);
        self.close_turn();
    }

    /// Meta entries are hidden, but the one that names a pasted image's file fills in its path.
    fn meta_user(&mut self, entry: &Entry) {
        let Some(message) = entry.raw.message.as_ref() else {
            return;
        };
        let text = content_text(message.get("content"));
        let Some(path) = text
            .trim()
            .strip_prefix("[Image: source: ")
            .and_then(|rest| rest.strip_suffix(']'))
        else {
            return;
        };
        let Some(TimelineItem::User(user)) = self
            .items
            .iter_mut()
            .rev()
            .find(|item| matches!(item, TimelineItem::User(_)))
        else {
            return;
        };
        if let Some(image) = user.images.iter_mut().find(|image| image.path.is_none()) {
            image.path = Some(path.to_owned());
        }
    }

    fn system(&mut self, entry: &mut Entry) {
        match entry.raw.subtype.as_deref() {
            Some("compact_boundary") => {
                let metadata = entry.raw.compact_metadata.as_ref();
                let field = |key: &str| metadata.and_then(|metadata| metadata.get(key));
                if field("trigger").and_then(Value::as_str) == Some("manual") {
                    self.close_turn();
                }
                let kind = NoticeKind::Compacted {
                    pre_tokens: field("preTokens").and_then(Value::as_u64),
                    post_tokens: field("postTokens").and_then(Value::as_u64),
                };
                self.push_notice(&entry.uuid, kind);
            }
            Some("turn_duration") => {
                if self.turn.active {
                    self.turn.duration_ms = entry
                        .raw
                        .duration_ms
                        .as_ref()
                        .and_then(Value::as_f64)
                        .map(|ms| ms as u64);
                    self.close_turn();
                }
            }
            Some("local_command") => {
                if let Some(Value::String(text)) = entry.raw.content.take() {
                    self.user_text(entry, &text, Vec::new(), false);
                }
            }
            Some("scheduled_task_fire") => {
                if let Some(Value::String(text)) = entry.raw.content.take() {
                    self.begin_turn(entry);
                    self.push_notice(&entry.uuid, NoticeKind::Info(text));
                }
            }
            _ => {}
        }
    }

    fn queued_command(&mut self, entry: &mut Entry) {
        let Some(attachment) = entry.raw.attachment.as_mut() else {
            return;
        };
        if attachment.is_meta && !is_channel_origin(attachment.origin.as_ref()) {
            return;
        }
        let (text, images) = match attachment.prompt.take() {
            Some(Value::String(text)) => (text, Vec::new()),
            Some(Value::Array(blocks)) => {
                let images = blocks.iter().filter_map(image_attachment).collect();
                (content_text(Some(&Value::Array(blocks))), images)
            }
            _ => return,
        };
        self.user_text(entry, &text, images, true);
    }

    fn track_todos(&mut self, tool_id: &str, name: &str, input: &Value) {
        if self.is_subagent() {
            return;
        }
        match name {
            "TodoWrite" => {
                if let Some(todos) = input.get("todos").and_then(Value::as_array) {
                    self.todos = todos.iter().filter_map(todo_from_value).collect();
                    self.pending_task_creates.clear();
                }
            }
            "TaskCreate" => {
                let content = string_field(input, "subject");
                if content.is_empty() {
                    return;
                }
                self.pending_task_creates
                    .insert(tool_id.to_owned(), content.clone());
                self.todos.push(TodoItem {
                    id: None,
                    content,
                    active_form: input
                        .get("activeForm")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    status: TodoStatus::Pending,
                });
            }
            "TaskUpdate" => {
                let Some(task_id) = input.get("taskId").and_then(json_id) else {
                    return;
                };
                let Some(position) = self
                    .todos
                    .iter()
                    .position(|todo| todo.id.as_deref() == Some(task_id.as_str()))
                else {
                    return;
                };
                let status = input.get("status").and_then(Value::as_str);
                if status == Some("deleted") {
                    self.todos.remove(position);
                    return;
                }
                let todo = &mut self.todos[position];
                if let Some(status) = status.and_then(parse_todo_status) {
                    todo.status = status;
                }
                if let Some(subject) = input.get("subject").and_then(Value::as_str) {
                    todo.content = subject.to_owned();
                }
                if let Some(active_form) = input.get("activeForm").and_then(Value::as_str) {
                    todo.active_form = Some(active_form.to_owned());
                }
            }
            _ => {}
        }
    }
}

fn block_type(block: &Value) -> Option<&str> {
    block.get("type")?.as_str()
}

fn take_string(value: &mut Value, key: &str) -> String {
    match value.get_mut(key).map(Value::take) {
        Some(Value::String(text)) => text,
        _ => String::new(),
    }
}

fn string_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// Task ids are strings, but accept numbers too.
fn json_id(value: &Value) -> Option<String> {
    match value {
        Value::String(id) => Some(id.clone()),
        Value::Number(id) => Some(id.to_string()),
        _ => None,
    }
}

fn task_id_from_text(text: &str) -> Option<String> {
    let rest = &text[text.find("Task #")? + "Task #".len()..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    (!digits.is_empty()).then_some(digits)
}

fn parse_todo_status(status: &str) -> Option<TodoStatus> {
    match status {
        "pending" => Some(TodoStatus::Pending),
        "in_progress" => Some(TodoStatus::InProgress),
        "completed" => Some(TodoStatus::Completed),
        _ => None,
    }
}

fn todo_from_value(value: &Value) -> Option<TodoItem> {
    let content = value.get("content")?.as_str()?.to_owned();
    Some(TodoItem {
        id: value.get("id").and_then(json_id),
        content,
        active_form: value
            .get("activeForm")
            .and_then(Value::as_str)
            .map(str::to_owned),
        status: value
            .get("status")
            .and_then(Value::as_str)
            .and_then(parse_todo_status)
            .unwrap_or(TodoStatus::Pending),
    })
}

/// Tool calls the CLI never ran because a permission rule, the user or auto mode refused them.
fn denied_tool_ids(message: &Value) -> HashSet<String> {
    message
        .get("tool_result_meta")
        .and_then(Value::as_array)
        .map(|meta| {
            meta.iter()
                .filter(|meta| {
                    meta.get("non_execution_kind")
                        .is_some_and(|kind| !kind.is_null())
                })
                .filter_map(|meta| meta.get("id").and_then(Value::as_str).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Origins whose meta messages the SDK still shows (the SDK's `bR`).
fn is_channel_origin(origin: Option<&Value>) -> bool {
    matches!(
        origin
            .and_then(|origin| origin.get("kind"))
            .and_then(Value::as_str),
        Some("channel" | "observer" | "observer-activity" | "slack-ping" | "peer")
    )
}

fn content_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|block| block_type(block) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn image_attachment(block: &Value) -> Option<ImageAttachment> {
    let source = block.get("source")?;
    match source.get("type").and_then(Value::as_str) {
        Some("base64") => Some(ImageAttachment {
            media_type: string_field(source, "media_type"),
            path: None,
            data: source
                .get("data")
                .and_then(Value::as_str)
                .map(str::to_owned),
        }),
        Some("url") => Some(ImageAttachment {
            media_type: string_field(source, "media_type"),
            path: source.get("url").and_then(Value::as_str).map(str::to_owned),
            data: None,
        }),
        _ => None,
    }
}

fn extract_tag<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..]
        .find(&close)
        .map_or(text.len(), |end| start + end);
    Some(&text[start..end])
}

fn parse_slash_command(text: &str) -> Option<SlashCommandInvocation> {
    let name = extract_tag(text, "command-name")?
        .trim()
        .trim_start_matches('/');
    if name.is_empty() {
        return None;
    }
    Some(SlashCommandInvocation {
        name: name.to_owned(),
        args: extract_tag(text, "command-args")
            .unwrap_or_default()
            .trim()
            .to_owned(),
        output: None,
    })
}

fn local_command_output(text: &str) -> Option<String> {
    if !text.starts_with("<local-command-stdout>") && !text.starts_with("<local-command-stderr>") {
        return None;
    }
    let output = ["local-command-stdout", "local-command-stderr"]
        .iter()
        .filter_map(|tag| extract_tag(text, tag))
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    Some(clean_output(&output))
}

fn clean_output(output: &str) -> String {
    if output.contains('\u{1b}') {
        strip_ansi(output)
    } else {
        output.to_owned()
    }
}

fn strip_ansi(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(char) = chars.next() {
        if char != '\u{1b}' {
            output.push(char);
            continue;
        }
        if chars.peek() == Some(&'[') {
            chars.next();
            for char in chars.by_ref() {
                if ('@'..='~').contains(&char) {
                    break;
                }
            }
        } else {
            chars.next();
        }
    }
    output
}

/// Context the CLI or an IDE adds around what the user typed.
const CONTEXT_TAGS: &[&str] = &["system-reminder", "ide_opened_file", "ide_selection"];

fn strip_context_tags(text: &str) -> String {
    let mut text = text.to_owned();
    for tag in CONTEXT_TAGS {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        while let Some(start) = text.find(&open) {
            let Some(end) = text[start..].find(&close) else {
                break;
            };
            text.replace_range(start..start + end + close.len(), "");
        }
    }
    text.trim().to_owned()
}

// ---------------------------------------------------------------------------------------------
// Subagents

/// Converts `<session-id>/subagents/agent-*.jsonl`, keyed by the tool call that started each one.
fn load_subagents(
    transcript: &Path,
    mut agent_tool_ids: HashMap<String, String>,
) -> HashMap<String, Vec<TimelineItem>> {
    let mut by_tool = HashMap::new();
    let Some(stem) = transcript.file_stem() else {
        return by_tool;
    };
    let directory = transcript.with_file_name(stem).join("subagents");
    let Ok(read_dir) = std::fs::read_dir(&directory) else {
        return by_tool;
    };
    let mut files: Vec<PathBuf> = read_dir
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
        })
        .collect();
    files.sort();

    let mut unlinked = Vec::new();
    for path in files {
        let Ok(mut file) = read_transcript_file(&path) else {
            continue;
        };
        let ordered = visible_entries(&file, true, true);
        let agent_id = path
            .file_stem()
            .map(|stem| stem.to_string_lossy())
            .and_then(|stem| stem.strip_prefix("agent-").map(str::to_owned));
        let tool_id =
            subagent_tool_id(&path).or_else(|| agent_tool_ids.get(agent_id.as_deref()?).cloned());
        let mut converter = Converter::new(Some(tool_id.clone().unwrap_or_default()));
        for index in ordered {
            converter.convert(&mut file.entries[index]);
        }
        agent_tool_ids.extend(converter.agent_tool_ids);
        match tool_id {
            Some(tool_id) => {
                by_tool
                    .entry(tool_id)
                    .or_insert_with(Vec::new)
                    .extend(converter.items);
            }
            None => unlinked.push((agent_id, converter.items)),
        }
    }
    // Agents spawned by another subagent are only known once that subagent is converted.
    for (agent_id, mut items) in unlinked {
        let Some(tool_id) = agent_id.and_then(|agent_id| agent_tool_ids.get(&agent_id).cloned())
        else {
            continue;
        };
        for item in &mut items {
            set_parent_tool_use_id(item, &tool_id);
        }
        by_tool
            .entry(tool_id)
            .or_insert_with(Vec::new)
            .extend(items);
    }
    by_tool
}

fn subagent_tool_id(transcript: &Path) -> Option<String> {
    let meta_path = transcript.with_extension("meta.json");
    let meta: Value = serde_json::from_slice(&std::fs::read(meta_path).ok()?).ok()?;
    Some(meta.get("toolUseId")?.as_str()?.to_owned())
}

fn set_parent_tool_use_id(item: &mut TimelineItem, tool_id: &str) {
    let parent = match item {
        TimelineItem::Assistant(text) => &mut text.parent_tool_use_id,
        TimelineItem::Thinking(thinking) => &mut thinking.parent_tool_use_id,
        TimelineItem::Tool(tool) => &mut tool.parent_tool_use_id,
        TimelineItem::User(_) | TimelineItem::Notice(_) | TimelineItem::TurnEnd(_) => return,
    };
    *parent = Some(tool_id.to_owned());
}

/// Places each subagent's items right after the tool call that started it, recursively.
fn nest_subagent_items(
    items: Vec<TimelineItem>,
    mut subagents: HashMap<String, Vec<TimelineItem>>,
) -> Vec<TimelineItem> {
    fn nest(
        items: Vec<TimelineItem>,
        subagents: &mut HashMap<String, Vec<TimelineItem>>,
        output: &mut Vec<TimelineItem>,
    ) {
        for item in items {
            let children = match &item {
                TimelineItem::Tool(tool) => subagents.remove(&tool.id),
                TimelineItem::User(_)
                | TimelineItem::Assistant(_)
                | TimelineItem::Thinking(_)
                | TimelineItem::Notice(_)
                | TimelineItem::TurnEnd(_) => None,
            };
            output.push(item);
            if let Some(children) = children {
                nest(children, subagents, output);
            }
        }
    }
    let mut output = Vec::with_capacity(items.len());
    nest(items, &mut subagents, &mut output);
    output
}

// ---------------------------------------------------------------------------------------------
// Timestamps

/// Parses the RFC 3339 timestamps the CLI writes, like `2026-10-26T21:15:41.604Z`.
pub fn parse_timestamp(text: &str) -> Option<SystemTime> {
    let bytes = text.as_bytes();
    if bytes.len() < 19
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !matches!(bytes[10], b'T' | b't' | b' ')
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    let number = |range: std::ops::Range<usize>| -> Option<i64> {
        let digits = text.get(range)?;
        digits
            .bytes()
            .all(|byte| byte.is_ascii_digit())
            .then(|| digits.parse().ok())
            .flatten()
    };
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }

    let mut rest = &text[19..];
    let mut nanos = 0u32;
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
        for (position, byte) in fraction.bytes().take(digits.min(9)).enumerate() {
            nanos += u32::from(byte - b'0') * 10u32.pow(8 - position as u32);
        }
        rest = &fraction[digits..];
    }
    let offset_seconds = match rest {
        "" | "Z" | "z" => 0,
        _ => {
            let sign = match rest.as_bytes()[0] {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let hours: i64 = rest.get(1..3)?.parse().ok()?;
            let minutes: i64 = rest.get(4..6).or_else(|| rest.get(3..5))?.parse().ok()?;
            sign * (hours * 3600 + minutes * 60)
        }
    };
    let seconds = days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second
        - offset_seconds;
    let seconds = u64::try_from(seconds).ok()?;
    Some(UNIX_EPOCH + Duration::new(seconds, nanos))
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
