use super::*;

#[test]
fn model_labels_drop_the_prefix_and_the_date() {
    assert_eq!(model_label("claude-opus-5-5").as_deref(), Some("Opus 5.5"));
    assert_eq!(
        model_label("claude-sonnet-4-5-20250929").as_deref(),
        Some("Sonnet 4.5")
    );
    assert_eq!(
        model_label("claude-haiku-4-5-20251001").as_deref(),
        Some("Haiku 4.5")
    );
    assert_eq!(
        model_label("claude-fable-5-1[1m]").as_deref(),
        Some("Fable 5.1")
    );
    assert_eq!(model_label("gpt-5"), None);
}

#[test]
fn last_string_field_takes_the_latest_value() {
    let tail = r#"{"model":"claude-sonnet-4-5"}
{"message":{"model":"claude-opus-5-5"},"effort":"high"}"#;
    assert_eq!(
        last_string_field(tail, "model").as_deref(),
        Some("claude-opus-5-5")
    );
    assert_eq!(last_string_field(tail, "effort").as_deref(), Some("high"));
    assert_eq!(last_string_field(tail, "missing"), None);
}

#[test]
fn chip_previews_collapse_whitespace_and_cut_long_text() {
    assert_eq!(chip_preview("  fn  main()\n{}  "), "fn main() {}");
    let preview = chip_preview(&"a".repeat(100));
    assert_eq!(preview.chars().count(), CHIP_PREVIEW_CHARS);
    assert!(preview.ends_with('…'));
}

#[test]
fn status_line_splits_into_mode_bits_and_the_right_side() {
    let status = parse_status_line(
        "  ⏵⏵ auto mode on (shift+tab to cycle) · ← 2 agents            ⎇ Worktree · pr-feat/x  ",
    );
    assert_eq!(
        status,
        StatusLine {
            mode: Some(PermissionMode {
                symbol: "⏵⏵".to_owned(),
                label: "Auto mode".to_owned(),
            }),
            left: vec!["← 2 agents".to_owned()],
            right: Some("⎇ Worktree · pr-feat/x".to_owned()),
        }
    );
}

#[test]
fn manual_mode_without_a_hint_is_still_the_mode() {
    let status = parse_status_line(&format!("  ⏸ manual mode on{}", "\0".repeat(40)));
    assert_eq!(
        status,
        StatusLine {
            mode: Some(PermissionMode {
                symbol: "⏸".to_owned(),
                label: "Manual mode".to_owned(),
            }),
            left: Vec::new(),
            right: None,
        }
    );
}

#[test]
fn status_line_without_a_mode_keeps_its_text() {
    let status = parse_status_line("  ? for shortcuts");
    assert_eq!(
        status,
        StatusLine {
            mode: None,
            left: vec!["? for shortcuts".to_owned()],
            right: None,
        }
    );
}

#[test]
fn plan_mode_is_teal_and_auto_modes_amber() {
    assert_eq!(mode_color("Plan mode"), Some(PLAN_MODE_COLOR));
    assert_eq!(mode_color("Accept edits"), Some(AUTO_MODE_COLOR));
    assert_eq!(mode_color("Something else"), None);
}
