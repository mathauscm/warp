use claude_chat::presentation::{Row, WorkGroup};
use claude_chat::timeline::{
    AssistantText, TimelineItem, TodoStatus, ToolCall, ToolResult, ToolStatus, UserMessage,
};
use pathfinder_geometry::vector::vec2f;
use serde_json::json;
use warpui::elements::ChildAnchor;

use super::details::{
    DiffLineKind, clean_result_text, compacted_label, diff_texts, first_lines, first_sentence,
    format_tokens, strip_ansi, todo_lines, tool_diff,
};
use super::markdown::{Block, column_weights, language_label, split_blocks};
use super::{
    MENTION_HEIGHT, MENTION_OFFSET, first_changed_row, mention_placement, row_key, row_last_item,
};

fn tool(name: &str, input: serde_json::Value) -> ToolCall {
    ToolCall {
        id: "toolu_1".to_owned(),
        name: name.to_owned(),
        input,
        partial_input: String::new(),
        status: ToolStatus::Done,
        result: None,
        parent_tool_use_id: None,
        started_at: None,
        finished_at: None,
    }
}

#[test]
fn split_blocks_separates_prose_code_and_tables() {
    let blocks = split_blocks(
        "# Título\n\nUm parágrafo com `código`.\n\n```rust\nfn main() {}\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nFim.",
    );
    assert_eq!(blocks.len(), 4, "{blocks:#?}");
    assert!(matches!(blocks[0], Block::Prose(_)));
    assert_eq!(
        blocks[1],
        Block::Code {
            language: "rust".to_owned(),
            code: "fn main() {}".to_owned(),
        }
    );
    match &blocks[2] {
        Block::Table(table) => {
            assert_eq!(table.headers.len(), 2);
            assert_eq!(table.rows.len(), 1);
        }
        other => panic!("expected a table, got {other:?}"),
    }
    assert!(matches!(blocks[3], Block::Prose(_)));
}

#[test]
fn split_blocks_keeps_a_single_prose_run_together() {
    let blocks = split_blocks("Primeiro.\n\n- um\n- dois\n\nÚltimo.");
    assert_eq!(blocks.len(), 1);
}

#[test]
fn split_blocks_handles_an_unclosed_fence_while_streaming() {
    let blocks = split_blocks("Veja:\n\n```python\nprint('oi')");
    assert!(
        blocks
            .iter()
            .any(|block| matches!(block, Block::Code { code, .. } if code.contains("print"))),
        "{blocks:#?}"
    );
}

#[test]
fn split_blocks_of_empty_text_is_empty() {
    assert!(split_blocks("").is_empty());
}

#[test]
fn column_weights_follow_the_longest_cell() {
    let blocks = split_blocks("| n | descrição |\n|---|---|\n| 1 | uma descrição bem mais longa |");
    let Block::Table(table) = &blocks[0] else {
        panic!("expected a table");
    };
    let weights = column_weights(table);
    assert_eq!(weights.len(), 2);
    assert!(weights[1] > weights[0]);
}

#[test]
fn language_labels() {
    assert_eq!(language_label(""), "texto");
    assert_eq!(language_label("sh"), "bash");
    assert_eq!(language_label("ts"), "typescript");
    assert_eq!(language_label("rust title=main.rs"), "rust");
}

#[test]
fn diff_texts_marks_changes_with_context() {
    let old = "a\nb\nc\nd\n";
    let new = "a\nB\nc\nd\ne\n";
    let lines = diff_texts(old, new);
    let kinds: Vec<_> = lines.iter().map(|line| line.kind).collect();
    assert!(kinds.contains(&DiffLineKind::Added));
    assert!(kinds.contains(&DiffLineKind::Removed));
    assert!(
        lines
            .iter()
            .any(|line| line.kind == DiffLineKind::Added && line.text == "e")
    );
    assert!(
        lines
            .iter()
            .any(|line| line.kind == DiffLineKind::Removed && line.text == "b")
    );
}

#[test]
fn tool_diff_prefers_the_structured_patch() {
    let mut edit = tool(
        "Edit",
        json!({"file_path": "/a.rs", "old_string": "x", "new_string": "y"}),
    );
    edit.result = Some(ToolResult {
        text: "ok".to_owned(),
        is_error: false,
        structured: Some(json!({
            "structuredPatch": [
                {"lines": [" ctx", "-old", "+new"]},
                {"lines": ["+more"]}
            ]
        })),
    });
    let lines = tool_diff(&edit);
    let kinds: Vec<_> = lines.iter().map(|line| line.kind).collect();
    assert_eq!(
        kinds,
        vec![
            DiffLineKind::Context,
            DiffLineKind::Removed,
            DiffLineKind::Added,
            DiffLineKind::Gap,
            DiffLineKind::Added,
        ]
    );
    assert_eq!(lines[1].text, "old");
}

#[test]
fn tool_diff_of_a_new_file_adds_every_line() {
    let write = tool(
        "Write",
        json!({"file_path": "/a.txt", "content": "oi\ntchau"}),
    );
    let lines = tool_diff(&write);
    assert_eq!(lines.len(), 2);
    assert!(lines.iter().all(|line| line.kind == DiffLineKind::Added));
}

#[test]
fn clean_result_text_drops_system_reminders() {
    let text = "linha 1\n<system-reminder>\nsegredo\n</system-reminder>\nlinha 2\n\n";
    assert_eq!(clean_result_text(text), "linha 1\n\nlinha 2");
    assert_eq!(clean_result_text("a<system-reminder>sem fim"), "a");
}

#[test]
fn first_lines_counts_what_it_leaves_out() {
    assert_eq!(first_lines("a\nb\nc", 2), ("a\nb".to_owned(), 1));
    assert_eq!(first_lines("a", 2), ("a".to_owned(), 0));
}

#[test]
fn tokens_are_formatted_compactly() {
    assert_eq!(format_tokens(950), "950");
    assert_eq!(format_tokens(1_000), "1k");
    assert_eq!(format_tokens(12_345), "12k");
    assert_eq!(format_tokens(2_450), "2.5k");
    assert_eq!(format_tokens(180_000), "180k");
    assert_eq!(format_tokens(1_200_000), "1.2M");
    assert_eq!(
        compacted_label(Some(180_000), Some(22_000)),
        "Conversa compactada · 180k → 22k tokens"
    );
    assert_eq!(compacted_label(None, None), "Conversa compactada");
}

#[test]
fn strip_ansi_removes_escape_sequences() {
    assert_eq!(strip_ansi("\u{1b}[1;32mok\u{1b}[0m fim"), "ok fim");
    assert_eq!(strip_ansi("\u{1b}]8;;http://x\u{7}link"), "link");
    assert_eq!(strip_ansi("sem cor"), "sem cor");
}

#[test]
fn first_sentence_is_cut_with_an_ellipsis() {
    assert_eq!(first_sentence("\n**Planejando**\nresto", 40), "Planejando");
    assert_eq!(first_sentence("abcdefgh", 4), "abcd…");
}

#[test]
fn todo_lines_read_todo_write_and_task_tools() {
    let write = tool(
        "TodoWrite",
        json!({"todos": [
            {"content": "Ler", "status": "completed", "activeForm": "Lendo"},
            {"content": "Escrever", "status": "in_progress", "activeForm": "Escrevendo"},
            {"content": "Testar", "status": "pending", "activeForm": "Testando"}
        ]}),
    );
    let lines = todo_lines(&write);
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0].status, TodoStatus::Completed);
    assert_eq!(lines[1].status, TodoStatus::InProgress);
    assert_eq!(lines[2].text, "Testar");

    let create = tool("TaskCreate", json!({"subject": "Revisar"}));
    assert_eq!(todo_lines(&create)[0].text, "Revisar");
}

fn items() -> Vec<TimelineItem> {
    vec![
        TimelineItem::User(UserMessage {
            id: "u1".to_owned(),
            text: "oi".to_owned(),
            images: Vec::new(),
            command: None,
            timestamp: None,
        }),
        TimelineItem::Tool(tool("Bash", json!({"command": "ls"}))),
        TimelineItem::Assistant(AssistantText {
            id: "a1".to_owned(),
            markdown: "pronto".to_owned(),
            is_streaming: false,
            parent_tool_use_id: None,
            timestamp: None,
        }),
    ]
}

#[test]
fn row_keys_are_stable_item_ids() {
    let items = items();
    assert_eq!(row_key(&Row::UserBubble { index: 0 }, &items), "user:u1");
    assert_eq!(row_key(&Row::Tool { index: 1 }, &items), "tool:toolu_1");
    assert_eq!(
        row_key(&Row::AssistantProse { index: 2 }, &items),
        "assistant:a1"
    );
    let group = Row::WorkGroup(WorkGroup {
        id: "work:u1".to_owned(),
        items: vec![1],
        rows: vec![Row::Tool { index: 1 }],
        duration_ms: None,
        is_live: false,
        summary: String::new(),
        collapsed_by_default: true,
    });
    assert_eq!(row_key(&group, &items), "work:u1");
    assert_eq!(row_last_item(&group), 1);
}

#[test]
fn first_changed_row_takes_the_earliest_change() {
    let keys = |keys: &[&str]| keys.iter().map(|key| key.to_string()).collect::<Vec<_>>();
    let old = keys(&["a", "b", "c"]);
    assert_eq!(
        first_changed_row(&old, &keys(&["a", "b", "c", "d"]), None),
        3
    );
    assert_eq!(first_changed_row(&old, &keys(&["a", "x", "c"]), None), 1);
    assert_eq!(first_changed_row(&old, &keys(&["a", "b", "c"]), Some(2)), 2);
    assert_eq!(first_changed_row(&old, &keys(&["a"]), None), 1);
    assert_eq!(first_changed_row(&[], &keys(&["a"]), Some(0)), 0);
}

#[test]
fn split_blocks_turns_quoted_lines_into_a_quote() {
    let blocks = split_blocks("Antes.\n\n> citado\n> mais\n\nDepois.\n\n```\n> não é citação\n```");
    assert_eq!(blocks.len(), 4, "{blocks:#?}");
    assert!(matches!(&blocks[1], Block::Quote(inner) if inner.len() == 1));
    assert!(matches!(&blocks[3], Block::Code { code, .. } if code == "> não é citação"));
}

#[test]
fn code_blocks_get_syntax_colors_for_known_languages() {
    use pathfinder_color::ColorU;
    use syntax_tree::{ColorMap, highlight_snippet};
    let keyword = ColorU::new(1, 2, 3, 255);
    let color_map = ColorMap {
        keyword_color: keyword,
        function_color: ColorU::white(),
        string_color: ColorU::white(),
        type_color: ColorU::white(),
        number_color: ColorU::white(),
        comment_color: ColorU::white(),
        property_color: ColorU::white(),
        tag_color: ColorU::white(),
        by_capture: None,
    };
    let highlights = highlight_snippet("rs", "fn main() {}", color_map);
    assert!(highlights.contains(&(0..2, keyword)), "{highlights:?}");
    assert!(highlight_snippet("texto", "fn main() {}", color_map).is_empty());
}

const LINE_HEIGHT: f32 = 20.;
const VISIBLE_HEIGHT: f32 = 400.;

#[test]
fn mention_pill_goes_under_the_last_line_of_a_forward_selection() {
    let (offset, anchor) = mention_placement(vec2f(300., 100.), false, LINE_HEIGHT, VISIBLE_HEIGHT);
    assert_eq!(
        offset,
        vec2f(300., 100. + LINE_HEIGHT / 2. + MENTION_OFFSET)
    );
    assert!(matches!(anchor, ChildAnchor::TopRight));
}

#[test]
fn mention_pill_goes_above_the_first_line_of_a_backward_selection() {
    let (offset, anchor) = mention_placement(vec2f(300., 100.), true, LINE_HEIGHT, VISIBLE_HEIGHT);
    assert_eq!(
        offset,
        vec2f(300., 100. - LINE_HEIGHT / 2. - MENTION_OFFSET)
    );
    assert!(matches!(anchor, ChildAnchor::BottomLeft));
}

#[test]
fn mention_pill_goes_along_the_line_without_room_above_or_below() {
    let bottom = VISIBLE_HEIGHT - MENTION_HEIGHT / 2.;
    let (offset, anchor) =
        mention_placement(vec2f(300., bottom), false, LINE_HEIGHT, VISIBLE_HEIGHT);
    assert_eq!(offset, vec2f(300. + MENTION_OFFSET, bottom));
    assert!(matches!(anchor, ChildAnchor::MiddleLeft));

    let (offset, anchor) = mention_placement(vec2f(300., 10.), true, LINE_HEIGHT, VISIBLE_HEIGHT);
    assert_eq!(offset, vec2f(300. - MENTION_OFFSET, 10.));
    assert!(matches!(anchor, ChildAnchor::MiddleRight));
}
