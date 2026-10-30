use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use super::*;
use crate::timeline::{AssistantText, Notice, Thinking, ToolResult, TurnSummary, UserMessage};

fn at(seconds: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(seconds)
}

fn user(id: &str, seconds: u64) -> TimelineItem {
    TimelineItem::User(UserMessage {
        id: id.to_owned(),
        text: format!("prompt {id}"),
        images: Vec::new(),
        command: None,
        timestamp: Some(at(seconds)),
    })
}

fn assistant(id: &str, seconds: u64) -> TimelineItem {
    TimelineItem::Assistant(AssistantText {
        id: id.to_owned(),
        markdown: format!("text {id}"),
        is_streaming: false,
        parent_tool_use_id: None,
        timestamp: Some(at(seconds)),
    })
}

fn thinking(id: &str) -> TimelineItem {
    TimelineItem::Thinking(Thinking {
        id: id.to_owned(),
        text: "hmm".to_owned(),
        is_streaming: false,
        duration_ms: None,
        parent_tool_use_id: None,
    })
}

fn tool_call(id: &str, name: &str, input: Value, status: ToolStatus) -> ToolCall {
    ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        input,
        partial_input: String::new(),
        status,
        result: None,
        parent_tool_use_id: None,
        started_at: None,
        finished_at: None,
    }
}

fn tool(id: &str, name: &str, input: Value) -> TimelineItem {
    TimelineItem::Tool(tool_call(id, name, input, ToolStatus::Done))
}

fn read(id: &str, path: &str) -> TimelineItem {
    tool(id, "Read", json!({ "file_path": path }))
}

fn bash(id: &str, command: &str) -> TimelineItem {
    tool(id, "Bash", json!({ "command": command }))
}

fn notice(id: &str, kind: NoticeKind) -> TimelineItem {
    TimelineItem::Notice(Notice {
        id: id.to_owned(),
        kind,
    })
}

fn turn_end(id: &str, duration_ms: Option<u64>, is_error: bool) -> TimelineItem {
    TimelineItem::TurnEnd(TurnSummary {
        id: id.to_owned(),
        duration_ms,
        cost_usd: None,
        is_error,
        subtype: if is_error {
            "error_max_turns"
        } else {
            "success"
        }
        .to_owned(),
    })
}

fn group(row: &Row) -> &WorkGroup {
    match row {
        Row::WorkGroup(group) => group,
        other => panic!("expected a work group, got {other:?}"),
    }
}

#[test]
fn folds_a_finished_turn_except_the_final_answer() {
    let items = vec![
        user("u", 0),
        thinking("t"),
        read("r1", "/p/a.rs"),
        read("r2", "/p/b.rs"),
        bash("b", "cargo test"),
        assistant("mid", 10),
        tool("e", "Edit", json!({"file_path": "/p/a.rs"})),
        assistant("final", 63),
        turn_end("end", Some(63_000), false),
    ];
    let rows = build_rows(&items, false, at(100));
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0], Row::UserBubble { index: 0 });
    assert_eq!(rows[2], Row::AssistantProse { index: 7 });
    let group = group(&rows[1]);
    assert_eq!(group.id, "work:u");
    assert_eq!(group.items, vec![1, 2, 3, 4, 5, 6]);
    assert_eq!(group.summary, "Trabalhou por 1m 3s");
    assert_eq!(group.duration_ms, Some(63_000));
    assert!(group.collapsed_by_default);
    assert!(!group.is_live);
    assert_eq!(
        group.rows,
        vec![
            Row::Thinking { index: 1 },
            Row::ToolBatch(ToolBatch {
                id: "r1".to_owned(),
                items: vec![2, 3, 4],
                summary: "Leu 2 arquivos e executou 1 comando".to_owned(),
                kind: ToolKind::Read,
                is_live: false,
            }),
            Row::AssistantProse { index: 5 },
            Row::Tool { index: 6 },
        ]
    );
}

#[test]
fn shows_a_live_group_while_working() {
    let mut running = tool_call("b2", "Bash", json!({"command": "ls"}), ToolStatus::Running);
    running.started_at = Some(at(11));
    let items = vec![
        user("u1", 0),
        assistant("a1", 5),
        turn_end("e1", Some(5_000), false),
        user("u2", 100),
        assistant("a2", 105),
        bash("b1", "pwd"),
        TimelineItem::Tool(running),
    ];
    let rows = build_rows(&items, true, at(112));
    assert_eq!(
        &rows[..3],
        [
            Row::UserBubble { index: 0 },
            Row::AssistantProse { index: 1 },
            Row::UserBubble { index: 3 },
        ]
    );
    let live = group(&rows[3]);
    assert!(live.is_live);
    assert!(!live.collapsed_by_default);
    assert_eq!(live.id, "work:u2");
    assert_eq!(live.summary, "Trabalhando há 12s");
    assert_eq!(live.items, vec![4, 5, 6]);
    assert!(matches!(&live.rows[1], Row::ToolBatch(batch) if batch.is_live));

    // The same turn once finished keeps the group id.
    let mut finished = items.clone();
    if let TimelineItem::Tool(tool) = &mut finished[6] {
        tool.status = ToolStatus::Done;
    }
    finished.push(assistant("a3", 120));
    finished.push(turn_end("e2", None, false));
    let rows = build_rows(&finished, false, at(500));
    let folded = group(&rows[3]);
    assert_eq!(folded.id, "work:u2");
    assert_eq!(folded.summary, "Trabalhou por 20s");
    assert_eq!(rows[4], Row::AssistantProse { index: 7 });
}

#[test]
fn shows_a_live_group_before_anything_arrives() {
    let items = vec![user("u", 0)];
    let rows = build_rows(&items, true, at(3));
    let live = group(&rows[1]);
    assert_eq!(live.summary, "Trabalhando há 3s");
    assert!(live.rows.is_empty());
    assert_eq!(
        build_rows(&items, false, at(3)),
        vec![Row::UserBubble { index: 0 }]
    );
}

#[test]
fn does_not_fold_interrupted_or_failed_turns() {
    let interrupted = vec![
        user("u", 0),
        TimelineItem::Tool(tool_call(
            "b",
            "Bash",
            json!({"command": "sleep 100"}),
            ToolStatus::Interrupted,
        )),
        notice("i", NoticeKind::Interrupted),
        turn_end("e", Some(3_000), true),
    ];
    assert_eq!(
        build_rows(&interrupted, false, at(10)),
        vec![
            Row::UserBubble { index: 0 },
            Row::Tool { index: 1 },
            Row::Notice { index: 2 },
        ]
    );

    let max_turns = vec![
        user("u", 0),
        bash("b", "ls"),
        assistant("a", 1),
        turn_end("e", Some(3_000), true),
    ];
    assert_eq!(
        build_rows(&max_turns, false, at(10)),
        vec![
            Row::UserBubble { index: 0 },
            Row::Tool { index: 1 },
            Row::AssistantProse { index: 2 },
            Row::TurnError { index: 3 },
        ]
    );
}

#[test]
fn leaves_simple_answers_alone_and_pins_compaction_notices() {
    let simple = vec![
        user("u", 0),
        assistant("a", 1),
        turn_end("e", Some(1_000), false),
    ];
    assert_eq!(
        build_rows(&simple, false, at(10)),
        vec![
            Row::UserBubble { index: 0 },
            Row::AssistantProse { index: 1 }
        ]
    );

    let compacted = vec![
        user("u", 0),
        read("r", "/p/a"),
        notice(
            "c",
            NoticeKind::Compacted {
                pre_tokens: Some(1),
                post_tokens: Some(1),
            },
        ),
        bash("b", "ls"),
        notice("info", NoticeKind::Info("hook said hi".to_owned())),
        assistant("a", 30),
        turn_end("e", None, false),
    ];
    let rows = build_rows(&compacted, false, at(100));
    assert_eq!(rows.len(), 4);
    let folded = group(&rows[1]);
    assert_eq!(folded.items, vec![1, 3, 4]);
    // Without a duration on the turn end, it comes from the timestamps.
    assert_eq!(folded.summary, "Trabalhou por 30s");
    assert_eq!(rows[2], Row::Notice { index: 2 });
    assert_eq!(rows[3], Row::AssistantProse { index: 5 });
}

#[test]
fn starts_a_turn_after_a_turn_end_without_a_prompt() {
    let items = vec![
        user("u", 0),
        assistant("a1", 1),
        turn_end("e1", Some(1_000), false),
        notice(
            "n",
            NoticeKind::Info("Background build finished".to_owned()),
        ),
        bash("b", "cat log"),
        assistant("a2", 50),
        turn_end("e2", Some(4_000), false),
    ];
    let rows = build_rows(&items, false, at(100));
    assert_eq!(
        rows[..2],
        [
            Row::UserBubble { index: 0 },
            Row::AssistantProse { index: 1 }
        ]
    );
    let group = group(&rows[2]);
    assert_eq!(group.id, "work:n");
    assert_eq!(group.items, vec![3, 4]);
    assert_eq!(group.summary, "Trabalhou por 4s");
    assert_eq!(rows[3], Row::AssistantProse { index: 5 });
}

#[test]
fn keeps_subagents_out_of_the_main_rows() {
    let mut child_text = assistant("child", 2);
    if let TimelineItem::Assistant(text) = &mut child_text {
        text.parent_tool_use_id = Some("agent".to_owned());
    }
    let mut child_tool = tool_call(
        "child-read",
        "Read",
        json!({"file_path": "/x"}),
        ToolStatus::Done,
    );
    child_tool.parent_tool_use_id = Some("agent".to_owned());
    let items = vec![
        user("u", 0),
        tool("agent", "Agent", json!({"description": "Explore"})),
        TimelineItem::Tool(child_tool),
        child_text,
        read("r1", "/a"),
        read("r2", "/b"),
        assistant("a", 9),
        turn_end("e", Some(9_000), false),
    ];
    let rows = build_rows(&items, false, at(10));
    let group = group(&rows[1]);
    assert_eq!(group.items, vec![1, 4, 5]);
    assert_eq!(group.rows[0], Row::Tool { index: 1 });
    assert!(matches!(&group.rows[1], Row::ToolBatch(batch) if batch.items == vec![4, 5]));
    assert_eq!(subagent_items(&items, "agent"), vec![2, 3]);
    assert_eq!(
        build_subagent_rows(&items, "agent"),
        vec![Row::Tool { index: 2 }, Row::AssistantProse { index: 3 }]
    );
}

#[test]
fn thinking_after_the_last_call_is_not_batched() {
    let items = vec![
        read("r1", "/a"),
        thinking("t1"),
        read("r2", "/b"),
        thinking("t2"),
    ];
    let rows = work_rows(&items, &[0, 1, 2, 3]);
    assert_eq!(rows.len(), 2);
    assert!(matches!(&rows[0], Row::ToolBatch(batch) if batch.items == vec![0, 1, 2]));
    assert_eq!(rows[1], Row::Thinking { index: 3 });
}

fn summary(items: &[TimelineItem]) -> String {
    let tools: Vec<&ToolCall> = items
        .iter()
        .filter_map(|item| match item {
            TimelineItem::Tool(tool) => Some(tool),
            _ => None,
        })
        .collect();
    summarize_tools(&tools)
}

#[test]
fn summarizes_tool_runs_in_portuguese() {
    assert_eq!(
        summary(&[
            read("1", "/a"),
            read("2", "/b"),
            bash("3", "ls"),
            read("4", "/c"),
            read("5", "/d"),
            bash("6", "pwd"),
        ]),
        "Leu 4 arquivos e executou 2 comandos"
    );
    assert_eq!(
        summary(&[
            tool("1", "Edit", json!({"file_path": "/a"})),
            tool("2", "Edit", json!({"file_path": "/a"})),
            tool("3", "Write", json!({"file_path": "/b"})),
            tool("4", "MultiEdit", json!({"file_path": "/c"})),
        ]),
        "Editou 3 arquivos"
    );
    assert_eq!(
        summary(&[
            tool("1", "Grep", json!({"pattern": "x"})),
            tool("2", "Glob", json!({"pattern": "*.rs"})),
        ]),
        "Pesquisou 2 vezes"
    );
    assert_eq!(
        summary(&[
            read("1", "/a"),
            bash("2", "ls"),
            tool("3", "Edit", json!({"file_path": "/a"})),
            tool("4", "Grep", json!({"pattern": "x"})),
            tool("5", "Grep", json!({"pattern": "y"})),
        ]),
        "Executou 1 comando, editou 1 arquivo e realizou 3 outras ações"
    );
    assert_eq!(
        summary(&[
            tool("1", "WebSearch", json!({"query": "rust"})),
            tool("2", "WebFetch", json!({"url": "https://x"})),
        ]),
        "Buscou na web 1 vez e acessou 1 página"
    );
    assert_eq!(
        summary(&[
            tool("1", "mcp__claude_ai_Gmail__search_threads", json!({})),
            tool("2", "mcp__plugin_linear_linear__list_issues", json!({})),
        ]),
        "Usou Gmail e linear"
    );
    assert_eq!(
        summary(&[
            tool("1", "Mystery", json!({})),
            tool("2", "Mystery", json!({}))
        ]),
        "Usou 2 ferramentas"
    );
}

#[test]
fn labels_tools_by_status() {
    let label = |name: &str, input: Value, status: ToolStatus| {
        tool_label(&tool_call("id", name, input, status)).text()
    };
    let done = ToolStatus::Done;
    assert_eq!(
        label(
            "Bash",
            json!({"command": "cargo test\n--all"}),
            ToolStatus::Running
        ),
        "Executando cargo test …"
    );
    assert_eq!(label("Bash", json!({"command": "ls"}), done), "Executou ls");
    assert_eq!(
        label("Read", json!({"file_path": "/a.rs"}), ToolStatus::Streaming),
        "Lendo /a.rs"
    );
    assert_eq!(
        label("Read", json!({"file_path": "/a.rs"}), ToolStatus::Failed),
        "Leu /a.rs"
    );
    assert_eq!(label("Write", json!({"file_path": "/a"}), done), "Criou /a");
    assert_eq!(
        label(
            "Edit",
            json!({"file_path": "/a"}),
            ToolStatus::AwaitingApproval
        ),
        "Editando /a"
    );
    assert_eq!(
        label("MultiEdit", json!({"file_path": "/a"}), done),
        "Editou /a"
    );
    assert_eq!(
        label("NotebookEdit", json!({"notebook_path": "/n.ipynb"}), done),
        "Editou /n.ipynb"
    );
    assert_eq!(
        label("Grep", json!({"pattern": "fn main", "path": "/src"}), done),
        "Pesquisou fn main em /src"
    );
    assert_eq!(
        label("Glob", json!({"pattern": "**/*.rs"}), done),
        "Procurou arquivos **/*.rs"
    );
    assert_eq!(label("LS", json!({"path": "/src"}), done), "Listou /src");
    assert_eq!(
        label("WebFetch", json!({"url": "https://x.dev"}), done),
        "Acessou https://x.dev"
    );
    assert_eq!(
        label("WebSearch", json!({"query": "rust"}), ToolStatus::Running),
        "Buscando na web rust"
    );
    assert_eq!(
        label("WebSearch", json!({"query": "rust"}), done),
        "Buscou na web rust"
    );
    assert_eq!(
        label(
            "Agent",
            json!({"description": "Map code", "subagent_type": "Explore"}),
            done
        ),
        "Executou agente Map code"
    );
    assert_eq!(
        label("Task", json!({"subagent_type": "Explore"}), done),
        "Executou agente Explore"
    );
    assert_eq!(
        label("TodoWrite", json!({"todos": []}), done),
        "Atualizou tarefas"
    );
    assert_eq!(
        label("TaskCreate", json!({"subject": "Ship"}), done),
        "Criou tarefa Ship"
    );
    assert_eq!(
        label("TaskUpdate", json!({"taskId": "3"}), done),
        "Atualizou tarefa #3"
    );
    assert_eq!(
        label(
            "AskUserQuestion",
            json!({"questions": [{"question": "Qual cor?"}]}),
            ToolStatus::Running
        ),
        "Perguntando Qual cor?"
    );
    assert_eq!(
        label("ExitPlanMode", json!({"plan": "x"}), done),
        "Propôs plano"
    );
    assert_eq!(
        label("Skill", json!({"skill": "approve"}), done),
        "Usou skill approve"
    );
    assert_eq!(
        label("mcp__claude_ai_Gmail__search_threads", json!({}), done),
        "Usou search threads (Gmail)"
    );
    assert_eq!(
        label("mcp__payflow__list_cards", json!({}), ToolStatus::Running),
        "Usando list cards (payflow)"
    );
    assert_eq!(label("Mystery", json!({}), done), "Usou Mystery");
    assert_eq!(label("Bash", json!({}), done), "Executou");

    let read = tool_call(
        "id",
        "Read",
        json!({"file_path": "/repo/src/main.rs"}),
        done,
    );
    assert_eq!(
        tool_label_in(&read, Some(Path::new("/repo")))
            .target
            .as_deref(),
        Some("src/main.rs")
    );
    assert_eq!(
        tool_label_in(&read, Some(Path::new("/other")))
            .target
            .as_deref(),
        Some("/repo/src/main.rs")
    );
    assert_eq!(tool_label(&read).kind, ToolKind::Read);
}

#[test]
fn counts_edit_stats() {
    let edit = tool_call(
        "e",
        "Edit",
        json!({"file_path": "/a", "old_string": "a\nb\nc\n", "new_string": "a\nB\nc\nd\n"}),
        ToolStatus::Done,
    );
    assert_eq!(
        edit_stats(&edit),
        Some(DiffStats {
            added: 2,
            removed: 1,
            path: "/a".to_owned(),
        })
    );

    let multi = tool_call(
        "m",
        "MultiEdit",
        json!({"file_path": "/a", "edits": [
            {"old_string": "x", "new_string": "y"},
            {"old_string": "1\n2", "new_string": "1"},
        ]}),
        ToolStatus::Done,
    );
    assert_eq!(
        edit_stats(&multi).map(|stats| (stats.added, stats.removed)),
        Some((1, 2))
    );

    let mut write = tool_call(
        "w",
        "Write",
        json!({"file_path": "/new", "content": "one\ntwo\nthree\n"}),
        ToolStatus::Done,
    );
    assert_eq!(
        edit_stats(&write).map(|stats| (stats.added, stats.removed)),
        Some((3, 0))
    );
    write.result = Some(ToolResult {
        text: String::new(),
        is_error: false,
        structured: Some(
            json!({"type": "update", "originalFile": "one\n2\n", "structuredPatch": []}),
        ),
    });
    assert_eq!(
        edit_stats(&write).map(|stats| (stats.added, stats.removed)),
        Some((2, 1))
    );

    let mut patched = edit.clone();
    patched.result = Some(ToolResult {
        text: String::new(),
        is_error: false,
        structured: Some(json!({"structuredPatch": [
            {"oldStart": 1, "oldLines": 3, "newStart": 1, "newLines": 4,
             "lines": [" a", "-b", "+B", "+C", "+D", " c"]},
        ]})),
    });
    assert_eq!(
        edit_stats(&patched).map(|stats| (stats.added, stats.removed)),
        Some((3, 1))
    );

    assert_eq!(
        edit_stats(&tool_call(
            "r",
            "Read",
            json!({"file_path": "/a"}),
            ToolStatus::Done
        )),
        None
    );
    assert_eq!(
        edit_stats(&tool_call("e", "Edit", json!({}), ToolStatus::Done)),
        None
    );
}

#[test]
fn diffs_lines() {
    assert_eq!(line_diff("", ""), (0, 0));
    assert_eq!(line_diff("a\nb", "a\nb"), (0, 0));
    assert_eq!(line_diff("a\nb\nc", "c\nb\na"), (2, 2));
    assert_eq!(line_diff("", "x\ny"), (2, 0));
    assert_eq!(line_diff("x\ny\nz", "y"), (0, 2));
}

#[test]
fn formats_durations_in_portuguese() {
    assert_eq!(format_duration_pt(0), "0s");
    assert_eq!(format_duration_pt(400), "1s");
    assert_eq!(format_duration_pt(12_000), "12s");
    assert_eq!(format_duration_pt(59_400), "59s");
    assert_eq!(format_duration_pt(59_500), "1m");
    assert_eq!(format_duration_pt(63_000), "1m 3s");
    assert_eq!(format_duration_pt(3_600_000), "1h");
    assert_eq!(format_duration_pt(3_720_000), "1h 2m");
}

#[test]
fn builds_rows_for_a_loaded_transcript() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/history_capture/08511edb-8e0b-4d07-9989-51ab170193ce.jsonl");
    let transcript = crate::history::load_transcript(&path).unwrap();
    let rows = build_rows(&transcript.items, false, SystemTime::now());
    let groups: Vec<&WorkGroup> = rows
        .iter()
        .filter_map(|row| match row {
            Row::WorkGroup(group) => Some(group),
            _ => None,
        })
        .collect();
    assert!(
        groups
            .iter()
            .all(|group| !group.is_live && group.collapsed_by_default)
    );
    // The turn that only asked a question shows its answer instead of a group.
    assert_eq!(groups.len(), 5);
    assert_eq!(groups[0].summary, "Trabalhou por 5s");
    let answered: Vec<Vec<QuestionAnswer>> = rows
        .iter()
        .filter_map(|row| match row {
            Row::QuestionAnswers { index } => match &transcript.items[*index] {
                TimelineItem::Tool(tool) => Some(question_answers(tool)),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(
        answered,
        vec![vec![answer(
            Some("Color"),
            "Which color do you prefer?",
            "Blue"
        )]]
    );
    // The Agent tool's subagent answer is not a row of its own.
    assert_eq!(
        rows.iter()
            .filter(|row| matches!(row, Row::AssistantProse { .. }))
            .count(),
        6
    );
}

#[test]
fn a_made_artifact_stays_in_view_under_the_answer() {
    let items = vec![
        user("u", 0),
        tool(
            "c",
            "mcp__picolas__create_artifact",
            json!({"title": "Doc"}),
        ),
        tool("l", "mcp__picolas__list_artifacts", json!({})),
        assistant("final", 5),
        turn_end("end", Some(5_000), false),
    ];
    let rows = build_rows(&items, false, at(10));
    assert_eq!(rows[2], Row::AssistantProse { index: 3 });
    assert_eq!(rows[3], Row::Tool { index: 1 });
    assert_eq!(rows.len(), 4);
}

#[test]
fn the_effort_picker_command_is_left_out() {
    let mut effort = user("e", 0);
    if let TimelineItem::User(message) = &mut effort {
        message.command = Some(crate::timeline::SlashCommandInvocation {
            name: "effort".to_owned(),
            args: "high".to_owned(),
            output: None,
        });
    }
    let items = vec![effort, user("u", 1), assistant("a", 2)];
    let rows = build_rows(&items, false, at(10));
    assert_eq!(rows[0], Row::UserBubble { index: 1 });
    assert!(!rows.contains(&Row::UserBubble { index: 0 }));
}

fn answered_question(id: &str, text: &str, structured: Option<Value>) -> ToolCall {
    let mut call = tool_call(
        id,
        "AskUserQuestion",
        json!({"questions": [
            {"question": "Qual stack?", "header": "Stack", "options": [], "multiSelect": false},
            {"question": "Quais recursos?", "header": "Recursos", "options": [], "multiSelect": true},
        ]}),
        ToolStatus::Done,
    );
    call.result = Some(ToolResult {
        text: text.to_owned(),
        is_error: false,
        structured,
    });
    call
}

fn answer(header: Option<&str>, question: &str, answer: &str) -> QuestionAnswer {
    QuestionAnswer {
        header: header.map(str::to_owned),
        question: question.to_owned(),
        answer: answer.to_owned(),
    }
}

#[test]
fn reads_question_answers_from_the_structured_result_in_question_order() {
    let call = answered_question(
        "q",
        "ignored",
        Some(json!({"answers": {
            "Quais recursos?": ["Autenticação", "Testes"],
            "Qual stack?": "React",
        }})),
    );
    assert_eq!(
        question_answers(&call),
        vec![
            answer(Some("Stack"), "Qual stack?", "React"),
            answer(Some("Recursos"), "Quais recursos?", "Autenticação, Testes"),
        ]
    );
}

#[test]
fn reads_question_answers_from_the_result_text() {
    let call = answered_question(
        "q",
        "Your questions have been answered: \"Qual stack?\"=\"React, \"o de sempre\"\", \
         \"Outra pergunta\"=\"Sim\". You can now continue with these answers in mind.",
        None,
    );
    assert_eq!(
        question_answers(&call),
        vec![
            answer(Some("Stack"), "Qual stack?", "React, \"o de sempre\""),
            answer(None, "Outra pergunta", "Sim"),
        ]
    );
    let older = answered_question(
        "q",
        "User has answered your questions: \"Qual stack?\"=\"Vue\". You can now continue with \
         the user's answers in mind.",
        None,
    );
    assert_eq!(
        question_answers(&older),
        vec![answer(Some("Stack"), "Qual stack?", "Vue")]
    );
}

#[test]
fn a_dismissed_or_waiting_question_has_no_answers() {
    let mut denied = answered_question("q", "The user dismissed the question.", None);
    denied.status = ToolStatus::Denied;
    assert!(question_answers(&denied).is_empty());
    let mut failed = answered_question("q", "The user dismissed the question.", None);
    failed.result.as_mut().unwrap().is_error = true;
    assert!(question_answers(&failed).is_empty());
    let mut waiting = answered_question("q", "", None);
    waiting.status = ToolStatus::AwaitingApproval;
    waiting.result = None;
    assert!(question_answers(&waiting).is_empty());
    assert!(question_answers(&tool_call("r", "Read", json!({}), ToolStatus::Done)).is_empty());
}

#[test]
fn an_answered_question_stays_out_of_the_folded_work() {
    let answers = Some(json!({"answers": {"Qual stack?": "React"}}));
    let items = vec![
        user("u", 0),
        read("r", "/p/a.rs"),
        TimelineItem::Tool(answered_question("q", "", answers.clone())),
        assistant("final", 5),
        turn_end("end", Some(5_000), false),
    ];
    let rows = build_rows(&items, false, at(10));
    assert_eq!(group(&rows[1]).items, vec![1]);
    assert_eq!(
        rows[2..],
        [
            Row::QuestionAnswers { index: 2 },
            Row::AssistantProse { index: 3 },
        ]
    );

    let only_the_question = vec![
        user("u", 0),
        TimelineItem::Tool(answered_question("q", "", answers)),
        assistant("final", 5),
        turn_end("end", Some(5_000), false),
    ];
    assert_eq!(
        build_rows(&only_the_question, false, at(10)),
        vec![
            Row::UserBubble { index: 0 },
            Row::QuestionAnswers { index: 1 },
            Row::AssistantProse { index: 2 },
        ]
    );
}
