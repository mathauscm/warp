use std::path::PathBuf;

use super::*;

fn status(repo_root: &str, lines_added: u32, lines_removed: u32) -> GitStatus {
    GitStatus {
        repo_root: PathBuf::from(repo_root),
        branch: "main".to_owned(),
        lines_added,
        lines_removed,
    }
}

#[test]
fn formats_diff_parts() {
    assert_eq!(diff_parts(0, 0), (None, None));
    assert_eq!(
        diff_parts(12, 3),
        (Some("+12".to_owned()), Some("−3".to_owned()))
    );
    assert_eq!(diff_parts(7, 0), (Some("+7".to_owned()), None));
    assert_eq!(
        diff_parts(1234, 25_600),
        (Some("+1.2k".to_owned()), Some("−25k".to_owned()))
    );
}

#[test]
fn sums_changes_across_repositories() {
    let statuses = [status("/tmp/front", 12, 3), status("/tmp/api", 5, 0)];
    assert_eq!(total_changes(&statuses), (17, 3));
    assert_eq!(total_changes(&[]), (0, 0));
    assert_eq!(statuses[1].repo_name(), "api");
}
