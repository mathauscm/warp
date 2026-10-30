use std::time::SystemTime;

use serde_json::{Value, json};

use super::*;
use crate::reducer::{Conversation, ConversationStatus};
use crate::timeline::{PendingRequestKind, TimelineItem, ToolStatus};

/// Feeds the client's messages into a conversation and returns the lines it wrote.
struct Harness {
    client: CodexClient,
    conversation: Conversation,
    written: Vec<Value>,
    events: Vec<CodexEvent>,
}

impl Harness {
    fn new() -> Self {
        let mut config = CodexConfig::new("/tmp/project");
        config.client_version = "0.1.0".to_owned();
        let mut harness = Self {
            client: CodexClient::new(config),
            conversation: Conversation::new(None),
            written: Vec::new(),
            events: Vec::new(),
        };
        let events = harness.client.start();
        harness.apply(events);
        harness
    }

    fn apply(&mut self, events: Vec<CodexEvent>) {
        for event in events {
            match &event {
                CodexEvent::Write(line) => self.written.push(serde_json::from_str(line).unwrap()),
                CodexEvent::Message(message) => {
                    self.conversation.apply(message.clone(), SystemTime::now());
                }
                CodexEvent::ContextTokens { tokens, window } => {
                    self.conversation.apply_context_tokens(*tokens, *window);
                }
                CodexEvent::Models(models) => {
                    self.conversation
                        .apply_initialize_response(&CodexClient::initialize_response(
                            models, "default",
                        ));
                }
                CodexEvent::ThreadStarted { .. }
                | CodexEvent::TitleChanged(_)
                | CodexEvent::FileSuggestions { .. }
                | CodexEvent::Error(_) => {}
            }
            self.events.push(event);
        }
    }

    fn receive(&mut self, message: Value) {
        let events = self.client.handle(message);
        self.apply(events);
    }

    fn send(&mut self, uuid: &str, text: &str) {
        self.conversation
            .push_local_user_message(uuid, text, Vec::new(), SystemTime::now());
        let events = self.client.send_message(uuid, text, &[]);
        self.apply(events);
    }

    fn last_request(&self, method: &str) -> &Value {
        self.written
            .iter()
            .rev()
            .find(|line| line.get("method").and_then(Value::as_str) == Some(method))
            .unwrap_or_else(|| panic!("no {method} request in {:?}", self.written))
    }

    fn request_id(&self, method: &str) -> i64 {
        self.last_request(method)["id"].as_i64().unwrap()
    }

    /// Answers the handshake and starts the thread `thread-1`.
    fn ready(&mut self) {
        let id = self.request_id("initialize");
        self.receive(json!({ "id": id, "result": { "userAgent": "codex" } }));
        let id = self.request_id("model/list");
        self.receive(json!({ "id": id, "result": { "data": [
            {
                "id": "gpt-6-luna", "model": "gpt-6-luna", "displayName": "GPT-6-Luna",
                "description": "Fast", "hidden": false, "isDefault": true,
                "defaultReasoningEffort": "medium",
                "supportedReasoningEfforts": [
                    { "reasoningEffort": "low" }, { "reasoningEffort": "medium" },
                    { "reasoningEffort": "high" },
                ],
            },
            { "id": "hidden", "model": "hidden", "displayName": "Hidden", "hidden": true },
        ] } }));
        let id = self.request_id("thread/start");
        self.receive(json!({ "id": id, "result": {
            "model": "gpt-6-luna",
            "reasoningEffort": "medium",
            "thread": { "id": "thread-1", "name": null },
        } }));
    }

    fn start_turn(&mut self, uuid: &str, text: &str) {
        self.send(uuid, text);
        let id = self.request_id("turn/start");
        self.receive(
            json!({ "id": id, "result": { "turn": { "id": "turn-1", "status": "inProgress" } } }),
        );
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.receive(json!({ "method": method, "params": params }));
    }
}

#[test]
fn handshake_lists_models_and_starts_the_thread() {
    let mut harness = Harness::new();
    assert_eq!(harness.written[0]["method"], "initialize");
    assert_eq!(
        harness.written[0]["params"]["clientInfo"]["name"],
        "picolas"
    );
    harness.ready();

    let methods: Vec<&str> = harness
        .written
        .iter()
        .filter_map(|line| line.get("method").and_then(Value::as_str))
        .collect();
    assert_eq!(
        methods,
        ["initialize", "initialized", "model/list", "thread/start"]
    );
    let start = harness.last_request("thread/start");
    assert_eq!(start["params"]["cwd"], "/tmp/project");
    assert_eq!(start["params"]["approvalPolicy"], "untrusted");
    assert_eq!(start["params"]["sandbox"], "workspace-write");

    assert_eq!(harness.client.thread_id(), Some("thread-1"));
    assert_eq!(harness.conversation.status(), ConversationStatus::Idle);
    let session = harness.conversation.session();
    assert_eq!(session.session_id.as_deref(), Some("thread-1"));
    assert_eq!(session.model.as_deref(), Some("gpt-6-luna"));
    assert_eq!(session.effort.as_deref(), Some("medium"));
    let models = harness.conversation.models();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].display_name, "GPT-6-Luna");
    assert_eq!(models[0].supported_effort_levels, ["low", "medium", "high"]);
}

#[test]
fn a_message_sent_before_the_thread_is_ready_waits_for_it() {
    let mut harness = Harness::new();
    harness.send("user-1", "hi");
    assert!(
        harness
            .written
            .iter()
            .all(|line| line["method"] != "turn/start")
    );
    harness.ready();
    let turn = harness.last_request("turn/start");
    assert_eq!(turn["params"]["threadId"], "thread-1");
    assert_eq!(turn["params"]["input"][0]["text"], "hi");
    assert_eq!(turn["params"]["sandboxPolicy"]["type"], "workspaceWrite");
}

#[test]
fn streams_the_answer_and_ends_the_turn() {
    let mut harness = Harness::new();
    harness.ready();
    harness.start_turn("user-1", "hello");
    assert!(matches!(
        harness.conversation.status(),
        ConversationStatus::Working { .. }
    ));

    harness.notify(
        "item/started",
        json!({ "item": { "type": "agentMessage", "id": "msg-1", "text": "" } }),
    );
    harness.notify(
        "item/agentMessage/delta",
        json!({ "itemId": "msg-1", "delta": "Hel" }),
    );
    harness.notify(
        "item/agentMessage/delta",
        json!({ "itemId": "msg-1", "delta": "lo!" }),
    );
    match harness.conversation.items().last() {
        Some(TimelineItem::Assistant(text)) => {
            assert_eq!(text.markdown, "Hello!");
            assert!(text.is_streaming);
        }
        other => panic!("expected streaming text, got {other:?}"),
    }
    harness.notify(
        "item/completed",
        json!({ "item": { "type": "agentMessage", "id": "msg-1", "text": "Hello!" } }),
    );
    harness.notify(
        "thread/tokenUsage/updated",
        json!({ "tokenUsage": {
            "last": { "inputTokens": 1000, "outputTokens": 20 },
            "modelContextWindow": 258400,
        } }),
    );
    harness.notify(
        "turn/completed",
        json!({ "turn": { "id": "turn-1", "status": "completed", "durationMs": 1200 } }),
    );

    let items = harness.conversation.items();
    let texts: Vec<_> = items
        .iter()
        .filter_map(|item| match item {
            TimelineItem::Assistant(text) => Some((text.markdown.as_str(), text.is_streaming)),
            _ => None,
        })
        .collect();
    assert_eq!(texts, [("Hello!", false)]);
    match items.last() {
        Some(TimelineItem::TurnEnd(summary)) => {
            assert_eq!(summary.duration_ms, Some(1200));
            assert!(!summary.is_error);
        }
        other => panic!("expected a turn end, got {other:?}"),
    }
    assert_eq!(harness.conversation.status(), ConversationStatus::Idle);
    assert_eq!(harness.conversation.live_context_tokens(), Some(1020));
    assert_eq!(harness.conversation.context_window(), Some(258400));
}

#[test]
fn reasoning_summaries_become_thinking() {
    let mut harness = Harness::new();
    harness.ready();
    harness.start_turn("user-1", "think");
    harness.notify(
        "item/started",
        json!({ "item": { "type": "reasoning", "id": "rs-1", "summary": [] } }),
    );
    harness.notify(
        "item/reasoning/summaryTextDelta",
        json!({ "itemId": "rs-1", "delta": "Planning", "summaryIndex": 0 }),
    );
    harness.notify(
        "item/reasoning/textDelta",
        json!({ "itemId": "rs-1", "delta": "raw", "contentIndex": 0 }),
    );
    harness.notify(
        "item/completed",
        json!({ "item": { "type": "reasoning", "id": "rs-1", "summary": ["Planning"] } }),
    );
    let thinking: Vec<_> = harness
        .conversation
        .items()
        .iter()
        .filter_map(|item| match item {
            TimelineItem::Thinking(thinking) => {
                Some((thinking.text.clone(), thinking.is_streaming))
            }
            _ => None,
        })
        .collect();
    assert_eq!(thinking, [("Planning".to_owned(), false)]);
}

#[test]
fn empty_reasoning_adds_nothing() {
    let mut harness = Harness::new();
    harness.ready();
    harness.start_turn("user-1", "hi");
    harness.notify(
        "item/started",
        json!({ "item": { "type": "reasoning", "id": "rs-1", "summary": [] } }),
    );
    harness.notify(
        "item/completed",
        json!({ "item": { "type": "reasoning", "id": "rs-1", "summary": [] } }),
    );
    assert!(
        !harness
            .conversation
            .items()
            .iter()
            .any(|item| matches!(item, TimelineItem::Thinking(_)))
    );
}

fn command_item(status: &str, output: Option<&str>, exit_code: Option<i64>) -> Value {
    json!({
        "type": "commandExecution",
        "id": "exec-1",
        "command": "/bin/zsh -lc 'cargo build'",
        "cwd": "/tmp/project",
        "commandActions": [{ "type": "unknown", "command": "cargo build" }],
        "status": status,
        "aggregatedOutput": output,
        "exitCode": exit_code,
    })
}

#[test]
fn commands_ask_for_approval_on_the_permission_card() {
    let mut harness = Harness::new();
    harness.ready();
    harness.start_turn("user-1", "build it");
    harness.notify(
        "item/started",
        json!({ "item": command_item("inProgress", None, None) }),
    );
    harness.receive(json!({
        "id": 0,
        "method": "item/commandExecution/requestApproval",
        "params": {
            "threadId": "thread-1", "turnId": "turn-1", "itemId": "exec-1",
            "command": "/bin/zsh -lc 'cargo build'", "reason": "Needs the network",
        },
    }));

    assert_eq!(
        harness.conversation.status(),
        ConversationStatus::AwaitingUser
    );
    let request = &harness.conversation.pending_requests()[0];
    assert_eq!(request.request_id, "codex-0");
    assert_eq!(request.tool_use_id.as_deref(), Some("exec-1"));
    match &request.kind {
        PendingRequestKind::Permission {
            tool_name,
            input,
            description,
            suggestions,
            ..
        } => {
            assert_eq!(tool_name, "Bash");
            assert_eq!(input["command"], "cargo build");
            assert_eq!(description.as_deref(), Some("Needs the network"));
            assert_eq!(
                CodexClient::suggestion_decision(&suggestions[0]),
                Some("acceptForSession")
            );
        }
        other => panic!("expected a permission request, got {other:?}"),
    }
    match harness.conversation.item("exec-1") {
        Some(TimelineItem::Tool(tool)) => assert_eq!(tool.status, ToolStatus::AwaitingApproval),
        other => panic!("expected the command, got {other:?}"),
    }

    let events = harness.client.answer_approval("codex-0", "accept");
    assert_eq!(
        events,
        [CodexEvent::Write(
            json!({ "id": 0, "result": { "decision": "accept" } }).to_string()
        )]
    );
    harness
        .conversation
        .resolve_request("codex-0", crate::reducer::RequestDecision::Allowed);
    harness.notify(
        "item/completed",
        json!({ "item": command_item("completed", Some("Finished"), Some(0)) }),
    );
    match harness.conversation.item("exec-1") {
        Some(TimelineItem::Tool(tool)) => {
            assert_eq!(tool.status, ToolStatus::Done);
            assert_eq!(tool.result.as_ref().unwrap().text, "Finished");
        }
        other => panic!("expected the command, got {other:?}"),
    }
}

#[test]
fn declined_and_failing_commands() {
    let mut harness = Harness::new();
    harness.ready();
    harness.start_turn("user-1", "build it");
    harness.notify(
        "item/completed",
        json!({ "item": command_item("declined", None, None) }),
    );
    match harness.conversation.item("exec-1") {
        Some(TimelineItem::Tool(tool)) => assert_eq!(tool.status, ToolStatus::Denied),
        other => panic!("expected the command, got {other:?}"),
    }

    let mut item = command_item("completed", Some("error: nope"), Some(101));
    item["id"] = json!("exec-2");
    harness.notify("item/completed", json!({ "item": item }));
    match harness.conversation.item("exec-2") {
        Some(TimelineItem::Tool(tool)) => assert_eq!(tool.status, ToolStatus::Failed),
        other => panic!("expected the command, got {other:?}"),
    }
}

#[test]
fn single_reads_show_as_read_calls() {
    let tool = command_tool(
        "exec-1",
        &json!({
            "command": "/bin/zsh -lc 'sed -n 1,20p src/main.rs'",
            "commandActions": [{ "type": "read", "command": "sed -n 1,20p src/main.rs", "name": "main.rs", "path": "src/main.rs" }],
        }),
    );
    assert_eq!(tool.name, "Read");
    assert_eq!(tool.input["file_path"], "src/main.rs");
}

#[test]
fn patches_show_one_edit_per_file_with_their_diff() {
    let mut harness = Harness::new();
    harness.ready();
    harness.start_turn("user-1", "edit");
    let item = json!({
        "type": "fileChange",
        "id": "patch-1",
        "status": "inProgress",
        "changes": [
            { "path": "/tmp/project/a.rs", "kind": { "type": "update", "move_path": null },
              "diff": "@@ -1,2 +1,2 @@\n-old\n+new\n same\n" },
            { "path": "/tmp/project/b.rs", "kind": { "type": "add" }, "diff": "fn main() {}\n" },
        ],
    });
    harness.notify("item/started", json!({ "item": item }));
    let mut completed = item.clone();
    completed["status"] = json!("completed");
    harness.notify("item/completed", json!({ "item": completed }));

    match harness.conversation.item("patch-1#0") {
        Some(TimelineItem::Tool(tool)) => {
            assert_eq!(tool.name, "Edit");
            assert_eq!(tool.status, ToolStatus::Done);
            let structured = tool.result.as_ref().unwrap().structured.as_ref().unwrap();
            assert_eq!(
                structured["structuredPatch"][0]["lines"],
                json!(["-old", "+new", " same"])
            );
        }
        other => panic!("expected the edit, got {other:?}"),
    }
    match harness.conversation.item("patch-1#1") {
        Some(TimelineItem::Tool(tool)) => {
            assert_eq!(tool.name, "Write");
            assert_eq!(tool.input["content"], "fn main() {}\n");
        }
        other => panic!("expected the write, got {other:?}"),
    }
}

#[test]
fn interrupting_stops_the_turn() {
    let mut harness = Harness::new();
    harness.ready();
    harness.start_turn("user-1", "long task");
    let events = harness.client.interrupt();
    harness.apply(events);
    let interrupt = harness.last_request("turn/interrupt");
    assert_eq!(interrupt["params"]["turnId"], "turn-1");
    harness.notify(
        "turn/completed",
        json!({ "turn": { "id": "turn-1", "status": "interrupted" } }),
    );
    assert!(
        harness
            .conversation
            .items()
            .iter()
            .any(|item| matches!(item, TimelineItem::Notice(notice) if notice.kind == crate::timeline::NoticeKind::Interrupted))
    );
    assert_eq!(harness.conversation.status(), ConversationStatus::Idle);
}

#[test]
fn failed_turns_show_their_error() {
    let mut harness = Harness::new();
    harness.ready();
    harness.start_turn("user-1", "hi");
    harness.notify(
        "error",
        json!({ "error": { "message": "Rate limit reached" }, "willRetry": false }),
    );
    harness.notify(
        "turn/completed",
        json!({ "turn": { "id": "turn-1", "status": "failed", "error": null } }),
    );
    assert!(harness.conversation.items().iter().any(|item| matches!(
        item,
        TimelineItem::Notice(notice)
            if notice.kind == crate::timeline::NoticeKind::Error("Rate limit reached".to_owned())
    )));
}

#[test]
fn messages_sent_while_busy_run_after_the_turn() {
    let mut harness = Harness::new();
    harness.ready();
    harness.start_turn("user-1", "first");
    harness.send("user-2", "second");
    let turn_starts = |harness: &Harness| {
        harness
            .written
            .iter()
            .filter(|line| line["method"] == "turn/start")
            .count()
    };
    assert_eq!(turn_starts(&harness), 1);
    harness.notify(
        "turn/completed",
        json!({ "turn": { "id": "turn-1", "status": "completed" } }),
    );
    assert_eq!(turn_starts(&harness), 2);
    assert_eq!(
        harness.last_request("turn/start")["params"]["input"][0]["text"],
        "second"
    );
}

#[test]
fn unanswered_approvals_are_withdrawn_when_resolved_elsewhere() {
    let mut harness = Harness::new();
    harness.ready();
    harness.start_turn("user-1", "build");
    harness.receive(json!({
        "id": 7,
        "method": "item/fileChange/requestApproval",
        "params": { "threadId": "thread-1", "turnId": "turn-1", "itemId": "patch-1" },
    }));
    assert_eq!(harness.conversation.pending_requests().len(), 1);
    harness.notify(
        "serverRequest/resolved",
        json!({ "threadId": "thread-1", "requestId": 7 }),
    );
    assert!(harness.conversation.pending_requests().is_empty());
}

#[test]
fn unsupported_server_requests_get_an_error() {
    let mut harness = Harness::new();
    harness.receive(json!({ "id": "abc", "method": "item/tool/call", "params": {} }));
    let reply = harness.written.last().unwrap();
    assert_eq!(reply["id"], "abc");
    assert_eq!(reply["error"]["code"], -32601);
}

#[test]
fn plan_updates_fill_the_task_list() {
    let mut harness = Harness::new();
    harness.ready();
    harness.start_turn("user-1", "plan");
    harness.notify(
        "turn/plan/updated",
        json!({ "plan": [
            { "step": "Read the code", "status": "completed" },
            { "step": "Fix the bug", "status": "inProgress" },
        ] }),
    );
    let todos: Vec<_> = harness
        .conversation
        .todos()
        .iter()
        .map(|todo| todo.content.as_str())
        .collect();
    assert_eq!(todos, ["Read the code", "Fix the bug"]);
}

#[test]
fn permission_modes_map_to_policies() {
    let roots = [std::path::PathBuf::from("/tmp/other")];
    let (approval, sandbox) = turn_policy("default", &roots);
    assert_eq!(approval, "untrusted");
    assert_eq!(sandbox["type"], "workspaceWrite");
    assert_eq!(sandbox["writableRoots"], json!(["/tmp/other"]));
    assert_eq!(turn_policy("acceptEdits", &[]).0, "on-request");
    assert_eq!(turn_policy("plan", &[]).1["type"], "readOnly");
    let (approval, sandbox) = turn_policy("bypassPermissions", &[]);
    assert_eq!(approval, "never");
    assert_eq!(sandbox["type"], "dangerFullAccess");
}

#[test]
fn resuming_a_thread_falls_back_to_a_new_one() {
    let mut config = CodexConfig::new("/tmp/project");
    config.resume_thread_id = Some("old".to_owned());
    let mut client = CodexClient::new(config);
    client.start();
    let events = client.handle(json!({ "id": 1, "result": {} }));
    let resume = events
        .iter()
        .find_map(|event| match event {
            CodexEvent::Write(line) if line.contains("thread/resume") => Some(line.clone()),
            _ => None,
        })
        .unwrap();
    let resume: Value = serde_json::from_str(&resume).unwrap();
    assert_eq!(resume["params"]["threadId"], "old");
    let id = resume["id"].clone();
    let events = client.handle(json!({ "id": id, "error": { "message": "not found" } }));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, CodexEvent::Error(_)))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, CodexEvent::Write(line) if line.contains("thread/start")))
    );
}

#[test]
fn unwraps_shell_commands() {
    assert_eq!(unwrap_shell("/bin/zsh -lc 'ls -la'"), "ls -la");
    assert_eq!(unwrap_shell("bash -c 'echo '\\''hi'\\'''"), "echo 'hi'");
    assert_eq!(unwrap_shell("cargo test"), "cargo test");
}

#[test]
fn reads_the_models_cache() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("models_cache.json"),
        json!({ "models": [
            { "slug": "b", "display_name": "B", "visibility": "list", "priority": 9,
              "supported_reasoning_levels": [{ "effort": "low" }], "default_reasoning_level": "low" },
            { "slug": "hidden", "display_name": "H", "visibility": "hide", "priority": 1 },
            { "slug": "a", "display_name": "A", "visibility": "list", "priority": 4 },
        ] })
        .to_string(),
    )
    .unwrap();
    let models = read_models_cache(dir.path());
    let ids: Vec<_> = models.iter().map(|model| model.id.as_str()).collect();
    assert_eq!(ids, ["a", "b"]);
    assert!(models[0].is_default);
    assert_eq!(models[1].efforts, ["low"]);
}

#[test]
fn finds_codex_on_the_path() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let codex = bin.join("codex");
    std::fs::write(&codex, "#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path_env = bin.to_string_lossy().into_owned();
    assert_eq!(
        find_codex_in(Some(&path_env), Some(dir.path())),
        Some(codex)
    );
}

#[test]
fn file_suggestions_search_the_chat_folders() {
    let mut harness = Harness::new();
    harness.ready();
    let events = harness.client.file_suggestions("main");
    harness.apply(events);
    let search = harness.last_request("fuzzyFileSearch");
    assert_eq!(search["params"]["roots"], json!(["/tmp/project"]));
    let id = search["id"].clone();
    let events = harness.client.handle(json!({ "id": id, "result": { "files": [
        { "file_name": "main.rs", "path": "src/main.rs", "root": "/tmp/project", "score": 9, "match_type": "file" },
    ] } }));
    assert_eq!(
        events,
        [CodexEvent::FileSuggestions {
            query: "main".to_owned(),
            paths: vec!["src/main.rs".to_owned()],
        }]
    );
}
