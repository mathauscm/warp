CREATE TABLE claude_chat_panes (
    id INTEGER PRIMARY KEY NOT NULL REFERENCES pane_nodes(id),
    kind TEXT NOT NULL DEFAULT 'claude_chat',
    session_id TEXT,
    cwd TEXT NOT NULL
);
