use serde_json::json;

use super::*;

fn line(kind: &str, payload: serde_json::Value) -> String {
    json!({ "timestamp": "2026-10-29T23:25:53.000Z", "type": kind, "payload": payload }).to_string()
}

fn write_session(home: &Path, id: &str, lines: &[String]) -> PathBuf {
    let dir = home.join("sessions/2026/10/29");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("rollout-2026-10-29T23-25-53-{id}.jsonl"));
    std::fs::write(&path, lines.join("\n")).unwrap();
    path
}

const ID: &str = "01a18aa0-c31a-7ed0-9471-758ac549a227";

fn sample_lines() -> Vec<String> {
    vec![
        line(
            "session_meta",
            json!({ "id": ID, "cwd": "/tmp/project", "timestamp": "2026-10-29T23:25:53.000Z" }),
        ),
        line("turn_context", json!({ "model": "gpt-6-luna" })),
        line(
            "response_item",
            json!({ "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "<environment_context>…" }] }),
        ),
        line(
            "event_msg",
            json!({ "type": "item_completed", "item": {
                "type": "UserMessage", "id": "u1",
                "content": [{ "type": "text", "text": "List the files" }],
            } }),
        ),
        line(
            "event_msg",
            json!({ "type": "item_completed", "item": {
                "type": "CommandExecution", "id": "exec-1",
                "command": ["/bin/zsh", "-lc", "ls -la"],
                "parsed_cmd": [{ "type": "list_files", "cmd": "ls -la", "path": null }],
                "status": "completed", "aggregated_output": "a.rs\n", "exit_code": 0,
            } }),
        ),
        line(
            "event_msg",
            json!({ "type": "item_completed", "item": {
                "type": "AgentMessage", "id": "msg-1",
                "content": [{ "type": "Text", "text": "There is one file." }],
            } }),
        ),
        line(
            "event_msg",
            json!({ "type": "task_complete", "turn_id": "turn-1", "duration_ms": 7582 }),
        ),
    ]
}

#[test]
fn loads_a_session_into_the_timeline() {
    let home = tempfile::tempdir().unwrap();
    let path = write_session(home.path(), ID, &sample_lines());
    let loaded = load_session(&path).unwrap();
    assert_eq!(loaded.session_id.as_deref(), Some(ID));
    assert_eq!(loaded.cwd.as_deref(), Some("/tmp/project"));
    assert_eq!(loaded.model.as_deref(), Some("gpt-6-luna"));
    assert_eq!(loaded.title.as_deref(), Some("List the files"));

    let kinds: Vec<&str> = loaded
        .items
        .iter()
        .map(|item| match item {
            TimelineItem::User(_) => "user",
            TimelineItem::Assistant(_) => "assistant",
            TimelineItem::Thinking(_) => "thinking",
            TimelineItem::Tool(_) => "tool",
            TimelineItem::Notice(_) => "notice",
            TimelineItem::TurnEnd(_) => "turn-end",
        })
        .collect();
    assert_eq!(kinds, ["user", "tool", "assistant", "turn-end"]);
    match &loaded.items[1] {
        TimelineItem::Tool(tool) => {
            assert_eq!(tool.name, "LS");
            assert_eq!(tool.input["command"], "ls -la");
            assert_eq!(tool.status, ToolStatus::Done);
            assert_eq!(tool.result.as_ref().unwrap().text, "a.rs\n");
        }
        other => panic!("expected a tool, got {other:?}"),
    }
}

#[test]
fn thread_names_title_the_session() {
    let home = tempfile::tempdir().unwrap();
    let path = write_session(home.path(), ID, &sample_lines());
    std::fs::write(
        home.path().join("session_index.jsonl"),
        json!({ "id": ID, "thread_name": "Listar arquivos" }).to_string(),
    )
    .unwrap();
    assert_eq!(
        load_session(&path).unwrap().title.as_deref(),
        Some("Listar arquivos")
    );
    let sessions = list_sessions(home.path());
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, ID);
    assert_eq!(sessions[0].title.as_deref(), Some("Listar arquivos"));
    assert_eq!(sessions[0].cwd.as_deref(), Some(Path::new("/tmp/project")));
    assert_eq!(find_session(home.path(), ID), Some(path));
}

#[test]
fn legacy_events_load_when_there_are_no_items() {
    let lines = [
        line("session_meta", json!({ "id": ID, "cwd": "/tmp/project" })),
        line(
            "event_msg",
            json!({ "type": "user_message", "message": "Fix it" }),
        ),
        line(
            "event_msg",
            json!({ "type": "agent_reasoning", "text": "Looking at the code" }),
        ),
        line(
            "event_msg",
            json!({ "type": "exec_command_end", "call_id": "c1", "command": ["bash", "-lc", "cargo test"],
                    "aggregated_output": "failed", "exit_code": 1 }),
        ),
        line(
            "event_msg",
            json!({ "type": "patch_apply_end", "call_id": "p1", "success": true,
                    "changes": { "/tmp/project/a.rs": { "update": { "unified_diff": "@@ -1 +1 @@\n-a\n+b\n" } } } }),
        ),
        line(
            "event_msg",
            json!({ "type": "agent_message", "message": "Done" }),
        ),
        line("event_msg", json!({ "type": "turn_aborted" })),
    ];
    let entries: Vec<serde_json::Value> = lines
        .iter()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let loaded = build_transcript(&entries);
    assert_eq!(loaded.items.len(), 6);
    match &loaded.items[2] {
        TimelineItem::Tool(tool) => {
            assert_eq!(tool.input["command"], "cargo test");
            assert_eq!(tool.status, ToolStatus::Failed);
        }
        other => panic!("expected the command, got {other:?}"),
    }
    match &loaded.items[3] {
        TimelineItem::Tool(tool) => {
            assert_eq!(tool.name, "Edit");
            let structured = tool.result.as_ref().unwrap().structured.as_ref().unwrap();
            assert_eq!(
                structured["structuredPatch"][0]["lines"],
                json!(["-a", "+b"])
            );
        }
        other => panic!("expected the edit, got {other:?}"),
    }
    assert!(matches!(
        &loaded.items[5],
        TimelineItem::Notice(Notice {
            kind: NoticeKind::Interrupted,
            ..
        })
    ));
}

#[test]
fn ids_come_from_file_names_too() {
    assert_eq!(
        id_from_file_name(Path::new(&format!(
            "/x/rollout-2026-10-29T23-25-53-{ID}.jsonl"
        ))),
        Some(ID.to_owned())
    );
}
