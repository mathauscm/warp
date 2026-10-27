use std::fs;

use super::*;
use crate::protocol::{ControlRequestBody, control_request_line};

fn make_executable(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[test]
fn default_args_follow_the_sdk() {
    let config = SpawnConfig::new("/bin/claude", "/tmp/project");
    assert_eq!(
        config.args(),
        [
            "--output-format",
            "stream-json",
            "--verbose",
            "--input-format",
            "stream-json",
            "--permission-prompt-tool=stdio",
            "--include-partial-messages",
            "--allow-dangerously-skip-permissions",
            "--thinking-display",
            "summarized",
        ]
    );
}

#[test]
fn args_carry_session_options() {
    let mut config = SpawnConfig::new("/bin/claude", "/tmp/project");
    config.model = Some("haiku".to_owned());
    config.effort = Some("high".to_owned());
    config.permission_mode = Some("plan".to_owned());
    config.session_id = Some("new-id".to_owned());
    let args = config.args();
    let tail = &args[8..];
    assert_eq!(
        tail,
        [
            "--model",
            "haiku",
            "--effort",
            "high",
            "--permission-mode",
            "plan",
            "--thinking-display",
            "summarized",
            "--session-id",
            "new-id",
        ]
    );

    config.resume = Some("old-id".to_owned());
    config.resume_session_at = Some("msg-uuid".to_owned());
    config.fork_session = true;
    config.extra_args = vec!["--name".to_owned(), "Chat".to_owned()];
    let args = config.args();
    assert!(!args.contains(&"--session-id".to_owned()));
    assert!(args.ends_with(&[
        "--resume".to_owned(),
        "old-id".to_owned(),
        "--resume-session-at".to_owned(),
        "msg-uuid".to_owned(),
        "--fork-session".to_owned(),
        "--name".to_owned(),
        "Chat".to_owned(),
    ]));
}

#[test]
fn env_marks_the_sdk_host() {
    let mut config = SpawnConfig::new("/bin/claude", "/tmp/project");
    config.path_env = Some("/opt/bin:/usr/bin".to_owned());
    config.extra_env = vec![("FOO".to_owned(), "bar".to_owned())];
    let env = config.env();
    let get = |key: &str| {
        env.iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    };
    assert_eq!(get("CLAUDE_CODE_ENTRYPOINT"), Some(DEFAULT_ENTRYPOINT));
    assert_eq!(get("CLAUDE_CODE_SDK_READS_SESSION_STATE"), Some("1"));
    assert_eq!(
        get("CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING"),
        Some("true")
    );
    assert_eq!(get("PATH"), Some("/opt/bin:/usr/bin"));
    assert_eq!(get("FOO"), Some("bar"));
    assert!(SpawnConfig::env_remove().contains(&"CLAUDECODE"));

    config.file_checkpointing = false;
    assert!(
        !config
            .env()
            .iter()
            .any(|(name, _)| name == "CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING")
    );
}

#[test]
fn finds_claude_on_the_given_path() {
    let temp = tempfile::tempdir().unwrap();
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    fs::create_dir_all(&first).unwrap();
    // Not executable, so it's skipped.
    fs::write(first.join("claude"), "").unwrap();
    make_executable(&second.join("claude"));

    let path = std::env::join_paths([&first, &second]).unwrap();
    let home = temp.path().join("home");
    assert_eq!(
        find_claude_in(Some(path.to_str().unwrap()), Some(&home)),
        Some(second.join("claude"))
    );
}

#[test]
fn falls_back_to_home_installs() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path();
    make_executable(&home.join(".claude/local/claude"));
    assert_eq!(
        find_claude_in(Some(""), Some(home)),
        Some(home.join(".claude/local/claude"))
    );
    make_executable(&home.join(".local/bin/claude"));
    assert_eq!(
        find_claude_in(None, Some(home)),
        Some(home.join(".local/bin/claude"))
    );
}

#[test]
fn prefers_the_newest_nvm_node() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path();
    let nvm = home.join(".nvm/versions/node");
    for version in ["v9.11.0", "v22.3.0", "v18.20.1"] {
        make_executable(&nvm.join(version).join("bin/claude"));
    }
    let dirs = nvm_bin_dirs(&nvm);
    assert_eq!(
        dirs,
        [
            nvm.join("v22.3.0/bin"),
            nvm.join("v18.20.1/bin"),
            nvm.join("v9.11.0/bin"),
        ]
    );
    // Only fallbacks that don't exist on a dev machine are searched before nvm here, but the
    // system-wide ones might, so check the nvm choice directly.
    assert_eq!(
        dirs.iter()
            .map(|dir| dir.join("claude"))
            .find(|path| is_executable(path)),
        Some(nvm.join("v22.3.0/bin/claude"))
    );
}

/// Spawns the real CLI (no model call: only `initialize`), then closes stdin and expects a clean
/// exit. Run with `cargo test -p claude_chat -- --ignored live_smoke`.
#[test]
#[ignore]
fn live_smoke() {
    let path_env = std::env::var("PATH").ok();
    let claude = find_claude(path_env.as_deref()).expect("claude on PATH");
    let cwd = tempfile::tempdir().unwrap();
    let mut config = SpawnConfig::new(claude, cwd.path());
    config.model = Some("haiku".to_owned());
    config.path_env = path_env;
    config.extra_args = vec!["--no-session-persistence".to_owned()];

    let (process, driver) = spawn(&config).expect("spawn claude");
    let messages = process.messages();
    let consumer = async {
        assert!(process.send_line(control_request_line(
            "smoke-1",
            &ControlRequestBody::Initialize {
                sdk_mcp_servers: Vec::new(),
                append_system_prompt: None,
            },
        )));
        let mut initialized = false;
        while let Ok(message) = messages.recv().await {
            if let StdoutMessage::ControlResponse(response) = message
                && response.request_id == "smoke-1"
            {
                let value = response.outcome.expect("initialize succeeds");
                assert!(
                    value["commands"]
                        .as_array()
                        .is_some_and(|commands| !commands.is_empty())
                );
                initialized = true;
                process.close_stdin();
            }
        }
        initialized
    };
    let (status, initialized) =
        futures::executor::block_on(async { futures::join!(driver, consumer) });
    assert!(initialized);
    assert!(status.expect("exit status").success());
    assert!(process.exit_status().is_some());
}
