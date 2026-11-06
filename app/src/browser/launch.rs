//! The dev servers listed in a project's `.claude/launch.json`, the file Claude Code's preview
//! reads: VS Code launch.json-like `configurations` with a `name`, a `runtimeExecutable` and its
//! `runtimeArgs`, a `port`, and optionally `cwd`, `env` and `url`. Reading is tolerant: comments,
//! trailing commas, `command`/`args`/`program` spellings and ports given only in the arguments
//! all work.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow};
use serde_json::{Map, Value, json};

/// Where a project lists its dev servers, relative to the project's folder.
pub const LAUNCH_FILE: &str = ".claude/launch.json";

/// A dev server from a launch file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevServerConfig {
    pub name: String,
    /// The executable, or a whole command line when `shell` is set.
    pub program: String,
    pub args: Vec<String>,
    /// Whether `program` is a command line to run with `sh -c`.
    pub shell: bool,
    pub cwd: PathBuf,
    pub port: Option<u16>,
    /// The page to open instead of `http://localhost:<port>`.
    pub url: Option<String>,
    pub env: Vec<(String, String)>,
    /// The folder whose launch file lists the server.
    pub project_dir: PathBuf,
}

impl DevServerConfig {
    /// Identifies the server across reloads of its launch file.
    pub fn key(&self) -> String {
        format!("{}#{}", self.project_dir.display(), self.name)
    }

    /// The page the server serves, if known.
    pub fn page_url(&self) -> Option<String> {
        self.url
            .clone()
            .or_else(|| self.port.map(|port| format!("http://localhost:{port}")))
    }

    /// The command as typed in a shell, for showing.
    pub fn command_line(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// A project's launch file as read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchFile {
    Missing,
    Invalid(String),
    Servers(Vec<DevServerConfig>),
}

pub fn launch_file_path(project_dir: &Path) -> PathBuf {
    project_dir.join(LAUNCH_FILE)
}

pub fn read_launch_file(project_dir: &Path) -> LaunchFile {
    let path = launch_file_path(project_dir);
    match std::fs::read_to_string(&path) {
        Ok(text) => match parse_launch_file(&text, project_dir) {
            Ok(servers) => LaunchFile::Servers(servers),
            Err(err) => LaunchFile::Invalid(format!("{err:#}")),
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => LaunchFile::Missing,
        Err(err) => LaunchFile::Invalid(err.to_string()),
    }
}

/// Parses a launch file's text; relative folders are taken from `project_dir`.
pub fn parse_launch_file(text: &str, project_dir: &Path) -> Result<Vec<DevServerConfig>> {
    let value: Value = serde_json::from_str(&strip_jsonc(text)).context("JSON inválido")?;
    let configurations = match &value {
        Value::Array(configurations) => configurations,
        Value::Object(object) => match object.get("configurations") {
            Some(Value::Array(configurations)) => configurations,
            Some(_) => return Err(anyhow!("\"configurations\" deve ser uma lista")),
            None if object.contains_key("runtimeExecutable") || object.contains_key("command") => {
                std::slice::from_ref(&value)
            }
            None => return Ok(Vec::new()),
        },
        _ => return Err(anyhow!("esperava um objeto com \"configurations\"")),
    };
    Ok(configurations
        .iter()
        .enumerate()
        .filter_map(|(index, configuration)| {
            parse_configuration(configuration.as_object()?, index, project_dir)
        })
        .collect())
}

fn parse_configuration(
    object: &Map<String, Value>,
    index: usize,
    project_dir: &Path,
) -> Option<DevServerConfig> {
    let string = |key: &str| {
        object
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| expand_variables(value, project_dir))
    };
    let mut args = ["runtimeArgs", "args"]
        .iter()
        .flat_map(|key| string_list(object.get(*key), project_dir))
        .collect::<Vec<_>>();
    let program = string("program");
    let (program, shell) = match (string("runtimeExecutable"), string("command"), program) {
        (Some(executable), _, program) => {
            if let Some(program) = program {
                args.insert(0, program);
            }
            (executable, false)
        }
        (None, Some(command), _) => {
            let shell = args.is_empty() && command.contains(char::is_whitespace);
            (command, shell)
        }
        (None, None, Some(program)) => (program, false),
        (None, None, None) => return None,
    };

    let url = string("url");
    let port = object
        .get("port")
        .and_then(|port| match port {
            Value::Number(number) => number.as_u64().and_then(|port| u16::try_from(port).ok()),
            Value::String(text) => text.trim().parse().ok(),
            _ => None,
        })
        .or_else(|| url.as_deref().and_then(port_of_url))
        .or_else(|| {
            let words = if shell {
                program.split_whitespace().map(str::to_owned).collect()
            } else {
                args.clone()
            };
            port_in_args(&words)
        });
    let cwd = match string("cwd") {
        Some(cwd) if Path::new(&cwd).is_absolute() => PathBuf::from(cwd),
        Some(cwd) => project_dir.join(cwd),
        None => project_dir.to_path_buf(),
    };
    let env = object
        .get("env")
        .and_then(Value::as_object)
        .map(|env| {
            env.iter()
                .filter_map(|(key, value)| {
                    let value = match value {
                        Value::String(text) => expand_variables(text, project_dir),
                        Value::Number(number) => number.to_string(),
                        Value::Bool(flag) => flag.to_string(),
                        Value::Null | Value::Array(_) | Value::Object(_) => return None,
                    };
                    Some((key.clone(), value))
                })
                .collect()
        })
        .unwrap_or_default();
    Some(DevServerConfig {
        name: string("name").unwrap_or_else(|| format!("Servidor {}", index + 1)),
        program,
        args,
        shell,
        cwd,
        port,
        url,
        env,
        project_dir: project_dir.to_path_buf(),
    })
}

fn string_list(value: Option<&Value>, project_dir: &Path) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| match item {
                Value::String(text) => Some(expand_variables(text, project_dir)),
                Value::Number(number) => Some(number.to_string()),
                _ => None,
            })
            .collect(),
        Some(Value::String(text)) => text
            .split_whitespace()
            .map(|word| expand_variables(word, project_dir))
            .collect(),
        _ => Vec::new(),
    }
}

/// Replaces VS Code's `${workspaceFolder}` (and `${workspaceRoot}`) with the project's folder.
fn expand_variables(text: &str, project_dir: &Path) -> String {
    let dir = project_dir.display().to_string();
    text.replace("${workspaceFolder}", &dir)
        .replace("${workspaceRoot}", &dir)
}

fn port_of_url(url: &str) -> Option<u16> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let host = rest.split(['/', '?', '#']).next()?;
    host.rsplit_once(':')?.1.parse().ok()
}

/// A port passed like `--port 5173`, `--port=5173`, `-p 5173` or `PORT=5173`.
pub fn port_in_args(args: &[String]) -> Option<u16> {
    args.iter().enumerate().find_map(|(index, arg)| {
        if let Some(value) = arg
            .strip_prefix("--port=")
            .or_else(|| arg.strip_prefix("PORT="))
        {
            return value.parse().ok();
        }
        if matches!(arg.as_str(), "--port" | "-p") {
            return args.get(index + 1)?.parse().ok();
        }
        None
    })
}

/// Drops `//` and `/* */` comments and trailing commas, which VS Code's launch files allow.
fn strip_jsonc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            match c {
                '\\' => {
                    if let Some(escaped) = chars.next() {
                        out.push(escaped);
                    }
                }
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match (c, chars.peek().copied()) {
            ('"', _) => {
                in_string = true;
                out.push(c);
            }
            ('/', Some('/')) => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut previous = ' ';
                for c in chars.by_ref() {
                    if previous == '*' && c == '/' {
                        break;
                    }
                    previous = c;
                }
            }
            _ => out.push(c),
        }
    }
    remove_trailing_commas(&out)
}

fn remove_trailing_commas(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    let mut escaped = false;
    for (index, c) in text.char_indices() {
        if in_string {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            out.push(c);
            continue;
        }
        if c == '"' {
            in_string = true;
        }
        if c == ',' && text[index + 1..].trim_start().starts_with(['}', ']']) {
            continue;
        }
        out.push(c);
    }
    out
}

/// A dev server guessed from a project's files, for a new launch file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuessedServer {
    pub name: String,
    pub executable: String,
    pub args: Vec<String>,
    pub port: Option<u16>,
}

/// Guesses the project's dev server from its `package.json` (`dev` or `start` script), else from
/// a Django or static site, else assumes `npm run dev`.
pub fn guess_server(project_dir: &Path) -> GuessedServer {
    let folder_name = project_dir.file_name().map_or_else(
        || "app".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    if let Ok(package_json) = std::fs::read_to_string(project_dir.join("package.json"))
        && let Some(guess) =
            guess_from_package_json(&package_json, package_manager(project_dir), &folder_name)
    {
        return guess;
    }
    if project_dir.join("manage.py").is_file() {
        return GuessedServer {
            name: folder_name,
            executable: "python3".to_owned(),
            args: vec!["manage.py".into(), "runserver".into(), "8000".into()],
            port: Some(8000),
        };
    }
    if project_dir.join("index.html").is_file() {
        return GuessedServer {
            name: folder_name,
            executable: "python3".to_owned(),
            args: vec!["-m".into(), "http.server".into(), "8000".into()],
            port: Some(8000),
        };
    }
    GuessedServer {
        name: folder_name,
        executable: "npm".to_owned(),
        args: vec!["run".into(), "dev".into()],
        port: None,
    }
}

/// The package manager a JavaScript project uses, going by its lock file.
fn package_manager(project_dir: &Path) -> &'static str {
    [
        ("pnpm-lock.yaml", "pnpm"),
        ("yarn.lock", "yarn"),
        ("bun.lockb", "bun"),
        ("bun.lock", "bun"),
    ]
    .into_iter()
    .find(|(lock_file, _)| project_dir.join(lock_file).is_file())
    .map_or("npm", |(_, manager)| manager)
}

pub fn guess_from_package_json(
    package_json: &str,
    package_manager: &str,
    folder_name: &str,
) -> Option<GuessedServer> {
    let package: Value = serde_json::from_str(package_json).ok()?;
    let scripts = package.get("scripts")?.as_object()?;
    let (script_name, script) = ["dev", "start", "serve"]
        .into_iter()
        .find_map(|name| Some((name, scripts.get(name)?.as_str()?)))?;
    let has_dependency = |name: &str| {
        ["dependencies", "devDependencies"].iter().any(|section| {
            package
                .get(section)
                .and_then(Value::as_object)
                .is_some_and(|deps| deps.contains_key(name))
        })
    };
    let words: Vec<String> = script.split_whitespace().map(str::to_owned).collect();
    let mentions = |tool: &str| words.iter().any(|word| word == tool) || has_dependency(tool);
    let port = port_in_args(&words).or_else(|| {
        if mentions("vite") || has_dependency("@sveltejs/kit") {
            Some(5173)
        } else if mentions("next") || mentions("react-scripts") || mentions("nuxt") {
            Some(3000)
        } else if mentions("astro") {
            Some(4321)
        } else if mentions("ng") || has_dependency("@angular/core") {
            Some(4200)
        } else {
            None
        }
    });
    let name = package
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .map_or_else(|| folder_name.to_owned(), str::to_owned);
    let args = match (package_manager, script_name) {
        ("npm", "start") => vec!["start".to_owned()],
        ("npm", _) => vec!["run".to_owned(), script_name.to_owned()],
        (_, _) => vec![script_name.to_owned()],
    };
    Some(GuessedServer {
        name,
        executable: package_manager.to_owned(),
        args,
        port,
    })
}

/// A launch file listing `server`, in the format Claude Code's preview reads.
pub fn launch_file_text(server: &GuessedServer) -> String {
    let mut configuration = json!({
        "name": server.name,
        "runtimeExecutable": server.executable,
        "runtimeArgs": server.args,
    });
    if let Some(port) = server.port {
        configuration["port"] = json!(port);
    }
    let file = json!({
        "version": "0.0.1",
        "configurations": [configuration],
    });
    let mut text = serde_json::to_string_pretty(&file).unwrap_or_default();
    text.push('\n');
    text
}

/// Writes a launch file with the project's guessed dev server, unless the project has one.
pub fn create_launch_file(project_dir: &Path) -> Result<PathBuf> {
    let path = launch_file_path(project_dir);
    if path.exists() {
        return Ok(path);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, launch_file_text(&guess_server(project_dir)))?;
    Ok(path)
}

#[cfg(test)]
#[path = "launch_tests.rs"]
mod tests;
