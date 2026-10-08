//! Saved projects organized by tags, behind the Projects panel of the vertical
//! tabs sidebar. Works like the "Project Manager" VS Code extension
//! (alefragnani.project-manager) and uses the same `projects.json` format:
//!
//! ```json
//! [
//!     {
//!         "name": "Kinbox",
//!         "rootPath": "/Users/me/projects/aldeia/kinbox",
//!         "paths": [],
//!         "tags": ["Aldeia"],
//!         "enabled": true,
//!         "profile": ""
//!     }
//! ]
//! ```
//!
//! The list lives in `~/.warp-oss/projects.json`. The first time it's missing,
//! it's copied from the extension's file, so the projects saved in VS Code show
//! up right away. View preferences (list or tags, sort order, collapsed tags)
//! live in `~/.warp-oss/project_manager.toml`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const PROJECTS_FILE_NAME: &str = "projects.json";
const PREFS_FILE_NAME: &str = "project_manager.toml";
/// Where the VS Code extension keeps its list (under the user config dir).
const VSCODE_PROJECTS_FILE: &str =
    "Code/User/globalStorage/alefragnani.project-manager/projects.json";
/// Group of the projects without tags in the tags view.
pub const NO_TAG_LABEL: &str = "Sem tag";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub name: String,
    pub root_path: String,
    /// Extra folders of a multi-root project (kept for compatibility).
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Disabled projects stay in the file but aren't listed.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// VS Code profile (kept for compatibility).
    #[serde(default)]
    pub profile: String,
}

fn default_enabled() -> bool {
    true
}

impl Project {
    pub fn new(path: &Path) -> Self {
        Self {
            name: folder_name(path),
            root_path: path.display().to_string(),
            paths: Vec::new(),
            tags: Vec::new(),
            enabled: true,
            profile: String::new(),
        }
    }

    /// The project folder, with `~` and `$home` expanded like the extension does.
    pub fn root(&self) -> PathBuf {
        expand_home(&self.root_path)
    }
}

fn folder_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn expand_home(path: &str) -> PathBuf {
    let rest = path
        .strip_prefix("~/")
        .or_else(|| path.strip_prefix("$home/"));
    match (rest, dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(path),
    }
}

pub fn projects_file() -> PathBuf {
    warp_core::paths::data_dir().join(PROJECTS_FILE_NAME)
}

fn vscode_projects_file() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join(VSCODE_PROJECTS_FILE))
}

/// Reads the saved projects, importing the VS Code extension's list the first
/// time `~/.warp-oss/projects.json` doesn't exist.
pub fn load_projects() -> Result<Vec<Project>, String> {
    let path = projects_file();
    if !path.exists()
        && let Some(vscode_file) = vscode_projects_file().filter(|file| file.exists())
    {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        }
        std::fs::copy(&vscode_file, &path).map_err(|err| err.to_string())?;
    }
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return Ok(Vec::new());
    };
    parse_projects(&contents).map_err(|err| format!("erro em {}: {err}", path.display()))
}

pub fn parse_projects(contents: &str) -> serde_json::Result<Vec<Project>> {
    if contents.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(contents)
}

/// Writes the list with the same 4-space indentation the extension uses.
pub fn save_projects(projects: &[Project]) -> std::io::Result<()> {
    let mut contents = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"    ");
    let mut serializer = serde_json::Serializer::with_formatter(&mut contents, formatter);
    projects
        .serialize(&mut serializer)
        .map_err(std::io::Error::other)?;
    let path = projects_file();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, contents)
}

/// Makes sure the file exists, so it can be opened in the editor.
pub fn ensure_projects_file(projects: &[Project]) -> std::io::Result<PathBuf> {
    let path = projects_file();
    if !path.exists() {
        save_projects(projects)?;
    }
    Ok(path)
}

/// Order of the projects, like the extension's `projectManager.sortList`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortOrder {
    /// The order of the file.
    Saved,
    #[default]
    Name,
    Path,
}

impl SortOrder {
    pub fn next(self) -> Self {
        match self {
            Self::Saved => Self::Name,
            Self::Name => Self::Path,
            Self::Path => Self::Saved,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Saved => "Ordem: como salvos",
            Self::Name => "Ordem: nome",
            Self::Path => "Ordem: caminho",
        }
    }
}

/// How the Projects panel shows the list; saved between sessions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewPrefs {
    /// A flat list instead of groups by tag.
    #[serde(default)]
    pub view_as_list: bool,
    #[serde(default)]
    pub sort: SortOrder,
    #[serde(default)]
    pub collapsed_tags: Vec<String>,
}

fn prefs_file() -> PathBuf {
    warp_core::paths::data_dir().join(PREFS_FILE_NAME)
}

pub fn load_prefs() -> ViewPrefs {
    std::fs::read_to_string(prefs_file())
        .ok()
        .and_then(|contents| toml::from_str(&contents).ok())
        .unwrap_or_default()
}

pub fn save_prefs(prefs: &ViewPrefs) -> std::io::Result<()> {
    let contents = toml::to_string_pretty(prefs).map_err(std::io::Error::other)?;
    let path = prefs_file();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, contents)
}

/// Every tag used by the projects, sorted.
pub fn all_tags(projects: &[Project]) -> Vec<String> {
    projects
        .iter()
        .flat_map(|project| project.tags.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Whether the project matches the search box: name or folder contains the
/// query, ignoring case.
pub fn matches_query(project: &Project, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty()
        || project.name.to_lowercase().contains(&query)
        || project.root_path.to_lowercase().contains(&query)
}

/// Whether the project passes the tag filter (any selected tag, or "Sem tag"
/// for projects without tags). An empty filter lets everything through.
pub fn matches_tag_filter(project: &Project, filter: &[String]) -> bool {
    filter.is_empty()
        || filter.iter().any(|tag| {
            project.tags.contains(tag) || (tag == NO_TAG_LABEL && project.tags.is_empty())
        })
}

/// Indices of the enabled projects that pass the search and the tag filter,
/// in the chosen order.
pub fn visible_projects(
    projects: &[Project],
    query: &str,
    tag_filter: &[String],
    sort: SortOrder,
) -> Vec<usize> {
    let mut indices: Vec<usize> = projects
        .iter()
        .enumerate()
        .filter(|(_, project)| {
            project.enabled
                && matches_query(project, query)
                && matches_tag_filter(project, tag_filter)
        })
        .map(|(index, _)| index)
        .collect();
    match sort {
        SortOrder::Saved => {}
        SortOrder::Name => indices.sort_by_cached_key(|&i| projects[i].name.to_lowercase()),
        SortOrder::Path => indices.sort_by_cached_key(|&i| projects[i].root_path.to_lowercase()),
    }
    indices
}

/// The visible projects grouped by tag, tags sorted by name and "Sem tag"
/// last. A project with several tags shows up in each of them.
pub fn group_by_tag(projects: &[Project], visible: &[usize]) -> Vec<(String, Vec<usize>)> {
    let mut groups: Vec<(String, Vec<usize>)> = all_tags(projects)
        .into_iter()
        .map(|tag| {
            let members: Vec<usize> = visible
                .iter()
                .copied()
                .filter(|&i| projects[i].tags.contains(&tag))
                .collect();
            (tag, members)
        })
        .filter(|(_, members)| !members.is_empty())
        .collect();
    let untagged: Vec<usize> = visible
        .iter()
        .copied()
        .filter(|&i| projects[i].tags.is_empty())
        .collect();
    if !untagged.is_empty() {
        groups.push((NO_TAG_LABEL.to_owned(), untagged));
    }
    groups
}

/// Tags typed as "Aldeia, Estudos": trimmed, without blanks or repeats.
pub fn parse_tags(input: &str) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    for tag in input.split(',').map(str::trim) {
        if !tag.is_empty() && !tags.iter().any(|existing| existing == tag) {
            tags.push(tag.to_owned());
        }
    }
    tags
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
