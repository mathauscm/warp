//! The "Navegador" pane: browser tabs over native web views (see [`WebViewSlot`]), a start page
//! listing the project's dev servers from `.claude/launch.json`, and the log of a running server.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use async_channel::Sender;
use pathfinder_color::ColorU;
use picolas_webview::{NavigationState, WebViewEvent};
use warpui::clipboard::ClipboardContent;
use warpui::elements::{
    Align, Border, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, Element, Expanded,
    Fill, Flex, Hoverable, Icon as SvgIcon, MainAxisSize, MouseStateHandle, ParentElement, Radius,
    Rect, Shrinkable, Text,
};
use warpui::fonts::{Properties, Weight};
use warpui::platform::Cursor;
use warpui::text_layout::ClipConfig;
use warpui::ui_components::components::{UiComponent, UiComponentStyles};
use warpui::ui_components::text_input::TextInput;
use warpui::{
    AppContext, Entity, FocusContext, ModelHandle, SingletonEntity, TypedActionView, View,
    ViewContext, ViewHandle,
};

use super::BrowserContext;
use super::launch::{
    DevServerConfig, LAUNCH_FILE, LaunchFile, create_launch_file, launch_file_path,
    read_launch_file,
};
use super::server::{ServerEvent, ServerProcess, port_answers};
use super::tabs::active_after_close;
use super::url::{display_url, normalize_input};
use crate::appearance::Appearance;
use crate::editor::{EditorView, Event as EditorEvent, SingleLineEditorOptions, TextOptions};
use crate::native_chat::artifacts::web_view_slot::{WebViewHost, WebViewSlot};
use crate::native_chat::design::{self, ChatDesign, ChatDesignEvent, Palette, Sizes};
use crate::pane_group::focus_state::PaneFocusHandle;
use crate::pane_group::pane::view::{self, HeaderContent, StandardHeader, StandardHeaderOptions};
use crate::pane_group::{BackingView, PaneConfiguration, PaneEvent};
use crate::terminal::local_shell::LocalShellState;
use crate::workspace::WorkspaceAction;

const TITLE: &str = "Navegador";
const NEW_TAB_TITLE: &str = "Nova aba";
const HINT: &str = "Abra um servidor para pré-visualizar seu app ou insira uma URL.";
const ADDRESS_PLACEHOLDER: &str = "Insira uma URL ou localhost:porta";
/// Lines of server output kept in memory; the log file has them all.
const MAX_LOG_LINES: usize = 5000;
const LOG_PANEL_HEIGHT: f32 = 210.;
const TAB_STRIP_HEIGHT: f32 = 32.;
const TAB_HEIGHT: f32 = 24.;
const TAB_MAX_WIDTH: f32 = 190.;
const TAB_CLOSE_SIZE: f32 = 18.;
const TAB_CLOSE_ICON_SIZE: f32 = 11.;
const TOOLBAR_HEIGHT: f32 = 36.;
const ADDRESS_HEIGHT: f32 = 26.;
/// Side padding of the tab strip and toolbar.
const CHROME_PADDING: f32 = 6.;
const CHROME_RADIUS: f32 = 6.;
const CHROME_BUTTON_SIZE: f32 = 24.;
const CHROME_ICON_SIZE: f32 = 14.;
const START_PAGE_MAX_WIDTH: f32 = 440.;
const BRANCH_CHIP_MAX_WIDTH: f32 = 160.;
/// Redraws after a web view appears or the tab changes; see [`BrowserView::settle_frames`].
const SETTLE_FRAMES: u8 = 4;

const ICON_BACK: &str = "bundled/svg/picolas-arrow-left.svg";
const ICON_FORWARD: &str = "bundled/svg/picolas-arrow-right.svg";
const ICON_RELOAD: &str = "bundled/svg/picolas-reload.svg";
const ICON_CLOSE: &str = "bundled/svg/picolas-close.svg";
const ICON_PLUS: &str = "bundled/svg/picolas-plus.svg";
const ICON_GLOBE: &str = "bundled/svg/picolas-globe.svg";
const ICON_PLAY: &str = "bundled/svg/picolas-play.svg";
const ICON_STOP: &str = "bundled/svg/picolas-stop-square.svg";
const ICON_BRANCH: &str = "bundled/svg/picolas-branch.svg";
const ICON_TERMINAL: &str = "bundled/svg/picolas-terminal.svg";
const ICON_COPY: &str = "bundled/svg/picolas-copy.svg";
const ICON_EXTERNAL: &str = "bundled/svg/picolas-external.svg";
const ICON_FOLDER: &str = "bundled/svg/picolas-folder.svg";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserViewEvent {
    Pane(PaneEvent),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserViewAction {
    NewTab,
    SelectTab(usize),
    CloseTab(usize),
    Back,
    Forward,
    Reload,
    StopLoading,
    OpenInSystemBrowser,
    StartServer(String),
    StopServer(String),
    ShowLog(String),
    HideLog,
    CopyLog,
    CreateLaunchFile(PathBuf),
    EditLaunchFile(PathBuf),
    ClosePane,
}

struct Tab {
    id: usize,
    host: Rc<WebViewHost>,
    slot_id: String,
    /// A URL to load once the tab's web view exists.
    pending_url: Option<String>,
    state: NavigationState,
    /// The last load failure: the page and why.
    error: Option<(Option<String>, String)>,
}

impl Tab {
    /// Whether the tab shows the start page.
    fn is_blank(&self) -> bool {
        !self.host.has_web_view() && self.pending_url.is_none()
    }

    fn url(&self) -> Option<&str> {
        self.pending_url
            .as_deref()
            .or(self.state.url.as_deref())
            .filter(|url| *url != "about:blank")
    }

    fn title(&self) -> String {
        if let Some(title) = &self.state.title {
            return title.clone();
        }
        self.url()
            .map_or_else(|| NEW_TAB_TITLE.to_owned(), display_url)
    }
}

/// A folder whose launch file the start page lists.
struct Project {
    dir: PathBuf,
    launch: LaunchFile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ServerStatus {
    Starting,
    Ready,
    /// The port already answered, so no process was started.
    External,
    Exited(String),
}

struct ServerRun {
    config: DevServerConfig,
    process: Option<ServerProcess>,
    status: ServerStatus,
    lines: VecDeque<String>,
    log_path: Option<PathBuf>,
    /// The tab that opens the server's page once it answers.
    tab_id: usize,
}

impl ServerRun {
    fn is_running(&self) -> bool {
        matches!(self.status, ServerStatus::Starting | ServerStatus::Ready)
    }

    fn push_line(&mut self, line: String) {
        if self.lines.len() == MAX_LOG_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }
}

pub struct BrowserView {
    context: BrowserContext,
    tabs: Vec<Tab>,
    active: usize,
    next_tab_id: usize,
    address: ViewHandle<EditorView>,
    projects: Vec<Project>,
    runs: HashMap<String, ServerRun>,
    /// The server whose log shows at the bottom.
    log_for: Option<String>,
    branches: HashMap<PathBuf, String>,
    /// The login shell's `PATH` for dev servers, once read.
    path_env: Option<Option<String>>,
    /// Servers to start once `path_env` is read.
    queued_starts: Vec<String>,
    web_view_events: Sender<(usize, WebViewEvent)>,
    /// Wakes the view after each drawn frame; see [`Self::watch_next_frame`].
    frame_tx: Sender<()>,
    is_watching_frame: bool,
    /// Frames still to redraw so a new or newly selected web view's placeholder reaches the
    /// position cache it's shown by.
    settle_frames: u8,
    mouse_states: RefCell<HashMap<String, MouseStateHandle>>,
    pane_configuration: ModelHandle<PaneConfiguration>,
    focus_handle: Option<PaneFocusHandle>,
}

impl BrowserView {
    pub fn new(context: BrowserContext, url: Option<String>, ctx: &mut ViewContext<Self>) -> Self {
        let pane_configuration = ctx.add_model(|_| PaneConfiguration::new(TITLE));
        ctx.subscribe_to_model(&ChatDesign::handle(ctx), |_, _, event, ctx| match event {
            ChatDesignEvent::Changed => ctx.notify(),
        });

        let address = ctx.add_typed_action_view(|ctx| {
            let font_size = ChatDesign::as_ref(ctx).sizes().small_font_size;
            let appearance = Appearance::as_ref(ctx);
            let options = SingleLineEditorOptions {
                text: TextOptions::ui_text(Some(font_size), appearance),
                ..Default::default()
            };
            let mut editor = EditorView::single_line(options, ctx);
            editor.set_placeholder_text(ADDRESS_PLACEHOLDER, ctx);
            editor
        });
        ctx.subscribe_to_view(&address, |me, editor, event, ctx| match event {
            EditorEvent::Enter => {
                let text = editor.as_ref(ctx).buffer_text(ctx);
                if let Some(url) = normalize_input(&text) {
                    me.navigate(url, ctx);
                }
            }
            EditorEvent::Escape => {
                me.sync_address(true, ctx);
                me.focus_page(ctx);
            }
            EditorEvent::Focused => {
                // The page may hold the window's first responder, which would keep the keys.
                if let Some(tab) = me.tabs.get(me.active) {
                    tab.host.with(|web_view| web_view.release_focus());
                }
            }
            _ => {}
        });

        let (web_view_events, events_rx) = async_channel::unbounded();
        ctx.spawn_stream_local(
            events_rx,
            |me, (tab_id, event), ctx| me.on_web_view_event(tab_id, event, ctx),
            |_, _| {},
        );
        let (frame_tx, frame_rx) = async_channel::unbounded();
        ctx.spawn_stream_local(frame_rx, |me, (), ctx| me.on_frame_drawn(ctx), |_, _| {});

        let path_future = LocalShellState::handle(ctx)
            .update(ctx, |shell, ctx| shell.get_interactive_path_env_var(ctx));
        ctx.spawn(path_future, |me, path_env, ctx| {
            me.path_env = Some(path_env.or_else(|| std::env::var("PATH").ok()));
            for key in std::mem::take(&mut me.queued_starts) {
                me.start_server(&key, ctx);
            }
        });

        let mut view = Self {
            context,
            tabs: Vec::new(),
            active: 0,
            next_tab_id: 0,
            address,
            projects: Vec::new(),
            runs: HashMap::new(),
            log_for: None,
            branches: HashMap::new(),
            path_env: None,
            queued_starts: Vec::new(),
            web_view_events,
            frame_tx,
            is_watching_frame: false,
            settle_frames: 0,
            mouse_states: Default::default(),
            pane_configuration,
            focus_handle: None,
        };
        view.add_tab(ctx);
        view.reload_projects(ctx);
        if let Some(url) = url {
            view.navigate(url, ctx);
        }
        view.start_dev_server_from_env(ctx);
        view
    }

    pub fn pane_configuration(&self) -> ModelHandle<PaneConfiguration> {
        self.pane_configuration.clone()
    }

    pub fn focus(&mut self, ctx: &mut ViewContext<Self>) {
        if self.tabs.get(self.active).is_some_and(Tab::is_blank) {
            ctx.focus(&self.address);
        } else {
            ctx.focus_self();
        }
    }

    /// Opens `url` in the current tab when it's the start page, else in a new tab.
    pub fn open_url(&mut self, url: String, ctx: &mut ViewContext<Self>) {
        if !self.tabs.get(self.active).is_some_and(Tab::is_blank) {
            self.add_tab(ctx);
        }
        self.navigate(url, ctx);
    }

    /// Adds the folders of `context` the start page doesn't list yet.
    pub fn extend_context(&mut self, context: BrowserContext, ctx: &mut ViewContext<Self>) {
        let mut changed = false;
        for dir in context.dirs {
            if !self.context.dirs.contains(&dir) {
                self.context.dirs.push(dir);
                changed = true;
            }
        }
        if changed {
            self.reload_projects(ctx);
        }
    }

    fn mouse_state(&self, key: impl Into<String>) -> MouseStateHandle {
        self.mouse_states
            .borrow_mut()
            .entry(key.into())
            .or_default()
            .clone()
    }

    // --- Tabs and navigation -----------------------------------------------------------------

    fn add_tab(&mut self, ctx: &mut ViewContext<Self>) {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        self.tabs.push(Tab {
            id,
            host: Default::default(),
            slot_id: format!("browser-web-view:{}:{id}", ctx.view_id()),
            pending_url: None,
            state: NavigationState::default(),
            error: None,
        });
        self.select_tab_index(self.tabs.len() - 1, ctx);
    }

    fn select_tab_index(&mut self, index: usize, ctx: &mut ViewContext<Self>) {
        self.active = index.min(self.tabs.len().saturating_sub(1));
        self.settle_frames = SETTLE_FRAMES;
        self.sync_address(true, ctx);
        self.update_title(ctx);
        self.watch_next_frame(ctx);
        ctx.notify();
    }

    fn close_tab(&mut self, id: usize, ctx: &mut ViewContext<Self>) {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        let next = active_after_close(self.tabs.len(), self.active, index);
        let tab = self.tabs.remove(index);
        tab.host.with(|web_view| web_view.release_focus());
        tab.host.set(None);
        match next {
            Some(active) => self.select_tab_index(active, ctx),
            None => self.close(ctx),
        }
    }

    fn navigate(&mut self, url: String, ctx: &mut ViewContext<Self>) {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        tab.pending_url = Some(url);
        tab.error = None;
        self.settle_frames = SETTLE_FRAMES;
        self.ensure_web_views(ctx);
        self.sync_address(true, ctx);
        self.update_title(ctx);
        self.focus_page(ctx);
        self.watch_next_frame(ctx);
        ctx.notify();
    }

    fn focus_page(&mut self, ctx: &mut ViewContext<Self>) {
        if !self.tabs.get(self.active).is_some_and(Tab::is_blank) {
            ctx.focus_self();
        }
    }

    /// Creates the web view of each tab with a page to load, once the window can host it, and
    /// loads its page.
    fn ensure_web_views(&mut self, ctx: &mut ViewContext<Self>) {
        for index in 0..self.tabs.len() {
            if self.tabs[index].pending_url.is_none() {
                continue;
            }
            if !self.tabs[index].host.has_web_view() {
                let tab_id = self.tabs[index].id;
                let events = self.web_view_events.clone();
                let Some(web_view) = create_web_view(ctx, move |event| {
                    let _ = events.try_send((tab_id, event));
                }) else {
                    return;
                };
                self.tabs[index].host.set(Some(web_view));
            }
            let tab = &mut self.tabs[index];
            if let Some(url) = tab.pending_url.take() {
                let loaded = tab.host.with(|web_view| web_view.load_url(&url));
                if let Some(Err(err)) = loaded {
                    tab.error = Some((Some(url.clone()), format!("{err:#}")));
                }
                // Shown until the web view reports where it is.
                tab.state.url = Some(url);
                tab.state.title = None;
            }
        }
    }

    fn with_active_web_view(&self, f: impl FnOnce(&picolas_webview::WebView)) {
        if let Some(tab) = self.tabs.get(self.active) {
            tab.host.with(f);
        }
    }

    fn on_web_view_event(
        &mut self,
        tab_id: usize,
        event: WebViewEvent,
        ctx: &mut ViewContext<Self>,
    ) {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == tab_id) else {
            return;
        };
        let tab = &mut self.tabs[index];
        match event {
            WebViewEvent::StateChanged => {
                let Some(state) = tab.host.with(|web_view| web_view.navigation_state()) else {
                    return;
                };
                if state.loading && !tab.state.loading {
                    tab.error = None;
                }
                if state == tab.state {
                    return;
                }
                tab.state = state;
            }
            WebViewEvent::LoadFailed { url, message } => {
                log::info!("Browser tab couldn't load {url:?}: {message}");
                tab.error = Some((url, message));
            }
        }
        if index == self.active {
            self.sync_address(false, ctx);
            self.update_title(ctx);
        }
        ctx.notify();
    }

    /// Shows the current tab's URL in the address field, unless it's being typed in.
    fn sync_address(&mut self, force: bool, ctx: &mut ViewContext<Self>) {
        if !force && self.address.is_focused(ctx) {
            return;
        }
        let url = self
            .tabs
            .get(self.active)
            .and_then(Tab::url)
            .unwrap_or_default()
            .to_owned();
        self.address.update(ctx, |editor, ctx| {
            if editor.buffer_text(ctx) != url {
                editor.set_buffer_text_ignoring_undo(&url, ctx);
            }
        });
    }

    fn update_title(&self, ctx: &mut ViewContext<Self>) {
        let title = match self.tabs.get(self.active) {
            Some(tab) if !tab.is_blank() => tab.title(),
            _ => TITLE.to_owned(),
        };
        self.pane_configuration.update(ctx, |configuration, ctx| {
            configuration.set_title(title, ctx)
        });
    }

    /// Hooks the next drawn frame of the window, to show each web view only while its
    /// placeholder is on screen.
    fn watch_next_frame(&mut self, ctx: &mut ViewContext<Self>) {
        if self.is_watching_frame {
            return;
        }
        self.is_watching_frame = true;
        let frame_tx = self.frame_tx.clone();
        ctx.on_next_frame_drawn(move || {
            let _ = frame_tx.try_send(());
        });
    }

    fn on_frame_drawn(&mut self, ctx: &mut ViewContext<Self>) {
        self.is_watching_frame = false;
        let had_pending = self.tabs.iter().any(|tab| tab.pending_url.is_some());
        self.ensure_web_views(ctx);
        if had_pending || self.settle_frames > 0 {
            self.settle_frames = self.settle_frames.saturating_sub(1);
            ctx.notify();
        }
        let window_id = ctx.window_id();
        for tab in &self.tabs {
            let in_scene = ctx
                .element_position_by_id_at_last_frame(window_id, &tab.slot_id)
                .is_some();
            tab.host.set_in_scene(in_scene);
        }
        if self
            .tabs
            .iter()
            .any(|tab| tab.host.has_web_view() || tab.pending_url.is_some())
        {
            self.watch_next_frame(ctx);
        }
    }

    // --- Dev servers -------------------------------------------------------------------------

    fn reload_projects(&mut self, ctx: &mut ViewContext<Self>) {
        self.projects = self
            .context
            .dirs
            .iter()
            .map(|dir| Project {
                dir: dir.clone(),
                launch: read_launch_file(dir),
            })
            .collect();
        for dir in self.context.dirs.clone() {
            if self.branches.contains_key(&dir) {
                continue;
            }
            let dir_for_task = dir.clone();
            ctx.spawn(
                async move { git_branch(&dir_for_task).await },
                move |me, branch, ctx| {
                    if let Some(branch) = branch {
                        me.branches.insert(dir, branch);
                        ctx.notify();
                    }
                },
            );
        }
        ctx.notify();
    }

    fn server_config(&self, key: &str) -> Option<DevServerConfig> {
        self.projects
            .iter()
            .find_map(|project| match &project.launch {
                LaunchFile::Servers(servers) => {
                    servers.iter().find(|server| server.key() == key).cloned()
                }
                LaunchFile::Missing | LaunchFile::Invalid(_) => None,
            })
    }

    fn start_server(&mut self, key: &str, ctx: &mut ViewContext<Self>) {
        if self.runs.get(key).is_some_and(ServerRun::is_running) {
            self.show_log(key.to_owned(), ctx);
            return;
        }
        let Some(path_env) = self.path_env.clone() else {
            if !self.queued_starts.iter().any(|queued| queued == key) {
                self.queued_starts.push(key.to_owned());
            }
            return;
        };
        let Some(config) = self.server_config(key) else {
            return;
        };
        if !self.tabs.get(self.active).is_some_and(Tab::is_blank) {
            self.add_tab(ctx);
        }
        let tab_id = self.tabs[self.active].id;
        let mut run = ServerRun {
            config: config.clone(),
            process: None,
            status: ServerStatus::Starting,
            lines: VecDeque::new(),
            log_path: None,
            tab_id,
        };
        if let Some(port) = config.port
            && port_answers(port)
        {
            run.status = ServerStatus::External;
            run.push_line(format!(
                "localhost:{port} já está respondendo; abrindo a página sem iniciar outro servidor."
            ));
            self.runs.insert(key.to_owned(), run);
            self.show_log(key.to_owned(), ctx);
            if let Some(url) = config.page_url() {
                self.navigate(url, ctx);
            }
            return;
        }

        let log_path = server_log_path(&config);
        run.push_line(format!("$ {}", config.command_line()));
        match ServerProcess::spawn(&config, path_env.as_deref(), Some(log_path.clone())) {
            Ok((process, events)) => {
                run.push_line(format!("Log completo em {}", log_path.display()));
                run.process = Some(process);
                run.log_path = Some(log_path);
                let key_for_events = key.to_owned();
                ctx.spawn_stream_local(
                    events,
                    move |me, event, ctx| me.on_server_event(&key_for_events, event, ctx),
                    |_, _| {},
                );
            }
            Err(err) => {
                run.status = ServerStatus::Exited(format!("não iniciou: {err}"));
                run.push_line(format!(
                    "Não foi possível iniciar `{}` em {}: {err}",
                    config.program,
                    config.cwd.display()
                ));
            }
        }
        self.runs.insert(key.to_owned(), run);
        self.show_log(key.to_owned(), ctx);
    }

    fn stop_server(&mut self, key: &str, ctx: &mut ViewContext<Self>) {
        let Some(run) = self.runs.get_mut(key) else {
            return;
        };
        match &run.process {
            Some(process) if run.is_running() => {
                process.stop();
                run.push_line("Parando o servidor…".to_owned());
            }
            Some(_) | None => {
                self.runs.remove(key);
                if self.log_for.as_deref() == Some(key) {
                    self.log_for = None;
                }
            }
        }
        ctx.notify();
    }

    fn on_server_event(&mut self, key: &str, event: ServerEvent, ctx: &mut ViewContext<Self>) {
        let Some(run) = self.runs.get_mut(key) else {
            return;
        };
        match event {
            ServerEvent::Output(line) => run.push_line(line),
            ServerEvent::Ready => {
                if run.status != ServerStatus::Starting {
                    return;
                }
                run.status = ServerStatus::Ready;
                let Some(url) = run.config.page_url() else {
                    ctx.notify();
                    return;
                };
                run.push_line(format!("Pronto em {url}"));
                let tab_id = run.tab_id;
                match self.tabs.iter().position(|tab| tab.id == tab_id) {
                    Some(index) if self.tabs[index].is_blank() => {
                        self.select_tab_index(index, ctx);
                        self.navigate(url, ctx);
                    }
                    Some(_) | None => {
                        self.add_tab(ctx);
                        self.navigate(url, ctx);
                    }
                }
            }
            ServerEvent::Exited(description) => {
                run.push_line(format!("O servidor {description}."));
                run.status = ServerStatus::Exited(description);
                run.process = None;
            }
        }
        ctx.notify();
    }

    fn show_log(&mut self, key: String, ctx: &mut ViewContext<Self>) {
        self.log_for = Some(key);
        ctx.notify();
    }

    fn copy_log(&self, ctx: &mut ViewContext<Self>) {
        let Some(run) = self.log_for.as_ref().and_then(|key| self.runs.get(key)) else {
            return;
        };
        let text = run.lines.iter().cloned().collect::<Vec<_>>().join("\n");
        ctx.clipboard().write(ClipboardContent::plain_text(text));
    }

    fn create_launch_file(&mut self, dir: &Path, ctx: &mut ViewContext<Self>) {
        if let Err(err) = create_launch_file(dir) {
            log::warn!("Can't create {LAUNCH_FILE} in {dir:?}: {err:#}");
        }
        self.reload_projects(ctx);
    }

    /// `PICOLAS_BROWSER_START=<server name>` starts that dev server of the start page when the
    /// browser opens. Debug builds only.
    fn start_dev_server_from_env(&mut self, ctx: &mut ViewContext<Self>) {
        if !cfg!(debug_assertions) {
            return;
        }
        let Ok(name) = std::env::var("PICOLAS_BROWSER_START") else {
            return;
        };
        let key = self
            .projects
            .iter()
            .find_map(|project| match &project.launch {
                LaunchFile::Servers(servers) => servers
                    .iter()
                    .find(|server| server.name == name)
                    .map(DevServerConfig::key),
                LaunchFile::Missing | LaunchFile::Invalid(_) => None,
            });
        if let Some(key) = key {
            self.start_server(&key, ctx);
        }
    }

    /// The server whose log panel shows.
    fn log_run(&self) -> Option<&ServerRun> {
        self.log_for.as_ref().and_then(|key| self.runs.get(key))
    }

    /// The branch of the folder the browser was opened from, shown in the toolbar.
    fn toolbar_branch(&self) -> Option<&str> {
        self.branches
            .get(self.context.dirs.first()?)
            .map(String::as_str)
    }
}

/// Where a dev server's output is written, so Claude can read it too.
fn server_log_path(config: &DevServerConfig) -> PathBuf {
    let name: String = config
        .name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let file = match config.port {
        Some(port) => format!("{name}-{port}.log"),
        None => format!("{name}.log"),
    };
    std::env::temp_dir().join("picolas-dev-servers").join(file)
}

/// The branch `dir` is on (a short commit hash when detached); `None` outside git.
async fn git_branch(dir: &Path) -> Option<String> {
    let branch = warp_util::git::run_git_command(dir, &["rev-parse", "--abbrev-ref", "HEAD"])
        .await
        .ok()?;
    let branch = branch.trim();
    if branch != "HEAD" {
        return (!branch.is_empty()).then(|| branch.to_owned());
    }
    let hash = warp_util::git::run_git_command(dir, &["rev-parse", "--short", "HEAD"])
        .await
        .ok()?;
    Some(hash.trim().to_owned())
}

#[cfg(target_os = "macos")]
fn create_web_view(
    ctx: &mut ViewContext<BrowserView>,
    on_event: impl Fn(WebViewEvent) + 'static,
) -> Option<picolas_webview::WebView> {
    let parent = ctx
        .windows()
        .platform_window(ctx.window_id())
        .and_then(|window| {
            window
                .as_any()
                .downcast_ref::<warpui::platform::mac::Window>()?
                .content_view()
        })?;
    picolas_webview::WebView::new_browser(&parent, on_event)
        .inspect_err(|err| log::warn!("Can't create the browser web view: {err:#}"))
        .ok()
}

#[cfg(not(target_os = "macos"))]
fn create_web_view(
    _ctx: &mut ViewContext<BrowserView>,
    _on_event: impl Fn(WebViewEvent) + 'static,
) -> Option<picolas_webview::WebView> {
    None
}

impl Drop for BrowserView {
    fn drop(&mut self) {
        for run in self.runs.values() {
            if let Some(process) = &run.process {
                process.stop();
            }
        }
        for tab in &self.tabs {
            tab.host.set(None);
        }
    }
}

impl Entity for BrowserView {
    type Event = BrowserViewEvent;
}

/// What every render helper needs.
#[derive(Clone, Copy)]
struct Look<'a> {
    palette: &'a Palette,
    sizes: &'a Sizes,
    fonts: design::ChatFonts,
}

impl View for BrowserView {
    fn ui_name() -> &'static str {
        "BrowserView"
    }

    fn on_focus(&mut self, _focus_ctx: &FocusContext, ctx: &mut ViewContext<Self>) {
        if self.tabs.get(self.active).is_some_and(Tab::is_blank) {
            self.reload_projects(ctx);
        }
    }

    fn on_window_transferred(
        &mut self,
        _source_window_id: warpui::WindowId,
        _target_window_id: warpui::WindowId,
        ctx: &mut ViewContext<Self>,
    ) {
        // The web views belong to the old window's view hierarchy; new ones load each tab's page
        // in the new window after its next frame.
        for tab in &mut self.tabs {
            if tab.host.has_web_view() {
                tab.pending_url = tab.pending_url.take().or(tab.state.url.clone());
                tab.host.set(None);
            }
        }
        self.is_watching_frame = false;
        self.watch_next_frame(ctx);
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let design = ChatDesign::as_ref(app);
        let look = Look {
            palette: design.palette(app),
            sizes: design.sizes(),
            fonts: design::fonts(app),
        };
        let mut column = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(self.render_tab_strip(look))
            .with_child(self.render_toolbar(look));
        let tab = self.tabs.get(self.active);
        if let Some((url, message)) = tab.and_then(|tab| tab.error.as_ref()) {
            column.add_child(render_error_bar(url.as_deref(), message, look));
        }
        let body = match tab {
            Some(tab) if !tab.is_blank() => WebViewSlot::new(
                tab.host.clone(),
                &tab.slot_id,
                Rect::new()
                    .with_background_color(look.palette.page.0)
                    .finish(),
                design::is_dark(app),
                look.palette.page.0,
            )
            .finish(),
            Some(_) | None => self.render_start_page(look),
        };
        column.add_child(Expanded::new(1., body).finish());
        if let Some(run) = self.log_run() {
            column.add_child(self.render_log_panel(run, look));
        }
        Container::new(column.finish())
            .with_background_color(look.palette.page.0)
            .finish()
    }
}

// --- Chrome --------------------------------------------------------------------------------------

impl BrowserView {
    fn render_tab_strip(&self, look: Look<'_>) -> Box<dyn Element> {
        let Look { palette, .. } = look;
        let mut tabs = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(2.);
        for (index, tab) in self.tabs.iter().enumerate() {
            tabs.add_child(
                Shrinkable::new(1., self.render_tab(tab, index == self.active, look)).finish(),
            );
        }
        tabs.add_child(render_icon_button(
            ICON_PLUS,
            self.mouse_state("new-tab"),
            BrowserViewAction::NewTab,
            true,
            look,
        ));
        let row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(4.)
            .with_child(Expanded::new(1., tabs.finish()).finish())
            .with_child(render_icon_button(
                ICON_CLOSE,
                self.mouse_state("close-pane"),
                BrowserViewAction::ClosePane,
                true,
                look,
            ));
        Container::new(
            ConstrainedBox::new(row.finish())
                .with_height(TAB_STRIP_HEIGHT)
                .finish(),
        )
        .with_horizontal_padding(CHROME_PADDING)
        .with_background_color(palette.sidebar.0)
        .finish()
    }

    fn render_tab(&self, tab: &Tab, active: bool, look: Look<'_>) -> Box<dyn Element> {
        let Look {
            palette,
            sizes,
            fonts,
        } = look;
        let palette = palette.clone();
        let title = tab.title();
        let id = tab.id;
        let font_size = sizes.small_font_size;
        let icon_size = sizes.small_icon_size - 1.;
        let loading = tab.state.loading;
        let close = render_icon_button_sized(
            ICON_CLOSE,
            self.mouse_state(format!("tab-close:{id}")),
            BrowserViewAction::CloseTab(id),
            true,
            TAB_CLOSE_SIZE,
            TAB_CLOSE_ICON_SIZE,
            look,
        );
        let hoverable = Hoverable::new(self.mouse_state(format!("tab:{id}")), move |state| {
            let hovered = state.is_hovered();
            let color = if active || hovered {
                palette.text.0
            } else {
                palette.text_muted.0
            };
            let mut row = Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(6.)
                .with_child(svg(
                    ICON_GLOBE,
                    if loading {
                        palette.accent.0
                    } else {
                        palette.text_faint.0
                    },
                    icon_size,
                ))
                .with_child(
                    Shrinkable::new(
                        1.,
                        Text::new_inline(title.clone(), fonts.ui, font_size)
                            .with_color(color)
                            .with_clip(ClipConfig::ellipsis())
                            .with_selectable(false)
                            .finish(),
                    )
                    .finish(),
                );
            row.add_child(if active || hovered {
                close
            } else {
                ConstrainedBox::new(Rect::new().finish())
                    .with_width(TAB_CLOSE_SIZE)
                    .with_height(TAB_CLOSE_SIZE)
                    .finish()
            });
            let mut container = Container::new(
                ConstrainedBox::new(row.finish())
                    .with_height(TAB_HEIGHT)
                    .finish(),
            )
            .with_padding_left(8.)
            .with_padding_right(3.)
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(CHROME_RADIUS)));
            if active {
                container = container.with_background_color(palette.surface_strong.0);
            } else if hovered {
                container = container.with_background_color(palette.surface_hover.0);
            }
            container.finish()
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, _, _| ctx.dispatch_typed_action(BrowserViewAction::SelectTab(id)))
        .with_defer_events_to_children()
        .finish();
        ConstrainedBox::new(hoverable)
            .with_max_width(TAB_MAX_WIDTH)
            .finish()
    }

    fn render_toolbar(&self, look: Look<'_>) -> Box<dyn Element> {
        let Look { palette, sizes, .. } = look;
        let state = self
            .tabs
            .get(self.active)
            .map(|tab| tab.state.clone())
            .unwrap_or_default();
        let has_page = self
            .tabs
            .get(self.active)
            .is_some_and(|tab| !tab.is_blank());
        let mut row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(2.)
            .with_child(render_icon_button(
                ICON_BACK,
                self.mouse_state("back"),
                BrowserViewAction::Back,
                state.can_go_back,
                look,
            ))
            .with_child(render_icon_button(
                ICON_FORWARD,
                self.mouse_state("forward"),
                BrowserViewAction::Forward,
                state.can_go_forward,
                look,
            ))
            .with_child(if state.loading {
                render_icon_button(
                    ICON_CLOSE,
                    self.mouse_state("stop-loading"),
                    BrowserViewAction::StopLoading,
                    true,
                    look,
                )
            } else {
                render_icon_button(
                    ICON_RELOAD,
                    self.mouse_state("reload"),
                    BrowserViewAction::Reload,
                    has_page,
                    look,
                )
            });

        // One line of the address font: the editor fills whatever height it's given.
        let line_height = (sizes.small_font_size * 1.4).ceil();
        let address = Container::new(
            Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(7.)
                .with_child(svg(
                    ICON_GLOBE,
                    palette.text_faint.0,
                    sizes.small_icon_size - 1.,
                ))
                .with_child(
                    Shrinkable::new(
                        1.,
                        ConstrainedBox::new(
                            TextInput::new(
                                self.address.clone(),
                                UiComponentStyles::default()
                                    .set_background(Fill::None)
                                    .set_border_radius(CornerRadius::with_all(Radius::Pixels(0.)))
                                    .set_border_width(0.),
                            )
                            .build()
                            .finish(),
                        )
                        .with_height(line_height)
                        .finish(),
                    )
                    .finish(),
                )
                .finish(),
        )
        .with_horizontal_padding(10.)
        .with_vertical_padding(((ADDRESS_HEIGHT - line_height) / 2.).max(0.))
        .with_background_color(palette.surface.0)
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(CHROME_RADIUS)))
        .finish();
        row.add_child(
            Expanded::new(
                1.,
                Container::new(address)
                    .with_margin_left(4.)
                    .with_margin_right(4.)
                    .finish(),
            )
            .finish(),
        );
        // The log panel shows the branch when it's open.
        if self.log_run().is_none()
            && let Some(branch) = self.toolbar_branch()
        {
            row.add_child(render_branch_chip(branch, look));
        }
        if has_page {
            row.add_child(render_icon_button(
                ICON_EXTERNAL,
                self.mouse_state("system-browser"),
                BrowserViewAction::OpenInSystemBrowser,
                true,
                look,
            ));
        }
        Container::new(
            ConstrainedBox::new(row.finish())
                .with_height(TOOLBAR_HEIGHT)
                .finish(),
        )
        .with_horizontal_padding(CHROME_PADDING)
        .with_background_color(palette.sidebar.0)
        .with_border(Border::bottom(1.).with_border_color(palette.border.0))
        .finish()
    }

    // --- Start page -------------------------------------------------------------------------

    /// A card centered on the page: the dev servers to start, how to add them, and the folders
    /// they come from.
    fn render_start_page(&self, look: Look<'_>) -> Box<dyn Element> {
        let Look {
            palette,
            sizes,
            fonts,
        } = look;
        let mut card = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(6.);

        let mut has_launch_file = false;
        let mut launch_path = None;
        let mut servers = Vec::new();
        for project in &self.projects {
            match &project.launch {
                LaunchFile::Servers(list) => {
                    has_launch_file = true;
                    launch_path.get_or_insert_with(|| launch_file_path(&project.dir));
                    servers.extend(list.iter());
                }
                LaunchFile::Invalid(error) => {
                    has_launch_file = true;
                    launch_path.get_or_insert_with(|| launch_file_path(&project.dir));
                    card.add_child(
                        Text::new(
                            format!("Não foi possível ler {LAUNCH_FILE}: {error}"),
                            fonts.ui,
                            sizes.small_font_size,
                        )
                        .with_color(palette.error.0)
                        .finish(),
                    );
                }
                LaunchFile::Missing => {}
            }
        }
        if !servers.is_empty() {
            card.add_child(card_title("Servidores", look));
            for server in servers {
                card.add_child(self.render_server_row(server, look));
            }
        }

        let mut hint = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_spacing(4.)
            .with_child(
                Text::new(HINT, fonts.ui, sizes.small_font_size)
                    .with_color(palette.text_muted.0)
                    .finish(),
            );
        match (launch_path, self.context.dirs.first()) {
            (Some(path), _) => hint.add_child(render_link(
                "Edite esta lista em .claude/launch.json",
                self.mouse_state("edit-launch"),
                BrowserViewAction::EditLaunchFile(path),
                look,
            )),
            (None, Some(dir)) => {
                hint.add_child(muted_text("Edite esta lista em .claude/launch.json", look));
                hint.add_child(
                    Container::new(render_text_button(
                        "Criar .claude/launch.json",
                        self.mouse_state("create-launch"),
                        BrowserViewAction::CreateLaunchFile(dir.clone()),
                        look,
                    ))
                    .with_margin_top(8.)
                    .finish(),
                );
            }
            (None, None) => {}
        }
        card.add_child(
            Container::new(hint.finish())
                .with_margin_top(if has_launch_file { 8. } else { 0. })
                .finish(),
        );

        if !self.context.dirs.is_empty() {
            card.add_child(
                Container::new(card_title("Pastas", look))
                    .with_margin_top(12.)
                    .finish(),
            );
            for dir in &self.context.dirs {
                card.add_child(self.render_folder_row(dir, look));
            }
        }

        Container::new(
            Align::new(
                ConstrainedBox::new(
                    Container::new(card.finish())
                        .with_uniform_padding(16.)
                        .with_background_color(palette.surface.0)
                        .with_border(Border::all(1.).with_border_color(palette.border.0))
                        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(
                            sizes.card_radius,
                        )))
                        .finish(),
                )
                .with_max_width(START_PAGE_MAX_WIDTH)
                .finish(),
            )
            .finish(),
        )
        .with_uniform_padding(24.)
        .with_background_color(palette.page.0)
        .finish()
    }

    /// A folder of the start page, with its git branch.
    fn render_folder_row(&self, dir: &Path, look: Look<'_>) -> Box<dyn Element> {
        let Look {
            palette,
            sizes,
            fonts,
        } = look;
        let name = dir.file_name().map_or_else(
            || dir.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let mut row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(8.)
            .with_child(svg(
                ICON_FOLDER,
                palette.text_muted.0,
                sizes.small_icon_size,
            ))
            .with_child(
                Expanded::new(
                    1.,
                    Text::new_inline(name, fonts.ui, sizes.small_font_size)
                        .with_color(palette.text.0)
                        .with_clip(ClipConfig::ellipsis())
                        .finish(),
                )
                .finish(),
            );
        if let Some(branch) = self.branches.get(dir) {
            row.add_child(render_branch_chip(branch, look));
        }
        ConstrainedBox::new(row.finish())
            .with_height(TAB_HEIGHT)
            .finish()
    }

    fn render_server_row(&self, server: &DevServerConfig, look: Look<'_>) -> Box<dyn Element> {
        let Look {
            palette,
            sizes,
            fonts,
        } = look;
        let key = server.key();
        let run = self.runs.get(&key);
        let running = run.is_some_and(ServerRun::is_running);
        let status_color = match run.map(|run| &run.status) {
            Some(ServerStatus::Starting) => Some(palette.warning.0),
            Some(ServerStatus::Ready | ServerStatus::External) => Some(palette.success.0),
            Some(ServerStatus::Exited(_)) => Some(palette.text_faint.0),
            None => None,
        };
        let button = if running {
            render_round_button(
                ICON_STOP,
                self.mouse_state(format!("server-stop:{key}")),
                BrowserViewAction::StopServer(key.clone()),
                look,
            )
        } else {
            render_round_button(
                ICON_PLAY,
                self.mouse_state(format!("server-start:{key}")),
                BrowserViewAction::StartServer(key.clone()),
                look,
            )
        };
        let name = server.name.clone();
        let port = server.port.map(|port| format!(":{port}"));
        let command = server.command_line();
        let palette = palette.clone();
        let action = if run.is_some() {
            BrowserViewAction::ShowLog(key.clone())
        } else {
            BrowserViewAction::StartServer(key.clone())
        };
        Hoverable::new(self.mouse_state(format!("server:{key}")), move |state| {
            let mut title = Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(6.);
            if let Some(color) = status_color {
                title.add_child(
                    ConstrainedBox::new(
                        Rect::new()
                            .with_background_color(color)
                            .with_corner_radius(CornerRadius::with_all(Radius::Percentage(50.)))
                            .finish(),
                    )
                    .with_width(6.)
                    .with_height(6.)
                    .finish(),
                );
            }
            title.add_child(
                Text::new_inline(name.clone(), fonts.ui, sizes.small_font_size + 0.5)
                    .with_style(Properties::default().weight(Weight::Medium))
                    .with_color(palette.text.0)
                    .with_selectable(false)
                    .finish(),
            );
            if let Some(port) = &port {
                title.add_child(
                    Text::new_inline(port.clone(), fonts.mono, sizes.tiny_font_size + 0.5)
                        .with_color(palette.text_muted.0)
                        .with_selectable(false)
                        .finish(),
                );
            }
            let text = Flex::column()
                .with_spacing(2.)
                .with_child(title.finish())
                .with_child(
                    Text::new_inline(command.clone(), fonts.mono, sizes.tiny_font_size)
                        .with_color(palette.text_faint.0)
                        .with_clip(ClipConfig::ellipsis())
                        .with_selectable(false)
                        .finish(),
                )
                .finish();
            let mut container = Container::new(
                Flex::row()
                    .with_main_axis_size(MainAxisSize::Max)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(10.)
                    .with_child(Expanded::new(1., text).finish())
                    .with_child(button)
                    .finish(),
            )
            .with_horizontal_padding(10.)
            .with_vertical_padding(7.)
            .with_border(Border::all(1.).with_border_color(palette.border.0))
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(CHROME_RADIUS + 2.)));
            container = container.with_background_color(if state.is_hovered() {
                palette.surface_hover.0
            } else {
                palette.page.0
            });
            container.finish()
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
        .with_defer_events_to_children()
        .finish()
    }

    // --- Server log -------------------------------------------------------------------------

    fn render_log_panel(&self, run: &ServerRun, look: Look<'_>) -> Box<dyn Element> {
        let Look {
            palette,
            sizes,
            fonts,
        } = look;
        let key = run.config.key();
        let mut title = run.config.name.clone();
        if let Some(port) = run.config.port {
            title.push_str(&format!(" · localhost:{port}"));
        }
        let status = match &run.status {
            ServerStatus::Starting => ("iniciando", palette.warning.0),
            ServerStatus::Ready => ("rodando", palette.success.0),
            ServerStatus::External => ("já estava rodando", palette.success.0),
            ServerStatus::Exited(_) => ("parado", palette.text_faint.0),
        };
        let mut header = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(8.)
            .with_child(svg(
                ICON_TERMINAL,
                palette.text_muted.0,
                sizes.small_icon_size,
            ))
            .with_child(
                Shrinkable::new(
                    1.,
                    Text::new_inline(title, fonts.ui, sizes.small_font_size)
                        .with_style(Properties::default().weight(Weight::Medium))
                        .with_color(palette.text.0)
                        .with_clip(ClipConfig::ellipsis())
                        .finish(),
                )
                .finish(),
            )
            .with_child(
                Text::new_inline(status.0, fonts.ui, sizes.tiny_font_size)
                    .with_color(status.1)
                    .finish(),
            );
        if let Some(branch) = self.branches.get(&run.config.project_dir) {
            header.add_child(render_branch_chip(branch, look));
        }
        header.add_child(Expanded::new(1., Rect::new().finish()).finish());
        header.add_child(render_icon_button(
            ICON_COPY,
            self.mouse_state("log-copy"),
            BrowserViewAction::CopyLog,
            true,
            look,
        ));
        header.add_child(if run.is_running() {
            render_icon_button(
                ICON_STOP,
                self.mouse_state("log-stop"),
                BrowserViewAction::StopServer(key),
                true,
                look,
            )
        } else {
            render_icon_button(
                ICON_PLAY,
                self.mouse_state("log-start"),
                BrowserViewAction::StartServer(key),
                true,
                look,
            )
        });
        header.add_child(render_icon_button(
            ICON_CLOSE,
            self.mouse_state("log-close"),
            BrowserViewAction::HideLog,
            true,
            look,
        ));

        let font_size = sizes.code_font_size - 1.;
        let line_height = font_size * 1.5;
        let body_height = LOG_PANEL_HEIGHT - sizes.header_height;
        let visible_lines = ((body_height - 12.) / line_height).floor().max(1.) as usize;
        let mut lines = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);
        let skip = run.lines.len().saturating_sub(visible_lines);
        for line in run.lines.iter().skip(skip) {
            lines.add_child(
                ConstrainedBox::new(
                    Text::new_inline(line.clone(), fonts.mono, font_size)
                        .with_color(palette.text_muted.0)
                        .with_clip(ClipConfig::end())
                        .finish(),
                )
                .with_height(line_height)
                .finish(),
            );
        }

        Container::new(
            ConstrainedBox::new(
                Flex::column()
                    .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
                    .with_child(
                        Container::new(
                            ConstrainedBox::new(header.finish())
                                .with_height(sizes.header_height)
                                .finish(),
                        )
                        .with_horizontal_padding(10.)
                        .with_border(Border::bottom(1.).with_border_color(palette.border.0))
                        .finish(),
                    )
                    .with_child(
                        Expanded::new(
                            1.,
                            Container::new(lines.finish())
                                .with_horizontal_padding(12.)
                                .with_vertical_padding(6.)
                                .finish(),
                        )
                        .finish(),
                    )
                    .finish(),
            )
            .with_height(LOG_PANEL_HEIGHT)
            .finish(),
        )
        .with_background_color(palette.code_background.0)
        .with_border(Border::top(1.).with_border_color(palette.border.0))
        .finish()
    }
}

fn render_error_bar(url: Option<&str>, message: &str, look: Look<'_>) -> Box<dyn Element> {
    let Look {
        palette,
        sizes,
        fonts,
    } = look;
    let text = match url {
        Some(url) => format!("Não foi possível abrir {}: {message}", display_url(url)),
        None => format!("Não foi possível abrir a página: {message}"),
    };
    Container::new(
        Text::new_inline(text, fonts.ui, sizes.small_font_size)
            .with_color(palette.error.0)
            .with_clip(ClipConfig::ellipsis())
            .finish(),
    )
    .with_horizontal_padding(14.)
    .with_vertical_padding(7.)
    .with_background_color(palette.error_background.0)
    .with_border(Border::bottom(1.).with_border_color(palette.error_border.0))
    .finish()
}

fn render_branch_chip(branch: &str, look: Look<'_>) -> Box<dyn Element> {
    let Look {
        palette,
        sizes,
        fonts,
    } = look;
    let chip = Container::new(
        Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(5.)
            .with_child(svg(
                ICON_BRANCH,
                palette.text_muted.0,
                sizes.small_icon_size - 1.,
            ))
            .with_child(
                Shrinkable::new(
                    1.,
                    Text::new_inline(branch.to_owned(), fonts.ui, sizes.tiny_font_size + 0.5)
                        .with_style(Properties::default().weight(Weight::Medium))
                        .with_color(palette.text_muted.0)
                        .with_clip(ClipConfig::ellipsis())
                        .finish(),
                )
                .finish(),
            )
            .finish(),
    )
    .with_horizontal_padding(7.)
    .with_vertical_padding(3.)
    .with_background_color(palette.surface_strong.0)
    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(CHROME_RADIUS)))
    .finish();
    ConstrainedBox::new(chip)
        .with_max_width(BRANCH_CHIP_MAX_WIDTH)
        .finish()
}

/// The title of a group in the start page's card, like the side panel cards' titles.
fn card_title(title: &'static str, look: Look<'_>) -> Box<dyn Element> {
    Container::new(
        Text::new_inline(title, look.fonts.ui, look.sizes.small_font_size)
            .with_style(Properties::default().weight(Weight::Semibold))
            .with_color(look.palette.text.0)
            .finish(),
    )
    .with_margin_bottom(2.)
    .finish()
}

fn muted_text(text: &'static str, look: Look<'_>) -> Box<dyn Element> {
    Text::new(text, look.fonts.ui, look.sizes.small_font_size)
        .with_color(look.palette.text_faint.0)
        .finish()
}

fn render_link(
    label: &'static str,
    mouse_state: MouseStateHandle,
    action: BrowserViewAction,
    look: Look<'_>,
) -> Box<dyn Element> {
    let palette = look.palette.clone();
    let fonts = look.fonts;
    let font_size = look.sizes.small_font_size;
    Hoverable::new(mouse_state, move |state| {
        Text::new_inline(label, fonts.ui, font_size)
            .with_color(if state.is_hovered() {
                palette.link.0
            } else {
                palette.text_faint.0
            })
            .with_selectable(false)
            .finish()
    })
    .with_cursor(Cursor::PointingHand)
    .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
    .finish()
}

fn render_icon_button(
    icon: &'static str,
    mouse_state: MouseStateHandle,
    action: BrowserViewAction,
    enabled: bool,
    look: Look<'_>,
) -> Box<dyn Element> {
    render_icon_button_sized(
        icon,
        mouse_state,
        action,
        enabled,
        CHROME_BUTTON_SIZE,
        CHROME_ICON_SIZE,
        look,
    )
}

fn render_icon_button_sized(
    icon: &'static str,
    mouse_state: MouseStateHandle,
    action: BrowserViewAction,
    enabled: bool,
    size: f32,
    icon_size: f32,
    look: Look<'_>,
) -> Box<dyn Element> {
    let palette = look.palette.clone();
    let hoverable = Hoverable::new(mouse_state, move |state| {
        let hovered = state.is_hovered() && enabled;
        let color = if !enabled {
            palette.border_strong.0
        } else if hovered {
            palette.text.0
        } else {
            palette.text_muted.0
        };
        let mut button = Container::new(Align::new(svg(icon, color, icon_size)).finish())
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(CHROME_RADIUS - 1.)));
        if hovered {
            button = button.with_background_color(palette.surface_hover.0);
        }
        ConstrainedBox::new(button.finish())
            .with_width(size)
            .with_height(size)
            .finish()
    });
    if !enabled {
        return hoverable.finish();
    }
    hoverable
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
        .finish()
}

/// The ▶/■ button of a server row.
fn render_round_button(
    icon: &'static str,
    mouse_state: MouseStateHandle,
    action: BrowserViewAction,
    look: Look<'_>,
) -> Box<dyn Element> {
    let palette = look.palette.clone();
    let size = CHROME_BUTTON_SIZE + 2.;
    let icon_size = TAB_CLOSE_ICON_SIZE;
    Hoverable::new(mouse_state, move |state| {
        let background = if state.is_hovered() {
            palette.text.0
        } else {
            palette.surface_strong.0
        };
        let color = if state.is_hovered() {
            palette.page.0
        } else {
            palette.text.0
        };
        ConstrainedBox::new(
            Container::new(Align::new(svg(icon, color, icon_size)).finish())
                .with_background_color(background)
                .with_corner_radius(CornerRadius::with_all(Radius::Percentage(50.)))
                .finish(),
        )
        .with_width(size)
        .with_height(size)
        .finish()
    })
    .with_cursor(Cursor::PointingHand)
    .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
    .finish()
}

fn render_text_button(
    label: &'static str,
    mouse_state: MouseStateHandle,
    action: BrowserViewAction,
    look: Look<'_>,
) -> Box<dyn Element> {
    let palette = look.palette.clone();
    let fonts = look.fonts;
    let font_size = look.sizes.small_font_size;
    Hoverable::new(mouse_state, move |state| {
        Container::new(
            Text::new_inline(label, fonts.ui, font_size)
                .with_style(Properties::default().weight(Weight::Medium))
                .with_color(palette.on_accent.0)
                .with_selectable(false)
                .finish(),
        )
        .with_horizontal_padding(12.)
        .with_vertical_padding(6.)
        .with_background_color(if state.is_hovered() {
            mix(palette.accent.0, ColorU::black(), 0.1)
        } else {
            palette.accent.0
        })
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(CHROME_RADIUS)))
        .finish()
    })
    .with_cursor(Cursor::PointingHand)
    .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
    .finish()
}

fn svg(path: &'static str, color: ColorU, size: f32) -> Box<dyn Element> {
    ConstrainedBox::new(SvgIcon::new(path, color).finish())
        .with_width(size)
        .with_height(size)
        .finish()
}

/// `a` moved towards `b` by `amount` (0 to 1), keeping `a`'s alpha.
fn mix(a: ColorU, b: ColorU, amount: f32) -> ColorU {
    let channel =
        |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * amount).round() as u8;
    ColorU::new(channel(a.r, b.r), channel(a.g, b.g), channel(a.b, b.b), a.a)
}

impl TypedActionView for BrowserView {
    type Action = BrowserViewAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            BrowserViewAction::NewTab => {
                self.add_tab(ctx);
                self.reload_projects(ctx);
                ctx.focus(&self.address);
            }
            BrowserViewAction::SelectTab(id) => {
                if let Some(index) = self.tabs.iter().position(|tab| tab.id == *id) {
                    self.select_tab_index(index, ctx);
                }
            }
            BrowserViewAction::CloseTab(id) => self.close_tab(*id, ctx),
            BrowserViewAction::Back => self.with_active_web_view(|web_view| web_view.go_back()),
            BrowserViewAction::Forward => {
                self.with_active_web_view(|web_view| web_view.go_forward())
            }
            BrowserViewAction::Reload => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.error = None;
                }
                self.with_active_web_view(|web_view| web_view.reload());
                ctx.notify();
            }
            BrowserViewAction::StopLoading => {
                self.with_active_web_view(|web_view| web_view.stop_loading())
            }
            BrowserViewAction::OpenInSystemBrowser => {
                if let Some(url) = self.tabs.get(self.active).and_then(Tab::url) {
                    ctx.open_url(url);
                }
            }
            BrowserViewAction::StartServer(key) => self.start_server(key, ctx),
            BrowserViewAction::StopServer(key) => self.stop_server(key, ctx),
            BrowserViewAction::ShowLog(key) => self.show_log(key.clone(), ctx),
            BrowserViewAction::HideLog => {
                self.log_for = None;
                ctx.notify();
            }
            BrowserViewAction::CopyLog => self.copy_log(ctx),
            BrowserViewAction::CreateLaunchFile(dir) => self.create_launch_file(dir, ctx),
            BrowserViewAction::ClosePane => self.close(ctx),
            BrowserViewAction::EditLaunchFile(path) => {
                ctx.dispatch_typed_action_deferred(WorkspaceAction::OpenFileInSplitPane {
                    path: path.clone(),
                });
            }
        }
    }
}

impl BackingView for BrowserView {
    type PaneHeaderOverflowMenuAction = BrowserViewAction;
    type CustomAction = BrowserViewAction;
    type AssociatedData = ();

    fn handle_pane_header_overflow_menu_action(
        &mut self,
        action: &Self::PaneHeaderOverflowMenuAction,
        ctx: &mut ViewContext<Self>,
    ) {
        self.handle_action(action, ctx);
    }

    fn handle_custom_action(&mut self, action: &Self::CustomAction, ctx: &mut ViewContext<Self>) {
        self.handle_action(action, ctx);
    }

    fn close(&mut self, ctx: &mut ViewContext<Self>) {
        for tab in &self.tabs {
            tab.host.set(None);
        }
        ctx.emit(BrowserViewEvent::Pane(PaneEvent::Close));
    }

    fn focus_contents(&mut self, ctx: &mut ViewContext<Self>) {
        self.focus(ctx);
    }

    fn render_header_content(
        &self,
        _ctx: &view::HeaderRenderContext<'_>,
        app: &AppContext,
    ) -> HeaderContent {
        HeaderContent::Standard(StandardHeader {
            title: self.pane_configuration.as_ref(app).title().to_owned(),
            title_secondary: None,
            title_style: None,
            title_clip_config: ClipConfig::end(),
            title_max_width: None,
            left_of_title: None,
            right_of_title: None,
            left_of_overflow: None,
            options: StandardHeaderOptions::default(),
        })
    }

    /// The tab strip takes the pane header's place, like in the Claude desktop app.
    fn should_render_header(&self, _app: &AppContext) -> bool {
        false
    }

    fn set_focus_handle(&mut self, focus_handle: PaneFocusHandle, _ctx: &mut ViewContext<Self>) {
        self.focus_handle = Some(focus_handle);
    }
}
