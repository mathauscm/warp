//! Running a project's dev server: its output streamed line by line, a probe that reports when its
//! port answers, and stopping it with the processes it started.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_channel::{Receiver, Sender};

use super::launch::DevServerConfig;
use super::url::strip_ansi;

const PROBE_INTERVAL: Duration = Duration::from_millis(400);
/// How long to wait for the port to answer before giving up on opening the page.
const PROBE_TIMEOUT: Duration = Duration::from_secs(300);
const CONNECT_TIMEOUT: Duration = Duration::from_millis(250);
/// Time the server gets to exit after SIGTERM before it's killed.
const KILL_GRACE_PERIOD: Duration = Duration::from_secs(3);

/// Runs the server (`$2...`) and ends its process group once the app (`$1`) is gone, since an app
/// that's killed rather than quit can't stop its servers.
const WATCHDOG_SCRIPT: &str = r#"app=$1; shift
"$@" &
server=$!
while kill -0 "$app" 2>/dev/null && kill -0 "$server" 2>/dev/null; do sleep 1; done
if kill -0 "$server" 2>/dev/null; then kill -TERM 0; fi
wait "$server""#;

/// The process ids (and process group ids) of the dev servers still running, to stop them when the
/// app quits.
static RUNNING: Mutex<Vec<u32>> = Mutex::new(Vec::new());

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerEvent {
    /// A line the server printed, on stdout or stderr.
    Output(String),
    /// The server's port answers.
    Ready,
    /// The server exited, with how.
    Exited(String),
}

/// A running dev server, in its own process group.
pub struct ServerProcess {
    pid: u32,
    exited: Arc<AtomicBool>,
}

impl ServerProcess {
    /// Starts `config` with `path_env` as its `PATH`, writing its output to `log_path` too.
    pub fn spawn(
        config: &DevServerConfig,
        path_env: Option<&str>,
        log_path: Option<PathBuf>,
    ) -> std::io::Result<(Self, Receiver<ServerEvent>)> {
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(WATCHDOG_SCRIPT)
            .arg("picolas-dev-server")
            .arg(std::process::id().to_string());
        if config.shell {
            command.args(["/bin/sh", "-c", &config.program]);
        } else {
            command.arg(&config.program).args(&config.args);
        }
        command
            .current_dir(&config.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Dev servers like Vite and Create React App would open the system browser.
            .env("BROWSER", "none")
            .envs(config.env.iter().map(|(key, value)| (key, value)));
        if let Some(path_env) = path_env {
            command.env("PATH", path_env);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            command.process_group(0);
        }
        let mut child = command.spawn()?;
        let pid = child.id();
        if let Ok(mut running) = RUNNING.lock() {
            running.push(pid);
        }
        log::info!(
            "Started dev server {} (pid {pid}): {}",
            config.name,
            config.command_line()
        );

        let (tx, rx) = async_channel::unbounded();
        let log_file = log_path.and_then(|path| {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            std::fs::File::create(&path)
                .inspect_err(|err| log::warn!("Can't write the dev server log {path:?}: {err}"))
                .ok()
        });
        let log_file = Arc::new(std::sync::Mutex::new(log_file));
        if let Some(stdout) = child.stdout.take() {
            spawn_reader(stdout, tx.clone(), log_file.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_reader(stderr, tx.clone(), log_file);
        }

        let exited = Arc::new(AtomicBool::new(false));
        if let Some(port) = config.port {
            let exited = exited.clone();
            let tx = tx.clone();
            std::thread::spawn(move || {
                let started = Instant::now();
                while !exited.load(Ordering::Relaxed) && started.elapsed() < PROBE_TIMEOUT {
                    if port_answers(port) {
                        let _ = tx.try_send(ServerEvent::Ready);
                        return;
                    }
                    std::thread::sleep(PROBE_INTERVAL);
                }
            });
        }
        {
            let exited = exited.clone();
            let name = config.name.clone();
            std::thread::spawn(move || {
                let status = child.wait();
                exited.store(true, Ordering::Relaxed);
                if let Ok(mut running) = RUNNING.lock() {
                    running.retain(|running| *running != pid);
                }
                let description = match status {
                    Ok(status) => match status.code() {
                        Some(code) => format!("saiu com código {code}"),
                        None => "foi encerrado".to_owned(),
                    },
                    Err(err) => format!("falhou: {err}"),
                };
                log::info!("Dev server {name} (pid {pid}) {description}");
                let _ = tx.try_send(ServerEvent::Exited(description));
            });
        }
        Ok((Self { pid, exited }, rx))
    }

    /// Stops the server and every process it started: SIGTERM, then SIGKILL if it lingers.
    pub fn stop(&self) {
        if self.exited.load(Ordering::Relaxed) {
            return;
        }
        #[cfg(unix)]
        {
            let Ok(pgid) = i32::try_from(self.pid) else {
                return;
            };
            // SAFETY: signalling the process group the server was started in.
            unsafe { libc::killpg(pgid, libc::SIGTERM) };
            let exited = self.exited.clone();
            std::thread::spawn(move || {
                let started = Instant::now();
                while started.elapsed() < KILL_GRACE_PERIOD {
                    if exited.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                // SAFETY: as above; the group outlived its grace period.
                unsafe { libc::killpg(pgid, libc::SIGKILL) };
            });
        }
    }
}

/// Sends SIGTERM to every dev server still running and the processes it started.
pub fn stop_all() {
    let Ok(running) = RUNNING.lock() else {
        return;
    };
    #[cfg(unix)]
    for pid in running.iter() {
        if let Ok(pgid) = i32::try_from(*pid) {
            // SAFETY: signalling the process group a dev server was started in.
            unsafe { libc::killpg(pgid, libc::SIGTERM) };
        }
    }
}

fn spawn_reader(
    stream: impl Read + Send + 'static,
    tx: Sender<ServerEvent>,
    log_file: Arc<std::sync::Mutex<Option<std::fs::File>>>,
) {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        let mut buffer = Vec::new();
        loop {
            buffer.clear();
            match reader.read_until(b'\n', &mut buffer) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            let text = String::from_utf8_lossy(&buffer);
            let text = text.trim_end_matches(['\n', '\r']);
            // Progress output redraws its line with carriage returns; keep what's left showing.
            let text = text.rsplit('\r').next().unwrap_or(text);
            let line = strip_ansi(text);
            if let Ok(mut file) = log_file.lock()
                && let Some(file) = file.as_mut()
            {
                let _ = writeln!(file, "{line}");
            }
            if tx.try_send(ServerEvent::Output(line)).is_err() {
                return;
            }
        }
    });
}

/// Whether something accepts connections on `port` on this machine, over IPv4 or IPv6 (Node
/// resolves `localhost` to `::1`).
pub fn port_answers(port: u16) -> bool {
    [
        SocketAddr::from(([127, 0, 0, 1], port)),
        SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], port)),
    ]
    .iter()
    .any(|address| TcpStream::connect_timeout(address, CONNECT_TIMEOUT).is_ok())
}
