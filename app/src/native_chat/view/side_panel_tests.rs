use std::path::Path;
use std::time::Duration;

use claude_chat::protocol::{ContextCategory, ContextUsage};

use super::format::*;

fn category(name: &str, tokens: u64, kind: &str) -> ContextCategory {
    ContextCategory {
        name: name.to_owned(),
        tokens: Some(tokens),
        kind: Some(kind.to_owned()),
        is_deferred: kind == "deferred",
    }
}

#[test]
fn tokens_are_abbreviated() {
    assert_eq!(format_tokens(0), "0");
    assert_eq!(format_tokens(850), "850");
    assert_eq!(format_tokens(1_000), "1k");
    assert_eq!(format_tokens(78_400), "78.4k");
    assert_eq!(format_tokens(78_449), "78.4k");
    assert_eq!(format_tokens(120_400), "120k");
    assert_eq!(format_tokens(999_400), "999k");
    assert_eq!(format_tokens(999_600), "1M");
    assert_eq!(format_tokens(1_000_000), "1M");
    assert_eq!(format_tokens(1_500_000), "1.5M");
}

#[test]
fn percentages_round_and_keep_slivers_visible() {
    assert_eq!(format_percent(0.), "0%");
    assert_eq!(format_percent(0.4), "<1%");
    assert_eq!(format_percent(7.6), "8%");
    assert_eq!(format_percent(140.), "100%");
    assert_eq!(format_share(0.), "0%");
    assert_eq!(format_share(0.05), "<0.1%");
    assert_eq!(format_share(3.54), "3.5%");
    assert_eq!(format_share(4.0), "4%");
    assert_eq!(format_share(83.9), "84%");
}

#[test]
fn reset_times_read_naturally() {
    assert_eq!(format_reset(Duration::ZERO), "reinicia em instantes");
    assert_eq!(format_reset(Duration::from_secs(30)), "reinicia em 1m");
    assert_eq!(
        format_reset(Duration::from_secs(13 * 60)),
        "reinicia em 13m"
    );
    assert_eq!(
        format_reset(Duration::from_secs(2 * 3600)),
        "reinicia em 2h"
    );
    assert_eq!(
        format_reset(Duration::from_secs(2 * 3600 + 13 * 60)),
        "reinicia em 2h 13m"
    );
    assert_eq!(
        format_reset(Duration::from_secs(3 * 86_400)),
        "reinicia em 3d"
    );
    assert_eq!(
        format_reset(Duration::from_secs(3 * 86_400 + 4 * 3600 + 59)),
        "reinicia em 3d 4h"
    );
}

#[test]
fn costs_use_the_brazilian_format() {
    assert_eq!(format_cost(0.004), "US$ <0,01");
    assert_eq!(format_cost(0.034), "US$ 0,03");
    assert_eq!(format_cost(12.5), "US$ 12,50");
}

#[test]
fn home_is_abbreviated() {
    let home = Path::new("/Users/ana");
    assert_eq!(
        abbreviate_home(Path::new("/Users/ana/projects/app"), Some(home)),
        "~/projects/app"
    );
    assert_eq!(abbreviate_home(home, Some(home)), "~");
    assert_eq!(abbreviate_home(Path::new("/tmp/x"), Some(home)), "/tmp/x");
    assert_eq!(abbreviate_home(Path::new("/tmp/x"), None), "/tmp/x");
}

#[test]
fn server_names_drop_cli_namespacing() {
    assert_eq!(display_server_name("plugin:linear:linear"), "Linear");
    assert_eq!(display_server_name("plugin:context7:context7"), "Context7");
    assert_eq!(display_server_name("claude.ai Gmail"), "Gmail");
    assert_eq!(display_server_name("claude.ai monday.com"), "Monday.com");
    assert_eq!(display_server_name("payflow"), "Payflow");
    assert_eq!(display_server_name("GitHub"), "GitHub");
    assert_eq!(display_server_name(""), "Servidor");
}

#[test]
fn avatar_letters_and_tool_names() {
    assert_eq!(avatar_letter("Gmail"), "G");
    assert_eq!(avatar_letter("éclair"), "É");
    assert_eq!(avatar_letter("--"), "?");
    assert_eq!(
        display_tool_name("mcp__plugin_linear_linear__get_issue"),
        "get_issue"
    );
    assert_eq!(display_tool_name("mcp__payflow__list_cards"), "list_cards");
    assert_eq!(display_tool_name("Read"), "Read");
}

#[test]
fn server_states_rank_working_servers_first() {
    let order = ["disabled", "failed", "needs-auth", "pending", "connected"]
        .map(|status| ServerState::from_status(status).rank());
    assert_eq!(order, [3, 2, 1, 0, 0]);
    assert_eq!(
        ServerState::from_status("something-new"),
        ServerState::Pending
    );
}

#[test]
fn context_fill_prefers_the_breakdown() {
    let usage = ContextUsage {
        total_tokens: Some(32_202),
        max_tokens: Some(200_000),
        ..Default::default()
    };
    let fill = context_fill(Some(&usage), Some(50_000), Some(1_000_000)).unwrap();
    assert_eq!(fill.used, 32_202);
    assert_eq!(fill.window, Some(200_000));
    assert!((fill.percent.unwrap() - 16.101).abs() < 0.01);

    let live = context_fill(None, Some(50_000), Some(1_000_000)).unwrap();
    assert_eq!(live.used, 50_000);
    assert_eq!(live.percent, Some(5.));

    assert_eq!(context_fill(None, None, Some(200_000)), None);
}

#[test]
fn context_rows_translate_and_put_free_space_last() {
    let usage = ContextUsage {
        categories: vec![
            category("System prompt", 7_029, "used"),
            category("Free space", 167_798, "free"),
            category("System tools (deferred)", 31_884, "deferred"),
            category("Messages", 320, "used"),
            category("Brand new", 10, "used"),
        ],
        total_tokens: Some(32_202),
        max_tokens: Some(200_000),
        ..Default::default()
    };
    let rows = context_rows(&usage);
    let labels: Vec<&str> = rows.iter().map(|row| row.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "Prompt do sistema",
            "Sistema sob demanda",
            "Mensagens",
            "Brand new",
            "Livre",
        ]
    );
    assert!(rows[1].is_deferred);
    assert_eq!(rows[1].tone, CategoryTone::SystemTools);
    assert_eq!(rows[4].tone, CategoryTone::Free);
    assert_eq!(rows[3].tone, CategoryTone::Other);
    assert!((rows[0].percent.unwrap() - 3.5145).abs() < 0.001);
}

#[test]
fn card_order_round_trips_and_fills_gaps() {
    use super::SidePanelSection::{Connectors, Context, Environment, Usage};
    use super::order::{DEFAULT_ORDER, parse_order, serialize_order};

    let order = vec![Connectors, Environment, Usage, Context];
    assert_eq!(parse_order(&serialize_order(&order)), order);

    assert_eq!(
        parse_order(r#"{"order":["usage","bogus","usage","context"]}"#),
        vec![Usage, Context, Environment, Connectors]
    );
    assert_eq!(parse_order("not json"), DEFAULT_ORDER.to_vec());
}
