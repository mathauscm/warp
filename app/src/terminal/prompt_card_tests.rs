use super::*;

const COLUMNS: usize = 10;

fn find(lines: &[&str], rows: std::ops::Range<usize>) -> Option<PromptRules> {
    find_rules(
        rows,
        |row| is_rule_row(lines[row].chars(), COLUMNS),
        |row| lines[row].trim().is_empty(),
    )
}

fn find_all(lines: &[&str]) -> Option<PromptRules> {
    find(lines, 0..lines.len())
}

#[test]
fn rule_row_needs_only_rule_chars_across_most_of_the_width() {
    assert!(is_rule_row("──────────".chars(), 10));
    assert!(is_rule_row("───────   ".chars(), 10));
    assert!(!is_rule_row("────      ".chars(), 10));
    assert!(!is_rule_row("───── x ──".chars(), 10));
    assert!(!is_rule_row("".chars(), 0));
}

#[test]
fn finds_the_last_pair_of_rules_and_the_status_line_under_them() {
    let lines = [
        "old output",
        "──────────",
        "❯ old     ",
        "──────────",
        "answer    ",
        "──────────",
        "❯ hello   ",
        "  world   ",
        "──────────",
        "  status  ",
        "          ",
    ];
    assert_eq!(
        find_all(&lines),
        Some(PromptRules {
            top: 5,
            bottom: 8,
            footer_rows: 1,
        })
    );
}

#[test]
fn leaves_a_menu_under_the_prompt_out_of_the_card() {
    let lines = [
        "──────────",
        "❯ /       ",
        "──────────",
        "  /help   ",
        "  /clear  ",
        "  /model  ",
        "  /theme  ",
    ];
    assert_eq!(
        find_all(&lines),
        Some(PromptRules {
            top: 0,
            bottom: 2,
            footer_rows: 0,
        })
    );
}

#[test]
fn needs_a_row_between_the_rules() {
    assert_eq!(find_all(&["──────────", "──────────", "  status  "]), None);
}

#[test]
fn ignores_rules_far_above_the_bottom() {
    let mut lines = vec!["──────────", "❯ hi      ", "──────────"];
    lines.extend(std::iter::repeat_n("output    ", MAX_ROWS_BELOW_PROMPT + 1));
    assert_eq!(find_all(&lines), None);
}

#[test]
fn only_searches_the_given_rows() {
    let lines = ["──────────", "❯ hi      ", "──────────", "  status  "];
    assert_eq!(find(&lines, 1..lines.len()), None);
}
