use super::*;

fn project(name: &str, root_path: &str, tags: &[&str]) -> Project {
    Project {
        name: name.to_owned(),
        root_path: root_path.to_owned(),
        paths: Vec::new(),
        tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
        enabled: true,
        profile: String::new(),
    }
}

#[test]
fn parses_the_vscode_extension_format() {
    let contents = r#"[
        {
            "name": "Kinbox",
            "rootPath": "/Users/me/projects/aldeia/kinbox",
            "paths": [],
            "tags": ["Aldeia"],
            "enabled": true,
            "profile": ""
        },
        { "name": "openGym", "rootPath": "/Users/me/openGym" }
    ]"#;
    let projects = parse_projects(contents).unwrap();
    assert_eq!(
        projects[0],
        project("Kinbox", "/Users/me/projects/aldeia/kinbox", &["Aldeia"])
    );
    // Missing fields take the extension's defaults.
    assert!(projects[1].enabled);
    assert!(projects[1].tags.is_empty());
    assert!(parse_projects("  ").unwrap().is_empty());
}

#[test]
fn serializes_with_the_extension_field_names() {
    let json = serde_json::to_value(project("Kinbox", "/k", &["Aldeia"])).unwrap();
    assert_eq!(json["rootPath"], "/k");
    assert_eq!(json["tags"][0], "Aldeia");
    assert_eq!(json["enabled"], true);
}

#[test]
fn root_expands_home_prefixes() {
    let home = dirs::home_dir().unwrap();
    assert_eq!(project("a", "~/code/a", &[]).root(), home.join("code/a"));
    assert_eq!(
        project("a", "$home/code/a", &[]).root(),
        home.join("code/a")
    );
    assert_eq!(project("a", "/abs/a", &[]).root(), PathBuf::from("/abs/a"));
}

#[test]
fn visible_projects_filters_and_sorts() {
    let mut disabled = project("Zeta", "/z", &["Aldeia"]);
    disabled.enabled = false;
    let projects = vec![
        project("kinbox", "/w/kinbox", &["Aldeia"]),
        project("Curriculo", "/docs/curriculo", &["Personal"]),
        disabled,
        project("openGym", "/a/openGym", &[]),
    ];

    assert_eq!(
        visible_projects(&projects, "", &[], SortOrder::Name),
        [1, 0, 3]
    );
    assert_eq!(
        visible_projects(&projects, "", &[], SortOrder::Saved),
        [0, 1, 3]
    );
    assert_eq!(
        visible_projects(&projects, "", &[], SortOrder::Path),
        [3, 1, 0]
    );
    assert_eq!(
        visible_projects(&projects, "KIN", &[], SortOrder::Name),
        [0]
    );
    assert_eq!(
        visible_projects(&projects, "", &["Personal".to_owned()], SortOrder::Name),
        [1]
    );
    assert_eq!(
        visible_projects(&projects, "", &[NO_TAG_LABEL.to_owned()], SortOrder::Name),
        [3]
    );
}

#[test]
fn group_by_tag_lists_untagged_projects_last() {
    let projects = vec![
        project("kinbox", "/k", &["Aldeia", "Work"]),
        project("openGym", "/o", &[]),
        project("Rust", "/r", &["Estudos"]),
    ];
    let visible = [0, 1, 2];
    assert_eq!(
        group_by_tag(&projects, &visible),
        vec![
            ("Aldeia".to_owned(), vec![0]),
            ("Estudos".to_owned(), vec![2]),
            ("Work".to_owned(), vec![0]),
            (NO_TAG_LABEL.to_owned(), vec![1]),
        ]
    );
    // Tags without visible projects are hidden.
    assert_eq!(
        group_by_tag(&projects, &[2]),
        vec![("Estudos".to_owned(), vec![2])]
    );
}

#[test]
fn parse_tags_trims_and_drops_blanks_and_repeats() {
    assert_eq!(
        parse_tags(" Aldeia, Estudos,,Aldeia , "),
        ["Aldeia", "Estudos"]
    );
    assert!(parse_tags("  ").is_empty());
}

#[test]
fn sort_order_cycles_through_every_option() {
    assert_eq!(SortOrder::Saved.next(), SortOrder::Name);
    assert_eq!(SortOrder::Name.next(), SortOrder::Path);
    assert_eq!(SortOrder::Path.next(), SortOrder::Saved);
}
