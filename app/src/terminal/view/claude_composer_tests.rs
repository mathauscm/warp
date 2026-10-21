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

#[test]
fn erasing_the_prompt_moves_to_its_end_then_backspaces_every_character() {
    let rows = vec!["hello".to_owned(), "wor".to_owned()];
    // Cursor after "he": 3 characters, a line break and 3 more to its right.
    let keys = erase_prompt_keys(&rows, Some((0, 2)));
    assert_eq!(keys.len(), 7 + 9);
    assert!(keys[..7].iter().all(|key| *key == CURSOR_RIGHT));
    assert!(keys[7..].iter().all(|key| *key == BACKSPACE));
}

#[test]
fn erasing_the_prompt_from_its_end_only_backspaces() {
    let rows = vec!["abc".to_owned()];
    assert_eq!(erase_prompt_keys(&rows, Some((0, 3))), vec![BACKSPACE; 3]);
    assert_eq!(erase_prompt_keys(&rows, None), vec![BACKSPACE; 3]);
}

#[test]
fn slider_positions_map_onto_the_nearest_effort_level() {
    // A 100px track whose knob stops `SLIDER_INSET` short of each end.
    let left = 10.;
    let right = 110.;
    assert_eq!(slider_position(0., left, right), 0.);
    assert_eq!(slider_position(500., left, right), 1.);
    let middle = slider_position(60., left, right);
    assert!((middle - 0.5).abs() < f32::EPSILON);
    assert_eq!(nearest_effort(middle), 2);
    assert_eq!(nearest_effort(0.1), 0);
    assert_eq!(nearest_effort(0.9), EFFORTS.len() - 1);
    assert_eq!(effort_position(nearest_effort(0.74)), 0.75);
}

#[test]
fn a_settling_knob_ends_on_its_level() {
    let settle = SliderSettle {
        from: 0.4,
        to: 0.5,
        started_at: Instant::now() - SLIDER_SETTLE_DURATION,
    };
    assert_eq!(settle.position(), None);
    let settle = SliderSettle {
        started_at: Instant::now(),
        ..settle
    };
    let position = settle.position().expect("still settling");
    assert!((0.4..=0.5).contains(&position));
}

#[test]
fn settings_are_read_as_json_whatever_their_formatting() {
    let settings = r#"{
  "env": { "model": "nested" },
  "model": "claude-opus-5-5",
  "effortLevel": "high"
}"#;
    assert_eq!(
        settings_string(settings, "model").as_deref(),
        Some("claude-opus-5-5")
    );
    assert_eq!(
        settings_string(settings, "effortLevel").as_deref(),
        Some("high")
    );
    assert_eq!(settings_string(settings, "missing"), None);
    assert_eq!(settings_string("not json", "model"), None);
}

#[test]
fn the_slider_fill_is_a_pill_at_every_position() {
    // At the left end the fill is a circle as tall as the track; at the right end, the track.
    assert_eq!(slider_fill_width(0.), SLIDER_HEIGHT);
    assert_eq!(slider_fill_width(-1.), SLIDER_HEIGHT);
    assert_eq!(slider_fill_width(1.), SLIDER_WIDTH);
    for index in 0..EFFORTS.len() {
        let width = slider_fill_width(effort_position(index));
        assert!((SLIDER_HEIGHT..=SLIDER_WIDTH).contains(&width));
    }
    const { assert!(SLIDER_WIDTH > 2. * SLIDER_HEIGHT) };
}
