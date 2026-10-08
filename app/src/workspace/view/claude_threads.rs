//! Threads panel of the vertical tabs sidebar: Claude Code conversations
//! grouped by project folder (see `crate::claude_threads`). It takes the place
//! of the tab list while the header "Threads" button is active.
//!
//! Clicking a thread focuses the tab already running it, or resumes it with
//! `claude --resume` in a new tab; the `+` of a project starts a new thread in
//! its folder.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use pathfinder_geometry::vector::vec2f;
use warp_core::ui::Icon as WarpIcon;
use warp_core::ui::theme::Fill as WarpThemeFill;
use warp_core::ui::theme::color::internal_colors;
use warpui::r#async::Timer;
use warpui::elements::{
    ChildAnchor, ClippedScrollStateHandle, ClippedScrollable, ConstrainedBox, Container,
    CornerRadius, CrossAxisAlignment, DragAxis, Draggable, DraggableState, Element,
    Fill as ElementFill, Flex, Hoverable, MainAxisAlignment, MainAxisSize, MouseStateHandle,
    OffsetPositioning, Padding, ParentAnchor, ParentElement, ParentOffsetBounds, Radius,
    SavePosition, ScrollbarWidth, Shrinkable, Stack, Text,
};
use warpui::geometry::rect::RectF;
use warpui::platform::{Cursor, FilePickerConfiguration};
use warpui::prelude::Align;
use warpui::text_layout::ClipConfig;
use warpui::ui_components::components::UiComponent;
use warpui::{AppContext, Entity, SingletonEntity, TypedActionView, View, ViewContext};

use crate::appearance::Appearance;
use crate::claude_threads::{
    self, ClaudeThread, NEW_THREAD_COMMAND, ProjectThreads, ThreadProject, TranscriptCache,
};
use crate::terminal::CLIAgent;
use crate::terminal::cli_agent_sessions::{CLIAgentSessionStatus, CLIAgentSessionsModel};
use crate::workspace::WorkspaceAction;

/// How often the transcripts are re-read while the panel is visible, so new
/// conversations and title changes show up.
const POLL_INTERVAL: Duration = Duration::from_secs(4);
/// Threads written to this recently may be open in another Warp window or
/// app, so they can't be deleted.
const RECENTLY_ACTIVE: Duration = Duration::from_secs(120);
/// Threads listed per project until "Mostrar mais" is clicked.
const RECENT_THREAD_LIMIT: usize = 10;
const PANEL_PADDING: f32 = 8.;
const ROW_VERTICAL_PADDING: f32 = 5.;
const ROW_HORIZONTAL_PADDING: f32 = 6.;
const ROW_CORNER_RADIUS: f32 = 4.;
const ROW_SPACING: f32 = 2.;
const THREAD_INDENT: f32 = 18.;
const ICON_SIZE: f32 = 12.;
const ACTION_ICON_SIZE: f32 = 14.;
const ACTION_BUTTON_PADDING: f32 = 2.;
const ICON_TEXT_GAP: f32 = 8.;
const TITLE_FONT_SIZE: f32 = 12.;
const META_FONT_SIZE: f32 = 10.;
/// Fixed box for a thread's age, status dot or trash button, so swapping them
/// on hover doesn't move the row.
const META_WIDTH: f32 = 52.;
const META_HEIGHT: f32 = ACTION_ICON_SIZE + 2. * ACTION_BUTTON_PADDING;

#[derive(Debug, Clone, PartialEq)]
pub enum ClaudeThreadsAction {
    /// A project's header row is being dragged to reorder the projects.
    DragProject {
        path: PathBuf,
        position: RectF,
    },
    DropProject,
    AddProject,
    RemoveProject(PathBuf),
    ToggleProjectCollapsed(PathBuf),
    ToggleShowAllThreads(PathBuf),
    NewThread(PathBuf),
    OpenThread {
        session_id: String,
        cwd: PathBuf,
    },
    /// First click on the trash button: ask for confirmation in the row.
    AskDeleteThread(String),
    ConfirmDeleteThread(String),
    CancelDeleteThread,
}

pub struct ClaudeThreadsView {
    projects: Vec<ThreadProject>,
    threads: ProjectThreads,
    /// False until the first scan finishes, so empty projects don't flash
    /// "Nenhuma conversa" while loading.
    has_scanned: bool,
    /// Parsed transcripts, reused by the next scan when unchanged.
    transcript_cache: Arc<Mutex<TranscriptCache>>,
    /// Bumped on every scheduled scan; results of older scans are dropped.
    scan_epoch: usize,
    is_visible: bool,
    /// Projects showing every thread instead of the latest `RECENT_THREAD_LIMIT`.
    show_all_threads: HashSet<PathBuf>,
    /// The thread whose row is asking "Apagar?".
    pending_delete: Option<String>,
    scroll_state: ClippedScrollStateHandle,
    /// Hover state of each row and button, keyed by what it belongs to.
    mouse_states: RefCell<HashMap<String, MouseStateHandle>>,
    /// Drag state of each project header and thread row, keyed like
    /// `mouse_states`.
    drag_states: RefCell<HashMap<String, DraggableState>>,
}

impl ClaudeThreadsView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        // Status dots follow the Claude sessions running in the tabs.
        ctx.subscribe_to_model(&CLIAgentSessionsModel::handle(ctx), |me, _, _, ctx| {
            if me.is_visible {
                ctx.notify();
            }
        });
        Self {
            projects: claude_threads::load_projects(),
            threads: ProjectThreads::new(),
            has_scanned: false,
            transcript_cache: Arc::new(Mutex::new(TranscriptCache::new())),
            scan_epoch: 0,
            is_visible: false,
            show_all_threads: HashSet::new(),
            pending_delete: None,
            scroll_state: ClippedScrollStateHandle::default(),
            mouse_states: RefCell::new(HashMap::new()),
            drag_states: RefCell::new(HashMap::new()),
        }
    }

    /// Called when the panel is shown or hidden; transcripts are only polled
    /// while it's visible.
    pub fn set_visible(&mut self, visible: bool, ctx: &mut ViewContext<Self>) {
        if self.is_visible == visible {
            return;
        }
        self.is_visible = visible;
        if visible {
            self.schedule_scan(Duration::ZERO, ctx);
        }
    }

    fn schedule_scan(&mut self, delay: Duration, ctx: &mut ViewContext<Self>) {
        self.scan_epoch += 1;
        let epoch = self.scan_epoch;
        let projects: Vec<PathBuf> = self.projects.iter().map(|p| p.path.clone()).collect();
        let cache = self.transcript_cache.clone();
        ctx.spawn(
            async move {
                Timer::after(delay).await;
                let mut cache = cache.lock().unwrap_or_else(PoisonError::into_inner);
                claude_threads::scan(&projects, &mut cache)
            },
            move |me, threads, ctx| {
                if me.scan_epoch != epoch {
                    return;
                }
                me.threads = threads;
                me.has_scanned = true;
                ctx.notify();
                if me.is_visible {
                    me.schedule_scan(POLL_INTERVAL, ctx);
                }
            },
        );
    }

    fn pick_project_folder(&mut self, ctx: &mut ViewContext<Self>) {
        ctx.open_file_picker(
            move |result, ctx| {
                let Ok(paths) = result else { return };
                let Some(path) = paths.into_iter().next() else {
                    return;
                };
                if let Some(handle) = ctx.handle().upgrade(ctx) {
                    handle.update(ctx, |me, ctx| me.add_project(PathBuf::from(path), ctx));
                }
            },
            FilePickerConfiguration::new().folders_only(),
        );
    }

    fn add_project(&mut self, path: PathBuf, ctx: &mut ViewContext<Self>) {
        if self.projects.iter().any(|project| project.path == path) {
            return;
        }
        self.projects.push(ThreadProject::new(path));
        self.save_projects();
        self.schedule_scan(Duration::ZERO, ctx);
        ctx.notify();
    }

    fn remove_project(&mut self, path: &Path, ctx: &mut ViewContext<Self>) {
        self.projects.retain(|project| project.path != path);
        self.threads.remove(path);
        self.show_all_threads.remove(path);
        self.save_projects();
        ctx.notify();
    }

    fn toggle_project_collapsed(&mut self, path: &Path, ctx: &mut ViewContext<Self>) {
        if let Some(project) = self.projects.iter_mut().find(|p| p.path == path) {
            project.collapsed = !project.collapsed;
            self.save_projects();
            ctx.notify();
        }
    }

    fn save_projects(&self) {
        if let Err(err) = claude_threads::save_projects(&self.projects) {
            log::warn!("Failed to save Claude threads projects: {err}");
        }
    }

    /// Focuses the tab already running the thread, otherwise resumes it in a
    /// new tab opened in the thread's folder.
    fn open_thread(&mut self, session_id: &str, cwd: &Path, ctx: &mut ViewContext<Self>) {
        let running_in = CLIAgentSessionsModel::as_ref(ctx)
            .session_by_agent_session_id(session_id)
            .map(|(terminal_view_id, _)| terminal_view_id);
        if let Some(terminal_view_id) = running_in {
            ctx.dispatch_typed_action_deferred(WorkspaceAction::FocusTerminalViewInWorkspace {
                terminal_view_id,
            });
            return;
        }
        let Some((project, thread)) = self.threads.iter().find_map(|(project, threads)| {
            let thread = threads
                .iter()
                .find(|thread| thread.session_id == session_id)?;
            Some((project, thread))
        }) else {
            return;
        };
        let Some(command) = thread.resume_command() else {
            return;
        };
        ctx.dispatch_typed_action_deferred(WorkspaceAction::RunCommandInNewTab {
            directory: cwd.to_path_buf(),
            command,
            group: Some(project_group_name(project)),
        });
    }

    /// Moves the thread's transcript to the Trash and drops it from the list.
    fn delete_thread(&mut self, session_id: &str, ctx: &mut ViewContext<Self>) {
        self.pending_delete = None;
        let thread = self
            .threads
            .values()
            .flatten()
            .find(|thread| thread.session_id == session_id)
            .cloned();
        let Some(thread) = thread else {
            ctx.notify();
            return;
        };
        match claude_threads::trash_thread(&thread) {
            Ok(()) => {
                for threads in self.threads.values_mut() {
                    threads.retain(|t| t.session_id != session_id);
                }
            }
            Err(err) => log::warn!("Failed to move Claude thread to the Trash: {err}"),
        }
        ctx.notify();
    }

    fn mouse_state(&self, key: String) -> MouseStateHandle {
        self.mouse_states
            .borrow_mut()
            .entry(key)
            .or_default()
            .clone()
    }

    fn drag_state(&self, key: String) -> DraggableState {
        self.drag_states
            .borrow_mut()
            .entry(key)
            .or_default()
            .clone()
    }

    /// Moves the dragged project to the slot of the project block under the
    /// cursor. The order is saved when the drag ends.
    fn drag_project(&mut self, path: &Path, position: RectF, ctx: &mut ViewContext<Self>) {
        let Some(from) = self.projects.iter().position(|p| p.path == path) else {
            return;
        };
        let cursor_y = position.center().y();
        let to = self.projects.iter().position(|project| {
            ctx.element_position_by_id(project_block_position_id(&project.path))
                .is_some_and(|rect| rect.min_y() <= cursor_y && cursor_y < rect.max_y())
        });
        if let Some(to) = to
            && to != from
        {
            let project = self.projects.remove(from);
            self.projects.insert(to, project);
            ctx.notify();
        }
    }
}

/// Save-position id of a project's block (header and threads), used to find
/// where a dragged project would land.
fn project_block_position_id(path: &Path) -> String {
    format!("claude_threads_project_block:{}", path.display())
}

/// Quotes `path` for a POSIX shell.
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

/// Claude sessions opened from a project share a tab group named after its
/// folder.
fn project_group_name(project: &Path) -> String {
    ThreadProject::new(project.to_path_buf()).name()
}

impl Entity for ClaudeThreadsView {
    type Event = ();
}

impl TypedActionView for ClaudeThreadsView {
    type Action = ClaudeThreadsAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            ClaudeThreadsAction::AddProject => self.pick_project_folder(ctx),
            ClaudeThreadsAction::RemoveProject(path) => self.remove_project(path, ctx),
            ClaudeThreadsAction::ToggleProjectCollapsed(path) => {
                self.toggle_project_collapsed(path, ctx);
            }
            ClaudeThreadsAction::ToggleShowAllThreads(path) => {
                if !self.show_all_threads.remove(path) {
                    self.show_all_threads.insert(path.clone());
                }
                ctx.notify();
            }
            ClaudeThreadsAction::NewThread(path) => {
                ctx.dispatch_typed_action_deferred(WorkspaceAction::RunCommandInNewTab {
                    directory: path.clone(),
                    command: NEW_THREAD_COMMAND.to_owned(),
                    group: Some(project_group_name(path)),
                });
            }
            ClaudeThreadsAction::OpenThread { session_id, cwd } => {
                self.pending_delete = None;
                self.open_thread(session_id, cwd, ctx);
            }
            ClaudeThreadsAction::AskDeleteThread(session_id) => {
                self.pending_delete = Some(session_id.clone());
                ctx.notify();
            }
            ClaudeThreadsAction::ConfirmDeleteThread(session_id) => {
                self.delete_thread(session_id, ctx);
            }
            ClaudeThreadsAction::DragProject { path, position } => {
                self.drag_project(path, *position, ctx);
            }
            ClaudeThreadsAction::DropProject => {
                self.save_projects();
                ctx.notify();
            }
            ClaudeThreadsAction::CancelDeleteThread => {
                self.pending_delete = None;
                ctx.notify();
            }
        }
    }
}

/// Theme colors used by the rows, resolved once per render.
#[derive(Clone, Copy)]
struct Palette {
    main_text: WarpThemeFill,
    sub_text: WarpThemeFill,
    row_hover: WarpThemeFill,
    button_hover: WarpThemeFill,
    working: WarpThemeFill,
    claude: WarpThemeFill,
}

impl Palette {
    fn new(appearance: &Appearance) -> Self {
        let theme = appearance.theme();
        Self {
            main_text: theme.main_text_color(theme.background()),
            sub_text: theme.sub_text_color(theme.background()),
            row_hover: internal_colors::fg_overlay_2(theme),
            button_hover: internal_colors::fg_overlay_3(theme),
            working: theme.accent(),
            claude: CLIAgent::Claude
                .brand_color()
                .map(WarpThemeFill::Solid)
                .unwrap_or_else(|| theme.accent()),
        }
    }
}

impl View for ClaudeThreadsView {
    fn ui_name() -> &'static str {
        "ClaudeThreadsView"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let palette = Palette::new(appearance);

        let mut list = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(ROW_SPACING);
        if self.projects.is_empty() {
            list.add_child(render_empty_state(palette, appearance));
        }
        let now = SystemTime::now();
        for project in &self.projects {
            let mut block = Flex::column()
                .with_main_axis_size(MainAxisSize::Min)
                .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
                .with_spacing(ROW_SPACING)
                .with_child(self.render_project_row(project, palette, appearance));
            if !project.collapsed {
                self.add_thread_rows(&mut block, project, now, palette, appearance, app);
            }
            list.add_child(
                SavePosition::new(block.finish(), &project_block_position_id(&project.path))
                    .finish(),
            );
        }

        let scrollable = ClippedScrollable::vertical(
            self.scroll_state.clone(),
            Container::new(list.finish())
                .with_horizontal_padding(PANEL_PADDING)
                .with_padding_bottom(PANEL_PADDING)
                .finish(),
            ScrollbarWidth::Custom(4.),
            theme.nonactive_ui_detail().into(),
            theme.active_ui_detail().into(),
            ElementFill::None,
        )
        .with_overlayed_scrollbar()
        .finish();

        Flex::column()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(self.render_header(palette, appearance))
            .with_child(Shrinkable::new(1., scrollable).finish())
            .finish()
    }
}

impl ClaudeThreadsView {
    /// "Threads" title with the button that adds a project folder.
    fn render_header(&self, palette: Palette, appearance: &Appearance) -> Box<dyn Element> {
        let title = Text::new_inline("Threads", appearance.ui_font_family(), TITLE_FONT_SIZE)
            .with_color(palette.sub_text.into())
            .finish();
        let add_button = render_icon_button(
            WarpIcon::Plus,
            "Adicionar pasta",
            self.mouse_state("header:add".to_owned()),
            ClaudeThreadsAction::AddProject,
            palette,
            appearance,
        );
        Container::new(
            Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(title)
                .with_child(add_button)
                .finish(),
        )
        .with_vertical_padding(PANEL_PADDING)
        .with_horizontal_padding(PANEL_PADDING + ROW_HORIZONTAL_PADDING)
        .finish()
    }

    /// Chevron and folder name, with buttons for a new thread and (on hover)
    /// removing the project. Clicking the row collapses or expands it.
    fn render_project_row(
        &self,
        project: &ThreadProject,
        palette: Palette,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let font_family = appearance.ui_font_family();
        let path = project.path.clone();
        let key = path.display().to_string();
        let name = project.name();
        let chevron = if project.collapsed {
            WarpIcon::ChevronRight
        } else {
            WarpIcon::ChevronDown
        };
        let new_thread_button = render_icon_button(
            WarpIcon::Plus,
            "Nova thread",
            self.mouse_state(format!("project-new:{key}")),
            ClaudeThreadsAction::NewThread(path.clone()),
            palette,
            appearance,
        );
        let remove_button = render_icon_button(
            WarpIcon::X,
            "Remover pasta da lista",
            self.mouse_state(format!("project-remove:{key}")),
            ClaudeThreadsAction::RemoveProject(path.clone()),
            palette,
            appearance,
        );
        let drag_path = path.clone();

        let row = Hoverable::new(
            self.mouse_state(format!("project:{key}")),
            move |hover_state| {
                let mut buttons = Flex::row()
                    .with_main_axis_size(MainAxisSize::Min)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(2.);
                if hover_state.is_hovered() {
                    buttons.add_child(remove_button);
                }
                buttons.add_child(new_thread_button);

                let label = Flex::row()
                    .with_main_axis_size(MainAxisSize::Max)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(ICON_TEXT_GAP)
                    .with_child(render_icon(chevron, palette.sub_text))
                    .with_child(
                        Shrinkable::new(
                            1.,
                            Text::new_inline(name, font_family, TITLE_FONT_SIZE)
                                .with_clip(ClipConfig::ellipsis())
                                .with_color(palette.main_text.into())
                                .finish(),
                        )
                        .finish(),
                    )
                    .finish();
                let row = Flex::row()
                    .with_main_axis_size(MainAxisSize::Max)
                    .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_child(Shrinkable::new(1., label).finish())
                    .with_child(buttons.finish())
                    .finish();
                render_row_container(row, hover_state.is_hovered(), palette)
            },
        )
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(ClaudeThreadsAction::ToggleProjectCollapsed(path.clone()));
        })
        .with_defer_events_to_children()
        .finish();

        // Dragging the header up or down reorders the projects.
        Draggable::new(self.drag_state(format!("project:{key}")), row)
            .on_drag(move |ctx, _, position, _| {
                ctx.dispatch_typed_action(ClaudeThreadsAction::DragProject {
                    path: drag_path.clone(),
                    position,
                });
            })
            .on_drop(|ctx, _, _, _| {
                ctx.dispatch_typed_action(ClaudeThreadsAction::DropProject);
            })
            .with_drag_axis(DragAxis::VerticalOnly)
            .finish()
    }

    fn add_thread_rows(
        &self,
        list: &mut Flex,
        project: &ThreadProject,
        now: SystemTime,
        palette: Palette,
        appearance: &Appearance,
        app: &AppContext,
    ) {
        let threads = self
            .threads
            .get(&project.path)
            .map(Vec::as_slice)
            .unwrap_or_default();

        if threads.is_empty() {
            if self.has_scanned {
                list.add_child(render_note("Nenhuma conversa ainda", palette, appearance));
            }
            return;
        }

        let show_all = self.show_all_threads.contains(&project.path);
        let shown = if show_all {
            threads.len()
        } else {
            threads.len().min(RECENT_THREAD_LIMIT)
        };
        for thread in &threads[..shown] {
            list.add_child(self.render_thread_row(thread, now, palette, appearance, app));
        }

        if threads.len() > RECENT_THREAD_LIMIT {
            let label = if show_all {
                "Mostrar menos".to_owned()
            } else {
                format!("Mostrar mais ({})", threads.len() - RECENT_THREAD_LIMIT)
            };
            let path = project.path.clone();
            let mouse_state = self.mouse_state(format!("show-all:{}", path.display()));
            let font_family = appearance.ui_font_family();
            list.add_child(
                Hoverable::new(mouse_state, move |hover_state| {
                    let text = Text::new_inline(label, font_family, META_FONT_SIZE)
                        .with_color(palette.sub_text.into())
                        .finish();
                    let text = Container::new(text)
                        .with_padding_left(THREAD_INDENT)
                        .finish();
                    render_row_container(text, hover_state.is_hovered(), palette)
                })
                .with_cursor(Cursor::PointingHand)
                .on_click(move |ctx, _, _| {
                    ctx.dispatch_typed_action(ClaudeThreadsAction::ToggleShowAllThreads(
                        path.clone(),
                    ));
                })
                .finish(),
            );
        }
    }

    /// Claude icon, title and age of a thread. Threads running in a tab also
    /// get a status dot: working, waiting for the user, or idle. On hover the
    /// age gives way to a trash button, which asks for confirmation in the row
    /// before moving the conversation to the Trash.
    fn render_thread_row(
        &self,
        thread: &ClaudeThread,
        now: SystemTime,
        palette: Palette,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let font_family = appearance.ui_font_family();
        let running_session =
            CLIAgentSessionsModel::as_ref(app).session_by_agent_session_id(&thread.session_id);
        // A thread open in a tab, or written to moments ago (maybe by another
        // app), is in use and can't be deleted.
        let recently_active = now
            .duration_since(thread.updated_at)
            .is_ok_and(|age| age < RECENTLY_ACTIVE)
            || thread.updated_at > now;
        let can_delete = running_session.is_none() && !recently_active;
        let status_color = running_session.map(|(_, session)| {
            if matches!(session.status, CLIAgentSessionStatus::Blocked { .. }) {
                palette.claude
            } else if session.is_working_on_prompt() {
                palette.working
            } else {
                palette.sub_text
            }
        });
        let title = thread.title.clone();
        let age = claude_threads::format_age(thread.updated_at, now);
        let action = ClaudeThreadsAction::OpenThread {
            session_id: thread.session_id.clone(),
            cwd: thread.cwd.clone(),
        };
        let session_id = &thread.session_id;
        let is_confirming = self.pending_delete.as_deref() == Some(session_id.as_str());
        let trash_button = can_delete.then(|| {
            render_icon_button(
                WarpIcon::Trash,
                "Apagar conversa",
                self.mouse_state(format!("thread-delete:{session_id}")),
                ClaudeThreadsAction::AskDeleteThread(session_id.clone()),
                palette,
                appearance,
            )
        });
        let confirm_buttons = is_confirming.then(|| {
            [
                render_icon_button(
                    WarpIcon::Check,
                    "Mover para a Lixeira",
                    self.mouse_state(format!("thread-confirm:{session_id}")),
                    ClaudeThreadsAction::ConfirmDeleteThread(session_id.clone()),
                    palette,
                    appearance,
                ),
                render_icon_button(
                    WarpIcon::X,
                    "Cancelar",
                    self.mouse_state(format!("thread-cancel:{session_id}")),
                    ClaudeThreadsAction::CancelDeleteThread,
                    palette,
                    appearance,
                ),
            ]
        });
        // Dropped on a pane, a thread already open moves its pane there; any
        // other resumes in a new split from its folder.
        let drop_command = thread
            .resume_command()
            .map(|resume| format!("cd {} && {resume}", shell_quote(&thread.cwd)));
        let is_running = running_session.is_some();

        let row = Hoverable::new(
            self.mouse_state(format!("thread:{session_id}")),
            move |hover_state| {
                let mut meta = Flex::row()
                    .with_main_axis_size(MainAxisSize::Min)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(4.);
                if let Some(buttons) = confirm_buttons {
                    meta.add_child(
                        Text::new_inline("Apagar?", font_family, META_FONT_SIZE)
                            .with_color(palette.main_text.into())
                            .finish(),
                    );
                    for button in buttons {
                        meta.add_child(button);
                    }
                } else if let Some(button) = trash_button.filter(|_| hover_state.is_hovered()) {
                    meta.add_child(button);
                } else {
                    if let Some(color) = status_color {
                        meta.add_child(
                            Text::new_inline("●", font_family, META_FONT_SIZE)
                                .with_color(color.into())
                                .finish(),
                        );
                    }
                    meta.add_child(
                        Text::new_inline(age, font_family, META_FONT_SIZE)
                            .with_color(palette.sub_text.into())
                            .finish(),
                    );
                }
                // The "Apagar?" prompt may be wider; everything else keeps the
                // same box so hovering doesn't shift the row.
                let meta = if is_confirming {
                    ConstrainedBox::new(meta.finish())
                        .with_height(META_HEIGHT)
                        .finish()
                } else {
                    ConstrainedBox::new(Align::new(meta.finish()).right().finish())
                        .with_width(META_WIDTH)
                        .with_height(META_HEIGHT)
                        .finish()
                };

                let label = Flex::row()
                    .with_main_axis_size(MainAxisSize::Max)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(ICON_TEXT_GAP)
                    .with_child(render_icon(WarpIcon::ClaudeLogo, palette.claude))
                    .with_child(
                        Shrinkable::new(
                            1.,
                            Text::new_inline(title, font_family, TITLE_FONT_SIZE)
                                .with_clip(ClipConfig::ellipsis())
                                .with_color(palette.main_text.into())
                                .finish(),
                        )
                        .finish(),
                    )
                    .finish();
                let row = Flex::row()
                    .with_main_axis_size(MainAxisSize::Max)
                    .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(ICON_TEXT_GAP)
                    .with_child(Shrinkable::new(1., label).finish())
                    .with_child(meta)
                    .finish();
                let row = Container::new(row)
                    .with_padding_left(THREAD_INDENT)
                    .finish();
                render_row_container(row, hover_state.is_hovered(), palette)
            },
        )
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(action.clone());
        })
        .with_defer_events_to_children()
        .finish();

        // Dragging a thread onto a pane of the active tab splits it there (see
        // `tab_merge`): an open thread moves its pane, so the same conversation
        // never runs twice.
        if drop_command.is_none() && !is_running {
            return row;
        }
        let drop_session_id = session_id.clone();
        Draggable::new(self.drag_state(format!("thread:{session_id}")), row)
            .on_drag(|ctx, _, position, _| {
                ctx.dispatch_typed_action(WorkspaceAction::DragClaudeThread { position });
            })
            .on_drop(move |ctx, _, _, _| {
                ctx.dispatch_typed_action(WorkspaceAction::DropClaudeThread {
                    session_id: drop_session_id.clone(),
                    command: drop_command.clone(),
                });
            })
            .finish()
    }
}

fn render_empty_state(palette: Palette, appearance: &Appearance) -> Box<dyn Element> {
    let hint = Text::new(
        "Adicione uma pasta com o + para ver as conversas do Claude Code feitas nela.",
        appearance.ui_font_family(),
        TITLE_FONT_SIZE,
    )
    .with_color(palette.sub_text.into())
    .finish();
    Container::new(hint)
        .with_horizontal_padding(ROW_HORIZONTAL_PADDING)
        .with_vertical_padding(ROW_VERTICAL_PADDING)
        .finish()
}

fn render_note(text: &'static str, palette: Palette, appearance: &Appearance) -> Box<dyn Element> {
    Container::new(
        Text::new_inline(text, appearance.ui_font_family(), META_FONT_SIZE)
            .with_color(palette.sub_text.into())
            .finish(),
    )
    .with_padding_left(THREAD_INDENT + ROW_HORIZONTAL_PADDING)
    .with_vertical_padding(ROW_VERTICAL_PADDING)
    .finish()
}

fn render_icon(icon: WarpIcon, color: WarpThemeFill) -> Box<dyn Element> {
    ConstrainedBox::new(icon.to_warpui_icon(color).finish())
        .with_width(ICON_SIZE)
        .with_height(ICON_SIZE)
        .finish()
}

/// Padding, rounded corners and hover background shared by every row.
fn render_row_container(
    child: Box<dyn Element>,
    is_hovered: bool,
    palette: Palette,
) -> Box<dyn Element> {
    let mut container = Container::new(child)
        .with_horizontal_padding(ROW_HORIZONTAL_PADDING)
        .with_vertical_padding(ROW_VERTICAL_PADDING)
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(ROW_CORNER_RADIUS)));
    if is_hovered {
        container = container.with_background(palette.row_hover);
    }
    container.finish()
}

/// Small icon button with a tooltip below it.
fn render_icon_button(
    icon: WarpIcon,
    tooltip: &'static str,
    mouse_state: MouseStateHandle,
    action: ClaudeThreadsAction,
    palette: Palette,
    appearance: &Appearance,
) -> Box<dyn Element> {
    let ui_builder = appearance.ui_builder().clone();
    Hoverable::new(mouse_state, move |hover_state| {
        let icon = ConstrainedBox::new(icon.to_warpui_icon(palette.sub_text).finish())
            .with_width(ACTION_ICON_SIZE)
            .with_height(ACTION_ICON_SIZE)
            .finish();
        let button = Container::new(icon)
            .with_padding(Padding::uniform(ACTION_BUTTON_PADDING))
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(ROW_CORNER_RADIUS)));
        if !hover_state.is_hovered() {
            return button.finish();
        }
        let button = button.with_background(palette.button_hover).finish();
        let tooltip = ui_builder.tool_tip(tooltip.to_owned()).build().finish();
        let mut stack = Stack::new().with_child(button);
        stack.add_positioned_overlay_child(
            tooltip,
            OffsetPositioning::offset_from_parent(
                vec2f(0., 4.),
                ParentOffsetBounds::WindowByPosition,
                ParentAnchor::BottomMiddle,
                ChildAnchor::TopMiddle,
            ),
        );
        stack.finish()
    })
    .with_cursor(Cursor::PointingHand)
    .on_click(move |ctx, _, _| {
        ctx.dispatch_typed_action(action.clone());
    })
    .finish()
}
