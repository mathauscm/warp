use super::*;

#[test]
fn closing_the_only_tab_leaves_none() {
    assert_eq!(active_after_close(1, 0, 0), None);
    assert_eq!(active_after_close(0, 0, 0), None);
}

#[test]
fn closing_the_selected_tab_selects_the_next_one() {
    assert_eq!(active_after_close(3, 1, 1), Some(1));
    assert_eq!(active_after_close(3, 0, 0), Some(0));
}

#[test]
fn closing_the_selected_last_tab_selects_the_one_before() {
    assert_eq!(active_after_close(3, 2, 2), Some(1));
    assert_eq!(active_after_close(2, 1, 1), Some(0));
}

#[test]
fn closing_another_tab_keeps_the_selected_one() {
    // A tab before the selected one shifts it left.
    assert_eq!(active_after_close(3, 2, 0), Some(1));
    // A tab after it doesn't move it.
    assert_eq!(active_after_close(3, 0, 2), Some(0));
    assert_eq!(active_after_close(4, 1, 3), Some(1));
}
