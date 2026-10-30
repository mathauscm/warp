//! Claude Code conversations ("threads") behind the Threads panel of the vertical tabs sidebar,
//! sorted into pinned threads, project folders and the recent threads outside every project.
//!
//! Projects are folders the user picks. They live in `~/.warp-oss/claude_threads.toml`, with the
//! pinned threads and the threads moved into a project:
//!
//! ```toml
//! pinned = ["5f0c…"]
//!
//! [[project]]
//! path      = "/Users/me/projects/kinbox"
//! collapsed = false
//!
//! [moved]
//! "9a1e…" = "/Users/me/projects/kinbox"
//! ```
//!
//! Workspaces group several folders, so a chat started in one has Claude work across all of them
//! (see [`ThreadWorkspace`]). Archived threads leave every other section for "Arquivadas":
//!
//! ```toml
//! archived = ["77b2…"]
//!
//! [[workspace]]
//! id   = "c41d…"
//! name = "Kinbox"
//! dirs = ["/Users/me/projects/kinbox-web", "/Users/me/projects/kinbox-api"]
//!
//! [workspace_threads]
//! "5f0c…" = "c41d…"
//! ```
//!
//! Threads are read straight from Claude Code's transcripts at
//! `~/.claude/projects/<slug>/<session id>.jsonl`, where the slug is the folder
//! the conversation started in with every non-alphanumeric character replaced
//! by `-`. A project lists the conversations started in its folder or in any
//! folder below it, plus the ones moved into it. The only changes made under `~/.claude` are
//! renaming a thread and deleting one, which moves its transcript to the system Trash.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use claude_chat::{codex, codex_history};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::native_chat::ChatProvider;

const CONFIG_FILE_NAME: &str = "claude_threads.toml";
/// Bytes read from each end of a transcript: the first prompt sits at the
/// start, and Claude re-appends the title records as the conversation grows.
const TRANSCRIPT_CHUNK_BYTES: u64 = 256 * 1024;
const TITLE_MAX_CHARS: usize = 80;

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

/// A named set of folders, like the front and back end of one product, that a chat works across:
/// Claude runs in the first folder and gets access to the others.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadWorkspace {
    pub id: String,
    pub name: String,
    /// The first one is where Claude runs; the others are added with `--add-dir`.
    #[serde(default)]
    pub dirs: Vec<PathBuf>,
    #[serde(default)]
    pub collapsed: bool,
}

impl ThreadWorkspace {
    pub fn new(name: String) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            dirs: Vec::new(),
            collapsed: false,
        }
    }

    pub fn primary_dir(&self) -> Option<&Path> {
        self.dirs.first().map(PathBuf::as_path)
    }

    /// Adds the folders not in the workspace yet, keeping their order. Returns whether any was.
    pub fn add_dirs(&mut self, dirs: impl IntoIterator<Item = PathBuf>) -> bool {
        let before = self.dirs.len();
        for dir in dirs {
            if !self.dirs.contains(&dir) {
                self.dirs.push(dir);
            }
        }
        self.dirs.len() != before
    }

    /// The chat settings of a chat in this workspace that runs in `cwd`: every other folder of the
    /// workspace is added to it.
    pub fn chat_workspace(&self, cwd: &Path) -> crate::native_chat::ChatWorkspace {
        crate::native_chat::ChatWorkspace {
            id: self.id.clone(),
            name: self.name.clone(),
            extra_dirs: self
                .dirs
                .iter()
                .filter(|dir| dir.as_path() != cwd)
                .cloned()
                .collect(),
        }
    }
}

/// The version written by this build; older files have none and load the same way.
const CONFIG_VERSION: u32 = 2;

/// What the Threads panel keeps between launches.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadsConfig {
    #[serde(default)]
    pub version: u32,
    /// Session ids of the threads listed under "Fixados", most recently pinned first.
    #[serde(default)]
    pub pinned: Vec<String>,
    /// Session ids of the threads listed only under "Arquivadas", most recently archived first.
    #[serde(default)]
    pub archived: Vec<String>,
    #[serde(default)]
    pub pinned_collapsed: bool,
    #[serde(default)]
    pub projects_collapsed: bool,
    #[serde(default)]
    pub workspaces_collapsed: bool,
    #[serde(default)]
    pub recent_collapsed: bool,
    /// "Arquivadas" starts collapsed.
    #[serde(default)]
    pub archived_expanded: bool,
    #[serde(default, rename = "project")]
    pub projects: Vec<ThreadProject>,
    #[serde(default, rename = "workspace")]
    pub workspaces: Vec<ThreadWorkspace>,
    /// Project each moved thread is listed under instead of the one holding its folder, by session
    /// id. The thread still resumes in its own folder.
    #[serde(default)]
    pub moved: BTreeMap<String, PathBuf>,
    /// Workspace id of each thread started in a workspace, by session id.
    #[serde(default)]
    pub workspace_threads: BTreeMap<String, String>,
}

impl ThreadsConfig {
    pub fn is_pinned(&self, session_id: &str) -> bool {
        self.pinned.iter().any(|id| id == session_id)
    }

    pub fn is_archived(&self, session_id: &str) -> bool {
        self.archived.iter().any(|id| id == session_id)
    }

    /// Archives the thread, which also unpins it, or brings it back.
    pub fn toggle_archived(&mut self, session_id: &str) {
        if self.is_archived(session_id) {
            self.archived.retain(|id| id != session_id);
        } else {
            self.pinned.retain(|id| id != session_id);
            self.archived.insert(0, session_id.to_owned());
        }
    }

    /// Pins the thread, which also unarchives it, or unpins it.
    pub fn toggle_pinned(&mut self, session_id: &str) {
        if self.is_pinned(session_id) {
            self.pinned.retain(|id| id != session_id);
        } else {
            self.archived.retain(|id| id != session_id);
            self.pinned.insert(0, session_id.to_owned());
        }
    }

    pub fn workspace(&self, id: &str) -> Option<&ThreadWorkspace> {
        self.workspaces.iter().find(|workspace| workspace.id == id)
    }

    pub fn workspace_mut(&mut self, id: &str) -> Option<&mut ThreadWorkspace> {
        self.workspaces
            .iter_mut()
            .find(|workspace| workspace.id == id)
    }

    /// The listed workspace a thread was started in.
    pub fn workspace_of(&self, session_id: &str) -> Option<&ThreadWorkspace> {
        self.workspace(self.workspace_threads.get(session_id)?)
    }

    /// Lists the thread under the workspace. Returns whether that changed anything.
    pub fn assign_to_workspace(&mut self, session_id: &str, workspace_id: &str) -> bool {
        if self.workspace(workspace_id).is_none()
            || self.workspace_threads.get(session_id).map(String::as_str) == Some(workspace_id)
        {
            return false;
        }
        self.workspace_threads
            .insert(session_id.to_owned(), workspace_id.to_owned());
        true
    }

    /// Drops the workspace; its threads go back to their projects or "Recentes".
    pub fn remove_workspace(&mut self, id: &str) {
        self.workspaces.retain(|workspace| workspace.id != id);
        self.workspace_threads
            .retain(|_, workspace_id| workspace_id != id);
    }

    /// The listed project a thread belongs to: the one it was moved to, else the one that most
    /// closely contains its folder.
    pub fn project_of(&self, thread: &ClaudeThread) -> Option<&Path> {
        let moved = self
            .moved
            .get(&thread.session_id)
            .and_then(|path| self.projects.iter().find(|project| &project.path == path));
        if let Some(project) = moved {
            return Some(&project.path);
        }
        self.projects
            .iter()
            .map(|project| project.path.as_path())
            .filter(|path| thread.cwd.starts_with(path))
            .max_by_key(|path| path.components().count())
    }

    /// Drops what the config remembers about a deleted thread.
    pub fn forget_thread(&mut self, session_id: &str) {
        self.pinned.retain(|id| id != session_id);
        self.archived.retain(|id| id != session_id);
        self.moved.remove(session_id);
        self.workspace_threads.remove(session_id);
    }
}

fn config_path() -> PathBuf {
    warp_core::paths::data_dir().join(CONFIG_FILE_NAME)
}

pub fn load_config() -> ThreadsConfig {
    let path = config_path();
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return ThreadsConfig::default();
    };
    match toml::from_str(&contents) {
        Ok(config) => config,
        Err(err) => {
            log::warn!("Failed to parse {}: {}", path.display(), err.message());
            ThreadsConfig::default()
        }
    }
}

pub fn save_config(config: &ThreadsConfig) -> std::io::Result<()> {
    let config = ThreadsConfig {
        version: CONFIG_VERSION,
        ..config.clone()
    };
    let contents = toml::to_string_pretty(&config).map_err(std::io::Error::other)?;
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, contents)
}

/// One Claude Code or Codex conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeThread {
    /// The CLI the conversation ran in.
    pub provider: ChatProvider,
    /// Claude Code's session id, or Codex's thread id.
    pub session_id: String,
    pub title: String,
    /// The folder the conversation runs in; `claude --resume` must start there.
    pub cwd: PathBuf,
    pub updated_at: SystemTime,
    /// The `<session id>.jsonl` file holding the conversation.
    pub transcript: PathBuf,
    /// The git branch checked out in `cwd` when the last prompt was sent.
    pub git_branch: Option<String>,
}

impl ClaudeThread {
    /// The shell command that reopens this conversation, or `None` when the
    /// session id isn't safe to put on a command line.
    pub fn resume_command(&self) -> Option<String> {
        is_session_id(&self.session_id).then(|| match self.provider {
            ChatProvider::Claude => format!("claude --resume {}", self.session_id),
            ChatProvider::Codex => format!("codex resume {}", self.session_id),
        })
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

/// The name Claude Code gives the transcript folder of `path`.
pub fn project_slug(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Reads every thread from `~/.claude/projects` and Codex's sessions, newest first.
pub fn scan(cache: &mut TranscriptCache) -> Vec<ClaudeThread> {
    let mut threads = match dirs::home_dir() {
        Some(home) => scan_in(&home.join(".claude").join("projects"), cache),
        None => Vec::new(),
    };
    if let Some(home) = codex::codex_home() {
        threads.extend(scan_codex_in(&home));
        threads.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    }
    threads
}

/// Codex's sessions under `codex_home`, as threads. Files read before and unchanged since come
/// from a cache of their own, apart from Claude's.
pub fn scan_codex_in(codex_home: &Path) -> Vec<ClaudeThread> {
    static CACHE: std::sync::Mutex<BTreeMap<PathBuf, (SystemTime, Option<ClaudeThread>)>> =
        std::sync::Mutex::new(BTreeMap::new());
    let mut cache = CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let names = codex_history::thread_names(codex_home);
    let files = codex_history::session_files(codex_home);
    cache.retain(|path, _| files.contains(path));
    files
        .into_iter()
        .filter_map(|path| {
            let modified = std::fs::metadata(&path)
                .and_then(|meta| meta.modified())
                .ok()?;
            if let Some((cached_at, thread)) = cache.get(&path)
                && *cached_at == modified
            {
                return thread.clone().map(|mut thread| {
                    if let Some(name) = names.get(&thread.session_id) {
                        thread.title = name.clone();
                    }
                    thread
                });
            }
            let thread = codex_history::summarize_session(&path, &names).and_then(|summary| {
                Some(ClaudeThread {
                    provider: ChatProvider::Codex,
                    title: clean_title(summary.title.as_deref().unwrap_or("Codex")),
                    cwd: summary.cwd?,
                    updated_at: summary.updated_at,
                    transcript: summary.path,
                    git_branch: None,
                    session_id: summary.id,
                })
            });
            cache.insert(path, (modified, thread.clone()));
            thread
        })
        .collect()
}

/// Reads every thread from the transcript folders in `root`, newest first.
pub fn scan_in(root: &Path, cache: &mut TranscriptCache) -> Vec<ClaudeThread> {
    let mut threads = Vec::new();
    let mut seen = HashSet::new();
    let folders = std::fs::read_dir(root).into_iter().flatten().flatten();
    for folder in folders {
        let Ok(files) = std::fs::read_dir(folder.path()) else {
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
            threads.extend(cached_or_read(&path, &metadata, cache));
        }
    }
    cache.retain(|path, _| seen.contains(path));
    threads.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    threads
}

/// Threads sorted into the sections of the panel.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ThreadGroups {
    /// In the order of `ThreadsConfig::pinned`.
    pub pinned: Vec<ClaudeThread>,
    /// The threads of each listed workspace that aren't pinned, by workspace id.
    pub by_workspace: HashMap<String, Vec<ClaudeThread>>,
    /// The threads of each listed project that aren't pinned nor in a workspace.
    pub by_project: HashMap<PathBuf, Vec<ClaudeThread>>,
    /// Threads outside every project and workspace, and not pinned.
    pub recent: Vec<ClaudeThread>,
    /// Archived threads, listed nowhere else.
    pub archived: Vec<ClaudeThread>,
}

/// Sorts `threads` (newest first) into sections, keeping their order within each.
pub fn group_threads(threads: Vec<ClaudeThread>, config: &ThreadsConfig) -> ThreadGroups {
    let mut groups = ThreadGroups::default();
    for thread in threads {
        if config.is_archived(&thread.session_id) {
            groups.archived.push(thread);
        } else if config.is_pinned(&thread.session_id) {
            groups.pinned.push(thread);
        } else if let Some(workspace) = config.workspace_of(&thread.session_id) {
            groups
                .by_workspace
                .entry(workspace.id.clone())
                .or_default()
                .push(thread);
        } else if let Some(project) = config.project_of(&thread) {
            groups
                .by_project
                .entry(project.to_path_buf())
                .or_default()
                .push(thread);
        } else {
            groups.recent.push(thread);
        }
    }
    groups
        .pinned
        .sort_by_key(|thread| config.pinned.iter().position(|id| *id == thread.session_id));
    groups
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
    let git_branch = summary.git_branch.clone();
    let (title, cwd) = summary.finish()?;
    Some(ClaudeThread {
        provider: ChatProvider::Claude,
        session_id,
        title,
        cwd,
        updated_at: modified,
        transcript: path.to_path_buf(),
        git_branch,
    })
}

/// Renames a thread the way Claude Code's `/rename` does: a `custom-title` record appended to its
/// transcript, which wins over every earlier title, also in `claude --resume`.
pub fn rename_thread(thread: &ClaudeThread, title: &str) -> Result<String, String> {
    match thread.provider {
        ChatProvider::Claude => rename_transcript(&thread.transcript, &thread.session_id, title),
        ChatProvider::Codex => {
            Err("Renomear conversas do Codex por aqui ainda não é suportado".to_owned())
        }
    }
}

/// Appends the `custom-title` record of [`rename_thread`] to the transcript at `path`.
pub fn rename_transcript(path: &Path, session_id: &str, title: &str) -> Result<String, String> {
    let title = clean_title(title);
    if title.is_empty() {
        return Err("O nome não pode ficar vazio".to_owned());
    }
    let record = serde_json::json!({
        "type": "custom-title",
        "customTitle": title,
        "sessionId": session_id,
    });
    let mut file = OpenOptions::new()
        .read(true)
        .append(true)
        .open(path)
        .map_err(|err| err.to_string())?;
    // Keep the record on its own line, even if the last one wasn't finished.
    let len = file.metadata().map_err(|err| err.to_string())?.len();
    let mut separator = "";
    if len > 0 {
        let mut last = [0u8; 1];
        file.seek(SeekFrom::Start(len - 1))
            .and_then(|_| file.read_exact(&mut last))
            .map_err(|err| err.to_string())?;
        if last[0] != b'\n' {
            separator = "\n";
        }
    }
    writeln!(file, "{separator}{record}").map_err(|err| err.to_string())?;
    Ok(title)
}

/// Deletes a thread by moving its transcript, and the folder Claude Code keeps
/// next to it for subagents and tool output, to the Trash. Claude no longer
/// lists it, and it can still be put back from the Trash.
pub fn trash_thread(thread: &ClaudeThread) -> Result<(), String> {
    // Codex indexes its sessions in a database of its own, which a moved file would leave stale.
    if thread.provider == ChatProvider::Codex {
        return Err("Conversas do Codex não podem ser apagadas por aqui".to_owned());
    }
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

/// Moves `path` to the home trash of the freedesktop.org Trash spec, which GNOME,
/// KDE and other desktops show and can restore from.
#[cfg(all(unix, not(target_os = "macos")))]
fn move_to_trash(path: &Path) -> Result<(), String> {
    let trash = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| dirs::home_dir().map(|home| home.join(".local/share")))
        .ok_or("pasta pessoal não encontrada")?
        .join("Trash");
    move_to_trash_dir(path, &trash)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn move_to_trash_dir(path: &Path, trash: &Path) -> Result<(), String> {
    use std::io::Write;

    let path = std::path::absolute(path).map_err(|err| err.to_string())?;
    let name = path
        .file_name()
        .ok_or_else(|| format!("caminho inválido: {}", path.display()))?
        .to_string_lossy()
        .into_owned();
    let (files, info) = (trash.join("files"), trash.join("info"));
    std::fs::create_dir_all(&files).map_err(|err| err.to_string())?;
    std::fs::create_dir_all(&info).map_err(|err| err.to_string())?;

    let encoded_path = path
        .to_string_lossy()
        .split('/')
        .map(|segment| urlencoding::encode(segment).into_owned())
        .collect::<Vec<_>>()
        .join("/");
    let deletion_date = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S");
    // Creating the .trashinfo with create_new reserves the name, so two deletions of
    // files with the same name never overwrite each other.
    for attempt in 1.. {
        let trashed_name = match attempt {
            1 => name.clone(),
            n => format!("{name}.{n}"),
        };
        let info_path = info.join(format!("{trashed_name}.trashinfo"));
        let mut info_file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&info_path)
        {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err.to_string()),
        };
        let moved = write!(
            info_file,
            "[Trash Info]\nPath={encoded_path}\nDeletionDate={deletion_date}\n"
        )
        .and_then(|()| std::fs::rename(&path, files.join(&trashed_name)));
        return moved.map_err(|err| {
            let _ = std::fs::remove_file(&info_path);
            err.to_string()
        });
    }
    unreachable!()
}

#[cfg(not(unix))]
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
    /// Later prompts win, since the branch can change during the conversation.
    git_branch: Option<String>,
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
        if let Some(branch) = record
            .get("gitBranch")
            .and_then(Value::as_str)
            .filter(|branch| !branch.is_empty())
        {
            self.git_branch = Some(branch.to_owned());
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

/// The branch checked out in the git repository holding `dir`, or the short commit id when HEAD is
/// detached. Reads `HEAD` directly (linked worktrees included) instead of running git.
pub fn git_branch(dir: &Path) -> Option<String> {
    let dot_git = dir
        .ancestors()
        .map(|ancestor| ancestor.join(".git"))
        .find(|dot_git| dot_git.exists())?;
    let git_dir = if dot_git.is_file() {
        let contents = std::fs::read_to_string(&dot_git).ok()?;
        let target = PathBuf::from(
            contents
                .lines()
                .find_map(|line| line.strip_prefix("gitdir:"))?
                .trim(),
        );
        if target.is_absolute() {
            target
        } else {
            dot_git.parent()?.join(target)
        }
    } else {
        dot_git
    };
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    match head.strip_prefix("ref:") {
        Some(reference) => {
            let reference = reference.trim();
            Some(
                reference
                    .strip_prefix("refs/heads/")
                    .unwrap_or(reference)
                    .to_owned(),
            )
        }
        None if head.is_empty() => None,
        None => Some(head.chars().take(7).collect()),
    }
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
