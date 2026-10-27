use claude_chat::protocol::{ContextUsage, ModelInfo, SlashCommand};
use claude_chat::timeline::{TodoItem, TodoStatus};
use serde_json::json;

use super::logic::*;
use super::ring_icon;
use crate::native_chat::view::SidePanelSection;

fn command(name: &str, description: &str, builtin: bool) -> SlashCommand {
    SlashCommand {
        name: name.to_owned(),
        description: description.to_owned(),
        argument_hint: String::new(),
        aliases: Vec::new(),
        builtin,
    }
}

fn names(entries: &[CommandEntry], indices: &[usize]) -> Vec<String> {
    indices
        .iter()
        .map(|&index| entries[index].name.clone())
        .collect()
}

#[test]
fn permission_modes_cycle_in_order_and_wrap() {
    assert_eq!(next_permission_mode(None).value, "acceptEdits");
    assert_eq!(next_permission_mode(Some("default")).value, "acceptEdits");
    assert_eq!(next_permission_mode(Some("acceptEdits")).value, "plan");
    assert_eq!(next_permission_mode(Some("plan")).value, "auto");
    assert_eq!(
        next_permission_mode(Some("auto")).value,
        "bypassPermissions"
    );
    assert_eq!(
        next_permission_mode(Some("bypassPermissions")).value,
        "default"
    );
    assert_eq!(
        next_permission_mode(Some("something new")).value,
        "acceptEdits"
    );
    assert_eq!(
        PERMISSION_MODES[permission_mode_index(Some("dontAsk"))].label,
        "Acesso total"
    );
}

#[test]
fn slash_query_stops_at_the_first_space() {
    assert_eq!(slash_query("/"), Some(""));
    assert_eq!(slash_query("/mo"), Some("mo"));
    assert_eq!(slash_query("/model opus"), None);
    assert_eq!(slash_query("hello /model"), None);
}

#[test]
fn command_entries_put_app_commands_first_without_duplicates() {
    let cli = [
        command("model", "Set the AI model", true),
        command("init", "Initialize a CLAUDE.md", true),
        command("review", "Review a pull request", false),
    ];
    let entries = command_entries(cli.iter());
    assert_eq!(
        entries.iter().filter(|entry| entry.name == "model").count(),
        1
    );
    assert_eq!(entries[0].name, "model");
    assert_eq!(entries[0].description, "Trocar o modelo");
    assert!(entries.iter().any(|entry| entry.name == "init"));
    assert!(
        entries
            .iter()
            .any(|entry| entry.name == "review" && !entry.builtin)
    );
}

#[test]
fn filter_commands_ranks_prefixes_and_builtins_first() {
    let cli = [
        command("compare", "A user command", false),
        command("init", "Initialize a CLAUDE.md file", true),
        command("commit", "Commit the changes", true),
    ];
    let entries = command_entries(cli.iter());

    let matches = filter_commands(&entries, "co");
    let found = names(&entries, &matches);
    // Builtin prefix matches before the user command, then description matches.
    let compare = found.iter().position(|name| name == "compare").unwrap();
    let commit = found.iter().position(|name| name == "commit").unwrap();
    let compact = found.iter().position(|name| name == "compact").unwrap();
    assert!(commit < compare);
    assert!(compact < compare);

    let matches = filter_commands(&entries, "init");
    assert_eq!(names(&entries, &matches)[0], "init");

    let matches = filter_commands(&entries, "claude.md");
    assert_eq!(names(&entries, &matches), vec!["init".to_owned()]);

    assert!(filter_commands(&entries, "zzz").is_empty());
    assert_eq!(filter_commands(&entries, "").len(), entries.len());
}

#[test]
fn ui_commands_map_to_actions() {
    assert_eq!(ui_command("model"), Some(UiCommand::ModelPicker));
    assert_eq!(
        ui_command("mcp"),
        Some(UiCommand::OpenSidePanel(SidePanelSection::Connectors))
    );
    assert_eq!(
        ui_command("cost"),
        Some(UiCommand::OpenSidePanel(SidePanelSection::Usage))
    );
    assert_eq!(ui_command("clear"), Some(UiCommand::NewChat));
    assert_eq!(ui_command("init"), None);
    assert_eq!(parse_command(" /model opus "), Some(("model", "opus")));
    assert_eq!(parse_command("/compact"), Some(("compact", "")));
    assert_eq!(parse_command("hello"), None);
}

#[test]
fn mention_query_is_the_trailing_at_word() {
    assert_eq!(mention_query("look at @src/ma"), Some((8, "src/ma")));
    assert_eq!(mention_query("@"), Some((0, "")));
    assert_eq!(mention_query("mail me@example.com"), None);
    assert_eq!(mention_query("@src/main.rs and"), None);
    assert_eq!(
        complete_mention("look at @src/ma", 8, "src/main.rs"),
        "look at @src/main.rs "
    );
    assert_eq!(
        complete_mention("@my", 0, "my file.txt"),
        "@\"my file.txt\" "
    );
}

#[test]
fn mentions_are_quoted_before_the_message() {
    let quotes = vec!["first line\n\nsecond line\n".to_owned(), "other".to_owned()];
    assert_eq!(
        message_with_quotes(&quotes, "  what is this?  "),
        "> first line\n>\n> second line\n\n> other\n\nwhat is this?"
    );
    assert_eq!(message_with_quotes(&[], "hi"), "hi");
    assert_eq!(message_with_quotes(&["x".to_owned()], " "), "> x");
}

#[test]
fn preview_flattens_and_truncates() {
    assert_eq!(preview("a\n  b   c", 10), "a b c");
    assert_eq!(preview("abcdef ghij", 7), "abcdef…");
}

#[test]
fn model_names_and_efforts() {
    let models = vec![
        ModelInfo {
            value: "default".to_owned(),
            resolved_model: Some("claude-opus-5-5".to_owned()),
            display_name: "Default (recommended)".to_owned(),
            supports_effort: true,
            supported_effort_levels: vec!["low".to_owned(), "high".to_owned()],
            ..Default::default()
        },
        ModelInfo {
            value: "haiku".to_owned(),
            resolved_model: Some("claude-haiku-4-5-20251001".to_owned()),
            display_name: "Haiku 4.5".to_owned(),
            ..Default::default()
        },
    ];
    assert_eq!(
        model_display_name(&models, None, Some("claude-opus-5-5")),
        "Opus 5.5"
    );
    assert_eq!(
        model_display_name(&models, Some("haiku"), None),
        "Haiku 4.5"
    );
    assert_eq!(model_display_name(&[], Some("sonnet"), None), "Sonnet");
    assert_eq!(model_display_name(&[], None, None), "Claude");
    assert_eq!(model_display_name(&models, None, None), "Opus 5.5");
    assert_eq!(
        model_label("claude-sonnet-4-5-20250929").as_deref(),
        Some("Sonnet 4.5")
    );

    assert_eq!(effort_levels(models.first()), vec!["low", "high"]);
    assert!(effort_levels(models.get(1)).is_empty());
    assert_eq!(effort_levels(None).len(), DEFAULT_EFFORTS.len());
    assert_eq!(effort_label("xhigh"), "Extra alto");
    let levels: Vec<String> = DEFAULT_EFFORTS
        .iter()
        .map(|&level| level.to_owned())
        .collect();
    assert_eq!(default_effort_index(&levels), Some(2));
    assert_eq!(
        default_effort_index(&["low".to_owned(), "max".to_owned()]),
        Some(1)
    );
    assert_eq!(default_effort_index(&[]), None);
    assert_eq!(effort_position(4, 5), 1.);
}

#[test]
fn context_fraction_prefers_the_live_prompt_size() {
    let usage = ContextUsage {
        percentage: Some(8.),
        max_tokens: Some(200_000),
        ..Default::default()
    };
    assert_eq!(context_fraction(Some(&usage), None, None), Some(0.08));
    assert_eq!(
        context_fraction(Some(&usage), Some(50_000), None),
        Some(0.25)
    );
    assert_eq!(context_fraction(None, Some(10), Some(100)), Some(0.1));
    assert_eq!(context_fraction(None, None, None), None);
    assert_eq!(format_tokens(78_400), "78.4k");
    assert_eq!(format_tokens(1_000_000), "1M");
    assert_eq!(ring_icon(0.), None);
    assert_eq!(ring_icon(0.001), Some("bundled/svg/picolas-ring-005.svg"));
    assert_eq!(ring_icon(1.), Some("bundled/svg/picolas-ring-100.svg"));
}

#[test]
fn todo_progress_counts_completed() {
    let todo = |content: &str, status| TodoItem {
        id: None,
        content: content.to_owned(),
        active_form: Some(format!("{content}ing")),
        status,
    };
    let todos = vec![
        todo("Read", TodoStatus::Completed),
        todo("Writ", TodoStatus::InProgress),
        todo("Test", TodoStatus::Pending),
    ];
    assert_eq!(todo_progress(&todos), (1, 3));
    assert_eq!(current_todo(&todos), Some("Writing"));
}

#[test]
fn permission_previews_and_suggestion_labels() {
    assert_eq!(
        permission_preview("Bash", &json!({ "command": "ls -la" })),
        PermissionPreview::Command("ls -la".to_owned())
    );
    match permission_preview(
        "Write",
        &json!({ "file_path": "/tmp/a.txt", "content": "one\ntwo\n" }),
    ) {
        PermissionPreview::File(stats) => {
            assert_eq!(stats.path, "/tmp/a.txt");
            assert_eq!(stats.added, 2);
        }
        other => panic!("unexpected preview {other:?}"),
    }
    assert_eq!(
        tool_display_name("mcp__github__create_issue", None),
        "create_issue (github)"
    );

    assert_eq!(
        suggestion_label(&json!({
            "type": "addRules",
            "rules": [{ "toolName": "Bash", "ruleContent": "echo hello *" }],
            "behavior": "allow",
            "destination": "localSettings",
        })),
        "Sempre permitir “echo hello *”"
    );
    assert_eq!(
        suggestion_label(
            &json!({ "type": "setMode", "mode": "acceptEdits", "destination": "session" })
        ),
        "Aceitar edições nesta sessão"
    );
    assert_eq!(
        suggestion_label(
            &json!({ "type": "addDirectories", "directories": ["/a/work"], "destination": "session" })
        ),
        "Sempre permitir em work nesta sessão"
    );
    assert_eq!(
        suggestion_label(&json!({
            "type": "addRules",
            "rules": [{ "toolName": "WebFetch", "ruleContent": "domain:example.edu" }],
            "destination": "localSettings",
        })),
        "Sempre permitir example.edu"
    );
    assert_eq!(
        question_answer(&["Red", "Blue"], ""),
        Some("Red, Blue".to_owned())
    );
    assert_eq!(
        question_answer(&["Red"], " Green "),
        Some("Green".to_owned())
    );
    assert_eq!(question_answer(&[], ""), None);
}

#[test]
fn artifact_kinds_have_placeholders_and_instructions() {
    use claude_chat::artifacts::ArtifactKind;
    assert_eq!(
        artifact_placeholder(ArtifactKind::Motion),
        "O que esta animação deve mostrar?"
    );
    let instructions = artifact_instructions(ArtifactKind::Presentation);
    assert!(instructions.contains("\"presentation\""));
    assert!(instructions.contains("create_artifact"));
}
