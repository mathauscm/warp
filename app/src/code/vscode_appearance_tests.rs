use pathfinder_color::ColorU;

use super::*;

#[test]
fn strip_jsonc_removes_comments_and_trailing_commas() {
    let text = r#"{
        // line comment
        "a": "http://x", /* block */
        "b": [1, 2,],
    }"#;
    let value: Value = serde_json::from_str(&strip_jsonc(text)).expect("valid json");
    assert_eq!(value["a"], "http://x");
    assert_eq!(value["b"], serde_json::json!([1, 2]));
}

#[test]
fn parse_color_handles_short_and_alpha_forms() {
    assert_eq!(parse_color("#0a0a0d"), Some(ColorU::new(10, 10, 13, 255)));
    assert_eq!(parse_color("#515c7e40"), Some(ColorU::new(81, 92, 126, 64)));
    assert_eq!(parse_color("#fff"), Some(ColorU::new(255, 255, 255, 255)));
    assert_eq!(parse_color("red"), None);
}

#[test]
fn scope_color_prefers_the_most_specific_selector() {
    let theme = Theme {
        colors: HashMap::new(),
        token_rules: vec![
            (vec!["keyword".to_owned()], "#111111".to_owned()),
            (
                vec!["keyword.control.import".to_owned()],
                "#222222".to_owned(),
            ),
            (vec!["keyword.operator".to_owned()], "#333333".to_owned()),
        ],
    };
    assert_eq!(
        scope_color(&theme, "keyword.control.import"),
        parse_color("#222222")
    );
    assert_eq!(
        scope_color(&theme, "keyword.control"),
        parse_color("#111111")
    );
    assert_eq!(scope_color(&theme, "keywords"), None);
}
