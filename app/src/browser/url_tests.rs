use serde_json::json;

use super::*;

#[test]
fn normalizes_what_is_typed_in_the_address_field() {
    let cases = [
        ("  ", None),
        ("https://example.com/a", Some("https://example.com/a")),
        ("localhost:5174", Some("http://localhost:5174")),
        ("localhost", Some("http://localhost")),
        ("127.0.0.1:8000/admin", Some("http://127.0.0.1:8000/admin")),
        ("[::1]:3000", Some("http://[::1]:3000")),
        ("app.localhost:3000", Some("http://app.localhost:3000")),
        ("devbox:8080", Some("http://devbox:8080")),
        ("192.168.0.10", Some("http://192.168.0.10")),
        ("5174", Some("http://localhost:5174")),
        (":5174", Some("http://localhost:5174")),
        ("example.com", Some("https://example.com")),
        ("docs.rs/serde", Some("https://docs.rs/serde")),
        ("about:blank", Some("about:blank")),
        ("/tmp/page.html", Some("file:///tmp/page.html")),
        (
            "rust lifetimes",
            Some("https://www.google.com/search?q=rust+lifetimes"),
        ),
        ("vite", Some("https://www.google.com/search?q=vite")),
    ];
    for (input, expected) in cases {
        assert_eq!(normalize_input(input).as_deref(), expected, "{input:?}");
    }
}

#[test]
fn recognizes_local_urls() {
    assert!(is_local_url("http://localhost:5173/"));
    assert!(is_local_url("https://127.0.0.1:8443/x"));
    assert!(is_local_url("http://0.0.0.0:3000"));
    assert!(is_local_url("http://[::1]:3000"));
    assert!(is_local_url("http://app.localhost:3000"));
    assert!(!is_local_url("http://example.com:3000"));
    assert!(!is_local_url("localhost:3000"));
    assert!(!is_local_url("ftp://localhost:21"));
}

#[test]
fn finds_local_urls_in_dev_server_output() {
    let output = "\u{1b}[32m  VITE v5.0.0\u{1b}[39m  ready in 300 ms\n\n  \
                  ➜  Local:   \u{1b}[36mhttp://localhost:\u{1b}[1m5174\u{1b}[22m/\u{1b}[39m\n  \
                  ➜  Network: http://0.0.0.0:5174/.\nSee http://localhost:5174/ again, and \
                  https://vitejs.dev for docs.";
    assert_eq!(local_urls_in(output), ["http://localhost:5174/".to_owned()]);
    assert_eq!(
        local_urls_in("(served at http://127.0.0.1:8000)"),
        ["http://127.0.0.1:8000".to_owned()]
    );
    assert!(local_urls_in("http://localhost/ without a port").is_empty());
    assert!(local_urls_in("nothing here").is_empty());
}

#[test]
fn finds_the_page_of_a_tool_call() {
    assert_eq!(
        tool_page_url(
            "mcp__playwright__browser_navigate",
            &json!({ "url": "https://example.com/login" }),
            None,
        )
        .as_deref(),
        Some("https://example.com/login")
    );
    assert_eq!(
        tool_page_url(
            "Bash",
            &json!({ "command": "npm run dev" }),
            Some("  ➜  Local:   http://localhost:5173/"),
        )
        .as_deref(),
        Some("http://localhost:5173/")
    );
    assert_eq!(
        tool_page_url(
            "Bash",
            &json!({ "command": "curl -s http://localhost:3000/health" }),
            Some("ok"),
        )
        .as_deref(),
        Some("http://localhost:3000/health")
    );
    // Only browser tools' remote URLs count; a fetch of a website isn't a page to preview.
    assert_eq!(
        tool_page_url("WebFetch", &json!({ "url": "https://example.com" }), None),
        None
    );
}

#[test]
fn strips_terminal_escape_sequences() {
    assert_eq!(
        strip_ansi("\u{1b}[1;32mok\u{1b}[0m \u{1b}]0;title\u{7}done"),
        "ok done"
    );
}

#[test]
fn shows_urls_without_scheme_and_trailing_slash() {
    assert_eq!(display_url("http://localhost:5174/"), "localhost:5174");
    assert_eq!(display_url("https://example.com/a/"), "example.com/a");
    assert_eq!(browsable("http://0.0.0.0:3000/"), "http://localhost:3000/");
}
