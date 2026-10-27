//! Local "Artefatos": self-contained HTML pages Claude creates through tools of an in-process MCP
//! server that the host registers with the CLI (`initialize.sdkMcpServers`) and answers over
//! `mcp_message` control requests.
//!
//! On disk each artifact is a folder `<root>/<id>/` holding `index.html`, `meta.json`, an optional
//! `thumbnail.png` and `versions/<n>.html` snapshots of earlier versions.

use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Name of the in-process MCP server, as registered in `initialize.sdkMcpServers`.
pub const SERVER_NAME: &str = "picolas";

/// Largest accepted HTML body, in bytes.
pub const MAX_HTML_BYTES: usize = 5 * 1024 * 1024;

const MAX_TITLE_CHARS: usize = 200;
const MAX_DESCRIPTION_CHARS: usize = 2000;
const MAX_ID_LEN: usize = 64;
const ID_LEN: usize = 10;
const DEFAULT_PROTOCOL_VERSION: &str = "2025-06-18";

const INDEX_FILE: &str = "index.html";
const META_FILE: &str = "meta.json";
const THUMBNAIL_FILE: &str = "thumbnail.png";
const VERSIONS_DIR: &str = "versions";
const TRASH_DIR: &str = ".trash";

const TOOL_CREATE: &str = "create_artifact";
const TOOL_UPDATE: &str = "update_artifact";
const TOOL_LIST: &str = "list_artifacts";
const TOOL_READ: &str = "read_artifact";
const TOOLS: [&str; 4] = [TOOL_CREATE, TOOL_UPDATE, TOOL_LIST, TOOL_READ];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArtifactKind {
    Document,
    Presentation,
    Design,
    Dashboard,
    Motion,
    #[serde(other)]
    Other,
}

impl ArtifactKind {
    pub const ALL: [ArtifactKind; 6] = [
        ArtifactKind::Document,
        ArtifactKind::Presentation,
        ArtifactKind::Design,
        ArtifactKind::Dashboard,
        ArtifactKind::Motion,
        ArtifactKind::Other,
    ];

    /// The wire name used in tool inputs and `meta.json`.
    pub fn as_str(self) -> &'static str {
        match self {
            ArtifactKind::Document => "document",
            ArtifactKind::Presentation => "presentation",
            ArtifactKind::Design => "design",
            ArtifactKind::Dashboard => "dashboard",
            ArtifactKind::Motion => "motion",
            ArtifactKind::Other => "other",
        }
    }

    /// Case-insensitive inverse of [`ArtifactKind::as_str`].
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        Self::ALL
            .into_iter()
            .find(|kind| kind.as_str().eq_ignore_ascii_case(value))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtifactMeta {
    pub id: String,
    pub title: String,
    pub kind: ArtifactKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The Claude session that created the artifact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Working directory of the session that created the artifact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// Starts at 1 and grows by one on every update.
    pub version: u32,
}

/// Filesystem-backed artifact collection. Holds no in-memory state, so several sessions may share
/// one root.
#[derive(Debug, Clone)]
pub struct ArtifactStore {
    root: PathBuf,
}

impl ArtifactStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// All readable artifacts, most recently updated first. Folders with a missing or corrupt
    /// `meta.json` are skipped.
    pub fn list(&self) -> Vec<ArtifactMeta> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Vec::new(),
            Err(error) => {
                log::warn!(
                    "Failed to read artifacts dir {}: {error}",
                    self.root.display()
                );
                return Vec::new();
            }
        };
        let mut artifacts = entries
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                if !is_valid_id(&name) {
                    return None;
                }
                self.read_meta(&name)
            })
            .collect::<Vec<_>>();
        artifacts.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| b.created_at.cmp(&a.created_at))
                .then_with(|| a.id.cmp(&b.id))
        });
        artifacts
    }

    pub fn get(&self, id: &str) -> Option<ArtifactMeta> {
        if !is_valid_id(id) {
            return None;
        }
        self.read_meta(id)
    }

    pub fn read_html(&self, id: &str) -> Result<String> {
        let path = self.html_path(id)?;
        fs::read_to_string(&path).with_context(|| format!("Failed to read {}", path.display()))
    }

    pub fn create(
        &self,
        title: &str,
        kind: ArtifactKind,
        html: &str,
        description: Option<String>,
        session_id: Option<String>,
        cwd: Option<PathBuf>,
    ) -> Result<ArtifactMeta> {
        let title = validate_title(title)?;
        validate_html(html)?;
        let description = description
            .map(|description| description.trim().to_owned())
            .filter(|description| !description.is_empty())
            .map(|description| truncate_chars(&description, MAX_DESCRIPTION_CHARS));

        fs::create_dir_all(&self.root)
            .with_context(|| format!("Failed to create {}", self.root.display()))?;
        let (id, dir) = self.create_unique_dir()?;

        let now = Utc::now();
        let meta = ArtifactMeta {
            id,
            title,
            kind,
            description,
            created_at: now,
            updated_at: now,
            session_id,
            cwd,
            version: 1,
        };
        let written = write_atomic(&dir.join(INDEX_FILE), html.as_bytes())
            .and_then(|()| write_meta(&dir, &meta));
        if let Err(error) = written {
            let _ = fs::remove_dir_all(&dir);
            return Err(error);
        }
        Ok(meta)
    }

    /// Replaces the HTML, keeping the previous body as `versions/<old version>.html`.
    pub fn update(&self, id: &str, html: &str, title: Option<&str>) -> Result<ArtifactMeta> {
        validate_html(html)?;
        let title = title
            .filter(|title| !title.trim().is_empty())
            .map(validate_title)
            .transpose()?;
        let dir = self.dir(id)?;
        let mut meta = self
            .read_meta(id)
            .ok_or_else(|| anyhow!("Artifact {id} not found"))?;

        let versions_dir = dir.join(VERSIONS_DIR);
        fs::create_dir_all(&versions_dir)
            .with_context(|| format!("Failed to create {}", versions_dir.display()))?;
        let index = dir.join(INDEX_FILE);
        match fs::read(&index) {
            Ok(previous) => write_atomic(
                &versions_dir.join(format!("{}.html", meta.version)),
                &previous,
            )?,
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("Failed to read {}", index.display()));
            }
        }

        write_atomic(&index, html.as_bytes())?;
        meta.version += 1;
        meta.updated_at = Utc::now().max(meta.updated_at);
        if let Some(title) = title {
            meta.title = title;
        }
        write_meta(&dir, &meta)?;
        Ok(meta)
    }

    /// Moves the artifact into `<root>/.trash/` rather than deleting it.
    pub fn delete(&self, id: &str) -> Result<()> {
        let dir = self.dir(id)?;
        if !dir.is_dir() {
            bail!("Artifact {id} not found");
        }
        let trash = self.root.join(TRASH_DIR);
        fs::create_dir_all(&trash)
            .with_context(|| format!("Failed to create {}", trash.display()))?;
        let mut target = trash.join(id);
        if target.exists() {
            target = trash.join(format!("{id}-{}", Utc::now().timestamp_millis()));
        }
        fs::rename(&dir, &target)
            .with_context(|| format!("Failed to move {} to trash", dir.display()))
    }

    pub fn html_path(&self, id: &str) -> Result<PathBuf> {
        Ok(self.dir(id)?.join(INDEX_FILE))
    }

    pub fn thumbnail_path(&self, id: &str) -> Result<PathBuf> {
        Ok(self.dir(id)?.join(THUMBNAIL_FILE))
    }

    /// Path of the snapshot saved when `version` was replaced.
    pub fn version_path(&self, id: &str, version: u32) -> Result<PathBuf> {
        Ok(self
            .dir(id)?
            .join(VERSIONS_DIR)
            .join(format!("{version}.html")))
    }

    fn dir(&self, id: &str) -> Result<PathBuf> {
        if !is_valid_id(id) {
            bail!("Invalid artifact id {id:?}");
        }
        Ok(self.root.join(id))
    }

    fn read_meta(&self, id: &str) -> Option<ArtifactMeta> {
        let path = self.root.join(id).join(META_FILE);
        let contents = match fs::read(&path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == ErrorKind::NotFound => return None,
            Err(error) => {
                log::warn!("Failed to read artifact meta {}: {error}", path.display());
                return None;
            }
        };
        match serde_json::from_slice::<ArtifactMeta>(&contents) {
            Ok(meta) if meta.id == id => Some(meta),
            Ok(meta) => {
                log::warn!(
                    "Skipping artifact {}: meta id {:?} does not match its folder",
                    path.display(),
                    meta.id
                );
                None
            }
            Err(error) => {
                log::warn!("Skipping corrupt artifact meta {}: {error}", path.display());
                None
            }
        }
    }

    fn create_unique_dir(&self) -> Result<(String, PathBuf)> {
        for _ in 0..16 {
            let id = new_id();
            let dir = self.root.join(&id);
            match fs::create_dir(&dir) {
                Ok(()) => return Ok((id, dir)),
                Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("Failed to create {}", dir.display()));
                }
            }
        }
        bail!("Could not allocate a unique artifact id")
    }
}

fn new_id() -> String {
    let uuid = uuid::Uuid::new_v4().simple().to_string();
    uuid[..ID_LEN].to_owned()
}

/// Ids come from Claude's tool input, so they must never be able to escape the store root.
fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && !id.starts_with('.')
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn validate_title(title: &str) -> Result<String> {
    let title = title.trim();
    if title.is_empty() {
        bail!("`title` must not be empty");
    }
    Ok(truncate_chars(title, MAX_TITLE_CHARS))
}

fn validate_html(html: &str) -> Result<()> {
    if html.trim().is_empty() {
        bail!("`html` must not be empty");
    }
    if html.len() > MAX_HTML_BYTES {
        bail!(
            "`html` is {} bytes; the limit is {MAX_HTML_BYTES} bytes",
            html.len()
        );
    }
    Ok(())
}

fn truncate_chars(value: &str, max: usize) -> String {
    match value.char_indices().nth(max) {
        Some((end, _)) => value[..end].to_owned(),
        None => value.to_owned(),
    }
}

fn write_meta(dir: &Path, meta: &ArtifactMeta) -> Result<()> {
    let json = serde_json::to_vec_pretty(meta)?;
    write_atomic(&dir.join(META_FILE), &json)
}

fn write_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("Invalid path {}", path.display()))?;
    let temp = path.with_file_name(format!(
        ".{file_name}.{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let result = (|| -> std::io::Result<()> {
        let mut file = fs::File::create(&temp)?;
        file.write_all(contents)?;
        file.sync_all()?;
        fs::rename(&temp, path)
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&temp);
        return Err(error).with_context(|| format!("Failed to write {}", path.display()));
    }
    Ok(())
}

/// Where tool calls come from; recorded on artifacts the session creates.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ArtifactContext {
    pub session_id: Option<String>,
    pub cwd: Option<PathBuf>,
}

/// Answers one JSON-RPC message of the MCP protocol for the [`SERVER_NAME`] server. Returns the
/// JSON-RPC response to send back in `mcp_response`, or `None` for notifications and responses.
pub fn handle_mcp_message(
    store: &ArtifactStore,
    context: &ArtifactContext,
    message: &Value,
) -> Option<Value> {
    let Some(object) = message.as_object() else {
        return Some(rpc_error(Value::Null, -32600, "Invalid Request"));
    };
    let id = object.get("id").cloned();
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        let is_response = object.contains_key("result") || object.contains_key("error");
        if is_response {
            return None;
        }
        return id.map(|id| rpc_error(id, -32600, "Invalid Request: missing method"));
    };
    let id = id?;
    let params = object.get("params").cloned().unwrap_or(Value::Null);

    let result = match method {
        "initialize" => Ok(initialize_result(&params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_definitions() })),
        "tools/call" => call_tool(store, context, &params),
        _ => Err((-32601, format!("Method not found: {method}"))),
    };
    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => rpc_error(id, code, &message),
    })
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

fn initialize_result(params: &Value) -> Value {
    let protocol_version = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_PROTOCOL_VERSION);
    json!({
        "protocolVersion": protocol_version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
        "instructions": SERVER_INSTRUCTIONS,
    })
}

const SERVER_INSTRUCTIONS: &str = "Saves self-contained HTML pages (documents, slide decks, \
dashboards, design mocks, animations) to the Artefatos gallery of the Picolas app, which renders \
them interactively. Use these tools when the user asks for an artifact or for a page meant to be \
viewed in the app; otherwise answer in the chat as usual.";

const HTML_GUIDANCE: &str = "The `html` must be one complete, self-contained HTML document \
(<!doctype html>, inline <style> and <script>, a <title>). It renders locally in a WebKit view: \
make it responsive, readable in both light and dark mode (use prefers-color-scheme), and do not \
depend on network resources except scripts/styles from cdnjs.cloudflare.com, cdn.jsdelivr.net, \
unpkg.com or Google Fonts; embed images as inline SVG or data URIs. Max 5 MB.";

fn tool_definitions() -> Vec<Value> {
    let kinds = ArtifactKind::ALL.map(ArtifactKind::as_str);
    vec![
        json!({
            "name": TOOL_CREATE,
            "description": format!(
                "Save a new artifact: a standalone interactive HTML page in the user's Artefatos \
                 gallery, shown to the user as a card, so don't repeat its HTML in the chat or \
                 write it to a file. `kind`: document (reports, docs), presentation (slide deck \
                 with keyboard navigation), design (UI mocks, landing pages), dashboard (KPIs, \
                 charts), motion (animations), other. To revise an artifact use update_artifact. \
                 {HTML_GUIDANCE}"
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": {
                        "type": "string",
                        "description": "Short human-readable title shown in the gallery, in the user's language.",
                    },
                    "kind": {
                        "type": "string",
                        "enum": kinds,
                        "description": "Gallery category of the artifact.",
                    },
                    "html": {
                        "type": "string",
                        "description": "The complete self-contained HTML document.",
                    },
                    "description": {
                        "type": "string",
                        "description": "Optional one-sentence summary shown under the title.",
                    },
                },
                "required": ["title", "kind", "html"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": TOOL_UPDATE,
            "description": format!(
                "Replace the HTML of an existing artifact with a new full version (the previous \
                 version is kept in its history). Use it when the user asks to change, fix or \
                 extend an artifact that already exists; call read_artifact first if you do not \
                 have its current HTML. Pass the complete document, not a diff. {HTML_GUIDANCE}"
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Id of the artifact to update." },
                    "html": {
                        "type": "string",
                        "description": "The complete new self-contained HTML document.",
                    },
                    "title": { "type": "string", "description": "Optional new title." },
                },
                "required": ["id", "html"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": TOOL_LIST,
            "description": "List the artifacts in the user's Artefatos gallery (id, title, kind, \
                            last update), newest first. Use it to find the id of an artifact the \
                            user refers to before reading or updating it.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "annotations": { "readOnlyHint": true },
        }),
        json!({
            "name": TOOL_READ,
            "description": "Return the current HTML of an artifact so you can review or edit it \
                            before calling update_artifact.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Id of the artifact to read." },
                },
                "required": ["id"],
                "additionalProperties": false,
            },
            "annotations": { "readOnlyHint": true },
        }),
    ]
}

fn call_tool(
    store: &ArtifactStore,
    context: &ArtifactContext,
    params: &Value,
) -> Result<Value, (i64, String)> {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return Err((-32602, "Invalid params: missing tool name".to_owned()));
    };
    let empty = json!({});
    let arguments = match params.get("arguments") {
        None | Some(Value::Null) => &empty,
        Some(arguments) if arguments.is_object() => arguments,
        Some(_) => return Ok(tool_error("`arguments` must be an object")),
    };
    let outcome = match name {
        TOOL_CREATE => create_tool(store, context, arguments),
        TOOL_UPDATE => update_tool(store, arguments),
        TOOL_LIST => Ok(list_tool(store)),
        TOOL_READ => read_tool(store, arguments),
        _ => return Err((-32602, format!("Unknown tool: {name}"))),
    };
    Ok(outcome.unwrap_or_else(|error| tool_error(&format!("{error:#}"))))
}

fn create_tool(
    store: &ArtifactStore,
    context: &ArtifactContext,
    arguments: &Value,
) -> Result<Value> {
    let title = required_str(arguments, "title")?;
    let kind_name = required_str(arguments, "kind")?;
    let kind = ArtifactKind::parse(kind_name).ok_or_else(|| {
        let kinds = ArtifactKind::ALL.map(ArtifactKind::as_str).join(", ");
        anyhow!("Invalid `kind` {kind_name:?}; expected one of: {kinds}")
    })?;
    let html = required_str(arguments, "html")?;
    let description = optional_str(arguments, "description")?.map(str::to_owned);
    let meta = store.create(
        title,
        kind,
        html,
        description,
        context.session_id.clone(),
        context.cwd.clone(),
    )?;
    Ok(tool_success(
        format!("Created artifact {} “{}”", meta.id, meta.title),
        meta_json(store, &meta),
    ))
}

fn update_tool(store: &ArtifactStore, arguments: &Value) -> Result<Value> {
    let id = required_str(arguments, "id")?;
    let html = required_str(arguments, "html")?;
    let title = optional_str(arguments, "title")?;
    let meta = store.update(id, html, title)?;
    Ok(tool_success(
        format!(
            "Updated artifact {} “{}” to version {}",
            meta.id, meta.title, meta.version
        ),
        meta_json(store, &meta),
    ))
}

fn list_tool(store: &ArtifactStore) -> Value {
    let artifacts = store.list();
    let text = if artifacts.is_empty() {
        "No artifacts yet.".to_owned()
    } else {
        artifacts
            .iter()
            .map(|meta| {
                format!(
                    "- {} “{}” ({}, updated {})",
                    meta.id,
                    meta.title,
                    meta.kind.as_str(),
                    meta.updated_at.to_rfc3339()
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let items = artifacts
        .iter()
        .map(|meta| {
            json!({
                "id": meta.id,
                "title": meta.title,
                "kind": meta.kind,
                "updated_at": meta.updated_at,
            })
        })
        .collect::<Vec<_>>();
    tool_success(text, json!({ "artifacts": items }))
}

fn read_tool(store: &ArtifactStore, arguments: &Value) -> Result<Value> {
    let id = required_str(arguments, "id")?;
    let meta = store
        .get(id)
        .ok_or_else(|| anyhow!("Artifact {id:?} not found; use list_artifacts to find ids"))?;
    let html = store.read_html(id)?;
    Ok(tool_success(
        html,
        json!({
            "id": meta.id,
            "title": meta.title,
            "kind": meta.kind,
            "version": meta.version,
        }),
    ))
}

fn meta_json(store: &ArtifactStore, meta: &ArtifactMeta) -> Value {
    let path = store
        .html_path(&meta.id)
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    json!({
        "id": meta.id,
        "title": meta.title,
        "kind": meta.kind,
        "version": meta.version,
        "path": path,
    })
}

fn required_str<'a>(arguments: &'a Value, key: &str) -> Result<&'a str> {
    match arguments.get(key) {
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(value),
        Some(Value::String(_)) => bail!("`{key}` must not be empty"),
        None | Some(Value::Null) => bail!("Missing required argument `{key}`"),
        Some(_) => bail!("`{key}` must be a string"),
    }
}

fn optional_str<'a>(arguments: &'a Value, key: &str) -> Result<Option<&'a str>> {
    match arguments.get(key) {
        Some(Value::String(value)) => Ok(Some(value)),
        None | Some(Value::Null) => Ok(None),
        Some(_) => bail!("`{key}` must be a string"),
    }
}

fn tool_success(text: String, structured: Value) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": structured,
    })
}

fn tool_error(message: &str) -> Value {
    json!({
        "content": [{ "type": "text", "text": message }],
        "isError": true,
    })
}

/// The tool names Claude sees for this server (`mcp__picolas__<tool>`).
pub fn tool_names() -> Vec<String> {
    TOOLS.iter().map(|tool| full_tool_name(tool)).collect()
}

fn full_tool_name(tool: &str) -> String {
    format!("mcp__{SERVER_NAME}__{tool}")
}

/// Whether `tool_name` is one of this server's tools that creates or changes an artifact.
pub fn is_artifact_tool(tool_name: &str) -> bool {
    bare_tool_name(tool_name).is_some_and(|tool| TOOLS.contains(&tool))
}

/// Whether `tool_name` creates or updates an artifact, as opposed to listing or reading them.
pub fn makes_artifact(tool_name: &str) -> bool {
    matches!(
        bare_tool_name(tool_name),
        Some("create_artifact" | "update_artifact")
    )
}

fn bare_tool_name(tool_name: &str) -> Option<&str> {
    tool_name
        .strip_prefix("mcp__")?
        .strip_prefix(SERVER_NAME)?
        .strip_prefix("__")
}

/// The artifact a tool call refers to, so the chat can render an artifact card for it.
///
/// `result` may be the tool's `structuredContent`, the whole MCP result, or the plain
/// `tool_result` content (a string or an array of text blocks) as it appears in the transcript.
pub fn artifact_id_from_tool_result(
    tool_name: &str,
    input: &Value,
    result: Option<&Value>,
) -> Option<String> {
    match bare_tool_name(tool_name)? {
        TOOL_CREATE => result.and_then(id_from_result),
        TOOL_UPDATE | TOOL_READ => input
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| is_valid_id(id))
            .map(str::to_owned),
        _ => None,
    }
}

fn id_from_result(result: &Value) -> Option<String> {
    match result {
        Value::Object(object) => {
            if object.get("isError").and_then(Value::as_bool) == Some(true) {
                return None;
            }
            object
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| is_valid_id(id))
                .map(str::to_owned)
                .or_else(|| object.get("structuredContent").and_then(id_from_result))
                .or_else(|| object.get("content").and_then(id_from_result))
                .or_else(|| object.get("text").and_then(id_from_result))
        }
        Value::Array(items) => items.iter().find_map(id_from_result),
        Value::String(text) => id_from_created_text(text),
        Value::Null | Value::Bool(_) | Value::Number(_) => None,
    }
}

fn id_from_created_text(text: &str) -> Option<String> {
    let rest = text.split("Created artifact ").nth(1)?;
    let id = rest.split_whitespace().next()?;
    is_valid_id(id).then(|| id.to_owned())
}

/// Gallery label for a kind, in pt-BR.
pub fn kind_label_pt(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Document => "Documento",
        ArtifactKind::Presentation => "Apresentação",
        ArtifactKind::Design => "Design",
        ArtifactKind::Dashboard => "Painel",
        ArtifactKind::Motion => "Motion",
        ArtifactKind::Other => "Artefato",
    }
}

/// Composer text prefilled when the user starts a new artifact of `kind` from the gallery.
pub fn starter_prompt_pt(kind: ArtifactKind) -> String {
    match kind {
        ArtifactKind::Document => "Crie um documento sobre: ",
        ArtifactKind::Presentation => "Crie uma apresentação de slides sobre: ",
        ArtifactKind::Design => "Crie um design de interface para: ",
        ArtifactKind::Dashboard => "Crie um painel interativo com: ",
        ArtifactKind::Motion => "Crie uma animação sobre: ",
        ArtifactKind::Other => "Crie um artefato interativo sobre: ",
    }
    .to_owned()
}

#[cfg(test)]
#[path = "artifacts_tests.rs"]
mod tests;
