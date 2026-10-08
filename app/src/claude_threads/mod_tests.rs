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

#[test]
fn scan_groups_threads_by_project_and_skips_sibling_folders() {
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

    let kinbox = PathBuf::from("/w/kinbox");
    let mut cache = TranscriptCache::new();
    let threads = scan_in(root.path(), std::slice::from_ref(&kinbox), &mut cache);

    let mut ids: Vec<&str> = threads[&kinbox]
        .iter()
        .map(|thread| thread.session_id.as_str())
        .collect();
    ids.sort();
    assert_eq!(ids, ["s1", "s2"]);
    // The worktree folder shares the slug prefix, so it is read but not listed.
    assert_eq!(cache.len(), 3);
}
