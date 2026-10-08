//! Claude Code conversations ("threads") grouped by project folder, behind the
//! Threads panel of the vertical tabs sidebar.
//!
//! Projects are folders the user picks. They live in
//! `~/.warp-oss/claude_threads.toml`:
//!
//! ```toml
//! [[project]]
//! path      = "/Users/me/projects/kinbox"
//! collapsed = false
//! ```
//!
//! Threads are read straight from Claude Code's transcripts at
//! `~/.claude/projects/<slug>/<session id>.jsonl`, where the slug is the folder
//! the conversation started in with every non-alphanumeric character replaced
//! by `-`. A project lists the conversations started in its folder or in any
//! folder below it. The only change made under `~/.claude` is deleting a
//! thread, which moves its transcript to the macOS Trash.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::Value;

const CONFIG_FILE_NAME: &str = "claude_threads.toml";
/// Bytes read from each end of a transcript: the first prompt sits at the
/// start, and Claude re-appends the title records as the conversation grows.
const TRANSCRIPT_CHUNK_BYTES: u64 = 256 * 1024;
const TITLE_MAX_CHARS: usize = 80;

/// Command that starts a new thread in the project folder.
pub const NEW_THREAD_COMMAND: &str = "claude";

/// A folder whose Claude Code conversations are listed in the Threads panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadProject {
    pub path: PathBuf,
    #[serde(default)]
    pub collapsed: bool,
}

impl ThreadProject {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            collapsed: false,
        }
    }

    /// The folder name shown in the panel.
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ConfigFile {
    #[serde(default, rename = "project")]
    projects: Vec<ThreadProject>,
}

fn config_path() -> PathBuf {
    warp_core::paths::data_dir().join(CONFIG_FILE_NAME)
}

pub fn load_projects() -> Vec<ThreadProject> {
    let path = config_path();
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    match toml::from_str::<ConfigFile>(&contents) {
        Ok(config) => config.projects,
        Err(err) => {
            log::warn!("Failed to parse {}: {}", path.display(), err.message());
            Vec::new()
        }
    }
}

pub fn save_projects(projects: &[ThreadProject]) -> std::io::Result<()> {
    let config = ConfigFile {
        projects: projects.to_vec(),
    };
    let contents = toml::to_string_pretty(&config).map_err(std::io::Error::other)?;
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, contents)
}

/// One Claude Code conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeThread {
    pub session_id: String,
    pub title: String,
    /// The folder the conversation runs in; `claude --resume` must start there.
    pub cwd: PathBuf,
    pub updated_at: SystemTime,
    /// The `<session id>.jsonl` file holding the conversation.
    pub transcript: PathBuf,
}

impl ClaudeThread {
    /// The shell command that reopens this conversation, or `None` when the
    /// session id isn't safe to put on a command line.
    pub fn resume_command(&self) -> Option<String> {
        is_session_id(&self.session_id).then(|| format!("claude --resume {}", self.session_id))
    }
}

fn is_session_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// What a transcript file says about itself, kept between scans so unchanged
/// files aren't read again.
#[derive(Debug, Clone)]
pub struct TranscriptCacheEntry {
    len: u64,
    modified: SystemTime,
    thread: Option<ClaudeThread>,
}

pub type TranscriptCache = HashMap<PathBuf, TranscriptCacheEntry>;

/// The threads of each project, newest first.
pub type ProjectThreads = HashMap<PathBuf, Vec<ClaudeThread>>;

/// The name Claude Code gives the transcript folder of `path`.
pub fn project_slug(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Reads the threads of every project from `~/.claude/projects`.
pub fn scan(projects: &[PathBuf], cache: &mut TranscriptCache) -> ProjectThreads {
    match dirs::home_dir() {
        Some(home) => scan_in(&home.join(".claude").join("projects"), projects, cache),
        None => projects.iter().map(|p| (p.clone(), Vec::new())).collect(),
    }
}

/// Reads the threads of every project from the transcript folders in `root`.
pub fn scan_in(root: &Path, projects: &[PathBuf], cache: &mut TranscriptCache) -> ProjectThreads {
    let mut result: ProjectThreads = projects.iter().map(|p| (p.clone(), Vec::new())).collect();
    let slugs: Vec<String> = projects.iter().map(|p| project_slug(p)).collect();
    let mut seen = HashSet::new();

    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let dir_name = entry.file_name().to_string_lossy().into_owned();
            // The project folder itself or one below it. Sibling folders that
            // only share the prefix (`kinbox-worktree` for `kinbox`) are
            // dropped by the `cwd` check below.
            let is_candidate = slugs.iter().any(|slug| {
                dir_name == *slug
                    || dir_name
                        .strip_prefix(slug.as_str())
                        .is_some_and(|rest| rest.starts_with('-'))
            });
            if !is_candidate {
                continue;
            }
            let Ok(files) = std::fs::read_dir(entry.path()) else {
                continue;
            };
            for file in files.flatten() {
                let path = file.path();
                if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                    continue;
                }
                let Ok(metadata) = file.metadata() else {
                    continue;
                };
                seen.insert(path.clone());
                let Some(thread) = cached_or_read(&path, &metadata, cache) else {
                    continue;
                };
                for project in projects {
                    if thread.cwd.starts_with(project) {
                        result
                            .entry(project.clone())
                            .or_default()
                            .push(thread.clone());
                    }
                }
            }
        }
    }

    cache.retain(|path, _| seen.contains(path));
    for threads in result.values_mut() {
        threads.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    }
    result
}

fn cached_or_read(
    path: &Path,
    metadata: &std::fs::Metadata,
    cache: &mut TranscriptCache,
) -> Option<ClaudeThread> {
    let len = metadata.len();
    let modified = metadata.modified().ok()?;
    if let Some(entry) = cache.get(path)
        && entry.len == len
        && entry.modified == modified
    {
        return entry.thread.clone();
    }
    let thread = read_transcript(path, modified);
    cache.insert(
        path.to_path_buf(),
        TranscriptCacheEntry {
            len,
            modified,
            thread: thread.clone(),
        },
    );
    thread
}

fn read_transcript(path: &Path, modified: SystemTime) -> Option<ClaudeThread> {
    let session_id = path.file_stem()?.to_str()?.to_owned();
    let (head, tail) = read_ends(path).ok()?;
    let mut summary = TranscriptSummary::default();
    for line in head.lines().chain(tail.lines()) {
        summary.add_line(line);
    }
    let (title, cwd) = summary.finish()?;
    Some(ClaudeThread {
        session_id,
        title,
        cwd,
        updated_at: modified,
        transcript: path.to_path_buf(),
    })
}

/// Deletes a thread by moving its transcript, and the folder Claude Code keeps
/// next to it for subagents and tool output, to the Trash. Claude no longer
/// lists it, and it can still be put back from the Trash.
pub fn trash_thread(thread: &ClaudeThread) -> Result<(), String> {
    move_to_trash(&thread.transcript)?;
    let sidecar = thread.transcript.with_extension("");
    if sidecar.is_dir() {
        move_to_trash(&sidecar)?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn move_to_trash(path: &Path) -> Result<(), String> {
    use objc2::rc::autoreleasepool;
    use objc2_foundation::{NSFileManager, NSString, NSURL};

    autoreleasepool(|_| {
        let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        NSFileManager::defaultManager()
            .trashItemAtURL_resultingItemURL_error(&url, None)
            .map_err(|err| err.localizedDescription().to_string())
    })
}

#[cfg(not(target_os = "macos"))]
fn move_to_trash(path: &Path) -> Result<(), String> {
    Err(format!(
        "mover para a lixeira não é suportado aqui: {}",
        path.display()
    ))
}

/// The first and last `TRANSCRIPT_CHUNK_BYTES` of the file (the whole file
/// when it's small). Lines cut at the chunk edges fail to parse and are skipped.
fn read_ends(path: &Path) -> std::io::Result<(String, String)> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let mut head = Vec::new();
    (&mut file)
        .take(TRANSCRIPT_CHUNK_BYTES)
        .read_to_end(&mut head)?;
    let mut tail = Vec::new();
    if len > TRANSCRIPT_CHUNK_BYTES {
        let start = len
            .saturating_sub(TRANSCRIPT_CHUNK_BYTES)
            .max(TRANSCRIPT_CHUNK_BYTES);
        file.seek(SeekFrom::Start(start))?;
        file.read_to_end(&mut tail)?;
    }
    Ok((
        String::from_utf8_lossy(&head).into_owned(),
        String::from_utf8_lossy(&tail).into_owned(),
    ))
}

/// Collects the title candidates and the folder of a transcript, line by line.
#[derive(Debug, Default)]
pub(crate) struct TranscriptSummary {
    /// Set with `/rename`.
    custom_title: Option<String>,
    /// Generated by Claude Code from the conversation.
    ai_title: Option<String>,
    /// Older Claude Code versions wrote `summary` records instead of titles.
    summary: Option<String>,
    first_prompt: Option<String>,
    cwd: Option<PathBuf>,
}

impl TranscriptSummary {
    pub(crate) fn add_line(&mut self, line: &str) {
        // Transcripts are mostly tool output; only parse the records we need.
        let is_wanted = [
            "\"customTitle\"",
            "\"aiTitle\"",
            "\"type\":\"summary\"",
            "\"type\":\"user\"",
        ]
        .iter()
        .any(|marker| line.contains(marker));
        if !is_wanted {
            return;
        }
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            return;
        };
        let text_field = |name: &str| {
            record
                .get(name)
                .and_then(Value::as_str)
                .map(clean_title)
                .filter(|title| !title.is_empty())
        };
        match record.get("type").and_then(Value::as_str) {
            // Later records win: the title can change during the conversation.
            Some("custom-title") => {
                self.custom_title = text_field("customTitle").or(self.custom_title.take())
            }
            Some("ai-title") => self.ai_title = text_field("aiTitle").or(self.ai_title.take()),
            Some("summary") => self.summary = text_field("summary").or(self.summary.take()),
            Some("user") => self.add_user_record(&record),
            _ => {}
        }
    }

    fn add_user_record(&mut self, record: &Value) {
        let is_flagged = |name: &str| record.get(name).and_then(Value::as_bool) == Some(true);
        if is_flagged("isSidechain") || is_flagged("isMeta") || is_flagged("isCompactSummary") {
            return;
        }
        if self.cwd.is_none()
            && let Some(cwd) = record.get("cwd").and_then(Value::as_str)
        {
            self.cwd = Some(PathBuf::from(cwd));
        }
        if self.first_prompt.is_none() {
            self.first_prompt = record
                .get("message")
                .and_then(|message| message.get("content"))
                .and_then(prompt_text)
                .map(|text| clean_title(&text))
                .filter(|title| !title.is_empty());
        }
    }

    /// The title to show and the conversation folder, or `None` for a
    /// transcript without any prompt (a session opened and closed right away).
    pub(crate) fn finish(self) -> Option<(String, PathBuf)> {
        let title = self
            .custom_title
            .or(self.ai_title)
            .or(self.summary)
            .or(self.first_prompt)?;
        Some((title, self.cwd?))
    }
}

/// The text the user typed, skipping tool results, slash command records and
/// other `<tag>` wrappers Claude Code stores as user messages.
fn prompt_text(content: &Value) -> Option<String> {
    let is_typed = |text: &&str| {
        let text = text.trim_start();
        !text.is_empty() && !text.starts_with('<') && !text.starts_with("[Request interrupted")
    };
    match content {
        Value::String(text) => Some(text.as_str()).filter(is_typed).map(str::to_owned),
        Value::Array(parts) => parts
            .iter()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .find(is_typed)
            .map(str::to_owned),
        _ => None,
    }
}

/// One line, collapsed whitespace, at most `TITLE_MAX_CHARS` characters.
fn clean_title(text: &str) -> String {
    let single_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.chars().count() <= TITLE_MAX_CHARS {
        return single_line;
    }
    let mut title: String = single_line.chars().take(TITLE_MAX_CHARS - 1).collect();
    title.push('…');
    title
}

/// How long ago `updated_at` was, as shown next to a thread ("5 min", "ontem").
pub fn format_age(updated_at: SystemTime, now: SystemTime) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;

    let seconds = now
        .duration_since(updated_at)
        .map(|age| age.as_secs())
        .unwrap_or(0);
    match seconds {
        s if s < MINUTE => "agora".to_owned(),
        s if s < HOUR => format!("{} min", s / MINUTE),
        s if s < DAY => format!("{} h", s / HOUR),
        s if s < 2 * DAY => "ontem".to_owned(),
        s if s < 7 * DAY => format!("{} d", s / DAY),
        _ => chrono::DateTime::<chrono::Local>::from(updated_at)
            .format("%d/%m")
            .to_string(),
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
