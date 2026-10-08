//! Git status decorations for the file tree, like VS Code's: changed files are
//! tinted and tagged with a letter, and folders containing changes get a dot.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use pathfinder_color::ColorU;
use warp_util::git::run_git_command;

use crate::code::vscode_appearance;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitFileStatus {
    Modified,
    Added,
    Renamed,
    Untracked,
    Deleted,
    Conflicted,
}

impl GitFileStatus {
    pub fn letter(self) -> &'static str {
        match self {
            Self::Modified => "M",
            Self::Added => "A",
            Self::Renamed => "R",
            Self::Untracked => "U",
            Self::Deleted => "D",
            Self::Conflicted => "!",
        }
    }

    /// The VS Code theme's `gitDecoration.*` color, or VS Code's default.
    pub fn color(self) -> ColorU {
        let theme_color = vscode_appearance::palette().and_then(|palette| {
            let decorations = &palette.git_decorations;
            match self {
                Self::Modified => decorations.modified,
                Self::Added => decorations.added,
                Self::Renamed => decorations.renamed,
                Self::Untracked => decorations.untracked,
                Self::Deleted => decorations.deleted,
                Self::Conflicted => decorations.conflicting,
            }
        });
        theme_color.unwrap_or(match self {
            Self::Modified => ColorU::new(0xe2, 0xc0, 0x8d, 0xff),
            Self::Added => ColorU::new(0x81, 0xb8, 0x8b, 0xff),
            Self::Renamed | Self::Untracked => ColorU::new(0x73, 0xc9, 0x91, 0xff),
            Self::Deleted => ColorU::new(0xc7, 0x4e, 0x39, 0xff),
            Self::Conflicted => ColorU::new(0xe4, 0x67, 0x6b, 0xff),
        })
    }

    /// Which status a folder shows when it contains several.
    fn priority(self) -> u8 {
        match self {
            Self::Conflicted => 5,
            Self::Modified => 4,
            Self::Deleted => 3,
            Self::Added | Self::Renamed => 2,
            Self::Untracked => 1,
        }
    }

    fn from_porcelain(code: &str) -> Option<Self> {
        let mut chars = code.chars();
        let (index, worktree) = (chars.next()?, chars.next()?);
        Some(match (index, worktree) {
            ('?', '?') => Self::Untracked,
            ('!', '!') => return None,
            ('U', _) | (_, 'U') | ('A', 'A') | ('D', 'D') => Self::Conflicted,
            ('D', _) | (_, 'D') => Self::Deleted,
            ('R', _) | (_, 'R') => Self::Renamed,
            ('A', _) => Self::Added,
            _ => Self::Modified,
        })
    }
}

/// Status per absolute path, for files and for the folders that contain them.
#[derive(Debug, Default)]
pub struct GitStatuses {
    files: HashMap<PathBuf, GitFileStatus>,
    folders: HashMap<PathBuf, GitFileStatus>,
}

impl GitStatuses {
    pub fn file(&self, path: &Path) -> Option<GitFileStatus> {
        self.files.get(path).copied()
    }

    pub fn folder(&self, path: &Path) -> Option<GitFileStatus> {
        self.folders.get(path).copied()
    }

    fn insert(&mut self, repo_root: &Path, path: PathBuf, status: GitFileStatus) {
        let mut folder = path.parent();
        while let Some(dir) = folder {
            let entry = self.folders.entry(dir.to_path_buf()).or_insert(status);
            if status.priority() > entry.priority() {
                *entry = status;
            }
            if dir == repo_root || !dir.starts_with(repo_root) {
                break;
            }
            folder = dir.parent();
        }
        self.files.insert(path, status);
    }
}

/// Reads `git status` for the repos shown in the tree: each root's repo, or
/// the repos directly inside a root that isn't one (a folder holding several).
pub async fn load(roots: Vec<PathBuf>) -> GitStatuses {
    let mut repos: Vec<PathBuf> = Vec::new();
    for root in roots {
        let candidates = if root.join(".git").exists() {
            vec![root]
        } else {
            match repo_toplevel(&root).await {
                Some(toplevel) => vec![toplevel],
                None => child_repos(&root),
            }
        };
        for repo in candidates {
            if !repos.contains(&repo) {
                repos.push(repo);
            }
        }
    }

    let mut statuses = GitStatuses::default();
    for repo in repos {
        let Ok(output) = run_git_command(
            &repo,
            &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        )
        .await
        else {
            continue;
        };
        let mut entries = output.split('\0');
        while let Some(entry) = entries.next() {
            if entry.len() < 4 {
                continue;
            }
            let (code, path) = entry.split_at(3);
            let Some(status) = GitFileStatus::from_porcelain(code) else {
                continue;
            };
            // Renames and copies are followed by the original path.
            if code.starts_with(['R', 'C']) {
                entries.next();
            }
            statuses.insert(&repo, repo.join(path), status);
        }
    }
    statuses
}

async fn repo_toplevel(dir: &Path) -> Option<PathBuf> {
    let toplevel = run_git_command(dir, &["rev-parse", "--show-toplevel"])
        .await
        .ok()?;
    let toplevel = toplevel.trim();
    (!toplevel.is_empty()).then(|| PathBuf::from(toplevel))
}

fn child_repos(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && path.join(".git").exists())
        .collect()
}

#[cfg(test)]
#[path = "git_status_tests.rs"]
mod tests;
