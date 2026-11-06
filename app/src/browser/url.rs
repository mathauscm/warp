//! Turning what's typed in the address field into a URL, and finding local pages in text.

use serde_json::Value;

/// Hosts that point at this machine.
const LOCAL_HOSTS: &[&str] = &["localhost", "127.0.0.1", "0.0.0.0", "[::1]"];

/// Schemes that are complete URLs without `//`.
const OPAQUE_SCHEMES: &[&str] = &["about:", "data:", "blob:", "mailto:"];

/// The URL to load for the address field's `input`: complete URLs as they are, `host:port` and
/// local addresses over `http://`, domains over `https://`, a bare port on localhost, and anything
/// else as a web search. `None` when the input is blank.
pub fn normalize_input(input: &str) -> Option<String> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    if has_scheme(input) {
        return Some(input.to_owned());
    }
    if let Some(port) = input.strip_prefix(':').unwrap_or(input).parse::<u16>().ok() {
        return Some(format!("http://localhost:{port}"));
    }
    if input.starts_with('/') {
        return Some(format!("file://{input}"));
    }
    if input.contains(char::is_whitespace) {
        return Some(search_url(input));
    }
    let host = host_of(input);
    let host_name = host.rsplit_once(':').map_or(host, |(name, port)| {
        if port.parse::<u16>().is_ok() {
            name
        } else {
            host
        }
    });
    let has_port = host_name.len() < host.len();
    if is_local_host(host_name) || is_ip_address(host_name) || (has_port && !host_name.is_empty()) {
        return Some(format!("http://{input}"));
    }
    if host_name.contains('.') && !host_name.starts_with('.') && !host_name.ends_with('.') {
        return Some(format!("https://{input}"));
    }
    Some(search_url(input))
}

fn has_scheme(input: &str) -> bool {
    if OPAQUE_SCHEMES
        .iter()
        .any(|scheme| input.to_ascii_lowercase().starts_with(scheme))
    {
        return true;
    }
    input.split_once("://").is_some_and(|(scheme, _)| {
        !scheme.is_empty()
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    })
}

fn search_url(query: &str) -> String {
    let encoded: String = query
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            b' ' => "+".to_owned(),
            _ => format!("%{byte:02X}"),
        })
        .collect();
    format!("https://www.google.com/search?q={encoded}")
}

/// The `host[:port]` part of a URL or of a scheme-less address.
fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let rest = rest.rsplit_once('@').map_or(rest, |(_, rest)| rest);
    rest.split(['/', '?', '#']).next().unwrap_or(rest)
}

fn is_local_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    LOCAL_HOSTS.contains(&host.as_str()) || host.ends_with(".localhost")
}

fn is_ip_address(host: &str) -> bool {
    host.parse::<std::net::Ipv4Addr>().is_ok()
}

/// Whether `url` is an `http(s)` page on this machine.
pub fn is_local_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return false;
    }
    let host = host_of(url);
    let host_name = match host.rsplit_once(':') {
        Some((name, port)) if port.parse::<u16>().is_ok() => name,
        _ => host,
    };
    is_local_host(host_name)
}

/// `0.0.0.0` (what servers print when they listen everywhere) can't be browsed to; this machine
/// is `localhost`.
pub fn browsable(url: &str) -> String {
    url.replacen("://0.0.0.0", "://localhost", 1)
}

/// The local pages `text` mentions (like a dev server's `Local: http://localhost:5173/`), in
/// order and without repeats.
pub fn local_urls_in(text: &str) -> Vec<String> {
    let text = strip_ansi(text);
    let mut urls: Vec<String> = Vec::new();
    let mut rest = text.as_str();
    while let Some(start) = rest.find("http") {
        let candidate = &rest[start..];
        let end = candidate
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '`' | ')'))
            .unwrap_or(candidate.len());
        let url = candidate[..end].trim_end_matches(['.', ',', ';', ':', ']', '}']);
        if is_local_url(url) && host_of(url).contains(':') {
            let url = browsable(url);
            if !urls.contains(&url) {
                urls.push(url);
            }
        }
        rest = &candidate[end.max(4)..];
    }
    urls
}

/// The page a chat tool call is about: the `url` a browser tool (like Playwright's or Chrome
/// DevTools' MCP `navigate`) was given, else the first local page in its input or result.
pub fn tool_page_url(tool_name: &str, input: &Value, result: Option<&str>) -> Option<String> {
    let input_url = || {
        let url = input.get("url")?.as_str()?;
        (url.starts_with("http://") || url.starts_with("https://")).then(|| browsable(url))
    };
    if tool_name.starts_with("mcp__")
        && let Some(url) = input_url()
    {
        return Some(url);
    }
    result
        .and_then(|result| local_urls_in(result).into_iter().next())
        .or_else(|| {
            input
                .as_object()?
                .values()
                .filter_map(Value::as_str)
                .find_map(|text| local_urls_in(text).into_iter().next())
        })
}

/// Removes terminal color and cursor escape sequences.
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {
                chars.next();
            }
        }
    }
    out
}

/// A URL without its scheme and trailing slash, for a tab with no title.
pub fn display_url(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.strip_suffix('/').unwrap_or(rest).to_owned()
}

#[cfg(test)]
#[path = "url_tests.rs"]
mod tests;
