use std::path::{Path, PathBuf};

use super::*;

#[test]
fn from_porcelain_maps_status_codes() {
    assert_eq!(
        GitFileStatus::from_porcelain(" M "),
        Some(GitFileStatus::Modified)
    );
    assert_eq!(
        GitFileStatus::from_porcelain("M  "),
        Some(GitFileStatus::Modified)
    );
    assert_eq!(
        GitFileStatus::from_porcelain("?? "),
        Some(GitFileStatus::Untracked)
    );
    assert_eq!(
        GitFileStatus::from_porcelain("A  "),
        Some(GitFileStatus::Added)
    );
    assert_eq!(
        GitFileStatus::from_porcelain(" D "),
        Some(GitFileStatus::Deleted)
    );
    assert_eq!(
        GitFileStatus::from_porcelain("R  "),
        Some(GitFileStatus::Renamed)
    );
    assert_eq!(
        GitFileStatus::from_porcelain("UU "),
        Some(GitFileStatus::Conflicted)
    );
    assert_eq!(GitFileStatus::from_porcelain("!! "), None);
}

#[test]
fn folders_take_the_most_important_status_of_their_files() {
    let repo = Path::new("/repo");
    let mut statuses = GitStatuses::default();
    statuses.insert(
        repo,
        PathBuf::from("/repo/src/new.ts"),
        GitFileStatus::Untracked,
    );
    statuses.insert(
        repo,
        PathBuf::from("/repo/src/app.ts"),
        GitFileStatus::Modified,
    );

    assert_eq!(
        statuses.file(Path::new("/repo/src/app.ts")),
        Some(GitFileStatus::Modified)
    );
    assert_eq!(
        statuses.folder(Path::new("/repo/src")),
        Some(GitFileStatus::Modified)
    );
    assert_eq!(
        statuses.folder(Path::new("/repo")),
        Some(GitFileStatus::Modified)
    );
    assert_eq!(statuses.folder(Path::new("/")), None);
}
