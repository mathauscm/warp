use std::path::{Path, PathBuf};

use super::*;

#[test]
fn branch_name_from_ref_strips_local_and_origin_prefixes() {
    assert_eq!(
        branch_name_from_ref("refs/heads/pr-feat/kin-1"),
        Some("pr-feat/kin-1")
    );
    assert_eq!(
        branch_name_from_ref("refs/remotes/origin/pr-feat/kin-1"),
        Some("pr-feat/kin-1")
    );
    assert_eq!(branch_name_from_ref("refs/remotes/origin/HEAD"), None);
    assert_eq!(branch_name_from_ref("refs/tags/v1"), None);
}

#[test]
fn sibling_with_suffix_appends_worktree_to_folder_name() {
    assert_eq!(
        sibling_with_suffix(Path::new("/projects/aldeia/kinbox")),
        PathBuf::from("/projects/aldeia/kinbox-worktree")
    );
}

#[test]
fn repo_slots_pair_main_and_worktree_folders() {
    let slots = repo_slots(
        Path::new("/w/kinbox"),
        Path::new("/w/kinbox-worktree"),
        vec!["kinbox-web".to_owned()],
    );
    assert_eq!(slots.len(), 1);
    assert_eq!(slots[0].main, PathBuf::from("/w/kinbox/kinbox-web"));
    assert_eq!(
        slots[0].worktree,
        PathBuf::from("/w/kinbox-worktree/kinbox-web")
    );
}

#[test]
fn expand_home_resolves_tilde() {
    let home = dirs::home_dir().expect("home dir");
    assert_eq!(expand_home("~/projects"), home.join("projects"));
    assert_eq!(expand_home("/abs/path"), PathBuf::from("/abs/path"));
}

#[test]
fn summarize_git_error_skips_progress_and_joins_wrapped_lines() {
    let output = "From github.com:kinboxapp/kinbox-api-v2\n\
                  error: You're on a case-insensitive filesystem, and the remote you are\n\
                  trying to fetch from has references that only differ in casing.\n\
                  \n\
                  hint: run git fetch --prune\n\
                  , ";
    assert_eq!(
        summarize_git_error(output),
        "error: You're on a case-insensitive filesystem, and the remote you are \
         trying to fetch from has references that only differ in casing."
    );
}

#[test]
fn summarize_git_error_falls_back_to_first_line() {
    assert_eq!(
        summarize_git_error("\nsomething went wrong\n, "),
        "something went wrong"
    );
    assert_eq!(summarize_git_error(", "), "erro desconhecido do git");
}

#[test]
fn checked_out_elsewhere_matches_old_and_new_git_messages() {
    assert!(is_checked_out_elsewhere(
        "fatal: 'master' is already used by worktree at '/w/kinbox/kinbox-web'"
    ));
    assert!(is_checked_out_elsewhere(
        "fatal: 'master' is already checked out at '/w/kinbox/kinbox-web'"
    ));
    assert!(!is_checked_out_elsewhere(
        "error: pathspec 'master' did not match"
    ));
}
