//! Finds and runs the user's `claude` CLI as a long-lived `stream-json` process.
//!
//! [`spawn`] returns a [`ClaudeProcess`] handle plus a single [`ProcessDriver`] future. This crate
//! has no executor: the caller must spawn the driver (on a background executor) for any IO to
//! happen, and consume [`ClaudeProcess::messages`] as a `Stream`.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::ExitStatus;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_channel::{Receiver, Sender};
use async_process::{Child, ChildStderr, ChildStdin, ChildStdout, Stdio};
use command::r#async::Command;
use futures::future::{self, Either};
use futures::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use futures::{Future, FutureExt};

use crate::protocol::StdoutMessage;

/// How long the CLI gets to exit after SIGTERM (or after stdin closes) before it is killed.
const KILL_GRACE_PERIOD: Duration = Duration::from_secs(5);

/// The value of `CLAUDE_CODE_ENTRYPOINT`. The CLI also accepts custom values, but this is the one
/// the Agent SDK sends and the one every flow (like claude.ai MCP auth links) was verified with.
pub const DEFAULT_ENTRYPOINT: &str = "sdk-ts";

/// Looks for the `claude` executable in `path_env` (a `PATH`-style list, typically from the
/// user's login shell), then in the usual install locations.
pub fn find_claude(path_env: Option<&str>) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    find_claude_in(path_env, home.as_deref())
}

/// [`find_claude`] with an explicit home directory.
pub fn find_claude_in(path_env: Option<&str>, home: Option<&Path>) -> Option<PathBuf> {
    find_executable_in("claude", path_env, home)
}

/// Looks for the executable `name` in `path_env`, then in the usual install locations of npm and
/// Homebrew CLIs.
pub(crate) fn find_executable_in(
    name: &str,
    path_env: Option<&str>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    let file_name = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    candidate_dirs(path_env, home)
        .into_iter()
        .map(|dir| dir.join(&file_name))
        .find(|candidate| is_executable(candidate))
}

fn candidate_dirs(path_env: Option<&str>, home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = path_env
        .map(|path| std::env::split_paths(path).collect())
        .unwrap_or_default();
    if let Some(home) = home {
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".claude/local"));
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    if let Some(home) = home {
        dirs.extend(nvm_bin_dirs(&home.join(".nvm/versions/node")));
    }
    dirs
}

/// `<root>/<version>/bin` for every installed node version, newest first.
fn nvm_bin_dirs(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut versions: Vec<(Vec<u64>, PathBuf)> = entries
        .filter_map(Result::ok)
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let version = name
                .trim_start_matches('v')
                .split('.')
                .map(|part| part.parse().unwrap_or_default())
                .collect();
            (version, entry.path().join("bin"))
        })
        .collect();
    versions.sort_by(|(left, _), (right, _)| right.cmp(left));
    versions.into_iter().map(|(_, dir)| dir).collect()
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Everything needed to start a session.
#[derive(Debug, Clone, PartialEq)]
pub struct SpawnConfig {
    pub claude_path: PathBuf,
    pub cwd: PathBuf,
    /// More folders Claude may read and edit besides `cwd`, whose `CLAUDE.md` files it also loads.
    pub add_dirs: Vec<PathBuf>,
    /// Continue this saved session.
    pub resume: Option<String>,
    /// With `resume`, branch into a new session id instead of appending.
    pub fork_session: bool,
    /// With `resume`, drop everything after this message uuid.
    pub resume_session_at: Option<String>,
    /// The id for a new session, so its transcript file is known before the first turn.
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub permission_mode: Option<String>,
    /// `summarized` gives readable thinking; without it thinking text is empty.
    pub thinking_display: Option<String>,
    /// The `PATH` for the CLI and the tools it runs, usually the login shell's.
    pub path_env: Option<String>,
    pub entrypoint: String,
    /// Needed for `rewind_files`.
    pub file_checkpointing: bool,
    pub extra_env: Vec<(String, String)>,
    pub extra_args: Vec<String>,
}

impl SpawnConfig {
    pub fn new(claude_path: impl Into<PathBuf>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            claude_path: claude_path.into(),
            cwd: cwd.into(),
            add_dirs: Vec::new(),
            resume: None,
            fork_session: false,
            resume_session_at: None,
            session_id: None,
            model: None,
            effort: None,
            permission_mode: None,
            thinking_display: Some("summarized".to_owned()),
            path_env: None,
            entrypoint: DEFAULT_ENTRYPOINT.to_owned(),
            file_checkpointing: true,
            extra_env: Vec::new(),
            extra_args: Vec::new(),
        }
    }

    pub fn args(&self) -> Vec<String> {
        let mut args: Vec<String> = [
            "--output-format",
            "stream-json",
            "--verbose",
            "--input-format",
            "stream-json",
            "--permission-prompt-tool=stdio",
            "--include-partial-messages",
            // Lets the user pick the full-access mode later with `set_permission_mode`.
            "--allow-dangerously-skip-permissions",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let mut flag = |name: &str, value: &Option<String>| {
            if let Some(value) = value {
                args.push(name.to_owned());
                args.push(value.clone());
            }
        };
        flag("--model", &self.model);
        flag("--effort", &self.effort);
        flag("--permission-mode", &self.permission_mode);
        flag("--thinking-display", &self.thinking_display);
        match &self.resume {
            Some(_) => {
                flag("--resume", &self.resume);
                flag("--resume-session-at", &self.resume_session_at);
                if self.fork_session {
                    args.push("--fork-session".to_owned());
                }
            }
            None => flag("--session-id", &self.session_id),
        }
        for dir in self.extra_dirs() {
            args.push("--add-dir".to_owned());
            args.push(dir.to_string_lossy().into_owned());
        }
        args.extend(self.extra_args.iter().cloned());
        args
    }

    /// `add_dirs` other than `cwd`.
    fn extra_dirs(&self) -> impl Iterator<Item = &PathBuf> {
        self.add_dirs.iter().filter(|dir| **dir != self.cwd)
    }

    /// Variables to set on top of the inherited environment.
    pub fn env(&self) -> Vec<(String, String)> {
        let mut env = vec![
            ("CLAUDE_CODE_ENTRYPOINT".to_owned(), self.entrypoint.clone()),
            (
                "CLAUDE_CODE_SDK_READS_SESSION_STATE".to_owned(),
                "1".to_owned(),
            ),
        ];
        if self.file_checkpointing {
            env.push((
                "CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING".to_owned(),
                "true".to_owned(),
            ));
        }
        if let Some(path) = &self.path_env {
            env.push(("PATH".to_owned(), path.clone()));
        }
        if self.extra_dirs().next().is_some() {
            env.push((
                "CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD".to_owned(),
                "1".to_owned(),
            ));
        }
        env.extend(self.extra_env.iter().cloned());
        env
    }

    /// Inherited variables to drop: `CLAUDECODE` makes the CLI think it runs nested inside another
    /// Claude Code, and the others change how the bundled Node runtime behaves.
    pub fn env_remove() -> &'static [&'static str] {
        &["CLAUDECODE", "NODE_OPTIONS", "DEBUG"]
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.claude_path);
        command.args(self.args()).current_dir(&self.cwd);
        for name in Self::env_remove() {
            command.env_remove(name);
        }
        command.envs(
            self.env()
                .into_iter()
                .map(|(key, value)| (OsString::from(key), OsString::from(value))),
        );
        command
    }
}

/// The future that does all of a process's IO (stdin writes, stdout parsing, stderr logging) and
/// waits for it to exit. It resolves once the process is gone and its pipes are drained.
pub type ProcessDriver = Pin<Box<dyn Future<Output = io::Result<ExitStatus>> + Send>>;

/// A running `claude` process.
pub type ClaudeProcess = LineProcess<StdoutMessage>;

/// A running CLI that talks in one JSON value per line, whose stdout lines are parsed into `T`.
/// Cheap to clone; dropping every clone closes stdin, which ends the session gracefully.
pub struct LineProcess<T> {
    pid: u32,
    stdin: Sender<String>,
    kill: Sender<()>,
    messages: Receiver<T>,
    exit_status: Arc<Mutex<Option<ExitStatus>>>,
}

impl<T> Clone for LineProcess<T> {
    fn clone(&self) -> Self {
        Self {
            pid: self.pid,
            stdin: self.stdin.clone(),
            kill: self.kill.clone(),
            messages: self.messages.clone(),
            exit_status: self.exit_status.clone(),
        }
    }
}

impl<T> LineProcess<T> {
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Queues one NDJSON line (without the trailing newline). Returns false once stdin is closed.
    pub fn send_line(&self, line: String) -> bool {
        self.stdin.try_send(line).is_ok()
    }

    /// The parsed stdout messages. The stream ends when stdout closes.
    pub fn messages(&self) -> Receiver<T> {
        self.messages.clone()
    }

    /// Ends the session gracefully: the CLI finishes and exits when its stdin closes.
    pub fn close_stdin(&self) {
        self.stdin.close();
    }

    /// Sends SIGTERM, then SIGKILL if the process is still alive after 5 seconds.
    pub fn kill(&self) {
        let _ = self.kill.try_send(());
    }

    /// Set once the process has exited.
    pub fn exit_status(&self) -> Option<ExitStatus> {
        *self
            .exit_status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Starts the CLI. The returned driver must be spawned on an executor; see the module docs.
pub fn spawn(config: &SpawnConfig) -> io::Result<(ClaudeProcess, ProcessDriver)> {
    log::info!(
        "Spawning claude in {} with args {:?}",
        config.cwd.display(),
        config.args()
    );
    spawn_lines(config.command(), "claude", |line| {
        StdoutMessage::parse_line(line).map_err(|error| error.to_string())
    })
}

/// Starts `command` with piped stdio and parses each stdout line with `parse`. `name` labels the
/// process in logs. The returned driver must be spawned on an executor; see the module docs.
pub(crate) fn spawn_lines<T: Send + 'static>(
    mut command: Command,
    name: &'static str,
    parse: fn(&str) -> Result<T, String>,
) -> io::Result<(LineProcess<T>, ProcessDriver)> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let pid = child.id();
    log::info!("Spawned {name} (pid {pid})");
    let missing = |pipe: &str| io::Error::other(format!("{name} {pipe} was not piped"));
    let stdin = child.stdin.take().ok_or_else(|| missing("stdin"))?;
    let stdout = child.stdout.take().ok_or_else(|| missing("stdout"))?;
    let stderr = child.stderr.take().ok_or_else(|| missing("stderr"))?;

    let (stdin_tx, stdin_rx) = async_channel::unbounded();
    let (kill_tx, kill_rx) = async_channel::bounded(1);
    let (messages_tx, messages_rx) = async_channel::unbounded();
    let exit_status = Arc::new(Mutex::new(None));

    let handle = LineProcess {
        pid,
        stdin: stdin_tx.clone(),
        kill: kill_tx,
        messages: messages_rx,
        exit_status: exit_status.clone(),
    };

    let driver = async move {
        let lifecycle = async {
            let status = wait_for_exit(&mut child, pid, name, &kill_rx, &stdin_tx).await;
            // Unblocks the writer when the process exits on its own.
            stdin_tx.close();
            if let Ok(status) = &status {
                *exit_status
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(*status);
                log::info!("{name} (pid {pid}) exited with {status}");
            }
            status
        };
        let (status, (), (), ()) = futures::join!(
            lifecycle,
            write_stdin(stdin, stdin_rx, pid, name),
            read_stdout(stdout, messages_tx, pid, name, parse),
            read_stderr(stderr, pid, name),
        );
        status
    };
    Ok((handle, driver.boxed()))
}

async fn wait_for_exit(
    child: &mut Child,
    pid: u32,
    name: &str,
    kill_rx: &Receiver<()>,
    stdin_tx: &Sender<String>,
) -> io::Result<ExitStatus> {
    let signal = {
        let status = child.status();
        let kill = kill_rx.recv();
        futures::pin_mut!(status, kill);
        match future::select(status, kill).await {
            Either::Left((status, _)) => return status,
            Either::Right((signal, _)) => signal,
        }
    };
    match signal {
        Ok(()) => terminate(pid, name),
        // Every handle was dropped: close stdin and give the CLI time to exit by itself.
        Err(_) => {
            stdin_tx.close();
            if let Some(status) = wait_with_timeout(child, KILL_GRACE_PERIOD).await {
                return status;
            }
            terminate(pid, name);
        }
    }
    if let Some(status) = wait_with_timeout(child, KILL_GRACE_PERIOD).await {
        return status;
    }
    log::warn!("{name} (pid {pid}) ignored SIGTERM; killing it");
    child.kill()?;
    child.status().await
}

async fn wait_with_timeout(child: &mut Child, timeout: Duration) -> Option<io::Result<ExitStatus>> {
    let status = child.status();
    let timer = async_io::Timer::after(timeout);
    futures::pin_mut!(status);
    match future::select(status, timer).await {
        Either::Left((status, _)) => Some(status),
        Either::Right(_) => None,
    }
}

fn terminate(pid: u32, name: &str) {
    #[cfg(unix)]
    {
        // SAFETY: `kill` has no memory-safety preconditions.
        let result = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        if result != 0 {
            log::warn!(
                "Failed to send SIGTERM to {name} (pid {pid}): {}",
                io::Error::last_os_error()
            );
        }
    }
    #[cfg(not(unix))]
    let _ = (pid, name);
}

async fn write_stdin(mut stdin: ChildStdin, lines: Receiver<String>, pid: u32, name: &str) {
    while let Ok(mut line) = lines.recv().await {
        line.push('\n');
        let written = async {
            stdin.write_all(line.as_bytes()).await?;
            stdin.flush().await
        }
        .await;
        if let Err(error) = written {
            log::warn!("Failed to write to {name} (pid {pid}) stdin: {error}");
            break;
        }
    }
    let _ = stdin.close().await;
}

async fn read_stdout<T>(
    stdout: ChildStdout,
    messages: Sender<T>,
    pid: u32,
    name: &str,
    parse: fn(&str) -> Result<T, String>,
) {
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match parse(trimmed) {
                    Ok(message) => {
                        // Keep draining stdout even if nobody listens, so the CLI never blocks.
                        let _ = messages.send(message).await;
                    }
                    Err(error) => {
                        log::warn!("Skipping non-JSON line from {name} (pid {pid}): {error}");
                    }
                }
            }
            Err(error) => {
                log::warn!("Failed to read {name} (pid {pid}) stdout: {error}");
                break;
            }
        }
    }
}

async fn read_stderr(stderr: ChildStderr, pid: u32, name: &str) {
    let mut reader = BufReader::new(stderr);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) | Err(_) => break,
            Ok(_) => log::info!("{name} (pid {pid}) stderr: {}", line.trim_end()),
        }
    }
}

#[cfg(test)]
#[path = "process_tests.rs"]
mod tests;
