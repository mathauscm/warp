//! The git branch and uncommitted changes of the chat's folder, shown in the composer's footer.

use std::path::{Path, PathBuf};

/// Where the chat's folder stands in git.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitStatus {
    /// The repository's top-level folder.
    pub repo_root: PathBuf,
    /// The branch, or a short commit hash when HEAD is detached.
    pub branch: String,
    /// Lines added and removed against HEAD, untracked files included.
    pub lines_added: u32,
    pub lines_removed: u32,
}

impl GitStatus {
    /// The repository's folder name.
    pub fn repo_name(&self) -> String {
        self.repo_root.file_name().map_or_else(
            || self.repo_root.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        )
    }
}

/// Reads the git status of each of `dirs`, once per repository, in their order; folders outside
/// git are left out.
pub async fn load_all(dirs: Vec<PathBuf>) -> Vec<GitStatus> {
    let mut statuses: Vec<GitStatus> = Vec::new();
    for dir in dirs {
        if let Some(status) = load(&dir).await
            && !statuses
                .iter()
                .any(|known| known.repo_root == status.repo_root)
        {
            statuses.push(status);
        }
    }
    statuses
}

/// Lines added and removed across `statuses`.
pub fn total_changes(statuses: &[GitStatus]) -> (u32, u32) {
    statuses.iter().fold((0, 0), |(added, removed), status| {
        (added + status.lines_added, removed + status.lines_removed)
    })
}

/// Reads the git status of `cwd`; `None` when it's not in a git repository. Runs git, so call it
/// off the main thread.
#[cfg(feature = "local_fs")]
pub async fn load(cwd: &Path) -> Option<GitStatus> {
    let toplevel = warp_util::git::run_git_command(cwd, &["rev-parse", "--show-toplevel"])
        .await
        .ok()?;
    let repo_root = PathBuf::from(toplevel.trim());
    if repo_root.as_os_str().is_empty() {
        return None;
    }
    let summary = crate::util::git::get_repo_git_summary(&repo_root).await?;
    Some(GitStatus {
        repo_root,
        branch: summary.branch,
        lines_added: summary.lines_added,
        lines_removed: summary.lines_removed,
    })
}

#[cfg(not(feature = "local_fs"))]
pub async fn load(_cwd: &Path) -> Option<GitStatus> {
    None
}

/// A diff stat as its `+added` and `−removed` parts, each left out when zero.
pub fn diff_parts(lines_added: u32, lines_removed: u32) -> (Option<String>, Option<String>) {
    (
        (lines_added > 0).then(|| format!("+{}", compact(lines_added))),
        (lines_removed > 0).then(|| format!("−{}", compact(lines_removed))),
    )
}

/// `1234` as `1.2k`, so a big diff keeps the footer short.
fn compact(count: u32) -> String {
    if count < 1000 {
        count.to_string()
    } else if count < 10_000 {
        format!("{:.1}k", f64::from(count) / 1000.)
    } else {
        format!("{}k", count / 1000)
    }
}

#[cfg(test)]
#[path = "git_status_tests.rs"]
mod tests;
