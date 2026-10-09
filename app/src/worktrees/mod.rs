//! Git operations behind the footer "Worktree" button.
//!
//! A workspace is one or more git repos (e.g. a front end and a back end that
//! share branch names) plus a fixed "slot" folder holding one linked worktree
//! per repo. The button switches every slot to the same branch, or creates a
//! new branch in all of them from the up-to-date base branch.
//!
//! Workspaces come from `~/.warp/worktrees.toml`:
//!
//! ```toml
//! [[workspace]]
//! name     = "kinbox"
//! root     = "~/projects/aldeia/kinbox"
//! worktree = "~/projects/aldeia/kinbox-worktree"  # default: "<root>-worktree"
//! repos    = ["kinbox-web", "kinbox-api-v2"]      # default: the repos found in root
//! base     = "master"                             # default: detected per repo
//! ```
//!
//! Without a matching entry, the git repo of the current directory (or a folder
//! whose children are git repos) is used, with its slot at `<repo>-worktree`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use warp_util::git::run_git_command;

const CONFIG_DIR: &str = ".warp";
const CONFIG_FILE_NAME: &str = "worktrees.toml";
const WORKTREE_SUFFIX: &str = "-worktree";
const DEFAULT_REMOTE: &str = "origin";

#[derive(Debug, Default, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    workspace: Vec<WorkspaceConfig>,
}

#[derive(Debug, Deserialize)]
struct WorkspaceConfig {
    name: Option<String>,
    root: String,
    worktree: Option<String>,
    #[serde(default)]
    repos: Vec<String>,
    base: Option<String>,
}

/// One repo of a workspace: its main checkout and its slot in the worktree folder.
#[derive(Debug, Clone)]
pub struct RepoSlot {
    pub name: String,
    main: PathBuf,
    worktree: PathBuf,
}

#[derive(Debug, Clone)]
pub struct WorktreeWorkspace {
    pub name: String,
    /// The folder opened in the new tab after switching or creating a branch.
    pub worktree_root: PathBuf,
    pub repos: Vec<RepoSlot>,
    /// Base branch from the config; detected per repo when `None`.
    base: Option<String>,
}

#[derive(Debug, Clone)]
pub struct WorktreeBranch {
    pub name: String,
    /// Names of the workspace repos that have this branch (locally or on the remote).
    pub repos: Vec<String>,
    /// Whether the worktree slots are currently on this branch.
    pub is_current: bool,
}

/// What happened after a successful switch or create, for the toast.
#[derive(Debug, Clone)]
pub struct WorktreeOutcome {
    pub message: String,
    pub worktree_root: PathBuf,
}

/// Finds the workspace that `cwd` belongs to.
pub async fn resolve_workspace(cwd: PathBuf) -> Result<WorktreeWorkspace, String> {
    if let Some(workspace) = workspace_from_config(&cwd)? {
        return Ok(workspace);
    }
    if let Some(workspace) = workspace_from_git_repo(&cwd).await {
        return Ok(workspace);
    }
    if let Some(workspace) = workspace_from_repo_folder(&cwd) {
        return Ok(workspace);
    }
    Err(format!(
        "{} não é um repositório git nem um workspace de ~/{CONFIG_DIR}/{CONFIG_FILE_NAME}",
        cwd.display()
    ))
}

/// Lists the branches of every repo in the workspace, most recent first, merged
/// by name. The base branch and branches checked out in another folder (like the
/// main checkout) are left out, since git won't check them out in the slot too.
pub async fn list_branches(workspace: &WorktreeWorkspace) -> Result<Vec<WorktreeBranch>, String> {
    let mut by_name: HashMap<String, (i64, Vec<String>)> = HashMap::new();
    let mut current: Option<String> = None;

    for repo in &workspace.repos {
        let base = base_branch(workspace, repo).await;
        let refs = git(
            &repo.main,
            &[
                "for-each-ref",
                "--format=%(committerdate:unix)%09%(refname)",
                "refs/heads",
                "refs/remotes/origin",
            ],
        )
        .await
        .map_err(|err| format!("{}: {err}", repo.name))?;
        let busy = branches_checked_out_elsewhere(repo).await;

        for line in refs.lines() {
            let Some((date, refname)) = line.split_once('\t') else {
                continue;
            };
            let Some(name) = branch_name_from_ref(refname) else {
                continue;
            };
            if base.as_deref() == Some(name) || busy.iter().any(|busy| busy == name) {
                continue;
            }
            let date = date.trim().parse::<i64>().unwrap_or_default();
            let entry = by_name.entry(name.to_owned()).or_default();
            entry.0 = entry.0.max(date);
            if !entry.1.contains(&repo.name) {
                entry.1.push(repo.name.clone());
            }
        }

        if current.is_none() && repo.worktree.exists() {
            current = git(&repo.worktree, &["branch", "--show-current"])
                .await
                .ok()
                .map(|name| name.trim().to_owned())
                .filter(|name| !name.is_empty());
        }
    }

    let mut branches: Vec<(i64, WorktreeBranch)> = by_name
        .into_iter()
        .map(|(name, (date, repos))| {
            let is_current = current.as_deref() == Some(name.as_str());
            (
                date,
                WorktreeBranch {
                    name,
                    repos,
                    is_current,
                },
            )
        })
        .collect();
    branches.sort_by(|(a_date, a), (b_date, b)| b_date.cmp(a_date).then(a.name.cmp(&b.name)));
    Ok(branches.into_iter().map(|(_, branch)| branch).collect())
}

/// Checks out `branch` in the slot of every repo that has it, then returns the
/// folder to open.
pub async fn switch_branch(
    workspace: WorktreeWorkspace,
    branch: WorktreeBranch,
) -> Result<WorktreeOutcome, String> {
    ensure_slots(&workspace).await?;
    let targets: Vec<&RepoSlot> = workspace
        .repos
        .iter()
        .filter(|repo| branch.repos.contains(&repo.name))
        .collect();
    ensure_clean(&targets).await?;

    for repo in &targets {
        git(&repo.worktree, &["checkout", &branch.name])
            .await
            .map_err(|err| format!("{}: {err}", repo.name))?;
    }

    let missing: Vec<&str> = workspace
        .repos
        .iter()
        .filter(|repo| !branch.repos.contains(&repo.name))
        .map(|repo| repo.name.as_str())
        .collect();
    let mut message = format!("{}: worktree na branch {}", workspace.name, branch.name);
    if !missing.is_empty() {
        message.push_str(&format!(
            " ({} não tem essa branch e ficou como estava)",
            missing.join(", ")
        ));
    }
    Ok(WorktreeOutcome {
        message,
        worktree_root: workspace.worktree_root,
    })
}

/// Creates `name` in the slot of every repo: checks out the base branch, pulls it
/// from the remote and branches off it. Everything is validated before the first
/// checkout so a failure doesn't leave the repos on different branches.
pub async fn create_branch(
    workspace: WorktreeWorkspace,
    name: String,
) -> Result<WorktreeOutcome, String> {
    let name = name.trim().to_owned();
    if name.is_empty() {
        return Err("digite o nome da nova branch na busca".to_owned());
    }
    let Some(first) = workspace.repos.first() else {
        return Err("o workspace não tem repositórios".to_owned());
    };
    git(&first.main, &["check-ref-format", "--branch", &name])
        .await
        .map_err(|_| format!("nome de branch inválido: {name}"))?;

    ensure_slots(&workspace).await?;
    ensure_clean(&workspace.repos.iter().collect::<Vec<_>>()).await?;

    let mut plans = Vec::with_capacity(workspace.repos.len());
    for repo in &workspace.repos {
        let has_remote = git(&repo.main, &["remote", "get-url", DEFAULT_REMOTE])
            .await
            .is_ok();

        let exists_on_remote = has_remote
            && remote_branch_exists(&repo.main, &name)
                .await
                .map_err(|err| format!("{}: {err}", repo.name))?;
        if exists_on_remote || ref_exists(&repo.main, &format!("refs/heads/{name}")).await {
            return Err(format!(
                "a branch {name} já existe em {}; busque por ela para abrir",
                repo.name
            ));
        }

        let Some(base) = base_branch(&workspace, repo).await else {
            return Err(format!(
                "{}: não achei a branch base (configure `base` em ~/{CONFIG_DIR}/{CONFIG_FILE_NAME})",
                repo.name
            ));
        };
        plans.push((repo, base, has_remote));
    }

    for (repo, base, has_remote) in &plans {
        branch_off_base(repo, base, *has_remote, &name)
            .await
            .map_err(|err| format!("{}: {err}", repo.name))?;
    }

    let mut bases: Vec<&str> = plans.iter().map(|(_, base, _)| base.as_str()).collect();
    bases.dedup();
    Ok(WorktreeOutcome {
        message: format!(
            "{}: branch {name} criada a partir da {}",
            workspace.name,
            bases.join(", ")
        ),
        worktree_root: workspace.worktree_root,
    })
}

/// Creates `name` in the slot of `repo` from the up-to-date `base`.
///
/// Only `base` is pulled: fetching every branch fails on case-insensitive
/// filesystems when the remote has branches that differ only in casing.
async fn branch_off_base(
    repo: &RepoSlot,
    base: &str,
    has_remote: bool,
    name: &str,
) -> Result<(), String> {
    let slot = &repo.worktree;
    match git(slot, &["checkout", base]).await {
        Ok(_) => {
            if has_remote {
                git(slot, &["pull", DEFAULT_REMOTE, base]).await?;
            }
            git(slot, &["checkout", "-b", name]).await?;
        }
        // Git won't check out `base` in the slot while the main checkout has it.
        Err(err) if is_checked_out_elsewhere(&err) => {
            let start = if has_remote {
                git(slot, &["fetch", DEFAULT_REMOTE, base]).await?;
                format!("{DEFAULT_REMOTE}/{base}")
            } else {
                base.to_owned()
            };
            // `--no-track` keeps the new branch from tracking the base branch, so the
            // first push creates `origin/<name>` instead of pushing to the base.
            git(slot, &["checkout", "--no-track", "-b", name, &start]).await?;
        }
        Err(err) => return Err(err),
    }
    Ok(())
}

fn is_checked_out_elsewhere(error: &str) -> bool {
    error.contains("is already used by worktree") || error.contains("is already checked out at")
}

/// Asks the remote directly, without fetching into local refs.
async fn remote_branch_exists(repo: &Path, name: &str) -> Result<bool, String> {
    let refname = format!("refs/heads/{name}");
    let heads = git(repo, &["ls-remote", "--heads", DEFAULT_REMOTE, &refname]).await?;
    Ok(heads
        .lines()
        .any(|line| line.split('\t').nth(1) == Some(refname.as_str())))
}

fn workspace_from_config(cwd: &Path) -> Result<Option<WorktreeWorkspace>, String> {
    let Some(path) = dirs::home_dir().map(|home| home.join(CONFIG_DIR).join(CONFIG_FILE_NAME))
    else {
        return Ok(None);
    };
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    let config: ConfigFile = toml::from_str(&contents)
        .map_err(|err| format!("erro em {}: {}", path.display(), err.message()))?;

    for entry in config.workspace {
        let root = expand_home(&entry.root);
        let worktree_root = entry
            .worktree
            .as_deref()
            .map(expand_home)
            .unwrap_or_else(|| sibling_with_suffix(&root));
        if !cwd.starts_with(&root) && !cwd.starts_with(&worktree_root) {
            continue;
        }

        let repo_names = if !entry.repos.is_empty() {
            entry.repos
        } else if is_git_checkout(&root) {
            Vec::new()
        } else {
            child_repo_names(&root)
        };
        let name = entry.name.unwrap_or_else(|| folder_name(&root));
        let repos = if repo_names.is_empty() {
            vec![RepoSlot {
                name: name.clone(),
                main: root.clone(),
                worktree: worktree_root.clone(),
            }]
        } else {
            repo_slots(&root, &worktree_root, repo_names)
        };
        return Ok(Some(WorktreeWorkspace {
            name,
            worktree_root,
            repos,
            base: entry.base,
        }));
    }
    Ok(None)
}

async fn workspace_from_git_repo(cwd: &Path) -> Option<WorktreeWorkspace> {
    // The common dir points at the main checkout even when `cwd` is inside a
    // linked worktree, so the slot is the same from either side.
    let common_dir = git(
        cwd,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .await
    .ok()?;
    let common_dir = PathBuf::from(common_dir.trim());
    if common_dir.file_name()? != ".git" {
        return None;
    }
    let main = common_dir.parent()?.to_path_buf();
    let name = folder_name(&main);
    let worktree_root = sibling_with_suffix(&main);
    Some(WorktreeWorkspace {
        name: name.clone(),
        worktree_root: worktree_root.clone(),
        repos: vec![RepoSlot {
            name,
            main,
            worktree: worktree_root,
        }],
        base: None,
    })
}

/// A folder whose children are git repos, like a front end and a back end side
/// by side. Also accepts being inside the slot folder (`<root>-worktree`).
fn workspace_from_repo_folder(cwd: &Path) -> Option<WorktreeWorkspace> {
    let folder = folder_name(cwd);
    let (root, worktree_root) = match folder.strip_suffix(WORKTREE_SUFFIX) {
        Some(stem) if cwd.with_file_name(stem).is_dir() => {
            (cwd.with_file_name(stem), cwd.to_path_buf())
        }
        _ => (cwd.to_path_buf(), sibling_with_suffix(cwd)),
    };
    let repo_names = child_repo_names(&root);
    if repo_names.is_empty() {
        return None;
    }
    Some(WorktreeWorkspace {
        name: folder_name(&root),
        repos: repo_slots(&root, &worktree_root, repo_names),
        worktree_root,
        base: None,
    })
}

/// Creates the linked worktree of every repo whose slot doesn't exist yet.
async fn ensure_slots(workspace: &WorktreeWorkspace) -> Result<(), String> {
    for repo in &workspace.repos {
        if repo.worktree.exists() {
            continue;
        }
        if let Some(parent) = repo.worktree.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("{}: {err}", parent.display()))?;
        }
        // Drop registrations of slots that were deleted by hand, otherwise
        // `worktree add` refuses the path.
        let _ = git(&repo.main, &["worktree", "prune"]).await;
        let path = repo.worktree.to_string_lossy();
        git(&repo.main, &["worktree", "add", "--detach", &path])
            .await
            .map_err(|err| format!("{}: {err}", repo.name))?;
    }
    Ok(())
}

/// Branches checked out in a worktree of `repo` other than its slot.
async fn branches_checked_out_elsewhere(repo: &RepoSlot) -> Vec<String> {
    let Ok(list) = git(&repo.main, &["worktree", "list", "--porcelain"]).await else {
        return Vec::new();
    };
    let slot = canonical(&repo.worktree);
    let mut busy = Vec::new();
    let mut path: Option<PathBuf> = None;
    for line in list.lines() {
        if let Some(worktree) = line.strip_prefix("worktree ") {
            path = Some(canonical(Path::new(worktree)));
        } else if let Some(branch) = line.strip_prefix("branch refs/heads/")
            && path.as_ref() != Some(&slot)
        {
            busy.push(branch.to_owned());
        }
    }
    busy
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

async fn ensure_clean(repos: &[&RepoSlot]) -> Result<(), String> {
    for repo in repos {
        let status = git(&repo.worktree, &["status", "--porcelain"])
            .await
            .map_err(|err| format!("{}: {err}", repo.name))?;
        if !status.trim().is_empty() {
            return Err(format!(
                "{}: há alterações sem commit no worktree. Faça commit ou stash antes.",
                repo.name
            ));
        }
    }
    Ok(())
}

async fn base_branch(workspace: &WorktreeWorkspace, repo: &RepoSlot) -> Option<String> {
    if let Some(base) = &workspace.base {
        return Some(base.clone());
    }
    if let Ok(head) = git(
        &repo.main,
        &[
            "symbolic-ref",
            "--short",
            &format!("refs/remotes/{DEFAULT_REMOTE}/HEAD"),
        ],
    )
    .await
        && let Some(name) = head.trim().strip_prefix(&format!("{DEFAULT_REMOTE}/"))
    {
        return Some(name.to_owned());
    }
    for candidate in ["main", "master"] {
        if ref_exists(&repo.main, &format!("refs/heads/{candidate}")).await
            || ref_exists(
                &repo.main,
                &format!("refs/remotes/{DEFAULT_REMOTE}/{candidate}"),
            )
            .await
        {
            return Some(candidate.to_owned());
        }
    }
    None
}

async fn ref_exists(repo: &Path, refname: &str) -> bool {
    git(repo, &["rev-parse", "--verify", "--quiet", refname])
        .await
        .is_ok()
}

/// Runs git and reduces a failure to its error message.
async fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    run_git_command(repo, args).await.map_err(|err| {
        let message = err.to_string();
        summarize_git_error(
            message
                .strip_prefix("Git command failed: ")
                .unwrap_or(&message),
        )
    })
}

/// The `error:`/`fatal:` message of a failed git command, with its wrapped lines
/// joined, or its first line when there is none. Progress lines like `From <url>`
/// come before the error, so the first line alone can hide it.
fn summarize_git_error(output: &str) -> String {
    let lines: Vec<&str> = output
        .lines()
        .map(|line| line.trim().trim_end_matches(',').trim())
        .collect();
    let is_error = |line: &str| line.starts_with("error:") || line.starts_with("fatal:");
    let Some(start) = lines
        .iter()
        .position(|line| is_error(line))
        .or_else(|| lines.iter().position(|line| !line.is_empty()))
    else {
        return "erro desconhecido do git".to_owned();
    };

    let mut message = lines[start].to_owned();
    for line in &lines[start + 1..] {
        let starts_new_message = ["error:", "fatal:", "hint:", "warning:", "remote:"]
            .iter()
            .any(|prefix| line.starts_with(prefix));
        // `run_git_command` appends stdout after the stderr as ", <stdout>".
        if line.is_empty() || line.starts_with(',') || starts_new_message {
            break;
        }
        message.push(' ');
        message.push_str(line);
    }
    message
}

fn branch_name_from_ref(refname: &str) -> Option<&str> {
    if let Some(name) = refname.strip_prefix("refs/heads/") {
        return Some(name);
    }
    let name = refname.strip_prefix("refs/remotes/origin/")?;
    (name != "HEAD").then_some(name)
}

fn repo_slots(root: &Path, worktree_root: &Path, names: Vec<String>) -> Vec<RepoSlot> {
    names
        .into_iter()
        .map(|name| RepoSlot {
            main: root.join(&name),
            worktree: worktree_root.join(&name),
            name,
        })
        .collect()
}

fn child_repo_names(folder: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir() && is_git_checkout(&entry.path()))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn is_git_checkout(path: &Path) -> bool {
    path.join(".git").exists()
}

fn sibling_with_suffix(path: &Path) -> PathBuf {
    path.with_file_name(format!("{}{WORKTREE_SUFFIX}", folder_name(path)))
}

fn folder_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(path),
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
