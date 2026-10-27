use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};
use tempfile::TempDir;

use super::*;

const HTML: &str = "<!doctype html><html><head><title>t</title></head><body>hi</body></html>";

fn store() -> (TempDir, ArtifactStore) {
    let dir = TempDir::new().unwrap();
    let store = ArtifactStore::new(dir.path().join("artifacts"));
    (dir, store)
}

fn context() -> ArtifactContext {
    ArtifactContext {
        session_id: Some("session-1".to_owned()),
        cwd: Some(PathBuf::from("/tmp/project")),
    }
}

fn create(store: &ArtifactStore, title: &str) -> ArtifactMeta {
    store
        .create(title, ArtifactKind::Document, HTML, None, None, None)
        .unwrap()
}

fn call(store: &ArtifactStore, name: &str, arguments: Value) -> Value {
    let response = handle_mcp_message(
        store,
        &context(),
        &json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        }),
    )
    .unwrap();
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 7);
    response["result"].clone()
}

fn text_of(result: &Value) -> &str {
    let content = result["content"].as_array().unwrap();
    assert_eq!(content.len(), 1);
    assert_eq!(content[0]["type"], "text");
    content[0]["text"].as_str().unwrap()
}

fn assert_tool_error(result: &Value, needle: &str) {
    assert_eq!(result["isError"], true, "{result}");
    assert!(result.get("structuredContent").is_none());
    let text = text_of(result);
    assert!(text.contains(needle), "{text:?} should contain {needle:?}");
}

#[test]
fn artifacts_create_writes_files_and_meta() {
    let (_dir, store) = store();
    let meta = store
        .create(
            "  Relatório Q3 ",
            ArtifactKind::Dashboard,
            HTML,
            Some(" Vendas ".to_owned()),
            Some("s1".to_owned()),
            Some(PathBuf::from("/work")),
        )
        .unwrap();

    assert_eq!(meta.id.len(), 10);
    assert!(meta.id.bytes().all(|b| b.is_ascii_alphanumeric()));
    assert_eq!(meta.title, "Relatório Q3");
    assert_eq!(meta.kind, ArtifactKind::Dashboard);
    assert_eq!(meta.description.as_deref(), Some("Vendas"));
    assert_eq!(meta.session_id.as_deref(), Some("s1"));
    assert_eq!(meta.cwd, Some(PathBuf::from("/work")));
    assert_eq!(meta.version, 1);
    assert_eq!(meta.created_at, meta.updated_at);

    let html_path = store.html_path(&meta.id).unwrap();
    assert_eq!(html_path, store.root().join(&meta.id).join("index.html"));
    assert_eq!(fs::read_to_string(&html_path).unwrap(), HTML);
    assert_eq!(store.read_html(&meta.id).unwrap(), HTML);
    assert_eq!(
        store.thumbnail_path(&meta.id).unwrap(),
        store.root().join(&meta.id).join("thumbnail.png")
    );
    assert_eq!(store.get(&meta.id), Some(meta.clone()));

    let raw: Value =
        serde_json::from_slice(&fs::read(store.root().join(&meta.id).join("meta.json")).unwrap())
            .unwrap();
    assert_eq!(raw["kind"], "dashboard");
    assert_eq!(raw["version"], 1);
    assert!(raw["created_at"].as_str().is_some());

    let leftovers = fs::read_dir(store.root().join(&meta.id))
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
        .count();
    assert_eq!(leftovers, 0);
}

#[test]
fn artifacts_ids_are_unique() {
    let (_dir, store) = store();
    let mut ids = (0..20)
        .map(|i| create(&store, &format!("a{i}")).id)
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 20);
}

#[test]
fn artifacts_create_rejects_bad_input() {
    let (_dir, store) = store();
    assert!(
        store
            .create(" ", ArtifactKind::Other, HTML, None, None, None)
            .is_err()
    );
    assert!(
        store
            .create("t", ArtifactKind::Other, "  \n", None, None, None)
            .is_err()
    );
    let huge = "a".repeat(MAX_HTML_BYTES + 1);
    assert!(
        store
            .create("t", ArtifactKind::Other, &huge, None, None, None)
            .is_err()
    );
    assert!(store.list().is_empty());
}

#[test]
fn artifacts_long_title_is_truncated() {
    let (_dir, store) = store();
    let meta = create(&store, &"é".repeat(500));
    assert_eq!(meta.title.chars().count(), 200);
}

#[test]
fn artifacts_list_is_newest_updated_first() {
    let (_dir, store) = store();
    assert!(store.list().is_empty());
    let first = create(&store, "first");
    let second = create(&store, "second");
    let third = create(&store, "third");
    store.update(&first.id, "<p>v2</p>", None).unwrap();

    let ids = store
        .list()
        .into_iter()
        .map(|meta| meta.id)
        .collect::<Vec<_>>();
    assert_eq!(ids, vec![first.id, third.id, second.id]);
}

#[test]
fn artifacts_update_bumps_version_and_keeps_history() {
    let (_dir, store) = store();
    let meta = create(&store, "deck");

    let updated = store.update(&meta.id, "<p>v2</p>", Some("Deck 2")).unwrap();
    assert_eq!(updated.version, 2);
    assert_eq!(updated.title, "Deck 2");
    assert_eq!(updated.created_at, meta.created_at);
    assert!(updated.updated_at >= meta.updated_at);
    assert_eq!(store.read_html(&meta.id).unwrap(), "<p>v2</p>");
    assert_eq!(
        fs::read_to_string(store.version_path(&meta.id, 1).unwrap()).unwrap(),
        HTML
    );

    let third = store.update(&meta.id, "<p>v3</p>", Some("  ")).unwrap();
    assert_eq!(third.version, 3);
    assert_eq!(third.title, "Deck 2");
    assert_eq!(
        fs::read_to_string(store.version_path(&meta.id, 2).unwrap()).unwrap(),
        "<p>v2</p>"
    );
    assert_eq!(store.get(&meta.id), Some(third));
}

#[test]
fn artifacts_update_errors() {
    let (_dir, store) = store();
    let meta = create(&store, "x");
    assert!(store.update("missing123", "<p></p>", None).is_err());
    assert!(store.update(&meta.id, "", None).is_err());
    assert!(store.update("../escape", "<p></p>", None).is_err());
    assert_eq!(store.get(&meta.id).unwrap().version, 1);
}

#[test]
fn artifacts_delete_moves_to_trash() {
    let (_dir, store) = store();
    let keep = create(&store, "keep");
    let gone = create(&store, "gone");
    store.delete(&gone.id).unwrap();

    assert!(store.get(&gone.id).is_none());
    assert!(!store.root().join(&gone.id).exists());
    assert!(
        store
            .root()
            .join(".trash")
            .join(&gone.id)
            .join("index.html")
            .exists()
    );
    let listed = store.list();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, keep.id);
    assert!(store.delete(&gone.id).is_err());
}

#[test]
fn artifacts_corrupt_or_foreign_entries_are_skipped() {
    let (_dir, store) = store();
    let good = create(&store, "good");

    let corrupt = store.root().join("corrupt01");
    fs::create_dir_all(&corrupt).unwrap();
    fs::write(corrupt.join("meta.json"), "{ not json").unwrap();

    let mismatched = store.root().join("mismatch1");
    fs::create_dir_all(&mismatched).unwrap();
    let mut meta = serde_json::to_value(store.get(&good.id).unwrap()).unwrap();
    meta["id"] = json!("someoneelse");
    fs::write(mismatched.join("meta.json"), meta.to_string()).unwrap();

    fs::create_dir_all(store.root().join("nometa")).unwrap();
    fs::write(store.root().join("stray.txt"), "x").unwrap();

    let listed = store.list();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, good.id);
    assert!(store.get("corrupt01").is_none());
    assert!(store.get("mismatch1").is_none());
}

#[test]
fn artifacts_unknown_kind_in_meta_reads_as_other() {
    let (_dir, store) = store();
    let meta = create(&store, "x");
    let path = store.root().join(&meta.id).join("meta.json");
    let mut raw: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    raw["kind"] = json!("spreadsheet");
    fs::write(&path, raw.to_string()).unwrap();
    assert_eq!(store.get(&meta.id).unwrap().kind, ArtifactKind::Other);
}

#[test]
fn artifacts_invalid_ids_are_rejected() {
    let (_dir, store) = store();
    for id in ["", "..", "../x", "a/b", ".trash", "a b", &"x".repeat(65)] {
        assert!(store.get(id).is_none(), "{id:?}");
        assert!(store.html_path(id).is_err(), "{id:?}");
        assert!(store.thumbnail_path(id).is_err(), "{id:?}");
        assert!(store.delete(id).is_err(), "{id:?}");
    }
}

#[test]
fn artifacts_kind_round_trips() {
    for kind in ArtifactKind::ALL {
        assert_eq!(ArtifactKind::parse(kind.as_str()), Some(kind));
        assert_eq!(serde_json::to_value(kind).unwrap(), json!(kind.as_str()));
        assert!(!kind_label_pt(kind).is_empty());
        assert!(starter_prompt_pt(kind).starts_with("Crie "));
    }
    assert_eq!(
        ArtifactKind::parse(" Presentation "),
        Some(ArtifactKind::Presentation)
    );
    assert_eq!(ArtifactKind::parse("slides"), None);
    assert_eq!(kind_label_pt(ArtifactKind::Dashboard), "Painel");
    assert_eq!(kind_label_pt(ArtifactKind::Presentation), "Apresentação");
    assert_eq!(kind_label_pt(ArtifactKind::Other), "Artefato");
}

#[test]
fn artifacts_mcp_initialize() {
    let (_dir, store) = store();
    let response = handle_mcp_message(
        &store,
        &context(),
        &json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": { "name": "claude-code", "version": "2.1" },
            },
        }),
    )
    .unwrap();
    assert_eq!(response["id"], 0);
    let result = &response["result"];
    assert_eq!(result["protocolVersion"], "2025-03-26");
    assert_eq!(result["capabilities"], json!({ "tools": {} }));
    assert_eq!(result["serverInfo"]["name"], "picolas");
    assert!(result["serverInfo"]["version"].is_string());

    let response = handle_mcp_message(
        &store,
        &context(),
        &json!({ "jsonrpc": "2.0", "id": "a", "method": "initialize" }),
    )
    .unwrap();
    assert_eq!(response["id"], "a");
    assert_eq!(response["result"]["protocolVersion"], "2025-06-18");
}

#[test]
fn artifacts_mcp_notifications_and_responses_get_no_reply() {
    let (_dir, store) = store();
    for message in [
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": {} }),
        json!({ "jsonrpc": "2.0", "id": 3, "result": {} }),
        json!({ "jsonrpc": "2.0", "id": 3, "error": { "code": 1, "message": "x" } }),
    ] {
        assert_eq!(handle_mcp_message(&store, &context(), &message), None);
    }
}

#[test]
fn artifacts_mcp_ping_and_unknown_method() {
    let (_dir, store) = store();
    let ping = handle_mcp_message(
        &store,
        &context(),
        &json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }),
    )
    .unwrap();
    assert_eq!(ping, json!({ "jsonrpc": "2.0", "id": 1, "result": {} }));

    let unknown = handle_mcp_message(
        &store,
        &context(),
        &json!({ "jsonrpc": "2.0", "id": 2, "method": "resources/list" }),
    )
    .unwrap();
    assert_eq!(unknown["id"], 2);
    assert_eq!(unknown["error"]["code"], -32601);
    assert!(unknown.get("result").is_none());

    let invalid = handle_mcp_message(&store, &context(), &json!([1, 2])).unwrap();
    assert_eq!(invalid["error"]["code"], -32600);
    assert_eq!(invalid["id"], Value::Null);

    let missing_method =
        handle_mcp_message(&store, &context(), &json!({ "jsonrpc": "2.0", "id": 4 })).unwrap();
    assert_eq!(missing_method["error"]["code"], -32600);
}

#[test]
fn artifacts_mcp_tools_list_shape() {
    let (_dir, store) = store();
    let response = handle_mcp_message(
        &store,
        &context(),
        &json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {} }),
    )
    .unwrap();
    let tools = response["result"]["tools"].as_array().unwrap();
    let names = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec![
            "create_artifact",
            "update_artifact",
            "list_artifacts",
            "read_artifact"
        ]
    );
    for tool in tools {
        assert!(tool["description"].as_str().unwrap().len() > 40);
        assert_eq!(tool["inputSchema"]["type"], "object");
        assert!(tool["inputSchema"]["properties"].is_object());
    }
    let create = &tools[0]["inputSchema"];
    assert_eq!(create["required"], json!(["title", "kind", "html"]));
    assert_eq!(
        create["properties"]["kind"]["enum"],
        json!([
            "document",
            "presentation",
            "design",
            "dashboard",
            "motion",
            "other"
        ])
    );
}

#[test]
fn artifacts_mcp_create_tool() {
    let (_dir, store) = store();
    let result = call(
        &store,
        "create_artifact",
        json!({
            "title": "Pitch",
            "kind": "presentation",
            "html": HTML,
            "description": "Deck de vendas",
        }),
    );
    assert!(result.get("isError").is_none());
    let structured = &result["structuredContent"];
    let id = structured["id"].as_str().unwrap();
    assert_eq!(text_of(&result), format!("Created artifact {id} “Pitch”"));
    assert_eq!(structured["title"], "Pitch");
    assert_eq!(structured["kind"], "presentation");
    assert_eq!(structured["version"], 1);
    assert_eq!(
        structured["path"],
        store.html_path(id).unwrap().display().to_string()
    );

    let meta = store.get(id).unwrap();
    assert_eq!(meta.session_id.as_deref(), Some("session-1"));
    assert_eq!(meta.cwd, Some(PathBuf::from("/tmp/project")));
    assert_eq!(meta.description.as_deref(), Some("Deck de vendas"));
}

#[test]
fn artifacts_mcp_create_tool_errors() {
    let (_dir, store) = store();
    assert_tool_error(
        &call(
            &store,
            "create_artifact",
            json!({ "kind": "design", "html": HTML }),
        ),
        "title",
    );
    assert_tool_error(
        &call(
            &store,
            "create_artifact",
            json!({ "title": "t", "kind": "slides", "html": HTML }),
        ),
        "kind",
    );
    assert_tool_error(
        &call(
            &store,
            "create_artifact",
            json!({ "title": "t", "kind": "design", "html": "   " }),
        ),
        "html",
    );
    assert_tool_error(
        &call(
            &store,
            "create_artifact",
            json!({ "title": "t", "kind": "design", "html": 3 }),
        ),
        "must be a string",
    );
    assert_tool_error(
        &call(
            &store,
            "create_artifact",
            json!({ "title": "t", "kind": "design", "html": "a".repeat(MAX_HTML_BYTES + 1) }),
        ),
        "limit",
    );
    assert_tool_error(&call(&store, "create_artifact", json!("nope")), "object");
    assert!(store.list().is_empty());
}

#[test]
fn artifacts_mcp_unknown_tool_is_rpc_error() {
    let (_dir, store) = store();
    let response = handle_mcp_message(
        &store,
        &context(),
        &json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "tools/call",
            "params": { "name": "delete_everything", "arguments": {} },
        }),
    )
    .unwrap();
    assert_eq!(response["error"]["code"], -32602);

    let response = handle_mcp_message(
        &store,
        &context(),
        &json!({ "jsonrpc": "2.0", "id": 10, "method": "tools/call", "params": {} }),
    )
    .unwrap();
    assert_eq!(response["error"]["code"], -32602);
}

#[test]
fn artifacts_mcp_update_list_read_tools() {
    let (_dir, store) = store();
    let meta = create(&store, "Painel");

    let result = call(
        &store,
        "update_artifact",
        json!({ "id": meta.id, "html": "<p>v2</p>", "title": "Painel 2" }),
    );
    assert!(result.get("isError").is_none());
    assert_eq!(result["structuredContent"]["version"], 2);
    assert_eq!(result["structuredContent"]["title"], "Painel 2");
    assert!(text_of(&result).contains("version 2"));

    let result = call(&store, "list_artifacts", json!({}));
    let items = result["structuredContent"]["artifacts"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], meta.id.as_str());
    assert_eq!(items[0]["title"], "Painel 2");
    assert_eq!(items[0]["kind"], "document");
    assert!(items[0]["updated_at"].is_string());
    assert!(text_of(&result).contains(&meta.id));

    let result = call(&store, "read_artifact", json!({ "id": meta.id }));
    assert_eq!(text_of(&result), "<p>v2</p>");
    assert_eq!(result["structuredContent"]["version"], 2);

    assert_tool_error(
        &call(&store, "read_artifact", json!({ "id": "nothere99" })),
        "not found",
    );
    assert_tool_error(
        &call(
            &store,
            "update_artifact",
            json!({ "id": "../../etc", "html": "<p></p>" }),
        ),
        "Invalid artifact id",
    );
    assert_tool_error(
        &call(
            &store,
            "update_artifact",
            json!({ "id": meta.id, "html": "" }),
        ),
        "html",
    );
}

#[test]
fn artifacts_mcp_list_tool_empty_and_null_arguments() {
    let (_dir, store) = store();
    let response = handle_mcp_message(
        &store,
        &context(),
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "list_artifacts" },
        }),
    )
    .unwrap();
    let result = &response["result"];
    assert_eq!(text_of(result), "No artifacts yet.");
    assert_eq!(result["structuredContent"], json!({ "artifacts": [] }));
}

#[test]
fn artifacts_tool_names_and_card_ids() {
    assert_eq!(
        tool_names(),
        vec![
            "mcp__picolas__create_artifact",
            "mcp__picolas__update_artifact",
            "mcp__picolas__list_artifacts",
            "mcp__picolas__read_artifact",
        ]
    );
    assert!(is_artifact_tool("mcp__picolas__create_artifact"));
    assert!(!is_artifact_tool("mcp__other__create_artifact"));
    assert!(!is_artifact_tool("create_artifact"));
    assert!(!is_artifact_tool("mcp__picolas__nope"));

    let create = "mcp__picolas__create_artifact";
    let input = json!({ "title": "t" });
    assert_eq!(
        artifact_id_from_tool_result(create, &input, Some(&json!({ "id": "abc123" }))),
        Some("abc123".to_owned())
    );
    let full_result = json!({
        "content": [{ "type": "text", "text": "Created artifact abc123 “t”" }],
        "structuredContent": { "id": "abc123" },
    });
    assert_eq!(
        artifact_id_from_tool_result(create, &input, Some(&full_result)),
        Some("abc123".to_owned())
    );
    let transcript_blocks = json!([{ "type": "text", "text": "Created artifact xyz789 “t”" }]);
    assert_eq!(
        artifact_id_from_tool_result(create, &input, Some(&transcript_blocks)),
        Some("xyz789".to_owned())
    );
    assert_eq!(
        artifact_id_from_tool_result(create, &input, Some(&json!("Created artifact q1 “t”"))),
        Some("q1".to_owned())
    );
    let error = json!({ "content": [{ "type": "text", "text": "bad" }], "isError": true });
    assert_eq!(
        artifact_id_from_tool_result(create, &input, Some(&error)),
        None
    );
    assert_eq!(artifact_id_from_tool_result(create, &input, None), None);

    let update_input = json!({ "id": "abc123", "html": "<p></p>" });
    assert_eq!(
        artifact_id_from_tool_result("mcp__picolas__update_artifact", &update_input, None),
        Some("abc123".to_owned())
    );
    assert_eq!(
        artifact_id_from_tool_result("mcp__picolas__read_artifact", &update_input, None),
        Some("abc123".to_owned())
    );
    assert_eq!(
        artifact_id_from_tool_result(
            "mcp__picolas__update_artifact",
            &json!({ "id": "../x" }),
            None
        ),
        None
    );
    assert_eq!(
        artifact_id_from_tool_result("mcp__picolas__list_artifacts", &json!({}), None),
        None
    );
    assert_eq!(
        artifact_id_from_tool_result("Bash", &update_input, None),
        None
    );
}
