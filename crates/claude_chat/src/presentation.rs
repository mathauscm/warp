//! Groups timeline items into the rows a chat transcript shows: a finished turn's work folds into
//! one "Trabalhou por 1m 3s" group above the final answer, runs of tool calls collapse into one
//! summary sentence, and every tool call gets a short pt-BR label.

use std::collections::HashSet;
use std::path::Path;
use std::time::SystemTime;

use serde_json::Value;

use crate::timeline::{NoticeKind, TimelineItem, ToolCall, ToolStatus};

/// One row of the transcript. Indices point into the `items` given to [`build_rows`].
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    UserBubble {
        index: usize,
    },
    AssistantProse {
        index: usize,
    },
    Thinking {
        index: usize,
    },
    Tool {
        index: usize,
    },
    ToolBatch(ToolBatch),
    Notice {
        index: usize,
    },
    WorkGroup(WorkGroup),
    /// A turn that failed without a notice saying why, like hitting the turn limit.
    TurnError {
        index: usize,
    },
    /// A question Claude asked with AskUserQuestion, once the user answered it: a record of the
    /// answers, kept out of the work group.
    QuestionAnswers {
        index: usize,
    },
}

/// A turn's work behind one header: folded once the turn is done, open while it runs.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkGroup {
    /// Stable from the live header to the folded group, so the view keeps its expanded state.
    pub id: String,
    /// Every item the group holds, in order.
    pub items: Vec<usize>,
    /// What the group shows when expanded. Never contains another `WorkGroup`.
    pub rows: Vec<Row>,
    pub duration_ms: Option<u64>,
    pub is_live: bool,
    /// "Trabalhou por 1m 3s" or "Trabalhando há 12s".
    pub summary: String,
    pub collapsed_by_default: bool,
}

/// Consecutive tool calls (and the thinking between them) shown as one summary sentence.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolBatch {
    /// The first tool call's id.
    pub id: String,
    pub items: Vec<usize>,
    /// "Leu 4 arquivos e executou 2 comandos".
    pub summary: String,
    /// The kind of most of the calls, for the icon.
    pub kind: ToolKind,
    /// Whether a call in the batch is still running or waiting for approval.
    pub is_live: bool,
}

/// What a tool call does, which picks its icon and how it is counted in summaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolKind {
    Command,
    Read,
    Edit,
    Search,
    WebSearch,
    WebFetch,
    Subagent,
    Todo,
    Question,
    Plan,
    Skill,
    Integration,
    Other,
}

impl ToolKind {
    /// Calls of these kinds get their own row instead of joining a batch.
    fn is_standalone(self) -> bool {
        match self {
            ToolKind::Subagent | ToolKind::Question | ToolKind::Plan => true,
            ToolKind::Command
            | ToolKind::Read
            | ToolKind::Edit
            | ToolKind::Search
            | ToolKind::WebSearch
            | ToolKind::WebFetch
            | ToolKind::Todo
            | ToolKind::Skill
            | ToolKind::Integration
            | ToolKind::Other => false,
        }
    }

    /// Lower is mentioned first when a summary can only name two kinds.
    fn summary_priority(self) -> u8 {
        match self {
            ToolKind::Command => 0,
            ToolKind::Edit => 1,
            ToolKind::Read => 2,
            ToolKind::Search => 3,
            ToolKind::WebSearch => 4,
            ToolKind::WebFetch => 5,
            ToolKind::Subagent => 6,
            ToolKind::Integration => 7,
            ToolKind::Skill => 8,
            ToolKind::Todo => 9,
            ToolKind::Question => 10,
            ToolKind::Plan => 11,
            ToolKind::Other => 12,
        }
    }
}

/// A one-line description of a tool call, like "Leu" + "src/main.rs".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolLabel {
    /// Present tense while the call runs ("Lendo"), past tense once it is over ("Leu").
    pub verb: String,
    /// What the call acts on: a path, command, pattern, URL or `tool (server)`.
    pub target: Option<String>,
    pub kind: ToolKind,
}

impl ToolLabel {
    pub fn text(&self) -> String {
        match &self.target {
            Some(target) => format!("{} {target}", self.verb),
            None => self.verb.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffStats {
    pub added: usize,
    pub removed: usize,
    pub path: String,
}

// ---------------------------------------------------------------------------------------------
// Rows

/// Groups `items` into rows. `is_working` marks the last turn as still running; `now` times its
/// live header.
pub fn build_rows(items: &[TimelineItem], is_working: bool, now: SystemTime) -> Vec<Row> {
    let turns = split_turns(items);
    let mut rows = Vec::new();
    let turn_count = turns.len();
    for (position, turn) in turns.into_iter().enumerate() {
        let is_live = is_working && position + 1 == turn_count && turn.end.is_none();
        turn_rows(items, turn, is_live, now, &mut rows);
    }
    rows
}

/// The rows of a subagent's work, to show under the tool call that started it.
pub fn build_subagent_rows(items: &[TimelineItem], tool_use_id: &str) -> Vec<Row> {
    work_rows(items, &subagent_items(items, tool_use_id))
}

/// The items produced directly by the subagent that `tool_use_id` started.
pub fn subagent_items(items: &[TimelineItem], tool_use_id: &str) -> Vec<usize> {
    items
        .iter()
        .enumerate()
        .filter(|(_, item)| item.parent_tool_use_id() == Some(tool_use_id))
        .map(|(index, _)| index)
        .collect()
}

#[derive(Debug, Default)]
struct Turn {
    user: Option<usize>,
    body: Vec<usize>,
    end: Option<usize>,
}

/// A turn starts at a user message, or right after the previous turn ended when Claude acts on
/// its own (a background task finishing).
fn split_turns(items: &[TimelineItem]) -> Vec<Turn> {
    let mut turns = Vec::new();
    let mut current = Turn::default();
    let is_empty = |turn: &Turn| turn.user.is_none() && turn.body.is_empty() && turn.end.is_none();
    for (index, item) in items.iter().enumerate() {
        if item.parent_tool_use_id().is_some() {
            continue;
        }
        match item {
            TimelineItem::User(message) if is_effort_command(message) => continue,
            TimelineItem::User(_) => {
                if !is_empty(&current) {
                    turns.push(std::mem::take(&mut current));
                }
                current.user = Some(index);
            }
            TimelineItem::TurnEnd(_) => {
                current.end = Some(index);
                turns.push(std::mem::take(&mut current));
            }
            TimelineItem::Assistant(_)
            | TimelineItem::Thinking(_)
            | TimelineItem::Tool(_)
            | TimelineItem::Notice(_) => current.body.push(index),
        }
    }
    if !is_empty(&current) {
        turns.push(current);
    }
    turns
}

/// `/effort`, which the effort picker sends for the user, so it isn't part of the conversation.
fn is_effort_command(message: &crate::timeline::UserMessage) -> bool {
    message
        .command
        .as_ref()
        .is_some_and(|command| command.name == "effort")
}

fn turn_rows(
    items: &[TimelineItem],
    turn: Turn,
    is_live: bool,
    now: SystemTime,
    rows: &mut Vec<Row>,
) {
    if let Some(user) = turn.user {
        rows.push(Row::UserBubble { index: user });
    }
    let anchor = turn
        .user
        .or_else(|| turn.body.first().copied())
        .or(turn.end);
    let group_id = anchor.map_or_else(String::new, |anchor| format!("work:{}", items[anchor].id()));
    let started_at = turn_start(items, &turn);

    if is_live {
        let duration_ms = started_at.and_then(|started| millis_between(started, now));
        rows.push(Row::WorkGroup(WorkGroup {
            id: group_id,
            rows: work_rows(items, &turn.body),
            items: turn.body,
            duration_ms,
            is_live: true,
            summary: match duration_ms {
                Some(ms) => format!("Trabalhando há {}", format_duration_pt(ms)),
                None => "Trabalhando".to_owned(),
            },
            collapsed_by_default: false,
        }));
        return;
    }

    let end = turn.end.and_then(|end| match &items[end] {
        TimelineItem::TurnEnd(summary) => Some(summary),
        _ => None,
    });
    let has_failure_notice = turn.body.iter().any(|&index| {
        matches!(
            &items[index],
            TimelineItem::Notice(notice)
                if matches!(notice.kind, NoticeKind::Interrupted | NoticeKind::Error(_))
        )
    });
    let failed = has_failure_notice || end.is_some_and(|end| end.is_error);
    let answer = turn
        .body
        .iter()
        .rev()
        .copied()
        .find(|&index| matches!(items[index], TimelineItem::Assistant(_)));
    let (pinned, folded): (Vec<usize>, Vec<usize>) = turn
        .body
        .iter()
        .copied()
        .filter(|&index| Some(index) != answer)
        .partition(|&index| is_pinned(&items[index]));
    let (answered, folded): (Vec<usize>, Vec<usize>) = folded
        .into_iter()
        .partition(|&index| is_answered_question(&items[index]));

    if failed || folded.is_empty() {
        rows.extend(work_rows(items, &turn.body));
        if let Some(end_index) = turn.end
            && failed
            && !has_failure_notice
        {
            rows.push(Row::TurnError { index: end_index });
        }
        return;
    }

    let duration_ms = end.and_then(|end| end.duration_ms).or_else(|| {
        let finished = turn
            .body
            .iter()
            .filter_map(|&index| item_time(&items[index]))
            .max()?;
        millis_between(started_at?, finished)
    });
    rows.push(Row::WorkGroup(WorkGroup {
        id: group_id,
        rows: work_rows(items, &folded),
        items: folded,
        duration_ms,
        is_live: false,
        summary: match duration_ms {
            Some(ms) => format!("Trabalhou por {}", format_duration_pt(ms)),
            None => "Trabalhou".to_owned(),
        },
        collapsed_by_default: true,
    }));
    rows.extend(
        answered
            .into_iter()
            .map(|index| Row::QuestionAnswers { index }),
    );
    rows.extend(pinned.into_iter().map(|index| Row::Notice { index }));
    if let Some(answer) = answer {
        rows.push(Row::AssistantProse { index: answer });
    }
    rows.extend(
        turn.body
            .iter()
            .copied()
            .filter(|&index| is_made_artifact(&items[index]))
            .map(|index| Row::Tool { index }),
    );
}

/// An artifact the turn created or changed, which stays in view under the answer like the work's
/// result rather than folding with the rest of it.
fn is_made_artifact(item: &TimelineItem) -> bool {
    match item {
        TimelineItem::Tool(tool) => {
            tool.status == ToolStatus::Done && crate::artifacts::makes_artifact(&tool.name)
        }
        TimelineItem::User(_)
        | TimelineItem::Assistant(_)
        | TimelineItem::Thinking(_)
        | TimelineItem::Notice(_)
        | TimelineItem::TurnEnd(_) => false,
    }
}

fn is_answered_question(item: &TimelineItem) -> bool {
    match item {
        TimelineItem::Tool(tool) => !question_answers(tool).is_empty(),
        TimelineItem::User(_)
        | TimelineItem::Assistant(_)
        | TimelineItem::Thinking(_)
        | TimelineItem::Notice(_)
        | TimelineItem::TurnEnd(_) => false,
    }
}

/// Notices that stay visible when a turn folds.
fn is_pinned(item: &TimelineItem) -> bool {
    match item {
        TimelineItem::Notice(notice) => match notice.kind {
            NoticeKind::Compacted { .. }
            | NoticeKind::CompactSummary(_)
            | NoticeKind::Interrupted
            | NoticeKind::Error(_) => true,
            NoticeKind::Info(_) => false,
        },
        TimelineItem::User(_)
        | TimelineItem::Assistant(_)
        | TimelineItem::Thinking(_)
        | TimelineItem::Tool(_)
        | TimelineItem::TurnEnd(_) => false,
    }
}

fn turn_start(items: &[TimelineItem], turn: &Turn) -> Option<SystemTime> {
    turn.user
        .and_then(|user| item_time(&items[user]))
        .or_else(|| turn.body.iter().find_map(|&index| item_time(&items[index])))
}

fn item_time(item: &TimelineItem) -> Option<SystemTime> {
    match item {
        TimelineItem::User(message) => message.timestamp,
        TimelineItem::Assistant(text) => text.timestamp,
        TimelineItem::Tool(tool) => tool.finished_at.or(tool.started_at),
        TimelineItem::Thinking(_) | TimelineItem::Notice(_) | TimelineItem::TurnEnd(_) => None,
    }
}

fn millis_between(earlier: SystemTime, later: SystemTime) -> Option<u64> {
    u64::try_from(later.duration_since(earlier).ok()?.as_millis()).ok()
}

/// Rows for a stretch of work: runs of two or more tool calls become one batch.
fn work_rows(items: &[TimelineItem], indices: &[usize]) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut run: Vec<usize> = Vec::new();
    for &index in indices {
        match &items[index] {
            TimelineItem::Tool(tool) if !tool_label(tool).kind.is_standalone() => run.push(index),
            TimelineItem::Thinking(_) if !run.is_empty() => run.push(index),
            item => {
                flush_run(items, &mut run, &mut rows);
                rows.push(match item {
                    TimelineItem::User(_) => Row::UserBubble { index },
                    TimelineItem::Assistant(_) => Row::AssistantProse { index },
                    TimelineItem::Thinking(_) => Row::Thinking { index },
                    TimelineItem::Tool(_) if is_answered_question(item) => {
                        Row::QuestionAnswers { index }
                    }
                    TimelineItem::Tool(_) => Row::Tool { index },
                    TimelineItem::Notice(_) => Row::Notice { index },
                    TimelineItem::TurnEnd(_) => Row::TurnError { index },
                });
            }
        }
    }
    flush_run(items, &mut run, &mut rows);
    rows
}

fn flush_run(items: &[TimelineItem], run: &mut Vec<usize>, rows: &mut Vec<Row>) {
    let run = std::mem::take(run);
    // Thinking after the last call belongs to what comes next.
    let last_tool = run
        .iter()
        .rposition(|&index| matches!(items[index], TimelineItem::Tool(_)));
    let (run, trailing) = match last_tool {
        Some(last) => run.split_at(last + 1),
        None => (&run[..0], &run[..]),
    };
    let tools: Vec<&ToolCall> = run
        .iter()
        .filter_map(|&index| match &items[index] {
            TimelineItem::Tool(tool) => Some(tool),
            _ => None,
        })
        .collect();
    if tools.len() >= 2 {
        rows.push(Row::ToolBatch(ToolBatch {
            id: tools[0].id.clone(),
            items: run.to_vec(),
            summary: summarize_tools(&tools),
            kind: dominant_kind(&tools),
            is_live: tools.iter().any(|tool| is_running(tool.status)),
        }));
    } else {
        rows.extend(run.iter().map(|&index| match items[index] {
            TimelineItem::Tool(_) => Row::Tool { index },
            _ => Row::Thinking { index },
        }));
    }
    rows.extend(trailing.iter().map(|&index| Row::Thinking { index }));
}

fn is_running(status: ToolStatus) -> bool {
    match status {
        ToolStatus::Streaming | ToolStatus::AwaitingApproval | ToolStatus::Running => true,
        ToolStatus::Done | ToolStatus::Failed | ToolStatus::Denied | ToolStatus::Interrupted => {
            false
        }
    }
}

fn dominant_kind(tools: &[&ToolCall]) -> ToolKind {
    let kinds: Vec<ToolKind> = tools.iter().map(|tool| tool_label(tool).kind).collect();
    let mut best = kinds[0];
    let mut best_count = 0;
    for &kind in &kinds {
        let count = kinds.iter().filter(|&&other| other == kind).count();
        if count > best_count {
            best = kind;
            best_count = count;
        }
    }
    best
}

// ---------------------------------------------------------------------------------------------
// Summaries

/// A pt-BR sentence for a run of tool calls: "Leu 4 arquivos e executou 2 comandos". Names the two
/// most important kinds in order of appearance and counts the rest as other actions.
pub fn summarize_tools(tools: &[&ToolCall]) -> String {
    struct KindCount {
        kind: ToolKind,
        calls: usize,
        paths: HashSet<String>,
        servers: Vec<String>,
    }
    let mut counts: Vec<KindCount> = Vec::new();
    for tool in tools {
        let kind = tool_label(tool).kind;
        let position = match counts.iter().position(|count| count.kind == kind) {
            Some(position) => position,
            None => {
                counts.push(KindCount {
                    kind,
                    calls: 0,
                    paths: HashSet::new(),
                    servers: Vec::new(),
                });
                counts.len() - 1
            }
        };
        let count = &mut counts[position];
        count.calls += 1;
        if let Some(path) = edited_path(tool) {
            count.paths.insert(path.to_owned());
        }
        if let Some((server, _)) = parse_mcp_name(&tool.name)
            && !count.servers.contains(&server)
        {
            count.servers.push(server);
        }
    }

    let mut by_priority: Vec<usize> = (0..counts.len()).collect();
    by_priority.sort_by_key(|&position| counts[position].kind.summary_priority());
    let mut named: Vec<usize> = by_priority.iter().copied().take(2).collect();
    named.sort_unstable();
    let other_calls: usize = by_priority
        .iter()
        .skip(2)
        .map(|&position| counts[position].calls)
        .sum();

    let mut phrases: Vec<String> = named
        .iter()
        .map(|&position| {
            let count = &counts[position];
            let files = if count.paths.is_empty() {
                count.calls
            } else {
                count.paths.len()
            };
            kind_phrase(count.kind, count.calls, files, &count.servers)
        })
        .collect();
    if other_calls > 0 {
        phrases.push(format!(
            "realizou {other_calls} {}",
            plural(other_calls, "outra ação", "outras ações")
        ));
    }
    capitalize(&join_pt(&phrases))
}

fn kind_phrase(kind: ToolKind, calls: usize, files: usize, servers: &[String]) -> String {
    match kind {
        ToolKind::Read => format!("leu {files} {}", plural(files, "arquivo", "arquivos")),
        ToolKind::Edit => format!("editou {files} {}", plural(files, "arquivo", "arquivos")),
        ToolKind::Command => format!("executou {calls} {}", plural(calls, "comando", "comandos")),
        ToolKind::Search => format!("pesquisou {calls} {}", plural(calls, "vez", "vezes")),
        ToolKind::WebSearch => {
            format!("buscou na web {calls} {}", plural(calls, "vez", "vezes"))
        }
        ToolKind::WebFetch => format!("acessou {calls} {}", plural(calls, "página", "páginas")),
        ToolKind::Subagent => format!("executou {calls} {}", plural(calls, "agente", "agentes")),
        ToolKind::Todo => "atualizou as tarefas".to_owned(),
        ToolKind::Question => format!("fez {calls} {}", plural(calls, "pergunta", "perguntas")),
        ToolKind::Plan => "propôs um plano".to_owned(),
        ToolKind::Skill => format!("usou {calls} {}", plural(calls, "skill", "skills")),
        ToolKind::Integration => format!("usou {}", join_pt(servers)),
        ToolKind::Other => format!(
            "usou {calls} {}",
            plural(calls, "ferramenta", "ferramentas")
        ),
    }
}

fn plural<'a>(count: usize, singular: &'a str, plural: &'a str) -> &'a str {
    if count == 1 { singular } else { plural }
}

/// "a", "a e b", "a, b e c".
fn join_pt(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [only] => only.clone(),
        [init @ .., last] => format!("{} e {last}", init.join(", ")),
    }
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// "12s", "1m 3s", "1h 2m".
pub fn format_duration_pt(ms: u64) -> String {
    // Rounded to whole seconds, but work that took any time never shows as "0s".
    let seconds = (ms.saturating_add(500) / 1000).max(u64::from(ms > 0));
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => {
            let (minutes, seconds) = (seconds / 60, seconds % 60);
            if seconds == 0 {
                format!("{minutes}m")
            } else {
                format!("{minutes}m {seconds}s")
            }
        }
        _ => {
            let (hours, minutes) = (seconds / 3600, seconds % 3600 / 60);
            if minutes == 0 {
                format!("{hours}h")
            } else {
                format!("{hours}h {minutes}m")
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Labels

pub fn tool_label(tool: &ToolCall) -> ToolLabel {
    tool_label_in(tool, None)
}

/// Like [`tool_label`], showing paths under `cwd` relative to it.
pub fn tool_label_in(tool: &ToolCall, cwd: Option<&Path>) -> ToolLabel {
    let input = &tool.input;
    let text = |key: &str| input.get(key).and_then(Value::as_str).map(str::to_owned);
    let path = |key: &str| {
        input
            .get(key)
            .and_then(Value::as_str)
            .map(|path| display_path(path, cwd))
    };
    let in_path = |target: Option<String>| match (target, path("path")) {
        (Some(target), Some(path)) => Some(format!("{target} em {path}")),
        (target, _) => target,
    };

    let (present, past, target, kind) = match tool.name.as_str() {
        "Bash" => (
            "Executando",
            "Executou",
            text("command").map(|command| first_line(&command)),
            ToolKind::Command,
        ),
        "BashOutput" => (
            "Lendo saída",
            "Leu saída",
            text("bash_id"),
            ToolKind::Command,
        ),
        "KillShell" | "KillBash" | "TaskStop" => (
            "Encerrando",
            "Encerrou",
            text("shell_id").or_else(|| text("task_id")),
            ToolKind::Command,
        ),
        "Monitor" => (
            "Monitorando",
            "Monitorou",
            text("description").or_else(|| text("command").map(|command| first_line(&command))),
            ToolKind::Command,
        ),
        "Read" => ("Lendo", "Leu", path("file_path"), ToolKind::Read),
        "Write" => ("Criando", "Criou", path("file_path"), ToolKind::Edit),
        "Edit" | "MultiEdit" => ("Editando", "Editou", path("file_path"), ToolKind::Edit),
        "NotebookEdit" => ("Editando", "Editou", path("notebook_path"), ToolKind::Edit),
        "Grep" => (
            "Pesquisando",
            "Pesquisou",
            in_path(text("pattern")),
            ToolKind::Search,
        ),
        "Glob" => (
            "Procurando arquivos",
            "Procurou arquivos",
            in_path(text("pattern")),
            ToolKind::Search,
        ),
        "LS" => ("Listando", "Listou", path("path"), ToolKind::Search),
        "WebFetch" => ("Acessando", "Acessou", text("url"), ToolKind::WebFetch),
        "WebSearch" => (
            "Buscando na web",
            "Buscou na web",
            text("query"),
            ToolKind::WebSearch,
        ),
        "Task" | "Agent" => (
            "Executando agente",
            "Executou agente",
            text("description").or_else(|| text("subagent_type")),
            ToolKind::Subagent,
        ),
        "TodoWrite" => (
            "Atualizando tarefas",
            "Atualizou tarefas",
            None,
            ToolKind::Todo,
        ),
        "TaskCreate" => (
            "Criando tarefa",
            "Criou tarefa",
            text("subject"),
            ToolKind::Todo,
        ),
        "TaskUpdate" => (
            "Atualizando tarefa",
            "Atualizou tarefa",
            input.get("taskId").and_then(|id| match id {
                Value::String(id) => Some(format!("#{id}")),
                Value::Number(id) => Some(format!("#{id}")),
                _ => None,
            }),
            ToolKind::Todo,
        ),
        "TaskList" | "TaskGet" => (
            "Consultando tarefas",
            "Consultou tarefas",
            None,
            ToolKind::Todo,
        ),
        "AskUserQuestion" => (
            "Perguntando",
            "Perguntou",
            input
                .pointer("/questions/0/question")
                .and_then(Value::as_str)
                .map(str::to_owned),
            ToolKind::Question,
        ),
        "ExitPlanMode" => ("Propondo plano", "Propôs plano", None, ToolKind::Plan),
        "EnterPlanMode" => (
            "Entrando no modo de plano",
            "Entrou no modo de plano",
            None,
            ToolKind::Plan,
        ),
        "Skill" => (
            "Usando skill",
            "Usou skill",
            text("skill").or_else(|| text("command")),
            ToolKind::Skill,
        ),
        "SlashCommand" => (
            "Executando comando",
            "Executou comando",
            text("command"),
            ToolKind::Skill,
        ),
        "ToolSearch" => (
            "Carregando ferramentas",
            "Carregou ferramentas",
            text("query"),
            ToolKind::Other,
        ),
        name => match parse_mcp_name(name) {
            Some((server, tool_name)) => (
                "Usando",
                "Usou",
                Some(format!("{tool_name} ({server})")),
                ToolKind::Integration,
            ),
            None => ("Usando", "Usou", Some(name.to_owned()), ToolKind::Other),
        },
    };
    ToolLabel {
        verb: if is_running(tool.status) {
            present
        } else {
            past
        }
        .to_owned(),
        target: target.filter(|target| !target.is_empty()),
        kind,
    }
}

/// `mcp__claude_ai_Gmail__search_threads` → ("Gmail", "search threads").
fn parse_mcp_name(name: &str) -> Option<(String, String)> {
    let rest = name.strip_prefix("mcp__")?;
    let (server, tool) = rest.split_once("__")?;
    let server = server
        .strip_prefix("claude_ai_")
        .or_else(|| {
            // Plugin servers are named `plugin_<plugin>_<server>`.
            let plugin = server.strip_prefix("plugin_")?;
            plugin.split_once('_').map(|(_, server)| server)
        })
        .unwrap_or(server);
    let readable = |text: &str| text.replace(['_', '-'], " ");
    Some((readable(server), readable(tool)))
}

fn display_path(path: &str, cwd: Option<&Path>) -> String {
    let Some(cwd) = cwd else {
        return path.to_owned();
    };
    match Path::new(path).strip_prefix(cwd) {
        Ok(relative) if !relative.as_os_str().is_empty() => relative.to_string_lossy().into_owned(),
        _ => path.to_owned(),
    }
}

fn first_line(text: &str) -> String {
    let mut lines = text.trim().lines();
    let first = lines.next().unwrap_or_default().trim_end();
    if lines.next().is_some() {
        format!("{first} …")
    } else {
        first.to_owned()
    }
}

fn edited_path(tool: &ToolCall) -> Option<&str> {
    match tool.name.as_str() {
        "Edit" | "MultiEdit" | "Write" => tool.input.get("file_path")?.as_str(),
        "NotebookEdit" => tool.input.get("notebook_path")?.as_str(),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------
// Question answers

/// What the user answered to one of the questions of an AskUserQuestion call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionAnswer {
    /// The question's short label, like "Stack", when it has one.
    pub header: Option<String>,
    pub question: String,
    /// The picked options' labels joined by ", ", or the user's own text.
    pub answer: String,
}

/// The answers to an AskUserQuestion call, in the order of its questions: from its structured
/// result, else from the text Claude received. Empty while it waits, and when it was dismissed or
/// failed.
pub fn question_answers(tool: &ToolCall) -> Vec<QuestionAnswer> {
    if tool.name != crate::reducer::ASK_USER_QUESTION_TOOL || tool.status != ToolStatus::Done {
        return Vec::new();
    }
    let Some(result) = tool.result.as_ref().filter(|result| !result.is_error) else {
        return Vec::new();
    };
    let mut answers: Vec<(String, String)> = match result
        .structured
        .as_ref()
        .and_then(|structured| structured.get("answers"))
        .and_then(Value::as_object)
    {
        Some(answers) => answers
            .iter()
            .filter_map(|(question, answer)| Some((question.clone(), answer_text(answer)?)))
            .collect(),
        None => parse_answers_text(&result.text),
    };
    let questions = tool
        .input
        .get("questions")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut ordered = Vec::new();
    for question in questions {
        let Some(text) = question.get("question").and_then(Value::as_str) else {
            continue;
        };
        let Some(position) = answers.iter().position(|(asked, _)| asked == text) else {
            continue;
        };
        let (question_text, answer) = answers.remove(position);
        ordered.push(QuestionAnswer {
            header: question
                .get("header")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|header| !header.is_empty())
                .map(str::to_owned),
            question: question_text,
            answer,
        });
    }
    ordered.extend(
        answers
            .into_iter()
            .map(|(question, answer)| QuestionAnswer {
                header: None,
                question,
                answer,
            }),
    );
    ordered.retain(|answer| !answer.answer.trim().is_empty());
    ordered
}

/// An answer in a structured result: a string, or a list of picked labels.
fn answer_text(answer: &Value) -> Option<String> {
    match answer {
        Value::String(text) => Some(text.clone()),
        Value::Array(labels) => Some(
            labels
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", "),
        ),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_) => None,
    }
}

/// The `"question"="answer"` pairs of the text Claude receives, like `Your questions have been
/// answered: "Q1"="A1", "Q2"="A2". You can now continue with these answers in mind.`
fn parse_answers_text(text: &str) -> Vec<(String, String)> {
    let Some(start) = text.find(": \"") else {
        return Vec::new();
    };
    let mut rest = &text[start + 2..];
    let mut pairs = Vec::new();
    while let Some(body) = rest.strip_prefix('"') {
        let Some(separator) = body.find("\"=\"") else {
            break;
        };
        let question = &body[..separator];
        let after = &body[separator + 3..];
        // Answers can hold quotes, so the closing one is the quote another pair or the end of the
        // list follows.
        let Some(end) = after.match_indices('"').map(|(at, _)| at).find(|&at| {
            let tail = &after[at + 1..];
            tail.starts_with(", \"") || tail.starts_with('.') || tail.trim().is_empty()
        }) else {
            break;
        };
        pairs.push((question.to_owned(), after[..end].to_owned()));
        rest = after[end + 1..].strip_prefix(", ").unwrap_or_default();
    }
    pairs
}

// ---------------------------------------------------------------------------------------------
// Diff stats

/// Lines added and removed by an Edit, MultiEdit or Write call: from the structured patch in its
/// result when there is one, else from its input.
pub fn edit_stats(tool: &ToolCall) -> Option<DiffStats> {
    let path = match tool.name.as_str() {
        "Edit" | "MultiEdit" | "Write" => tool.input.get("file_path")?.as_str()?.to_owned(),
        _ => return None,
    };
    let structured = tool
        .result
        .as_ref()
        .and_then(|result| result.structured.as_ref());
    if let Some((added, removed)) = structured.and_then(patch_stats) {
        return Some(DiffStats {
            added,
            removed,
            path,
        });
    }
    let input_text = |value: &Value, key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let (added, removed) = match tool.name.as_str() {
        "Edit" => line_diff(
            &input_text(&tool.input, "old_string"),
            &input_text(&tool.input, "new_string"),
        ),
        "MultiEdit" => tool
            .input
            .get("edits")
            .and_then(Value::as_array)
            .map(|edits| {
                edits.iter().fold((0, 0), |(added, removed), edit| {
                    let (more_added, more_removed) = line_diff(
                        &input_text(edit, "old_string"),
                        &input_text(edit, "new_string"),
                    );
                    (added + more_added, removed + more_removed)
                })
            })
            .unwrap_or_default(),
        _ => {
            let content = input_text(&tool.input, "content");
            match structured
                .and_then(|structured| structured.get("originalFile"))
                .and_then(Value::as_str)
            {
                Some(original) => line_diff(original, &content),
                None => (count_lines(&content), 0),
            }
        }
    };
    Some(DiffStats {
        added,
        removed,
        path,
    })
}

fn patch_stats(structured: &Value) -> Option<(usize, usize)> {
    let hunks = structured.get("structuredPatch")?.as_array()?;
    if hunks.is_empty() {
        return None;
    }
    let mut added = 0;
    let mut removed = 0;
    for line in hunks
        .iter()
        .filter_map(|hunk| hunk.get("lines")?.as_array())
        .flatten()
        .filter_map(Value::as_str)
    {
        if line.starts_with('+') {
            added += 1;
        } else if line.starts_with('-') {
            removed += 1;
        }
    }
    Some((added, removed))
}

fn count_lines(text: &str) -> usize {
    text.lines().count()
}

/// Above this many line comparisons, changed lines are counted without matching moved ones.
const MAX_DIFF_CELLS: usize = 4_000_000;

/// (added, removed) lines between two texts, from their longest common subsequence.
fn line_diff(old: &str, new: &str) -> (usize, usize) {
    let old: Vec<&str> = old.lines().collect();
    let new: Vec<&str> = new.lines().collect();
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let old = &old[prefix..old.len() - suffix];
    let new = &new[prefix..new.len() - suffix];
    if old.len().saturating_mul(new.len()) > MAX_DIFF_CELLS {
        return (new.len(), old.len());
    }
    let mut row = vec![0usize; new.len() + 1];
    for old_line in old {
        let mut diagonal = 0;
        for (column, new_line) in new.iter().enumerate() {
            let above = row[column + 1];
            row[column + 1] = if old_line == new_line {
                diagonal + 1
            } else {
                above.max(row[column])
            };
            diagonal = above;
        }
    }
    let common = row[new.len()];
    (new.len() - common, old.len() - common)
}

#[cfg(test)]
#[path = "presentation_tests.rs"]
mod tests;
