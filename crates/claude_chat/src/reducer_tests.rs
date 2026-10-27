use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use super::*;
use crate::protocol::{ControlResponse, question_answers};

fn start_time() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_800_000_000)
}

/// Replays a `live_*` fixture: CLI frames go through `apply`, and the `__sent__` lines we wrote
/// drive the UI-side calls at the point they happened.
struct Replay {
    conversation: Conversation,
    tracker: RequestTracker,
    replies: Vec<ControlReply>,
    now: SystemTime,
    /// User messages whose text starts with one of these are sent hidden, like the UI does for
    /// `/effort`.
    hide_prefixes: Vec<&'static str>,
    local_user_count: usize,
}

impl Replay {
    fn new() -> Self {
        Self {
            conversation: Conversation::new(None),
            tracker: RequestTracker::new(),
            replies: Vec::new(),
            now: start_time(),
            hide_prefixes: Vec::new(),
            local_user_count: 0,
        }
    }

    fn lines(name: &str) -> Vec<Value> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// Applies lines until `stop` matches one (which is applied too).
    fn run_until(&mut self, lines: &[Value], stop: impl Fn(&Value) -> bool) -> usize {
        for (index, line) in lines.iter().enumerate() {
            self.step(line.clone());
            if stop(line) {
                return index + 1;
            }
        }
        lines.len()
    }

    fn run(name: &str) -> Replay {
        Self::run_with(name, Vec::new())
    }

    fn run_with(name: &str, hide_prefixes: Vec<&'static str>) -> Replay {
        let mut replay = Replay::new();
        replay.hide_prefixes = hide_prefixes;
        let lines = Self::lines(name);
        replay.run_until(&lines, |_| false);
        replay
    }

    fn step(&mut self, line: Value) {
        self.now += Duration::from_millis(100);
        if line["type"] == "__sent__" {
            self.apply_sent(&line["line"]);
            return;
        }
        match StdoutMessage::from_value(line) {
            StdoutMessage::ControlResponse(response) => {
                if let Some(reply) = self.tracker.resolve(response) {
                    self.conversation.apply_control_reply(&reply);
                    self.replies.push(reply);
                }
            }
            message => {
                self.conversation.apply(message, self.now);
            }
        }
    }

    fn apply_sent(&mut self, sent: &Value) {
        match sent["type"].as_str() {
            Some("user") => {
                let uuid = match sent["uuid"].as_str() {
                    Some(uuid) => uuid.to_owned(),
                    None => {
                        self.local_user_count += 1;
                        format!("sent-{}", self.local_user_count)
                    }
                };
                let text = sent["message"]["content"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|block| block["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                if self
                    .hide_prefixes
                    .iter()
                    .any(|prefix| text.starts_with(prefix))
                {
                    self.conversation.hide_replies_to(&uuid);
                } else {
                    self.conversation
                        .push_local_user_message(&uuid, &text, Vec::new(), self.now);
                }
            }
            Some("control_response") => {
                let response = &sent["response"];
                let decision = if response["response"]["behavior"] == "allow" {
                    RequestDecision::Allowed
                } else {
                    RequestDecision::Denied
                };
                self.conversation
                    .resolve_request(response["request_id"].as_str().unwrap(), decision);
            }
            Some("control_request") => {
                if let Some(body) = request_body(&sent["request"]) {
                    self.tracker
                        .track(sent["request_id"].as_str().unwrap().to_owned(), body);
                }
            }
            _ => panic!("unexpected sent line {sent}"),
        }
    }

    fn tool(&self, id: &str) -> &ToolCall {
        match self.conversation.item(id) {
            Some(TimelineItem::Tool(tool)) => tool,
            other => panic!("expected tool {id}, got {other:?}"),
        }
    }

    fn user(&self, id: &str) -> &UserMessage {
        match self.conversation.item(id) {
            Some(TimelineItem::User(user)) => user,
            other => panic!("expected user {id}, got {other:?}"),
        }
    }

    fn assistant_texts(&self) -> Vec<&str> {
        self.conversation
            .items()
            .iter()
            .filter_map(|item| match item {
                TimelineItem::Assistant(text) => Some(text.markdown.as_str()),
                _ => None,
            })
            .collect()
    }

    fn notices(&self) -> Vec<&NoticeKind> {
        self.conversation
            .items()
            .iter()
            .filter_map(|item| match item {
                TimelineItem::Notice(notice) => Some(&notice.kind),
                _ => None,
            })
            .collect()
    }

    fn turn_ends(&self) -> Vec<&TurnSummary> {
        self.conversation
            .items()
            .iter()
            .filter_map(|item| match item {
                TimelineItem::TurnEnd(summary) => Some(summary),
                _ => None,
            })
            .collect()
    }
}

fn request_body(request: &Value) -> Option<ControlRequestBody> {
    let string = |key: &str| request[key].as_str().unwrap_or_default().to_owned();
    Some(match request["subtype"].as_str()? {
        "initialize" => ControlRequestBody::Initialize {
            sdk_mcp_servers: Vec::new(),
            append_system_prompt: None,
        },
        "interrupt" => ControlRequestBody::Interrupt,
        "set_model" => ControlRequestBody::SetModel {
            model: request["model"].as_str().map(str::to_owned),
        },
        "set_permission_mode" => ControlRequestBody::SetPermissionMode {
            mode: string("mode"),
        },
        "set_max_thinking_tokens" => ControlRequestBody::SetMaxThinkingTokens {
            max_thinking_tokens: request["max_thinking_tokens"].as_u64(),
            thinking_display: None,
        },
        "mcp_status" => ControlRequestBody::McpStatus,
        "mcp_authenticate" => ControlRequestBody::McpAuthenticate {
            server_name: string("serverName"),
        },
        "mcp_toggle" => ControlRequestBody::McpToggle {
            server_name: string("serverName"),
            enabled: request["enabled"].as_bool().unwrap_or_default(),
        },
        "mcp_reconnect" => ControlRequestBody::McpReconnect {
            server_name: string("serverName"),
        },
        "get_context_usage" => ControlRequestBody::GetContextUsage {
            detail: request["detail"].as_str().map(str::to_owned),
        },
        "get_usage" => ControlRequestBody::GetUsage {
            skip_behaviors: request["skip_behaviors"].as_bool(),
        },
        "remote_control" => ControlRequestBody::RemoteControl {
            enabled: request["enabled"].as_bool().unwrap_or_default(),
            name: None,
        },
        "rewind_files" => ControlRequestBody::RewindFiles {
            user_message_id: string("user_message_id"),
            dry_run: request["dry_run"].as_bool(),
        },
        "file_suggestions" => ControlRequestBody::FileSuggestions {
            query: string("query"),
        },
        _ => return None,
    })
}

fn apply_json(
    conversation: &mut Conversation,
    value: Value,
    now: SystemTime,
) -> Vec<ConversationChange> {
    conversation.apply(StdoutMessage::from_value(value), now)
}

// --- capture-driven -------------------------------------------------------------------------

#[test]
fn turns_capture_builds_the_timeline() {
    let replay = Replay::run("live_turns.jsonl");
    let conversation = &replay.conversation;

    let user = replay.user("d3f5615a-c40c-4829-8254-f4e5a0ee9963");
    assert!(user.text.starts_with("Use the Bash tool"));
    assert_eq!(user.command, None);

    let bash = replay.tool("toolu_01EtH9BDc5MF8oZWFL1RaF7t");
    assert_eq!(bash.name, "Bash");
    assert_eq!(bash.status, ToolStatus::Done);
    assert_eq!(bash.input["command"], "echo hello > perm-test.txt");
    assert!(bash.partial_input.is_empty());
    let result = bash.result.as_ref().unwrap();
    assert_eq!(result.text, "(Bash completed with no output)");
    assert_eq!(result.structured.as_ref().unwrap()["stdout"], "");

    let write = replay.tool("toolu_01ALM95g2GKWwjZ2VHktLDUF");
    assert_eq!(write.status, ToolStatus::Denied);
    assert!(write.result.as_ref().unwrap().is_error);

    // Streamed text and the committed `assistant` frames don't duplicate.
    let texts = replay.assistant_texts();
    assert_eq!(texts.iter().filter(|text| **text == "done").count(), 1);
    assert_eq!(texts.iter().filter(|text| **text == "denied").count(), 1);
    assert_eq!(texts.iter().filter(|text| **text == "red").count(), 1);
    assert!(
        conversation
            .items()
            .iter()
            .all(|item| !matches!(item, TimelineItem::Assistant(text) if text.is_streaming))
    );

    let notices = replay.notices();
    assert!(notices.contains(&&NoticeKind::Interrupted));
    assert!(notices.contains(&&NoticeKind::Compacted {
        pre_tokens: Some(28587),
        post_tokens: Some(4652),
    }));
    assert!(
        notices
            .iter()
            .any(|notice| matches!(notice, NoticeKind::CompactSummary(text) if text.starts_with("This session is being continued")))
    );
    assert!(
        !notices
            .iter()
            .any(|notice| matches!(notice, NoticeKind::Error(_)))
    );

    let compact = replay.user("sent-1");
    assert_eq!(
        compact.command,
        Some(SlashCommandInvocation {
            name: "compact".to_owned(),
            args: String::new(),
            output: Some("Compacted".to_owned()),
        })
    );

    let turn_ends = replay.turn_ends();
    assert_eq!(turn_ends.len(), 5);
    let interrupted = turn_ends
        .iter()
        .find(|summary| summary.subtype == "error_during_execution")
        .unwrap();
    assert!(interrupted.is_error);

    assert_eq!(conversation.status(), ConversationStatus::Idle);
    assert!(conversation.pending_requests().is_empty());
    assert_eq!(
        conversation.session().permission_mode.as_deref(),
        Some("default")
    );
    assert_eq!(
        conversation.session().model.as_deref(),
        Some("claude-haiku-4-5-20251001")
    );
    assert!((conversation.total_cost_usd() - 0.07577525).abs() < 1e-9);
    let five_hour = conversation.rate_limits().five_hour.as_ref().unwrap();
    assert!((five_hour.utilization_percent - 15.0).abs() < 1e-9);
    assert!(conversation.context_usage().is_some());
    assert_eq!(conversation.context_window(), Some(200_000));
    assert!(
        conversation
            .slash_commands()
            .all(|command| command.name != "doctor")
    );
    assert!(
        conversation
            .slash_commands()
            .any(|command| command.name == "compact")
    );
}

#[test]
fn permission_prompt_waits_for_the_user() {
    let lines = Replay::lines("live_turns.jsonl");
    let mut replay = Replay::new();
    replay.run_until(&lines, |line| {
        line["type"] == "control_request" && line["request"]["tool_name"] == "Bash"
    });
    let conversation = &replay.conversation;
    assert_eq!(conversation.status(), ConversationStatus::AwaitingUser);
    let [pending] = conversation.pending_requests() else {
        panic!("expected one pending request");
    };
    assert_eq!(pending.request_id, "a4d7d3b1-85fa-4040-a766-cffa66665b7c");
    assert_eq!(
        pending.tool_use_id.as_deref(),
        Some("toolu_01EtH9BDc5MF8oZWFL1RaF7t")
    );
    let PendingRequestKind::Permission {
        tool_name,
        suggestions,
        blocked_path,
        ..
    } = &pending.kind
    else {
        panic!("expected a permission prompt");
    };
    assert_eq!(tool_name, "Bash");
    assert_eq!(suggestions.len(), 2);
    assert!(blocked_path.is_some());
    assert_eq!(
        replay.tool("toolu_01EtH9BDc5MF8oZWFL1RaF7t").status,
        ToolStatus::AwaitingApproval
    );

    let changes = replay.conversation.resolve_request(
        "a4d7d3b1-85fa-4040-a766-cffa66665b7c",
        RequestDecision::Allowed,
    );
    assert!(changes.contains(&ConversationChange::PendingRequests));
    assert!(matches!(
        replay.conversation.status(),
        ConversationStatus::Working { .. }
    ));
    assert_eq!(
        replay.tool("toolu_01EtH9BDc5MF8oZWFL1RaF7t").status,
        ToolStatus::Running
    );
}

#[test]
fn tool_input_streams_before_the_call_is_complete() {
    let lines = Replay::lines("live_turns.jsonl");
    let mut replay = Replay::new();
    replay.run_until(&lines, |line| {
        line["type"] == "stream_event"
            && line["event"]["delta"]["type"] == "input_json_delta"
            && line["event"]["delta"]["partial_json"]
                .as_str()
                .is_some_and(|json| !json.is_empty())
    });
    let tool = replay.tool("toolu_01EtH9BDc5MF8oZWFL1RaF7t");
    assert_eq!(tool.status, ToolStatus::Streaming);
    assert!(!tool.partial_input.is_empty());
    assert_eq!(tool.input, Value::Null);
}

#[test]
fn tools_capture_handles_questions_subagents_and_plans() {
    let lines = Replay::lines("live_tools.jsonl");
    let mut replay = Replay::new();
    let consumed = replay.run_until(&lines, |line| {
        line["type"] == "control_request" && line["request"]["tool_name"] == "AskUserQuestion"
    });
    let [pending] = replay.conversation.pending_requests() else {
        panic!("expected the question");
    };
    let PendingRequestKind::Question { input, questions } = &pending.kind else {
        panic!("expected a question, got {:?}", pending.kind);
    };
    assert_eq!(questions.len(), 1);
    assert_eq!(questions[0].question, "Which color do you prefer?");
    assert_eq!(questions[0].header.as_deref(), Some("Color"));
    assert!(!questions[0].multi_select);
    assert_eq!(
        questions[0]
            .options
            .iter()
            .map(|option| option.label.as_str())
            .collect::<Vec<_>>(),
        ["Red", "Blue"]
    );
    let answer = question_answers(
        input,
        &[("Which color do you prefer?".to_owned(), "Blue".to_owned())],
        pending.tool_use_id.as_deref(),
    );
    assert_eq!(
        answer["updatedInput"]["answers"]["Which color do you prefer?"],
        "Blue"
    );
    assert_eq!(answer["toolUseID"], "toolu_01YCgUdDeCxKKy1RK2f7zcKc");

    replay.run_until(&lines[consumed..], |line| {
        line["type"] == "control_request" && line["request"]["tool_name"] == "ExitPlanMode"
    });
    let [pending] = replay.conversation.pending_requests() else {
        panic!("expected the plan");
    };
    let PendingRequestKind::PlanApproval {
        plan,
        plan_file_path,
        ..
    } = &pending.kind
    else {
        panic!("expected a plan approval");
    };
    assert!(plan.starts_with("Create plan.txt"));
    assert!(plan_file_path.as_deref().unwrap().ends_with(".md"));
    assert_eq!(
        replay.conversation.session().permission_mode.as_deref(),
        Some("plan")
    );

    let rest = Replay::lines("live_tools.jsonl");
    let mut full = Replay::new();
    full.run_until(&rest, |_| false);
    let conversation = &full.conversation;

    assert_eq!(
        full.tool("toolu_01YCgUdDeCxKKy1RK2f7zcKc").status,
        ToolStatus::Done
    );
    let agent = full.tool("toolu_01FVmNBq3BqyFKECMeFV3S9b");
    assert_eq!(agent.name, "Agent");
    assert_eq!(agent.status, ToolStatus::Done);
    assert_eq!(
        agent.result.as_ref().unwrap().structured.as_ref().unwrap()["agentId"],
        "af4fc5a9e914cd61b"
    );
    // The subagent's prompt is not a user bubble.
    assert!(
        conversation
            .items()
            .iter()
            .filter(|item| matches!(item, TimelineItem::User(_)))
            .count()
            == 3
    );
    let [task] = conversation.tasks() else {
        panic!("expected one task");
    };
    assert_eq!(task.status, "completed");
    assert_eq!(task.summary.as_deref(), Some("4"));
    assert_eq!(
        task.tool_use_id.as_deref(),
        Some("toolu_01FVmNBq3BqyFKECMeFV3S9b")
    );

    assert_eq!(
        full.tool("toolu_01JZepNS9TuoymPBA7JL2r3d").status,
        ToolStatus::Done
    );
    assert_eq!(
        conversation.session().permission_mode.as_deref(),
        Some("default")
    );
    // The plan file Write and the ToolSearch ran in parallel within one message.
    assert_eq!(
        full.tool("toolu_01AZxX7QyHkD8Mt8acmGU7js").status,
        ToolStatus::Done
    );
    assert_eq!(
        full.tool("toolu_01WLLtzeWHs5TpuMe9AjJxrY").status,
        ToolStatus::Done
    );
    assert_eq!(conversation.status(), ConversationStatus::Idle);
    assert_eq!(
        full.assistant_texts()
            .iter()
            .filter(|text| **text == "Blue")
            .count(),
        1
    );
    assert!(
        full.assistant_texts()
            .contains(&"Now creating the file as approved.")
    );
}

#[test]
fn thinking_capture_streams_readable_thinking() {
    let replay = Replay::run("live_thinking.jsonl");
    let thinking: Vec<&Thinking> = replay
        .conversation
        .items()
        .iter()
        .filter_map(|item| match item {
            TimelineItem::Thinking(thinking) => Some(thinking),
            _ => None,
        })
        .collect();
    let [thinking] = thinking.as_slice() else {
        panic!("expected one thinking block");
    };
    assert!(
        thinking
            .text
            .starts_with("The user is asking me to calculate 17 * 23.")
    );
    assert!(!thinking.is_streaming);
    assert_eq!(thinking.duration_ms, Some(968));
    let [answer] = replay.assistant_texts()[..] else {
        panic!("expected one answer");
    };
    assert!(answer.ends_with("**391**"));
    // `--replay-user-messages` echoes don't duplicate the bubble.
    assert_eq!(
        replay
            .conversation
            .items()
            .iter()
            .filter(|item| matches!(item, TimelineItem::User(_)))
            .count(),
        1
    );
}

#[test]
fn commands_capture_attaches_local_output_and_hides_ui_commands() {
    let replay = Replay::run_with("live_commands.jsonl", vec!["/effort"]);
    let conversation = &replay.conversation;

    assert!(
        conversation
            .item("35627771-33c1-49f1-a30f-6dd09edf5f00")
            .is_none()
    );
    assert_eq!(conversation.session().effort.as_deref(), Some("high"));

    let model = replay.user("aa9898e0-1c81-445e-9926-3419219582c1");
    let command = model.command.as_ref().unwrap();
    assert_eq!(
        (command.name.as_str(), command.args.as_str()),
        ("model", "sonnet")
    );
    assert!(
        command
            .output
            .as_deref()
            .unwrap()
            .starts_with("Set model to")
    );

    let remote = replay.user("c62af001-a7d2-499a-8b27-c56a0511d893");
    assert!(
        remote
            .command
            .as_ref()
            .unwrap()
            .output
            .as_deref()
            .unwrap()
            .contains("isn't available")
    );
    assert!(replay.notices().is_empty(), "{:?}", replay.notices());
    // Four visible commands, four turn ends; the hidden `/effort` has none.
    assert_eq!(replay.turn_ends().len(), 4);
    assert_eq!(conversation.status(), ConversationStatus::Idle);

    assert!(!conversation.mcp_servers().is_empty());
    let errors: Vec<_> = replay
        .replies
        .iter()
        .filter_map(|reply| reply.outcome.as_ref().err())
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
    let auth = replay
        .replies
        .iter()
        .find_map(|reply| match &reply.outcome {
            Ok(ReplyPayload::McpAuthenticate(auth)) => Some(auth),
            _ => None,
        })
        .unwrap();
    assert!(auth.callback_expected);
    assert!(auth.auth_url.as_deref().unwrap().starts_with("https://"));
}

#[test]
fn control_capture_tracks_session_chrome() {
    let replay = Replay::run("live_control.jsonl");
    let conversation = &replay.conversation;

    assert_eq!(
        conversation.account().unwrap().email.as_deref(),
        Some("user@example.com")
    );
    assert!(
        conversation
            .models()
            .iter()
            .any(|model| model.value == "haiku")
    );
    assert!(conversation.remote_control().available);
    assert!(!conversation.remote_control().enabled);
    let enabled = replay
        .replies
        .iter()
        .find_map(|reply| match &reply.outcome {
            Ok(ReplyPayload::RemoteControl(response)) if response.session_url.is_some() => {
                Some(response)
            }
            _ => None,
        })
        .unwrap();
    assert!(
        enabled
            .session_url
            .as_deref()
            .unwrap()
            .starts_with("https://claude.ai/code/")
    );

    let five_hour = conversation.rate_limits().five_hour.as_ref().unwrap();
    assert!((five_hour.utilization_percent - 14.0).abs() < 1e-9);
    assert!(five_hour.resets_at.is_some());

    let context = replay.user("sent-1");
    assert_eq!(context.command.as_ref().unwrap().name, "context");
    assert!(
        context
            .command
            .as_ref()
            .unwrap()
            .output
            .as_deref()
            .unwrap()
            .starts_with("## Context Usage")
    );
    let mcp = replay.user("sent-3");
    assert!(
        mcp.command
            .as_ref()
            .unwrap()
            .output
            .as_deref()
            .unwrap()
            .contains("MCP server")
    );
    assert!(conversation.context_usage().unwrap().total_tokens.unwrap() > 0);
    assert_eq!(conversation.status(), ConversationStatus::Idle);
}

// --- synthetic ------------------------------------------------------------------------------

fn stream(event: Value) -> Value {
    json!({"type": "stream_event", "event": event, "parent_tool_use_id": null, "session_id": "s"})
}

#[test]
fn interrupt_keeps_partial_text_and_interrupts_tools() {
    let now = start_time();
    let mut conversation = Conversation::new(Some("s".to_owned()));
    conversation.push_local_user_message("u1", "Count to 300", Vec::new(), now);
    assert_eq!(
        conversation.status(),
        ConversationStatus::Working { since: now }
    );

    let frames = [
        json!({"type": "command_lifecycle", "command_uuid": "u1", "state": "started"}),
        stream(
            json!({"type": "message_start", "message": {"id": "msg_1", "model": "m", "usage": {"input_tokens": 10, "cache_read_input_tokens": 90}}}),
        ),
        stream(
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        ),
        stream(
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "1\n2\n"}}),
        ),
        stream(
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "3"}}),
        ),
        stream(
            json!({"type": "content_block_start", "index": 1, "content_block": {"type": "tool_use", "id": "toolu_x", "name": "Bash", "input": {}}}),
        ),
        stream(
            json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"command\": \"ls"}}),
        ),
        json!({"type": "user", "message": {"role": "user", "content": [{"type": "text", "text": "[Request interrupted by user]"}]}, "parent_tool_use_id": null}),
        stream(json!({"type": "message_delta", "delta": {"stop_reason": null}, "usage": {}})),
        stream(json!({"type": "message_stop"})),
        json!({"type": "result", "subtype": "error_during_execution", "is_error": true, "terminal_reason": "aborted_streaming",
               "errors": ["[ede_diagnostic] result_type=user"], "total_cost_usd": 0.01, "user_message_uuid": "u1", "uuid": "r1"}),
    ];
    for (offset, frame) in frames.into_iter().enumerate() {
        apply_json(
            &mut conversation,
            frame,
            now + Duration::from_millis(offset as u64 * 10),
        );
    }

    let TimelineItem::Assistant(text) = conversation.item("msg_1:0").unwrap() else {
        panic!("expected assistant text");
    };
    assert_eq!(text.markdown, "1\n2\n3");
    assert!(!text.is_streaming);
    let TimelineItem::Tool(tool) = conversation.item("toolu_x").unwrap() else {
        panic!("expected the tool");
    };
    assert_eq!(tool.status, ToolStatus::Interrupted);
    assert_eq!(conversation.live_context_tokens(), Some(100));

    let kinds: Vec<_> = conversation
        .items()
        .iter()
        .filter_map(|item| match item {
            TimelineItem::Notice(notice) => Some(notice.kind.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(kinds, [NoticeKind::Interrupted]);
    let Some(TimelineItem::TurnEnd(summary)) = conversation.items().last() else {
        panic!("expected a turn end");
    };
    assert_eq!(summary.id, "r1");
    assert!(summary.is_error);
    assert_eq!(summary.cost_usd, Some(0.01));
    assert_eq!(conversation.status(), ConversationStatus::Idle);
}

#[test]
fn status_follows_the_turn() {
    let now = start_time();
    let mut conversation = Conversation::new(None);
    assert_eq!(conversation.status(), ConversationStatus::Starting);
    let changes = conversation.apply_initialize_response(&InitializeResponse::default());
    assert!(changes.contains(&ConversationChange::Status));
    assert_eq!(conversation.status(), ConversationStatus::Idle);

    let changes = conversation.push_local_user_message("u1", "hi", Vec::new(), now);
    assert_eq!(
        changes,
        [
            ConversationChange::ItemInserted {
                index: 0,
                id: "u1".to_owned()
            },
            ConversationChange::Status,
        ]
    );

    let changes = apply_json(
        &mut conversation,
        json!({"type": "control_request", "request_id": "p1", "request": {"subtype": "can_use_tool", "tool_name": "Write", "input": {"file_path": "a"}, "tool_use_id": "t1"}}),
        now,
    );
    assert!(changes.contains(&ConversationChange::PendingRequests));
    assert_eq!(conversation.status(), ConversationStatus::AwaitingUser);

    apply_json(
        &mut conversation,
        json!({"type": "control_cancel_request", "request_id": "p1"}),
        now,
    );
    assert!(conversation.pending_requests().is_empty());
    assert!(matches!(
        conversation.status(),
        ConversationStatus::Working { .. }
    ));

    apply_json(
        &mut conversation,
        json!({"type": "result", "subtype": "success", "is_error": false, "user_message_uuid": "u1"}),
        now,
    );
    assert_eq!(conversation.status(), ConversationStatus::Idle);

    let changes = conversation.mark_exited(Some("crashed".to_owned()), now);
    assert!(changes.contains(&ConversationChange::Status));
    assert_eq!(conversation.status(), ConversationStatus::Exited);
    assert_eq!(conversation.exit_error(), Some("crashed"));
}

#[test]
fn compacting_counts_as_working() {
    let now = start_time();
    let mut conversation = Conversation::new(None);
    conversation.apply_initialize_response(&InitializeResponse::default());
    apply_json(
        &mut conversation,
        json!({"type": "system", "subtype": "status", "status": "compacting"}),
        now,
    );
    assert!(conversation.is_compacting());
    assert!(matches!(
        conversation.status(),
        ConversationStatus::Working { .. }
    ));
    apply_json(
        &mut conversation,
        json!({"type": "system", "subtype": "status", "status": null, "compact_result": "failed", "compact_error": "Too short"}),
        now,
    );
    assert!(!conversation.is_compacting());
    assert_eq!(conversation.status(), ConversationStatus::Idle);
    assert!(matches!(
        &conversation.items()[0],
        TimelineItem::Notice(Notice { kind: NoticeKind::Error(error), .. }) if error == "Too short"
    ));
}

#[test]
fn todos_follow_todo_write_and_task_tools() {
    let now = start_time();
    let mut conversation = Conversation::new(None);
    let assistant = |id: &str, name: &str, input: Value| {
        json!({"type": "assistant", "parent_tool_use_id": null, "message": {"id": format!("msg_{id}"), "model": "m",
            "content": [{"type": "tool_use", "id": id, "name": name, "input": input}]}})
    };
    let tool_result = |id: &str, structured: Value| {
        json!({"type": "user", "parent_tool_use_id": null, "tool_use_result": structured,
            "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": "ok"}]}})
    };

    let changes = apply_json(
        &mut conversation,
        assistant(
            "t1",
            "TodoWrite",
            json!({"todos": [
                {"content": "Read code", "status": "completed", "activeForm": "Reading code"},
                {"content": "Write tests", "status": "in_progress", "activeForm": "Writing tests"},
            ]}),
        ),
        now,
    );
    assert!(changes.contains(&ConversationChange::Todos));
    assert_eq!(
        conversation
            .todos()
            .iter()
            .map(|todo| (todo.content.as_str(), todo.status))
            .collect::<Vec<_>>(),
        [
            ("Read code", TodoStatus::Completed),
            ("Write tests", TodoStatus::InProgress)
        ]
    );

    apply_json(
        &mut conversation,
        assistant(
            "t2",
            "TaskCreate",
            json!({"subject": "Ship it", "description": "d", "activeForm": "Shipping"}),
        ),
        now,
    );
    apply_json(
        &mut conversation,
        tool_result("t2", json!({"task": {"id": "7", "subject": "Ship it"}})),
        now,
    );
    apply_json(
        &mut conversation,
        assistant(
            "t3",
            "TaskUpdate",
            json!({"taskId": "7", "status": "in_progress"}),
        ),
        now,
    );
    let shipped = conversation.todos().last().unwrap();
    assert_eq!(shipped.id.as_deref(), Some("7"));
    assert_eq!(shipped.status, TodoStatus::InProgress);
    assert_eq!(shipped.active_form.as_deref(), Some("Shipping"));

    apply_json(
        &mut conversation,
        assistant(
            "t4",
            "TaskUpdate",
            json!({"taskId": "7", "status": "deleted"}),
        ),
        now,
    );
    assert_eq!(conversation.todos().len(), 2);
}

#[test]
fn unstreamed_assistant_blocks_get_their_own_items() {
    let now = start_time();
    let mut conversation = Conversation::new(None);
    apply_json(
        &mut conversation,
        json!({"type": "assistant", "parent_tool_use_id": "toolu_agent", "message": {"id": "msg_9", "model": "m",
            "content": [{"type": "text", "text": "a"}, {"type": "text", "text": "b"}]}}),
        now,
    );
    let ids: Vec<_> = conversation.items().iter().map(TimelineItem::id).collect();
    assert_eq!(ids, ["msg_9:u0", "msg_9:u1"]);
    assert_eq!(
        conversation.items()[0].parent_tool_use_id(),
        Some("toolu_agent")
    );
}

#[test]
fn api_errors_become_error_notices() {
    let now = start_time();
    let mut conversation = Conversation::new(None);
    apply_json(
        &mut conversation,
        json!({"type": "assistant", "error": "rate_limit", "message": {"id": "e1", "model": "<not synthetic>",
            "content": [{"type": "text", "text": "API Error: rate limited"}]}}),
        now,
    );
    assert!(matches!(
        &conversation.items()[0],
        TimelineItem::Notice(Notice { kind: NoticeKind::Error(text), .. }) if text == "API Error: rate limited"
    ));
}

#[test]
fn request_tracker_matches_replies() {
    let mut tracker = RequestTracker::new();
    let (id, line) = tracker.issue(ControlRequestBody::SetPermissionMode {
        mode: "plan".to_owned(),
    });
    let sent: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(sent["request_id"], id.as_str());
    assert_eq!(sent["request"]["subtype"], "set_permission_mode");
    assert!(tracker.is_in_flight(&id));

    let reply = tracker
        .resolve(ControlResponse {
            request_id: id.clone(),
            outcome: Ok(json!({"mode": "plan"})),
        })
        .unwrap();
    assert_eq!(
        reply.outcome,
        Ok(ReplyPayload::PermissionMode("plan".to_owned()))
    );
    assert!(!tracker.is_in_flight(&id));
    assert!(
        tracker
            .resolve(ControlResponse {
                request_id: id,
                outcome: Ok(Value::Null),
            })
            .is_none()
    );

    let mut conversation = Conversation::new(None);
    let changes = conversation.apply_control_reply(&reply);
    assert_eq!(changes, [ConversationChange::Session]);
    assert_eq!(
        conversation.session().permission_mode.as_deref(),
        Some("plan")
    );

    let (id, _) = tracker.issue(ControlRequestBody::McpStatus);
    let reply = tracker
        .resolve(ControlResponse {
            request_id: id,
            outcome: Err("boom".to_owned()),
        })
        .unwrap();
    assert_eq!(reply.outcome, Err("boom".to_owned()));
}

#[test]
fn hidden_messages_are_removed_from_the_timeline() {
    let now = start_time();
    let mut conversation = Conversation::new(None);
    conversation.push_local_user_message("a", "first", Vec::new(), now);
    conversation.push_local_user_message("b", "/effort high", Vec::new(), now);
    conversation.push_local_user_message("c", "third", Vec::new(), now);
    let changes = conversation.hide_replies_to("b");
    assert_eq!(
        changes,
        [ConversationChange::ItemRemoved {
            index: 1,
            id: "b".to_owned()
        }]
    );
    assert_eq!(conversation.index_of("c"), Some(1));
}

#[test]
fn parses_rfc3339_timestamps() {
    assert_eq!(
        parse_rfc3339("2026-10-26T22:20:00.097901+00:00"),
        Some(UNIX_EPOCH + Duration::from_millis(1_793_053_200_097))
    );
    assert_eq!(parse_rfc3339("nope"), None);
}
