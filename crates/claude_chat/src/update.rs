//! Tells when a newer Claude Code is out and installs it: the installed version comes from
//! `claude --version`, the latest from the npm registry.

use std::cmp::Ordering;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use command::r#async::Command;
use futures::future::{self, Either};
use serde_json::Value;

/// The npm registry's record of the newest published Claude Code.
pub const LATEST_VERSION_URL: &str = "https://registry.npmjs.org/@anthropic-ai/claude-code/latest";
const PACKAGE: &str = "@anthropic-ai/claude-code@latest";
const VERSION_TIMEOUT: Duration = Duration::from_secs(20);
const UPDATE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// How much of a failed command's output an error message keeps.
const ERROR_TAIL_CHARS: usize = 300;

/// A `major.minor.patch` version with an optional pre-release, ordered like semver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    numbers: [u64; 3],
    pre_release: Option<String>,
}

impl Version {
    /// Parses `1.2.3`, `v1.2.3`, `1.2.3-beta.1` or `1.2.3+build`.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim().trim_start_matches('v');
        let text = text.split_once('+').map_or(text, |(version, _)| version);
        let (core, pre_release) = match text.split_once('-') {
            Some((core, pre_release)) if !pre_release.is_empty() => {
                (core, Some(pre_release.to_owned()))
            }
            Some(_) => return None,
            None => (text, None),
        };
        let mut parts = core.split('.');
        let mut numbers = [0; 3];
        for number in &mut numbers {
            let part = parts.next()?;
            if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            *number = part.parse().ok()?;
        }
        if parts.next().is_some() {
            return None;
        }
        Some(Self {
            numbers,
            pre_release,
        })
    }

    /// The first version among `text`'s words, like in `2.1.291 (Claude Code)`.
    pub fn find_in(text: &str) -> Option<Self> {
        text.split_whitespace().find_map(Self::parse)
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.numbers.cmp(&other.numbers).then_with(|| {
            match (&self.pre_release, &other.pre_release) {
                (None, None) => Ordering::Equal,
                // A release is newer than its pre-releases.
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(left), Some(right)) => compare_pre_releases(left, right),
            }
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [major, minor, patch] = self.numbers;
        write!(f, "{major}.{minor}.{patch}")?;
        if let Some(pre_release) = &self.pre_release {
            write!(f, "-{pre_release}")?;
        }
        Ok(())
    }
}

/// Semver's pre-release order: numeric identifiers by value and below alphanumeric ones, and a
/// shorter list below a longer one it starts.
fn compare_pre_releases(left: &str, right: &str) -> Ordering {
    let mut left = left.split('.');
    let mut right = right.split('.');
    loop {
        let ordering = match (left.next(), right.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(left), Some(right)) => match (left.parse::<u64>(), right.parse::<u64>()) {
                (Ok(left), Ok(right)) => left.cmp(&right),
                (Ok(_), Err(_)) => Ordering::Less,
                (Err(_), Ok(_)) => Ordering::Greater,
                (Err(_), Err(_)) => left.cmp(right),
            },
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
}

/// The version in the registry's answer for [`LATEST_VERSION_URL`].
pub fn parse_registry_latest(body: &str) -> Option<Version> {
    let record: Value = serde_json::from_str(body).ok()?;
    Version::parse(record.get("version")?.as_str()?)
}

/// The version `claude --version` reports.
pub async fn installed_version(claude_path: &Path, path_env: Option<&str>) -> Option<Version> {
    let mut command = Command::new(claude_path);
    command.arg("--version");
    let output = run(command, path_env, VERSION_TIMEOUT).await.ok()?;
    if !output.status.success() {
        return None;
    }
    Version::find_in(&String::from_utf8_lossy(&output.stdout))
}

/// A way to install the latest Claude Code, in the order to try them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateMethod {
    /// `claude update`, which knows how Claude Code was installed.
    ClaudeUpdate,
    /// `npm install -g @anthropic-ai/claude-code@latest`.
    Npm,
}

/// Runs `method`; the error is the end of what the command printed.
pub async fn run_update(
    method: UpdateMethod,
    claude_path: &Path,
    path_env: Option<&str>,
) -> Result<(), String> {
    let command = match method {
        UpdateMethod::ClaudeUpdate => {
            let mut command = Command::new(claude_path);
            command.arg("update");
            command
        }
        UpdateMethod::Npm => {
            let npm = find_in_path("npm", path_env).ok_or("npm não encontrado")?;
            let mut command = Command::new(npm);
            command.args(["install", "-g", PACKAGE]);
            command
        }
    };
    let output = run(command, path_env, UPDATE_TIMEOUT).await?;
    if output.status.success() {
        return Ok(());
    }
    let mut message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if message.is_empty() {
        message = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    }
    Err(tail(&message, ERROR_TAIL_CHARS))
}

fn find_in_path(program: &str, path_env: Option<&str>) -> Option<PathBuf> {
    std::env::split_paths(path_env?)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

async fn run(
    mut command: Command,
    path_env: Option<&str>,
    timeout: Duration,
) -> Result<Output, String> {
    if let Some(path_env) = path_env {
        command.env("PATH", path_env);
    }
    command.kill_on_drop(true);
    let output = Box::pin(command.output());
    match future::select(output, Box::pin(async_io::Timer::after(timeout))).await {
        Either::Left((output, _)) => output.map_err(|err| err.to_string()),
        Either::Right(_) => Err("o comando demorou demais".to_owned()),
    }
}

fn tail(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_owned();
    }
    let tail: String = text.chars().skip(count - max_chars).collect();
    format!("…{tail}")
}

#[cfg(test)]
#[path = "update_tests.rs"]
mod tests;
