use std::path::{Path, PathBuf};

use super::*;

#[test]
fn workspace_system_prompt_lists_every_folder() {
    let workspace = ChatWorkspace {
        id: "w1".to_owned(),
        name: "Kinbox".to_owned(),
        extra_dirs: vec![PathBuf::from("/w/kinbox-api"), PathBuf::from("/w/plugin")],
    };
    assert_eq!(
        workspace.system_prompt(Path::new("/w/kinbox-web")),
        "This chat is in the workspace \"Kinbox\", which spans these projects: /w/kinbox-web \
         (the working directory), /w/kinbox-api, /w/plugin."
    );
}

#[test]
fn chat_workspace_round_trips_through_json() {
    let workspace = ChatWorkspace {
        id: "w1".to_owned(),
        name: "Kinbox".to_owned(),
        extra_dirs: vec![PathBuf::from("/w/kinbox-api")],
    };
    let json = serde_json::to_string(&workspace).unwrap();
    assert_eq!(
        serde_json::from_str::<ChatWorkspace>(&json).unwrap(),
        workspace
    );
    let without_dirs: ChatWorkspace = serde_json::from_str(r#"{"id":"w1","name":"K"}"#).unwrap();
    assert!(without_dirs.extra_dirs.is_empty());
}
