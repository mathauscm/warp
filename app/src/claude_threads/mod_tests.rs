use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::*;

fn summary_of(lines: &[&str]) -> Option<(String, PathBuf)> {
    let mut summary = TranscriptSummary::default();
    for line in lines {
        summary.add_line(line);
    }
    summary.finish()
}

fn user_line(cwd: &str, text: &str) -> String {
    serde_json::json!({
        "type": "user",
        "cwd": cwd,
        "message": { "role": "user", "content": text },
    })
    .to_string()
}

#[test]
fn git_branch_comes_from_the_latest_prompt() {
    let prompt = |branch: &str| {
        serde_json::json!({
            "type": "user",
            "cwd": "/w/kinbox",
            "gitBranch": branch,
            "message": { "role": "user", "content": "oi" },
        })
        .to_string()
    };
    let mut summary = TranscriptSummary::default();
    for line in [prompt("main"), prompt("feat/login"), prompt("")] {
        summary.add_line(&line);
    }
    assert_eq!(summary.git_branch.as_deref(), Some("feat/login"));
}

#[test]
fn project_slug_replaces_every_non_alphanumeric_character() {
    assert_eq!(
        project_slug(Path::new("/Users/me/projects/aldeia/kinbox-worktree")),
        "-Users-me-projects-aldeia-kinbox-worktree"
    );
    assert_eq!(
        project_slug(Path::new("/Users/me/warmbox-manager/.github")),
        "-Users-me-warmbox-manager--github"
    );
}

#[test]
fn custom_title_wins_over_ai_title_and_first_prompt() {
    let prompt = user_line("/w/kinbox", "corrige o bug do login");
    let lines = [
        prompt.as_str(),
        r#"{"type":"ai-title","aiTitle":"Bug do login","sessionId":"a"}"#,
        r#"{"type":"custom-title","customTitle":"Login quebrado","sessionId":"a"}"#,
    ];
    assert_eq!(
        summary_of(&lines),
        Some(("Login quebrado".to_owned(), PathBuf::from("/w/kinbox")))
    );
}

#[test]
fn latest_ai_title_wins() {
    let prompt = user_line("/w/kinbox", "oi");
    let lines = [
        prompt.as_str(),
        r#"{"type":"ai-title","aiTitle":"Primeiro","sessionId":"a"}"#,
        r#"{"type":"ai-title","aiTitle":"Segundo","sessionId":"a"}"#,
    ];
    assert_eq!(summary_of(&lines).unwrap().0, "Segundo");
}

#[test]
fn first_typed_prompt_is_the_fallback_title() {
    let command = user_line("/w/kinbox", "<command-name>/clear</command-name>");
    let prompt = user_line("/w/kinbox", "  explica\n o módulo   de cadências ");
    let later = user_line("/w/kinbox", "outra pergunta");
    let lines = [command.as_str(), prompt.as_str(), later.as_str()];
    assert_eq!(
        summary_of(&lines).unwrap().0,
        "explica o módulo de cadências"
    );
}

#[test]
fn prompt_text_skips_tool_results_in_content_arrays() {
    let tool_result = serde_json::json!([
        { "type": "tool_result", "content": "ok" },
        { "type": "text", "text": "e agora?" },
    ]);
    assert_eq!(prompt_text(&tool_result), Some("e agora?".to_owned()));
}

#[test]
fn transcript_without_prompt_is_skipped() {
    let lines = [r#"{"type":"mode","mode":"normal","sessionId":"a"}"#];
    assert_eq!(summary_of(&lines), None);
}

#[test]
fn sidechain_records_are_ignored() {
    let sidechain = serde_json::json!({
        "type": "user",
        "isSidechain": true,
        "cwd": "/w/kinbox",
        "message": { "content": "subagente" },
    })
    .to_string();
    assert_eq!(summary_of(&[sidechain.as_str()]), None);
}

#[test]
fn long_titles_are_cut_with_an_ellipsis() {
    let title = clean_title(&"a".repeat(200));
    assert_eq!(title.chars().count(), TITLE_MAX_CHARS);
    assert!(title.ends_with('…'));
}

#[test]
fn resume_command_rejects_unsafe_session_ids() {
    let thread = |session_id: &str| ClaudeThread {
        session_id: session_id.to_owned(),
        title: String::new(),
        cwd: PathBuf::from("/w"),
        updated_at: SystemTime::UNIX_EPOCH,
        transcript: PathBuf::from("/w/a.jsonl"),
        git_branch: None,
    };
    assert_eq!(
        thread("80870f2f-fdc6-49d4-a6b3-90cd2c3d3b1c").resume_command(),
        Some("claude --resume 80870f2f-fdc6-49d4-a6b3-90cd2c3d3b1c".to_owned())
    );
    assert_eq!(thread("x; rm -rf ~").resume_command(), None);
    assert_eq!(thread("").resume_command(), None);
}

#[test]
fn format_age_uses_short_relative_labels() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(30 * 24 * 3600);
    let ago = |seconds: u64| format_age(now - Duration::from_secs(seconds), now);
    assert_eq!(ago(10), "agora");
    assert_eq!(ago(5 * 60), "5 min");
    assert_eq!(ago(3 * 3600), "3 h");
    assert_eq!(ago(30 * 3600), "ontem");
    assert_eq!(ago(3 * 24 * 3600), "3 d");
}

fn thread(session_id: &str, cwd: &str) -> ClaudeThread {
    ClaudeThread {
        session_id: session_id.to_owned(),
        title: session_id.to_owned(),
        cwd: PathBuf::from(cwd),
        updated_at: SystemTime::UNIX_EPOCH,
        transcript: PathBuf::new(),
        git_branch: None,
    }
}

#[test]
fn scan_reads_every_transcript_folder() {
    let root = tempfile::tempdir().unwrap();
    let write = |cwd: &str, session_id: &str, prompt: &str| {
        let dir = root.path().join(project_slug(Path::new(cwd)));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{session_id}.jsonl")),
            user_line(cwd, prompt) + "\n",
        )
        .unwrap();
    };
    write("/w/kinbox", "s1", "na raiz");
    write("/w/kinbox/kinbox-web", "s2", "no subprojeto");
    write("/w/kinbox-worktree", "s3", "no worktree");

    let mut cache = TranscriptCache::new();
    let mut ids: Vec<String> = scan_in(root.path(), &mut cache)
        .into_iter()
        .map(|thread| thread.session_id)
        .collect();
    ids.sort();
    assert_eq!(ids, ["s1", "s2", "s3"]);
    assert_eq!(cache.len(), 3);
}

#[test]
fn threads_are_grouped_into_pinned_projects_and_recent() {
    let config = ThreadsConfig {
        pinned: vec!["pinned-2".to_owned(), "pinned-1".to_owned()],
        projects: vec![
            ThreadProject::new(PathBuf::from("/w/kinbox")),
            ThreadProject::new(PathBuf::from("/w/kinbox/web")),
            ThreadProject::new(PathBuf::from("/w/payflow")),
        ],
        moved: [("moved".to_owned(), PathBuf::from("/w/payflow"))].into(),
        ..Default::default()
    };
    let groups = group_threads(
        vec![
            thread("pinned-1", "/w/kinbox"),
            thread("root", "/w/kinbox"),
            thread("nested", "/w/kinbox/web/src"),
            thread("sibling", "/w/kinbox-worktree"),
            thread("moved", "/tmp"),
            thread("pinned-2", "/tmp"),
        ],
        &config,
    );
    let ids = |threads: &[ClaudeThread]| {
        threads
            .iter()
            .map(|thread| thread.session_id.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&groups.pinned), ["pinned-2", "pinned-1"]);
    assert_eq!(ids(&groups.by_project[Path::new("/w/kinbox")]), ["root"]);
    assert_eq!(
        ids(&groups.by_project[Path::new("/w/kinbox/web")]),
        ["nested"]
    );
    assert_eq!(ids(&groups.by_project[Path::new("/w/payflow")]), ["moved"]);
    assert_eq!(ids(&groups.recent), ["sibling"]);
}

#[test]
fn threads_moved_to_a_removed_project_go_back_to_their_folder() {
    let config = ThreadsConfig {
        projects: vec![ThreadProject::new(PathBuf::from("/w/kinbox"))],
        moved: [("s1".to_owned(), PathBuf::from("/w/gone"))].into(),
        ..Default::default()
    };
    assert_eq!(
        config.project_of(&thread("s1", "/w/kinbox/app")),
        Some(Path::new("/w/kinbox"))
    );
    assert_eq!(config.project_of(&thread("s2", "/elsewhere")), None);
}

#[test]
fn config_round_trips_through_toml() {
    let config = ThreadsConfig {
        pinned: vec!["a".to_owned()],
        recent_collapsed: true,
        projects: vec![ThreadProject::new(PathBuf::from("/w/kinbox"))],
        moved: [("b".to_owned(), PathBuf::from("/w/kinbox"))].into(),
        ..Default::default()
    };
    let text = toml::to_string_pretty(&config).unwrap();
    assert_eq!(toml::from_str::<ThreadsConfig>(&text).unwrap(), config);
    // Configs written before pinning existed still load.
    let old: ThreadsConfig =
        toml::from_str("[[project]]\npath = \"/w/kinbox\"\ncollapsed = true\n").unwrap();
    assert!(old.projects[0].collapsed && old.pinned.is_empty());
}

#[test]
fn rename_appends_a_custom_title_that_wins() {
    let dir = tempfile::tempdir().unwrap();
    let transcript = dir.path().join("abc.jsonl");
    let prompt = user_line("/w/kinbox", "corrige o bug do login");
    let ai_title = r#"{"type":"ai-title","aiTitle":"Bug do login","sessionId":"abc"}"#;
    // The last line is left unfinished, as when Claude is still writing it.
    std::fs::write(&transcript, format!("{prompt}\n{ai_title}")).unwrap();
    let thread = ClaudeThread {
        session_id: "abc".to_owned(),
        title: "Bug do login".to_owned(),
        cwd: PathBuf::from("/w/kinbox"),
        updated_at: SystemTime::UNIX_EPOCH,
        transcript: transcript.clone(),
        git_branch: None,
    };

    assert_eq!(
        rename_thread(&thread, "  Login   do app ").as_deref(),
        Ok("Login do app")
    );
    let contents = std::fs::read_to_string(&transcript).unwrap();
    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(
        summary_of(&lines).map(|(title, _)| title).as_deref(),
        Some("Login do app")
    );
    assert!(rename_thread(&thread, "   ").is_err());
}
