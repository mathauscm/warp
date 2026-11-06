use std::path::{Path, PathBuf};

use super::*;

fn project() -> PathBuf {
    PathBuf::from("/work/kinbox-web")
}

#[test]
fn reads_claude_code_preview_configurations() {
    let text = r#"{
        "version": "0.0.1",
        "configurations": [
            {
                "name": "kinbox-web",
                "runtimeExecutable": "npm",
                "runtimeArgs": ["run", "dev"],
                "port": 5174,
                "env": { "NODE_ENV": "development", "DEBUG": true }
            }
        ]
    }"#;
    let servers = parse_launch_file(text, &project()).unwrap();
    assert_eq!(servers.len(), 1);
    let server = &servers[0];
    assert_eq!(server.name, "kinbox-web");
    assert_eq!(server.program, "npm");
    assert_eq!(server.args, ["run", "dev"]);
    assert!(!server.shell);
    assert_eq!(server.port, Some(5174));
    assert_eq!(server.cwd, project());
    let mut env = server.env.clone();
    env.sort();
    assert_eq!(
        env,
        [
            ("DEBUG".to_owned(), "true".to_owned()),
            ("NODE_ENV".to_owned(), "development".to_owned())
        ]
    );
    assert_eq!(server.page_url().as_deref(), Some("http://localhost:5174"));
    assert_eq!(server.command_line(), "npm run dev");
}

#[test]
fn tolerates_comments_trailing_commas_and_other_spellings() {
    let text = r#"{
        // Dev servers
        "configurations": [
            /* the API */
            { "name": "api", "command": "python3 -m http.server 5199", "cwd": "backend", },
            { "command": "pnpm", "args": "dev --port 4000", },
            { "name": "no command" },
        ],
    }"#;
    let servers = parse_launch_file(text, &project()).unwrap();
    assert_eq!(servers.len(), 2);

    assert_eq!(servers[0].name, "api");
    assert!(servers[0].shell);
    assert_eq!(servers[0].program, "python3 -m http.server 5199");
    assert_eq!(servers[0].cwd, project().join("backend"));
    // A port only in the command line isn't taken for `http.server`'s positional port.
    assert_eq!(servers[0].port, None);

    assert_eq!(servers[1].name, "Servidor 2");
    assert_eq!(servers[1].program, "pnpm");
    assert_eq!(servers[1].args, ["dev", "--port", "4000"]);
    assert_eq!(servers[1].port, Some(4000));
}

#[test]
fn takes_the_port_from_the_url_and_expands_the_workspace_folder() {
    let text = r#"{ "configurations": [{
        "name": "docs",
        "runtimeExecutable": "node",
        "program": "${workspaceFolder}/server.js",
        "url": "http://localhost:8080/docs",
        "cwd": "/elsewhere"
    }]}"#;
    let servers = parse_launch_file(text, &project()).unwrap();
    let server = &servers[0];
    assert_eq!(server.args, ["/work/kinbox-web/server.js"]);
    assert_eq!(server.port, Some(8080));
    assert_eq!(server.cwd, Path::new("/elsewhere"));
    assert_eq!(
        server.page_url().as_deref(),
        Some("http://localhost:8080/docs")
    );
}

#[test]
fn keeps_strings_with_comment_like_text_intact() {
    let text = r#"{ "configurations": [{
        "name": "a // b, }",
        "command": "echo http://x/*y*/",
    }]}"#;
    let servers = parse_launch_file(text, &project()).unwrap();
    assert_eq!(servers[0].name, "a // b, }");
    assert_eq!(servers[0].program, "echo http://x/*y*/");
}

#[test]
fn rejects_invalid_files() {
    assert!(parse_launch_file("{ not json", &project()).is_err());
    assert!(parse_launch_file(r#"{ "configurations": 3 }"#, &project()).is_err());
    assert!(parse_launch_file("{}", &project()).unwrap().is_empty());
}

#[test]
fn finds_ports_in_arguments() {
    let args = |text: &str| {
        text.split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    assert_eq!(port_in_args(&args("vite --port 5174")), Some(5174));
    assert_eq!(port_in_args(&args("next dev -p 3001")), Some(3001));
    assert_eq!(port_in_args(&args("serve --port=8081")), Some(8081));
    assert_eq!(port_in_args(&args("PORT=4100 node app.js")), Some(4100));
    assert_eq!(port_in_args(&args("vite")), None);
}

#[test]
fn guesses_the_dev_script_of_a_vite_app() {
    let package_json = r#"{
        "name": "kinbox-web",
        "scripts": { "dev": "vite", "build": "vite build" },
        "devDependencies": { "vite": "^5" }
    }"#;
    let guess = guess_from_package_json(package_json, "npm", "folder").unwrap();
    assert_eq!(
        guess,
        GuessedServer {
            name: "kinbox-web".to_owned(),
            executable: "npm".to_owned(),
            args: vec!["run".to_owned(), "dev".to_owned()],
            port: Some(5173),
        }
    );
}

#[test]
fn guesses_start_scripts_and_explicit_ports() {
    let package_json = r#"{
        "scripts": { "start": "next start -p 3005" }
    }"#;
    let guess = guess_from_package_json(package_json, "yarn", "site").unwrap();
    assert_eq!(guess.name, "site");
    assert_eq!(guess.executable, "yarn");
    assert_eq!(guess.args, ["start"]);
    assert_eq!(guess.port, Some(3005));

    let npm_start = guess_from_package_json(package_json, "npm", "site").unwrap();
    assert_eq!(npm_start.args, ["start"]);

    let unknown = r#"{ "scripts": { "dev": "node server.js" } }"#;
    assert_eq!(
        guess_from_package_json(unknown, "npm", "x").unwrap().port,
        None
    );
    assert!(guess_from_package_json(r#"{ "scripts": { "test": "jest" } }"#, "npm", "x").is_none());
}

#[test]
fn the_template_reads_back_as_the_guessed_server() {
    let guess = GuessedServer {
        name: "kinbox-web".to_owned(),
        executable: "pnpm".to_owned(),
        args: vec!["dev".to_owned()],
        port: Some(5174),
    };
    let servers = parse_launch_file(&launch_file_text(&guess), &project()).unwrap();
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].name, "kinbox-web");
    assert_eq!(servers[0].command_line(), "pnpm dev");
    assert_eq!(servers[0].port, Some(5174));
}

#[test]
fn creating_a_launch_file_never_overwrites_one() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("package.json"),
        r#"{ "name": "app", "scripts": { "dev": "vite" } }"#,
    )
    .unwrap();
    std::fs::write(dir.path().join("pnpm-lock.yaml"), "").unwrap();

    let path = create_launch_file(dir.path()).unwrap();
    assert_eq!(path, dir.path().join(LAUNCH_FILE));
    match read_launch_file(dir.path()) {
        LaunchFile::Servers(servers) => {
            assert_eq!(servers[0].command_line(), "pnpm dev");
            assert_eq!(servers[0].port, Some(5173));
        }
        other => panic!("unexpected {other:?}"),
    }

    std::fs::write(&path, "{}").unwrap();
    create_launch_file(dir.path()).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}");
}

#[test]
fn a_missing_launch_file_reads_as_missing() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(read_launch_file(dir.path()), LaunchFile::Missing);
}
