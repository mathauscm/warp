use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use super::*;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

const CAPTURE: &str = "history_capture/08511edb-8e0b-4d07-9989-51ab170193ce.jsonl";
const CAPTURE_AGENT_TOOL: &str = "toolu_01FVmNBq3BqyFKECMeFV3S9b";

fn load(name: &str) -> LoadedTranscript {
    load_transcript(&fixture(name)).expect("fixture loads")
}

/// One short line per item, so tests can compare whole timelines.
fn describe(items: &[TimelineItem]) -> Vec<String> {
    items
        .iter()
        .map(|item| {
            let line = match item {
                TimelineItem::User(message) => match &message.command {
                    Some(command) => format!(
                        "command {} [{}] -> {:?}",
                        command.name, command.args, command.output
                    ),
                    None => format!("user {}", message.text),
                },
                TimelineItem::Assistant(text) => format!("assistant {}", text.markdown),
                TimelineItem::Thinking(thinking) => format!("thinking {}", thinking.text),
                TimelineItem::Tool(tool) => format!("tool {} {:?}", tool.name, tool.status),
                TimelineItem::Notice(notice) => match &notice.kind {
                    NoticeKind::Compacted {
                        pre_tokens,
                        post_tokens,
                    } => format!("compacted {pre_tokens:?} {post_tokens:?}"),
                    NoticeKind::CompactSummary(_) => "compact summary".to_owned(),
                    NoticeKind::Interrupted => "interrupted".to_owned(),
                    NoticeKind::Error(error) => format!("error {error}"),
                    NoticeKind::Info(info) => format!("info {info}"),
                },
                TimelineItem::TurnEnd(summary) => {
                    format!("turn-end {} {:?}", summary.subtype, summary.duration_ms)
                }
            };
            match item.parent_tool_use_id() {
                Some(parent) => format!("  [{parent}] {line}"),
                None => line,
            }
        })
        .collect()
}

#[test]
fn encodes_project_dirs_like_the_cli() {
    assert_eq!(
        encoded_project_dir(Path::new("/Users/me/projects/personal/warp")),
        "-Users-me-projects-personal-warp"
    );
    assert_eq!(
        encoded_project_dir(Path::new("/tmp/a b/ção.x")),
        "-tmp-a-b---o-x"
    );
    // Characters outside the BMP are two UTF-16 code units, so two dashes.
    assert_eq!(encoded_project_dir(Path::new("/x/😀")), "-x---");
}

#[test]
fn shortens_long_project_dirs_with_a_java_hash() {
    let long = format!("/{}", "a".repeat(250));
    let encoded = encoded_project_dir(Path::new(&long));
    let expected_hash = {
        let hash = long.encode_utf16().fold(0i32, |hash, unit| {
            hash.wrapping_mul(31).wrapping_add(i32::from(unit))
        });
        to_base36(i64::from(hash).unsigned_abs())
    };
    assert_eq!(
        encoded,
        format!("-{}-{expected_hash}", "a".repeat(MAX_ENCODED_DIR_LEN - 1))
    );
    // Known value of Java's "hello".hashCode() = 99162322.
    let hello = "hello".encode_utf16().fold(0i32, |hash, unit| {
        hash.wrapping_mul(31).wrapping_add(i32::from(unit))
    });
    assert_eq!(hello, 99_162_322);
    assert_eq!(to_base36(99_162_322), "1n1e4y");
    assert_eq!(to_base36(0), "0");
}

#[test]
fn builds_and_finds_transcript_paths() {
    let home = tempfile::tempdir().unwrap();
    let path = transcript_path(home.path(), Path::new("/nonexistent/project"), "abc");
    assert_eq!(
        path,
        home.path().join("projects/-nonexistent-project/abc.jsonl")
    );
    assert_eq!(find_transcript(home.path(), "abc"), None);

    let other_dir = home.path().join("projects/-some-other-dir");
    std::fs::create_dir_all(&other_dir).unwrap();
    std::fs::write(other_dir.join("abc.jsonl"), "").unwrap();
    assert_eq!(
        find_transcript(home.path(), "abc"),
        Some(other_dir.join("abc.jsonl"))
    );
}

#[test]
fn parses_timestamps() {
    let parsed = parse_timestamp("2026-10-26T21:15:41.604Z").unwrap();
    assert_eq!(
        parsed.duration_since(UNIX_EPOCH).unwrap(),
        Duration::from_millis(1_793_049_341_604)
    );
    assert_eq!(
        parse_timestamp("1970-01-01T01:00:00+01:00"),
        Some(UNIX_EPOCH)
    );
    assert_eq!(
        parse_timestamp("2000-03-01T00:00:00Z")
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
        951_868_800
    );
    assert_eq!(parse_timestamp("yesterday"), None);
    assert_eq!(parse_timestamp("2026-13-01T00:00:00Z"), None);
}

#[test]
fn loads_the_captured_session() {
    let transcript = load(CAPTURE);
    let agent = format!("[{CAPTURE_AGENT_TOOL}]");
    assert_eq!(
        describe(&transcript.items),
        vec![
            "user Use the Bash tool to run exactly: echo hello > perm-test.txt   Then reply with just: done",
            "tool Bash Done",
            "assistant done",
            "turn-end success Some(4538)",
            "user Use the Write tool to create denied.txt containing x. If it is denied, reply with just: denied",
            "tool Write Denied",
            "assistant denied",
            "turn-end success Some(3358)",
            "command model [sonnet] -> Some(\"Set model to `sonnet (claude-sonnet-5-5)`\")",
            "command model [haiku] -> Some(\"Set model to `haiku (claude-haiku-4-5-20251001)`\")",
            "user Write the numbers from 1 to 300, one per line, no other text.",
            "interrupted",
            "turn-end error_during_execution Some(991)",
            "user What color is this image? Answer with one word.",
            "tool Read Done",
            "assistant red",
            "turn-end success Some(2581)",
            "compacted Some(28587) Some(4652)",
            "compact summary",
            "command compact [] -> Some(\"Compacted\")",
            "user Use the AskUserQuestion tool to ask me which color I prefer, with options Red and Blue. Then reply with only my answer.",
            "tool AskUserQuestion Done",
            "assistant Blue",
            "turn-end success Some(3195)",
            "user Use the Agent tool with subagent_type general-purpose to compute 2+2, then reply with only the number.",
            "tool Agent Done",
            &format!("  {agent} assistant 4"),
            "assistant 4",
            "turn-end success Some(4092)",
            "user Make a plan to create a file plan.txt containing hi. Keep it to one line and call ExitPlanMode right away. After approval, create the file.",
            "tool Write Done",
            "tool ToolSearch Done",
            "tool ExitPlanMode Done",
            "assistant Now creating the file as approved.",
            "tool Write Done",
            "assistant Done. File created at /tmp/project/plan.txt",
            "turn-end success Some(5883)",
        ]
    );
    assert_eq!(
        transcript.session_id.as_deref(),
        Some("08511edb-8e0b-4d07-9989-51ab170193ce")
    );
    assert_eq!(transcript.cwd.as_deref(), Some("/tmp/project"));
    assert_eq!(transcript.git_branch.as_deref(), Some("HEAD"));
    assert_eq!(
        transcript.model.as_deref(),
        Some("claude-haiku-4-5-20251001")
    );
    assert_eq!(transcript.last_effort.as_deref(), Some("high"));
    assert!(transcript.title.is_some());
    assert_eq!(transcript.skipped_lines, 0);
    assert!(!transcript.truncated);

    let image_prompt = transcript
        .items
        .iter()
        .find_map(|item| match item {
            TimelineItem::User(message) if !message.images.is_empty() => Some(message),
            _ => None,
        })
        .unwrap();
    assert_eq!(image_prompt.images[0].media_type, "image/png");
    assert!(image_prompt.images[0].data.is_some());
    assert!(
        image_prompt.images[0]
            .path
            .as_deref()
            .is_some_and(|path| path.ends_with("images/1.png"))
    );

    let denied_write = transcript
        .items
        .iter()
        .find_map(|item| match item {
            TimelineItem::Tool(tool) if tool.status == ToolStatus::Denied => Some(tool),
            _ => None,
        })
        .unwrap();
    let result = denied_write.result.as_ref().unwrap();
    assert!(result.is_error);
    assert_eq!(result.text, "User declined this write");

    let bash = transcript
        .items
        .iter()
        .find_map(|item| match item {
            TimelineItem::Tool(tool) if tool.name == "Bash" => Some(tool),
            _ => None,
        })
        .unwrap();
    assert_eq!(bash.input["command"], "echo hello > perm-test.txt");
    assert!(bash.started_at.is_some() && bash.finished_at.is_some());
    assert_eq!(
        bash.result.as_ref().unwrap().structured.as_ref().unwrap()["stdout"],
        ""
    );
}

#[test]
fn last_user_message_uuid_skips_commands() {
    let transcript = load(CAPTURE);
    let last_prompt = transcript.items.iter().rev().find_map(|item| match item {
        TimelineItem::User(message) if message.command.is_none() => Some(message.id.clone()),
        _ => None,
    });
    assert_eq!(transcript.last_user_message_uuid, last_prompt);
}

#[test]
fn can_skip_subagents_and_pre_compaction_history() {
    let transcript = load_transcript_with(
        &fixture(CAPTURE),
        &LoadOptions {
            include_pre_compaction: false,
            include_subagents: false,
            max_items: None,
        },
    )
    .unwrap();
    let lines = describe(&transcript.items);
    assert!(lines.iter().all(|line| !line.starts_with("  [")));
    // The preserved answer moves after the compaction; everything older is gone.
    assert_eq!(
        &lines[..4],
        [
            "assistant red",
            "turn-end success Some(5)",
            "compacted Some(28587) Some(4652)",
            "compact summary",
        ]
    );
}

#[test]
fn drops_abandoned_branches_after_a_rewind() {
    let transcript = load("history_rewind.jsonl");
    assert_eq!(
        describe(&transcript.items),
        vec![
            "user first question",
            "assistant first answer",
            "turn-end success Some(1000)",
            "user edited prompt",
            "assistant edited answer",
            "turn-end success Some(1000)",
        ]
    );
    assert_eq!(transcript.title.as_deref(), Some("Branch test"));
    assert_eq!(transcript.last_user_message_uuid.as_deref(), Some("u3"));
}

#[test]
fn reattaches_parallel_tool_calls() {
    let transcript = load("history_parallel_tools.jsonl");
    assert_eq!(
        describe(&transcript.items),
        vec![
            "user read two files",
            "tool Read Done",
            "tool Grep Done",
            "assistant Both done",
            "turn-end success Some(5000)",
        ]
    );
    let TimelineItem::Tool(read) = &transcript.items[1] else {
        panic!("expected a tool");
    };
    assert_eq!(read.result.as_ref().unwrap().text, "contents of a");
}

#[test]
fn relinks_preserved_messages_and_keeps_older_history() {
    for name in [
        "history_compaction_messages.jsonl",
        "history_compaction_segment.jsonl",
    ] {
        let transcript = load(name);
        assert_eq!(
            describe(&transcript.items),
            vec![
                "user old question",
                "assistant old answer",
                "turn-end success Some(1000)",
                "user second question",
                "assistant kept answer",
                "compacted Some(1000) Some(100)",
                "compact summary",
                "turn-end success Some(1000)",
                "user after compaction",
                "assistant new answer",
                "turn-end success Some(1000)",
            ],
            "{name}"
        );

        let only_current = load_transcript_with(
            &fixture(name),
            &LoadOptions {
                include_pre_compaction: false,
                ..LoadOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            describe(&only_current.items),
            vec![
                "assistant kept answer",
                "compacted Some(1000) Some(100)",
                "compact summary",
                "turn-end success Some(0)",
                "user after compaction",
                "assistant new answer",
                "turn-end success Some(1000)",
            ],
            "{name}"
        );
    }
}

#[test]
fn converts_commands_interrupts_queued_messages_and_notifications() {
    let transcript = load("history_commands.jsonl");
    assert_eq!(
        describe(&transcript.items),
        vec![
            "command model [sonnet] -> Some(\"Set model to sonnet\")",
            "command ! [ls] -> Some(\"a.txt\")",
            "user What is this?",
            "assistant A cat",
            "turn-end success Some(2000)",
            "user Count to 100",
            "interrupted",
            "turn-end error_during_execution Some(1000)",
            "user Run tests",
            "tool Bash Failed",
            "user also lint",
            "assistant Tests failed",
            "turn-end success Some(4000)",
            "info Background build finished",
            "assistant Build done",
            "error API Error: overloaded",
            "turn-end error_during_execution Some(2000)",
        ]
    );
    let TimelineItem::User(image_prompt) = &transcript.items[2] else {
        panic!("expected the image prompt");
    };
    assert_eq!(
        image_prompt.images,
        vec![ImageAttachment {
            media_type: "image/png".to_owned(),
            path: Some("/tmp/img/1.png".to_owned()),
            data: Some("AAAA".to_owned()),
        }]
    );
    let TimelineItem::User(queued) = &transcript.items[10] else {
        panic!("expected the queued message");
    };
    assert_eq!(queued.id, "q1-source");
    assert_eq!(
        transcript.last_user_message_uuid.as_deref(),
        Some("q1-source")
    );
    assert_eq!(transcript.model.as_deref(), Some("claude-test-1"));
    assert_eq!(transcript.last_effort.as_deref(), Some("medium"));
    assert_eq!(transcript.title.as_deref(), Some("What is this?"));
    assert_eq!(transcript.cwd.as_deref(), Some("/tmp/project"));
    assert_eq!(transcript.git_branch.as_deref(), Some("main"));
}

#[test]
fn tracks_todos_denials_and_turn_durations() {
    let transcript = load("history_tasks.jsonl");
    assert_eq!(
        describe(&transcript.items),
        vec![
            "user plan it",
            "tool TodoWrite Done",
            "tool TaskCreate Done",
            "tool TaskUpdate Done",
            "tool Write Denied",
            "assistant ok",
            "turn-end success Some(4321)",
        ]
    );
    assert_eq!(
        transcript.todos,
        vec![
            TodoItem {
                id: None,
                content: "A".to_owned(),
                active_form: Some("Doing A".to_owned()),
                status: TodoStatus::Completed,
            },
            TodoItem {
                id: None,
                content: "B".to_owned(),
                active_form: Some("Doing B".to_owned()),
                status: TodoStatus::InProgress,
            },
            TodoItem {
                id: Some("1".to_owned()),
                content: "C".to_owned(),
                active_form: Some("Doing C".to_owned()),
                status: TodoStatus::InProgress,
            },
        ]
    );
    assert_eq!(transcript.title.as_deref(), Some("My title"));
}

#[test]
fn tail_keeps_whole_recent_turns() {
    let transcript = load_transcript_tail(&fixture("history_rewind.jsonl"), 2).unwrap();
    assert!(transcript.truncated);
    assert_eq!(
        describe(&transcript.items),
        vec![
            "user edited prompt",
            "assistant edited answer",
            "turn-end success Some(1000)",
        ]
    );
    // The title still comes from the whole transcript.
    assert_eq!(transcript.title.as_deref(), Some("Branch test"));

    let everything = load_transcript_tail(&fixture("history_rewind.jsonl"), 100).unwrap();
    assert!(!everything.truncated);
    assert_eq!(everything.items.len(), 6);
}

#[test]
fn tolerates_garbage_lines_and_unknown_entries() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("broken.jsonl");
    std::fs::write(
        &path,
        concat!(
            "not json\n",
            "\n",
            "{\"type\":\"file-history-snapshot\",\"snapshot\":{}}\n",
            "{\"type\":\"something-new\",\"uuid\":\"x\"}\n",
            "{\"type\":\"user\",\"uuid\":\"u1\",\"parentUuid\":null,\"isMeta\":\"weird\",",
            "\"attachment\":\"not an object\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
            "{\"type\":\"assistant\",\"uuid\":\"a1\",\"parentUuid\":\"u1\",\"message\":",
            "{\"id\":\"m\",\"content\":[{\"type\":\"text\",\"text\":\"hello\"},",
            "{\"type\":\"thinking\",\"thinking\":\"\"},{\"type\":\"text\",\"text\":\"again\"}]}}\n",
            "{\"type\":\"user\",\"uuid\":\"u2\",\"parentUuid\":\"a1\",\"message\":{\"content\":[1,2]}}",
        ),
    )
    .unwrap();
    let transcript = load_transcript(&path).unwrap();
    assert_eq!(transcript.skipped_lines, 1);
    assert_eq!(
        describe(&transcript.items),
        vec![
            "user hi",
            "assistant hello\n\nagain",
            "turn-end success None"
        ]
    );
    assert_eq!(transcript.items[1].id(), "a1:0");
    assert_eq!(transcript.session_id.as_deref(), Some("broken"));

    assert!(load_transcript(&dir.path().join("missing.jsonl")).is_err());
}

#[test]
fn strips_context_tags_and_ansi() {
    assert_eq!(
        strip_context_tags(
            "<system-reminder>x</system-reminder> hello <ide_selection>y</ide_selection>"
        ),
        "hello"
    );
    assert_eq!(strip_ansi("\u{1b}[1mbold\u{1b}[0m"), "bold");
    assert_eq!(
        parse_slash_command(
            "<command-message>approve</command-message>\n<command-name>/approve</command-name>"
        ),
        Some(SlashCommandInvocation {
            name: "approve".to_owned(),
            args: String::new(),
            output: None,
        })
    );
    assert_eq!(
        task_id_from_text("Task #12 created successfully"),
        Some("12".to_owned())
    );
}

/// Loads every transcript under `~/.claude/projects`, one at a time, to check real-world files.
/// Run with `cargo test -p claude_chat -- --ignored --nocapture real_transcripts`.
#[test]
#[ignore]
#[allow(clippy::disallowed_types)]
fn loads_real_transcripts() {
    use std::time::Instant;

    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let projects = Path::new(&home).join(".claude/projects");
    let Ok(dirs) = std::fs::read_dir(&projects) else {
        return;
    };
    let mut files = 0;
    let mut items = 0;
    let mut bytes = 0;
    let mut skipped_lines = 0;
    let mut slowest = (Duration::ZERO, PathBuf::new(), 0);
    let mut kinds = std::collections::BTreeMap::<String, usize>::new();
    let started = Instant::now();
    for dir in dirs.filter_map(Result::ok) {
        let Ok(entries) = std::fs::read_dir(dir.path()) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path
                .extension()
                .is_none_or(|extension| extension != "jsonl")
            {
                continue;
            }
            let size = entry.metadata().map(|metadata| metadata.len()).unwrap_or(0);
            let load_started = Instant::now();
            let transcript = load_transcript(&path)
                .unwrap_or_else(|error| panic!("{}: {error:#}", path.display()));
            let elapsed = load_started.elapsed();
            files += 1;
            bytes += size;
            items += transcript.items.len();
            skipped_lines += transcript.skipped_lines;
            for item in &transcript.items {
                let kind = match item {
                    TimelineItem::Tool(tool) => format!("tool {:?}", tool.status),
                    TimelineItem::Notice(notice) => match notice.kind {
                        NoticeKind::Compacted { .. } => "notice compacted".to_owned(),
                        NoticeKind::CompactSummary(_) => "notice summary".to_owned(),
                        NoticeKind::Interrupted => "notice interrupted".to_owned(),
                        NoticeKind::Error(_) => "notice error".to_owned(),
                        NoticeKind::Info(_) => "notice info".to_owned(),
                    },
                    TimelineItem::User(message) if message.command.is_some() => {
                        "command".to_owned()
                    }
                    TimelineItem::User(_) => "user".to_owned(),
                    TimelineItem::Assistant(_) => "assistant".to_owned(),
                    TimelineItem::Thinking(_) => "thinking".to_owned(),
                    TimelineItem::TurnEnd(summary) => format!("turn-end {}", summary.subtype),
                };
                *kinds.entry(kind).or_default() += 1;
            }
            if elapsed > slowest.0 {
                slowest = (elapsed, path.clone(), size);
            }
            drop(transcript);
        }
    }
    println!(
        "{files} transcripts, {} MB, {items} items, {skipped_lines} skipped lines in {:?}; \
         slowest {:?} for {} ({} MB)",
        bytes / 1_000_000,
        started.elapsed(),
        slowest.0,
        slowest.1.display(),
        slowest.2 / 1_000_000
    );
    println!("{kinds:?}");
}
