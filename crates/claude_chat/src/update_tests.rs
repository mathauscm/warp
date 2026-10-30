use super::*;

fn version(text: &str) -> Version {
    Version::parse(text).unwrap_or_else(|| panic!("{text} should parse"))
}

#[test]
fn parses_plain_prefixed_and_pre_release_versions() {
    assert_eq!(version("2.1.291").to_string(), "2.1.291");
    assert_eq!(version("v2.1.291").to_string(), "2.1.291");
    assert_eq!(version(" 2.1.291\n").to_string(), "2.1.291");
    assert_eq!(version("2.2.0-beta.1").to_string(), "2.2.0-beta.1");
    assert_eq!(version("2.2.0+build.5").to_string(), "2.2.0");
}

#[test]
fn rejects_what_is_not_a_version() {
    for text in [
        "", "2", "2.1", "2.1.x", "2.1.3.4", "2..3", "2.1.3-", "Claude", "(Claude",
    ] {
        assert_eq!(Version::parse(text), None, "{text:?}");
    }
}

#[test]
fn finds_the_version_in_claude_version_output() {
    assert_eq!(
        Version::find_in("2.1.291 (Claude Code)\n"),
        Some(version("2.1.291"))
    );
    assert_eq!(
        Version::find_in("Claude Code v2.0.5"),
        Some(version("2.0.5"))
    );
    assert_eq!(Version::find_in("command not found"), None);
}

#[test]
fn compares_numbers_by_value_not_as_text() {
    assert!(version("2.1.300") > version("2.1.291"));
    assert!(version("2.1.10") > version("2.1.9"));
    assert!(version("2.10.0") > version("2.9.99"));
    assert!(version("10.0.0") > version("9.9.9"));
    assert!(version("2.1.291") < version("2.1.300"));
    assert_eq!(
        version("2.1.291").cmp(&version("v2.1.291")),
        Ordering::Equal
    );
}

#[test]
fn a_release_is_newer_than_its_pre_releases() {
    assert!(version("2.2.0") > version("2.2.0-beta.3"));
    assert!(version("2.2.0-beta.3") > version("2.1.999"));
    assert!(version("2.2.0-beta.10") > version("2.2.0-beta.9"));
    assert!(version("2.2.0-beta") > version("2.2.0-alpha.7"));
    assert!(version("2.2.0-beta.1") > version("2.2.0-beta"));
    assert!(version("2.2.0-rc") > version("2.2.0-1"));
}

#[test]
fn reads_the_latest_version_from_the_registry() {
    let body = r#"{"name":"@anthropic-ai/claude-code","version":"2.1.300","bin":{}}"#;
    assert_eq!(parse_registry_latest(body), Some(version("2.1.300")));
    assert_eq!(parse_registry_latest(r#"{"name":"x"}"#), None);
    assert_eq!(parse_registry_latest(r#"{"version":"latest"}"#), None);
    assert_eq!(parse_registry_latest("<html>offline</html>"), None);
}

#[test]
fn keeps_the_end_of_long_errors() {
    assert_eq!(tail("short", 10), "short");
    assert_eq!(tail("abcdefghij", 4), "…ghij");
}
