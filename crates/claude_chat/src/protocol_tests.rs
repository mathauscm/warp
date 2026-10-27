use std::path::{Path, PathBuf};

use serde_json::json;

use super::*;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn live_fixture_paths() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(fixtures_dir())
        .expect("fixtures dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("live_") && name.ends_with(".jsonl"))
        })
        .collect();
    paths.sort();
    paths
}

const KNOWN_TYPES: &[&str] = &[
    "system",
    "stream_event",
    "assistant",
    "user",
    "result",
    "rate_limit_event",
    "command_lifecycle",
    "control_request",
    "control_response",
    "control_cancel_request",
];

/// Parses every line of a capture, failing on lines of a known type that fell back to `Unknown`.
fn assert_parses_cleanly(path: &Path) -> usize {
    let contents = std::fs::read_to_string(path).unwrap();
    let mut count = 0;
    for (number, line) in contents.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let message = StdoutMessage::parse_line(line)
            .unwrap_or_else(|error| panic!("{}:{}: {error}", path.display(), number + 1));
        if let StdoutMessage::Unknown(value) = &message {
            let kind = value
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            assert!(
                !KNOWN_TYPES.contains(&kind),
                "{}:{}: known type {kind} parsed as Unknown",
                path.display(),
                number + 1
            );
        }
        if let StdoutMessage::System(SystemMessage::Other { subtype, .. }) = &message {
            panic!(
                "{}:{}: unmodeled system subtype {subtype}",
                path.display(),
                number + 1
            );
        }
        if let StdoutMessage::StreamEvent(event) = &message {
            assert!(
                !matches!(event.event, StreamEvent::Unknown(_)),
                "{}:{}: stream event parsed as Unknown",
                path.display(),
                number + 1
            );
        }
        count += 1;
    }
    count
}

#[test]
fn parses_every_live_fixture_line() {
    let paths = live_fixture_paths();
    assert!(paths.len() >= 5, "expected the live_* fixtures");
    for path in paths {
        assert!(assert_parses_cleanly(&path) > 0);
    }
}

/// Runs over the unredacted captures when `CLAUDE_CHAT_CAPTURES_DIR` points at them.
#[test]
#[ignore]
fn parses_every_raw_capture_line() {
    let dir = std::env::var("CLAUDE_CHAT_CAPTURES_DIR").expect("CLAUDE_CHAT_CAPTURES_DIR");
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.to_string_lossy().ends_with(".stdout.jsonl") {
            assert_parses_cleanly(&path);
        }
    }
}

#[test]
fn unknown_types_and_subtypes_keep_the_raw_value() {
    let message = StdoutMessage::parse_line(r#"{"type":"brand_new","x":1}"#).unwrap();
    assert_eq!(
        message,
        StdoutMessage::Unknown(json!({"type":"brand_new","x":1}))
    );

    let message =
        StdoutMessage::parse_line(r#"{"type":"system","subtype":"brand_new","y":2}"#).unwrap();
    let StdoutMessage::System(SystemMessage::Other { subtype, raw }) = message else {
        panic!("expected Other");
    };
    assert_eq!(subtype, "brand_new");
    assert_eq!(raw["y"], 2);

    // A known type with a broken shape doesn't fail either.
    let message =
        StdoutMessage::parse_line(r#"{"type":"control_request","request":"nope"}"#).unwrap();
    assert!(matches!(message, StdoutMessage::Unknown(_)));

    assert!(StdoutMessage::parse_line("not json").is_err());
}

#[test]
fn tolerates_nulls_and_odd_field_types() {
    let line = json!({
        "type": "result",
        "subtype": "success",
        "is_error": null,
        "duration_ms": 12.7,
        "total_cost_usd": 0.5,
        "usage": "weird",
        "modelUsage": null,
        "permission_denials": null,
        "user_message_uuids": null,
    })
    .to_string();
    let StdoutMessage::Result(result) = StdoutMessage::parse_line(&line).unwrap() else {
        panic!("expected a result");
    };
    assert!(!result.is_error);
    assert_eq!(result.duration_ms, Some(13));
    assert_eq!(result.total_cost_usd, Some(0.5));
    assert_eq!(result.usage, None);
}

#[test]
fn parses_content_blocks() {
    let line = json!({
        "type": "user",
        "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "t1", "is_error": true,
             "content": [{"type": "text", "text": "a"}, {"type": "tool_reference", "tool_name": "X"}]},
            {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "QUJD"}},
            {"type": "server_tool_use", "id": "s"},
        ]},
        "isReplay": true,
        "tool_result_meta": [{"id": "t1", "non_execution_kind": "permission-rule"}],
    })
    .to_string();
    let StdoutMessage::User(user) = StdoutMessage::parse_line(&line).unwrap() else {
        panic!("expected a user frame");
    };
    assert!(user.is_replay);
    let blocks = user.message.content.blocks();
    let ContentBlock::ToolResult(result) = &blocks[0] else {
        panic!("expected a tool result");
    };
    assert!(result.is_error);
    assert_eq!(result.text(), "a\nX");
    assert_eq!(
        blocks[1],
        ContentBlock::Image(ImageBlock {
            media_type: Some("image/png".to_owned()),
            data: Some("QUJD".to_owned()),
        })
    );
    assert!(matches!(blocks[2], ContentBlock::Other(_)));
    assert_eq!(
        user.tool_result_meta[0].non_execution_kind.as_deref(),
        Some("permission-rule")
    );
}

#[test]
fn parses_can_use_tool_requests() {
    let line = r#"{"type":"control_request","request_id":"3ba3","request":{"subtype":"can_use_tool","tool_name":"AskUserQuestion","display_name":"AskUserQuestion","input":{"questions":[]},"tool_use_id":"toolu_1","requires_user_interaction":true}}"#;
    let StdoutMessage::ControlRequest(request) = StdoutMessage::parse_line(line).unwrap() else {
        panic!("expected a control request");
    };
    assert_eq!(request.request_id, "3ba3");
    let IncomingRequest::CanUseTool(can_use_tool) = request.request else {
        panic!("expected can_use_tool");
    };
    assert_eq!(can_use_tool.tool_name, "AskUserQuestion");
    assert!(can_use_tool.requires_user_interaction);
    assert_eq!(can_use_tool.tool_use_id.as_deref(), Some("toolu_1"));

    let line = r#"{"type":"control_request","request_id":"m1","request":{"subtype":"mcp_message","server_name":"artifacts","message":{"jsonrpc":"2.0","id":1,"method":"tools/list"}}}"#;
    let StdoutMessage::ControlRequest(request) = StdoutMessage::parse_line(line).unwrap() else {
        panic!("expected a control request");
    };
    assert_eq!(
        request.request,
        IncomingRequest::McpMessage {
            server_name: "artifacts".to_owned(),
            message: json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        }
    );

    let line = r#"{"type":"control_request","request_id":"o1","request":{"subtype":"oauth_token_refresh"}}"#;
    let StdoutMessage::ControlRequest(request) = StdoutMessage::parse_line(line).unwrap() else {
        panic!("expected a control request");
    };
    assert!(
        matches!(request.request, IncomingRequest::Other { subtype, .. } if subtype == "oauth_token_refresh")
    );

    assert_eq!(
        StdoutMessage::parse_line(r#"{"type":"control_cancel_request","request_id":"x"}"#).unwrap(),
        StdoutMessage::ControlCancelRequest {
            request_id: "x".to_owned()
        }
    );
}

#[test]
fn parses_control_responses() {
    let line = r#"{"type":"control_response","response":{"subtype":"error","request_id":"req_d2","error":"Unsupported control request subtype: bogus_subtype"}}"#;
    let StdoutMessage::ControlResponse(response) = StdoutMessage::parse_line(line).unwrap() else {
        panic!("expected a control response");
    };
    assert_eq!(response.request_id, "req_d2");
    assert_eq!(
        response.outcome,
        Err("Unsupported control request subtype: bogus_subtype".to_owned())
    );

    let line = r#"{"type":"control_response","response":{"subtype":"success","request_id":"r"}}"#;
    let StdoutMessage::ControlResponse(response) = StdoutMessage::parse_line(line).unwrap() else {
        panic!("expected a control response");
    };
    assert_eq!(response.outcome, Ok(Value::Null));
}

#[test]
fn decodes_typed_responses_from_captures() {
    let contents = std::fs::read_to_string(fixtures_dir().join("live_control.jsonl")).unwrap();
    let mut saw = (false, false, false, false);
    for line in contents.lines() {
        let StdoutMessage::ControlResponse(response) = StdoutMessage::parse_line(line).unwrap()
        else {
            continue;
        };
        let Ok(value) = response.outcome else {
            continue;
        };
        if value.get("commands").is_some() {
            let initialize = InitializeResponse::deserialize(&value).unwrap();
            assert!(!initialize.commands.is_empty());
            assert!(initialize.remote_control_available);
            let model = initialize
                .models
                .iter()
                .find(|model| model.value == "default")
                .unwrap();
            assert_eq!(
                model.supported_effort_levels,
                ["low", "medium", "high", "xhigh", "max"]
            );
            assert_eq!(
                initialize.account.unwrap().email.as_deref(),
                Some("user@example.com")
            );
            saw.0 = true;
        } else if value.get("mcpServers").is_some() {
            let status = McpStatusResponse::deserialize(&value).unwrap();
            assert!(
                status
                    .mcp_servers
                    .iter()
                    .all(|server| !server.name.is_empty())
            );
            saw.1 = true;
        } else if value.get("categories").is_some() {
            let usage = ContextUsage::deserialize(&value).unwrap();
            assert!(usage.total_tokens.unwrap() > 0);
            assert_eq!(usage.max_tokens, Some(200_000));
            assert!(
                usage
                    .categories
                    .iter()
                    .any(|category| category.kind.as_deref() == Some("free"))
            );
            saw.2 = true;
        } else if value.get("rate_limits").is_some() {
            let usage = UsageResponse::deserialize(&value).unwrap();
            let five_hour = usage.rate_limits.unwrap().five_hour.unwrap();
            assert_eq!(five_hour.utilization, Some(14.0));
            assert!(five_hour.resets_at.is_some());
            saw.3 = true;
        }
    }
    assert_eq!(saw, (true, true, true, true));
}

#[test]
fn local_command_stdout_strips_tags() {
    assert_eq!(
        local_command_stdout("<local-command-stdout>Compacted </local-command-stdout>"),
        Some("Compacted")
    );
    assert_eq!(local_command_stdout("plain"), None);
}

#[test]
fn builds_user_messages() {
    let line = user_message_line(
        "d3f5",
        "What color?",
        &[OutgoingImage {
            media_type: "image/png".to_owned(),
            data_base64: "QUJD".to_owned(),
        }],
    );
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap(),
        json!({
            "type": "user",
            "session_id": "",
            "parent_tool_use_id": null,
            "uuid": "d3f5",
            "message": {"role": "user", "content": [
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "QUJD"}},
                {"type": "text", "text": "What color?"},
            ]},
        })
    );
    assert!(!line.contains('\n'));
}

#[test]
fn builds_control_requests() {
    let cases = [
        (
            ControlRequestBody::Initialize {
                sdk_mcp_servers: Vec::new(),
                append_system_prompt: None,
            },
            json!({"subtype": "initialize"}),
        ),
        (
            ControlRequestBody::Initialize {
                sdk_mcp_servers: vec!["artifacts".to_owned()],
                append_system_prompt: Some("Be brief".to_owned()),
            },
            json!({"subtype": "initialize", "sdkMcpServers": ["artifacts"], "appendSystemPrompt": "Be brief"}),
        ),
        (
            ControlRequestBody::Interrupt,
            json!({"subtype": "interrupt"}),
        ),
        (
            ControlRequestBody::SetModel {
                model: Some("sonnet".to_owned()),
            },
            json!({"subtype": "set_model", "model": "sonnet"}),
        ),
        (
            ControlRequestBody::SetModel { model: None },
            json!({"subtype": "set_model", "model": null}),
        ),
        (
            ControlRequestBody::SetPermissionMode {
                mode: PermissionMode::AcceptEdits.as_str().to_owned(),
            },
            json!({"subtype": "set_permission_mode", "mode": "acceptEdits"}),
        ),
        (
            ControlRequestBody::SetMaxThinkingTokens {
                max_thinking_tokens: Some(2048),
                thinking_display: None,
            },
            json!({"subtype": "set_max_thinking_tokens", "max_thinking_tokens": 2048}),
        ),
        (
            ControlRequestBody::McpStatus,
            json!({"subtype": "mcp_status"}),
        ),
        (
            ControlRequestBody::McpAuthenticate {
                server_name: "plugin:postman:postman".to_owned(),
            },
            json!({"subtype": "mcp_authenticate", "serverName": "plugin:postman:postman"}),
        ),
        (
            ControlRequestBody::McpToggle {
                server_name: "srv".to_owned(),
                enabled: false,
            },
            json!({"subtype": "mcp_toggle", "serverName": "srv", "enabled": false}),
        ),
        (
            ControlRequestBody::McpReconnect {
                server_name: "srv".to_owned(),
            },
            json!({"subtype": "mcp_reconnect", "serverName": "srv"}),
        ),
        (
            ControlRequestBody::GetContextUsage {
                detail: Some("summary".to_owned()),
            },
            json!({"subtype": "get_context_usage", "detail": "summary"}),
        ),
        (
            ControlRequestBody::GetUsage {
                skip_behaviors: Some(true),
            },
            json!({"subtype": "get_usage", "skip_behaviors": true}),
        ),
        (
            ControlRequestBody::RemoteControl {
                enabled: true,
                name: None,
            },
            json!({"subtype": "remote_control", "enabled": true}),
        ),
        (
            ControlRequestBody::RewindFiles {
                user_message_id: "d3f5".to_owned(),
                dry_run: Some(true),
            },
            json!({"subtype": "rewind_files", "user_message_id": "d3f5", "dry_run": true}),
        ),
        (
            ControlRequestBody::FileSuggestions {
                query: "perm".to_owned(),
            },
            json!({"subtype": "file_suggestions", "query": "perm"}),
        ),
    ];
    for (body, expected) in cases {
        let line = control_request_line("r1", &body);
        assert_eq!(
            serde_json::from_str::<Value>(&line).unwrap(),
            json!({"type": "control_request", "request_id": "r1", "request": expected}),
        );
        assert_eq!(Some(body.subtype()), expected["subtype"].as_str());
    }
}

#[test]
fn builds_permission_replies() {
    let input = json!({"command": "echo hello > perm-test.txt", "description": "Create"});
    let allow = control_success_line(
        "a4d7",
        permission_allow(input.clone(), None, Some("toolu_01")),
    );
    assert_eq!(
        serde_json::from_str::<Value>(&allow).unwrap(),
        json!({"type": "control_response", "response": {"subtype": "success", "request_id": "a4d7",
            "response": {"behavior": "allow", "updatedInput": input, "toolUseID": "toolu_01"}}})
    );

    let suggestion = json!({"type": "setMode", "mode": "acceptEdits", "destination": "session"});
    assert_eq!(
        permission_allow(json!({}), Some(vec![suggestion.clone()]), None),
        json!({"behavior": "allow", "updatedInput": {}, "updatedPermissions": [suggestion]})
    );

    assert_eq!(
        permission_deny("User declined this write", false),
        json!({"behavior": "deny", "message": "User declined this write", "interrupt": false})
    );

    assert_eq!(
        plan_approve(json!({"plan": "p"}), true, None),
        json!({"behavior": "allow", "updatedInput": {"plan": "p"}, "updatedPermissions": [
            {"type": "setMode", "mode": "acceptEdits", "destination": "session"}]})
    );
    assert_eq!(
        plan_reject("Add tests first"),
        json!({"behavior": "deny", "message": "Add tests first", "interrupt": false})
    );

    assert_eq!(
        serde_json::from_str::<Value>(&control_success_line(
            "m1",
            mcp_message_response(json!({"id": 1}))
        ))
        .unwrap(),
        json!({"type": "control_response", "response": {"subtype": "success", "request_id": "m1",
            "response": {"mcp_response": {"id": 1}}}})
    );
    assert_eq!(
        serde_json::from_str::<Value>(&control_success_line("n", Value::Null)).unwrap(),
        json!({"type": "control_response", "response": {"subtype": "success", "request_id": "n"}})
    );
    assert_eq!(
        serde_json::from_str::<Value>(&control_error_line("e", "Unsupported")).unwrap(),
        json!({"type": "control_response", "response": {"subtype": "error", "request_id": "e", "error": "Unsupported"}})
    );
}

#[test]
fn question_answers_match_the_captured_reply() {
    let contents = std::fs::read_to_string(fixtures_dir().join("live_tools.jsonl")).unwrap();
    let mut request_input = None;
    let mut sent_input = None;
    for line in contents.lines() {
        let value: Value = serde_json::from_str(line).unwrap();
        if value["type"] == "control_request" && value["request"]["tool_name"] == "AskUserQuestion"
        {
            request_input = Some(value["request"]["input"].clone());
        }
        if value["type"] == "__sent__"
            && value["line"]["response"]["response"]["updatedInput"]
                .get("answers")
                .is_some()
        {
            sent_input = Some(value["line"]["response"]["response"].clone());
        }
    }
    let answers = [("Which color do you prefer?".to_owned(), "Blue".to_owned())];
    assert_eq!(
        question_answers(&request_input.unwrap(), &answers, None),
        sent_input.unwrap()
    );
}

#[test]
fn permission_modes_round_trip() {
    for mode in [
        "default",
        "acceptEdits",
        "plan",
        "auto",
        "dontAsk",
        "bypassPermissions",
    ] {
        assert_eq!(PermissionMode::parse(mode).unwrap().as_str(), mode);
    }
    assert_eq!(PermissionMode::parse("bogus"), None);
}

#[test]
fn file_suggestion_paths_accept_strings_and_objects() {
    let response = FileSuggestionsResponse {
        suggestions: vec![
            json!("src/main.rs"),
            json!({"path": "Cargo.toml"}),
            json!(3),
        ],
        cwd: None,
    };
    assert_eq!(response.paths(), ["src/main.rs", "Cargo.toml"]);
}
