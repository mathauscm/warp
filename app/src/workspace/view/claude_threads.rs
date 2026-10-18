//! Threads panel of the vertical tabs sidebar: Claude Code conversations sorted into pinned
//! threads, project folders and the recent threads outside every project (see
//! `crate::claude_threads`). It takes the place of the tab list while the header "Threads" button
//! is active.
//!
//! Clicking a thread focuses the tab already running it, or resumes it with
//! `claude --resume` in a new tab; the `+` of a project starts a new thread in
//! its folder.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use asset_macro::bundled_or_fetched_asset;
use pathfinder_color::ColorU;
use pathfinder_geometry::vector::vec2f;
use warp_core::ui::Icon as WarpIcon;
use warp_core::ui::theme::Fill as WarpThemeFill;
use warp_core::ui::theme::color::internal_colors;
use warpui::r#async::Timer;
use warpui::elements::{
    CacheOption, ChildAnchor, ChildView, Clipped, ClippedScrollStateHandle, ClippedScrollable,
    ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, DragAxis, Draggable,
    DraggableState, Element, Empty, Fill as ElementFill, Flex, Hoverable, Image, MainAxisAlignment,
    MainAxisSize, MouseStateHandle, OffsetPositioning, Padding, ParentAnchor, ParentElement,
    ParentOffsetBounds, PositionedElementAnchor, PositionedElementOffsetBounds, Radius,
    SavePosition, ScrollbarWidth, Shrinkable, Stack, Text,
};
use warpui::fonts::{FamilyId, Properties, Weight};
use warpui::geometry::rect::RectF;
use warpui::platform::{Cursor, FilePickerConfiguration};
use warpui::prelude::Align;
use warpui::text_layout::ClipConfig;
use warpui::ui_components::components::{UiComponent, UiComponentStyles};
use warpui::ui_components::text_input::TextInput;
use warpui::{
    AppContext, Entity, EntityId, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle,
    WindowId,
};

use crate::appearance::Appearance;
use crate::claude_threads::{
    self, ClaudeThread, NEW_THREAD_COMMAND, ThreadGroups, ThreadProject, ThreadsConfig,
    TranscriptCache,
};
use crate::editor::{EditorView, Event as EditorEvent, SingleLineEditorOptions, TextOptions};
use crate::menu::{Event as MenuEvent, Menu, MenuItem, MenuItemFields};
use crate::terminal::CLIAgent;
use crate::terminal::cli_agent_sessions::{CLIAgentSessionStatus, CLIAgentSessionsModel};
use crate::workspace::WorkspaceAction;
use crate::workspace::util::get_context_target_terminal_view;

/// How often the transcripts are re-read while the panel is visible, so new
/// conversations and title changes show up.
const POLL_INTERVAL: Duration = Duration::from_secs(4);
/// Time given to a deleted thread's Claude session to exit after its pane closes, before its
/// transcript goes to the Trash.
const SESSION_EXIT_GRACE: Duration = Duration::from_millis(800);
/// Title of a thread whose first prompt hasn't been sent yet.
const NEW_THREAD_TITLE: &str = "Nova thread";
/// Threads listed per project, and under "Recentes", until "Mostrar mais" is clicked.
const RECENT_THREAD_LIMIT: usize = 10;
const MENU_WIDTH: f32 = 210.;
/// Gap between a thread's "⋯" button and the menu opening to its right.
const MENU_OFFSET: f32 = 8.;
const PANEL_PADDING: f32 = 8.;
const PROJECT_SPACING: f32 = 6.;
/// Space above every section header but the first.
const SECTION_SPACING: f32 = 14.;
const ROW_SPACING: f32 = 1.;
const ROW_HORIZONTAL_PADDING: f32 = 8.;
const PROJECT_ROW_VERTICAL_PADDING: f32 = 5.;
const THREAD_ROW_VERTICAL_PADDING: f32 = 7.;
const ROW_CORNER_RADIUS: f32 = 8.;
const BUTTON_CORNER_RADIUS: f32 = 6.;
/// Lines a thread's title up with its project's name, past the chevron.
const THREAD_INDENT: f32 = CHEVRON_SIZE + ICON_TEXT_GAP;
const CHEVRON_SIZE: f32 = 12.;
const SECTION_CHEVRON_SIZE: f32 = 10.;
const DETAIL_ICON_SIZE: f32 = 11.;
const LOGO_SIZE: f32 = 12.;
const DETAIL_ICON_GAP: f32 = 5.;
const ACTION_ICON_SIZE: f32 = 14.;
const ACTION_BUTTON_PADDING: f32 = 3.;
const ICON_TEXT_GAP: f32 = 6.;
/// Space between a thread's title and its branch line.
const LINE_SPACING: f32 = 3.;
const SECTION_FONT_SIZE: f32 = 12.;
const BRAND_FONT_SIZE: f32 = 16.;
const BRAND_HEIGHT: f32 = 56.;
const BRAND_ICON_SIZE: f32 = 16.;
const BRAND_BUTTON_SIZE: f32 = 26.;
const BRAND_TEXT: ColorU = ColorU {
    r: 0xf4,
    g: 0xf7,
    b: 0xff,
    a: 0xff,
};
/// The blue of "Code", light enough to read on the banner's blues.
const BRAND_BLUE: ColorU = ColorU {
    r: 0x7a,
    g: 0xb0,
    b: 0xff,
    a: 0xff,
};
const BRAND_BACKGROUND: ColorU = ColorU {
    r: 0x02,
    g: 0x07,
    b: 0x17,
    a: 0xff,
};

const PROJECT_FONT_SIZE: f32 = 13.;
const TITLE_FONT_SIZE: f32 = 13.;
const META_FONT_SIZE: f32 = 11.;
/// Height of a row's right side, which swaps between the age, status, and
/// buttons on hover; fixed so the swap doesn't move the row.
const META_HEIGHT: f32 = ACTION_ICON_SIZE + 2. * ACTION_BUTTON_PADDING;

/// The collapsible parts of the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadSection {
    Pinned,
    Projects,
    Recent,
}

impl ThreadSection {
    fn label(self) -> &'static str {
        match self {
            Self::Pinned => "Fixados",
            Self::Projects => "Projetos",
            Self::Recent => "Recentes",
        }
    }
}

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
    ToggleSection(ThreadSection),
    /// Shows every thread of a project, or of "Recentes" when `None`.
    ToggleShowAllThreads(Option<PathBuf>),
    NewThread(PathBuf),
    OpenThread {
        session_id: String,
        cwd: PathBuf,
    },
    ToggleThreadMenu(String),
    /// Swaps the open "⋯" menu between its actions and the projects the thread can move to.
    ShowThreadMenuPage {
        session_id: String,
        page: ThreadMenuPage,
    },
    TogglePinThread(String),
    MoveThreadToProject {
        session_id: String,
        project: PathBuf,
    },
    /// Lists a moved thread under the project of its folder again.
    ResetThreadProject(String),
    /// Turns the thread's title into an editor for a new name.
    StartRenameThread(String),
    /// Asks for confirmation in the row before deleting.
    AskDeleteThread(String),
    ConfirmDeleteThread(String),
    CancelDeleteThread,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadMenuPage {
    Actions,
    Move,
}

pub struct ClaudeThreadsView {
    config: ThreadsConfig,
    /// Every thread read from the transcripts, newest first.
    threads: Vec<ClaudeThread>,
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
    show_all_recent: bool,
    /// The thread whose row is asking "Excluir?".
    pending_delete: Option<String>,
    /// Threads on their way to the Trash, hidden from the list meanwhile.
    deleting: HashSet<String>,
    thread_menu: ViewHandle<Menu<ClaudeThreadsAction>>,
    /// The thread whose "⋯" menu is open.
    menu_open_for: Option<String>,
    /// Set while the menu swaps to its projects page, so the close that follows selecting
    /// "Mover para projeto" leaves it open.
    keep_menu_open: bool,
    scroll_state: ClippedScrollStateHandle,
    /// Hover state of each row and button, keyed by what it belongs to.
    mouse_states: RefCell<HashMap<String, MouseStateHandle>>,
    /// Drag state of each project header and thread row, keyed like
    /// `mouse_states`.
    drag_states: RefCell<HashMap<String, DraggableState>>,
    /// The window whose focused pane gets its thread highlighted.
    window_id: WindowId,
    search_editor: ViewHandle<EditorView>,
    rename_editor: ViewHandle<EditorView>,
    /// The thread whose title is being edited.
    renaming: Option<String>,
    /// Lowercased; while set, only the threads whose title or branch contain it are listed, with
    /// every section and project expanded.
    search_query: String,
}

impl ClaudeThreadsView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let search_editor = ctx.add_typed_action_view(|ctx| {
            let appearance = Appearance::as_ref(ctx);
            let options = SingleLineEditorOptions {
                text: TextOptions::ui_text(Some(TITLE_FONT_SIZE), appearance),
                ..Default::default()
            };
            let mut editor = EditorView::single_line(options, ctx);
            editor.set_placeholder_text("Buscar threads...", ctx);
            editor
        });
        ctx.subscribe_to_view(&search_editor, |me, editor, event, ctx| {
            if let EditorEvent::Edited(_) = event {
                me.search_query = editor.as_ref(ctx).buffer_text(ctx).trim().to_lowercase();
                ctx.notify();
            }
        });
        let rename_editor = ctx.add_typed_action_view(|ctx| {
            let appearance = Appearance::as_ref(ctx);
            let options = SingleLineEditorOptions {
                text: TextOptions::ui_text(Some(TITLE_FONT_SIZE), appearance),
                ..Default::default()
            };
            EditorView::single_line(options, ctx)
        });
        ctx.subscribe_to_view(&rename_editor, |me, _, event, ctx| match event {
            EditorEvent::Enter => me.finish_rename(ctx),
            EditorEvent::Escape | EditorEvent::Blurred => me.cancel_rename(ctx),
            _ => {}
        });
        let thread_menu = ctx.add_typed_action_view(|_| {
            Menu::new()
                .prevent_interaction_with_other_elements()
                .with_drop_shadow()
                .with_width(MENU_WIDTH)
        });
        ctx.subscribe_to_view(&thread_menu, |me, _, event, ctx| match event {
            MenuEvent::Close { via_select_item } => {
                if *via_select_item && std::mem::take(&mut me.keep_menu_open) {
                    return;
                }
                me.menu_open_for = None;
                ctx.notify();
            }
            MenuEvent::ItemSelected | MenuEvent::ItemHovered => {}
        });
        // Status dots follow the Claude sessions running in the tabs.
        ctx.subscribe_to_model(&CLIAgentSessionsModel::handle(ctx), |me, _, _, ctx| {
            if me.is_visible {
                ctx.notify();
            }
        });
        Self {
            config: claude_threads::load_config(),
            threads: Vec::new(),
            has_scanned: false,
            transcript_cache: Arc::new(Mutex::new(TranscriptCache::new())),
            scan_epoch: 0,
            is_visible: false,
            show_all_threads: HashSet::new(),
            show_all_recent: false,
            pending_delete: None,
            deleting: HashSet::new(),
            thread_menu,
            menu_open_for: None,
            keep_menu_open: false,
            scroll_state: ClippedScrollStateHandle::default(),
            mouse_states: RefCell::new(HashMap::new()),
            drag_states: RefCell::new(HashMap::new()),
            window_id: ctx.window_id(),
            search_editor,
            rename_editor,
            renaming: None,
            search_query: String::new(),
        }
    }

    /// Re-renders so the thread of the newly focused pane gets highlighted.
    pub fn active_pane_changed(&mut self, ctx: &mut ViewContext<Self>) {
        if self.is_visible {
            ctx.notify();
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
        let cache = self.transcript_cache.clone();
        ctx.spawn(
            async move {
                Timer::after(delay).await;
                let mut cache = cache.lock().unwrap_or_else(PoisonError::into_inner);
                claude_threads::scan(&mut cache)
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
        if self
            .config
            .projects
            .iter()
            .any(|project| project.path == path)
        {
            return;
        }
        self.config.projects.push(ThreadProject::new(path));
        self.config.projects_collapsed = false;
        self.save_config();
        ctx.notify();
    }

    fn remove_project(&mut self, path: &Path, ctx: &mut ViewContext<Self>) {
        self.config.projects.retain(|project| project.path != path);
        self.config.moved.retain(|_, project| project != path);
        self.show_all_threads.remove(path);
        self.save_config();
        ctx.notify();
    }

    fn toggle_project_collapsed(&mut self, path: &Path, ctx: &mut ViewContext<Self>) {
        if let Some(project) = self.config.projects.iter_mut().find(|p| p.path == path) {
            project.collapsed = !project.collapsed;
            self.save_config();
            ctx.notify();
        }
    }

    fn toggle_section(&mut self, section: ThreadSection, ctx: &mut ViewContext<Self>) {
        let collapsed = match section {
            ThreadSection::Pinned => &mut self.config.pinned_collapsed,
            ThreadSection::Projects => &mut self.config.projects_collapsed,
            ThreadSection::Recent => &mut self.config.recent_collapsed,
        };
        *collapsed = !*collapsed;
        self.save_config();
        ctx.notify();
    }

    fn is_section_collapsed(&self, section: ThreadSection) -> bool {
        self.search_query.is_empty()
            && match section {
                ThreadSection::Pinned => self.config.pinned_collapsed,
                ThreadSection::Projects => self.config.projects_collapsed,
                ThreadSection::Recent => self.config.recent_collapsed,
            }
    }

    fn toggle_pin(&mut self, session_id: &str, ctx: &mut ViewContext<Self>) {
        if self.config.is_pinned(session_id) {
            self.config.pinned.retain(|id| id != session_id);
        } else {
            self.config.pinned.insert(0, session_id.to_owned());
            self.config.pinned_collapsed = false;
        }
        self.save_config();
        ctx.notify();
    }

    fn move_to_project(&mut self, session_id: &str, project: &Path, ctx: &mut ViewContext<Self>) {
        self.config
            .moved
            .insert(session_id.to_owned(), project.to_path_buf());
        if let Some(project) = self.config.projects.iter_mut().find(|p| p.path == project) {
            project.collapsed = false;
        }
        self.config.projects_collapsed = false;
        self.save_config();
        ctx.notify();
    }

    fn reset_project(&mut self, session_id: &str, ctx: &mut ViewContext<Self>) {
        if self.config.moved.remove(session_id).is_some() {
            self.save_config();
            ctx.notify();
        }
    }

    fn save_config(&self) {
        if let Err(err) = claude_threads::save_config(&self.config) {
            log::warn!("Failed to save the Claude threads config: {err}");
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
        let Some(thread) = self.find_thread(session_id) else {
            return;
        };
        let Some(command) = thread.resume_command() else {
            return;
        };
        let group = self.config.project_of(thread).map(project_group_name);
        ctx.dispatch_typed_action_deferred(WorkspaceAction::RunCommandInNewTab {
            directory: cwd.to_path_buf(),
            command,
            group,
        });
    }

    /// Closes the tab running the thread, if any, then moves its transcript to the Trash.
    fn delete_thread(&mut self, session_id: &str, ctx: &mut ViewContext<Self>) {
        self.pending_delete = None;
        self.config.forget_thread(session_id);
        self.save_config();
        let running_in = CLIAgentSessionsModel::as_ref(ctx)
            .session_by_agent_session_id(session_id)
            .map(|(terminal_view_id, _)| terminal_view_id);
        if let Some(terminal_view_id) = running_in {
            ctx.dispatch_typed_action_deferred(WorkspaceAction::CloseClaudeThreadPane {
                terminal_view_id,
            });
        }
        // A thread that hasn't written a transcript yet goes away with its tab.
        let Some(thread) = self
            .find_thread(session_id)
            .filter(|thread| !thread.transcript.as_os_str().is_empty())
            .cloned()
        else {
            ctx.notify();
            return;
        };
        let delay = if running_in.is_some() {
            SESSION_EXIT_GRACE
        } else {
            Duration::ZERO
        };
        let session_id = session_id.to_owned();
        self.deleting.insert(session_id.clone());
        ctx.spawn(
            async move {
                Timer::after(delay).await;
                claude_threads::trash_thread(&thread)
            },
            move |me, result, ctx| {
                me.deleting.remove(&session_id);
                match result {
                    Ok(()) => me.threads.retain(|t| t.session_id != session_id),
                    Err(err) => log::warn!("Failed to move Claude thread to the Trash: {err}"),
                }
                ctx.notify();
            },
        );
        ctx.notify();
    }

    fn find_thread(&self, session_id: &str) -> Option<&ClaudeThread> {
        self.threads
            .iter()
            .find(|thread| thread.session_id == session_id)
    }

    fn start_rename(&mut self, session_id: &str, ctx: &mut ViewContext<Self>) {
        let Some(title) = self
            .find_thread(session_id)
            .map(|thread| thread.title.clone())
        else {
            return;
        };
        self.pending_delete = None;
        self.renaming = Some(session_id.to_owned());
        self.rename_editor.update(ctx, |editor, ctx| {
            editor.clear_buffer_and_reset_undo_stack(ctx);
            editor.insert_selected_text(&title, ctx);
        });
        ctx.focus(&self.rename_editor);
        ctx.notify();
    }

    fn finish_rename(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(session_id) = self.renaming.take() else {
            return;
        };
        let name = self.rename_editor.as_ref(ctx).buffer_text(ctx);
        if let Some(thread) = self.find_thread(&session_id).cloned()
            && name.trim() != thread.title
        {
            match claude_threads::rename_thread(&thread, &name) {
                // Shown right away; the next scan reads it back from the transcript.
                Ok(title) => {
                    for thread in &mut self.threads {
                        if thread.session_id == session_id {
                            thread.title = title.clone();
                        }
                    }
                }
                Err(err) => log::warn!("Failed to rename Claude thread: {err}"),
            }
        }
        ctx.notify();
    }

    fn cancel_rename(&mut self, ctx: &mut ViewContext<Self>) {
        if self.renaming.take().is_some() {
            ctx.notify();
        }
    }

    /// Opens the "⋯" menu of a thread, or closes it when it's already open there.
    fn toggle_thread_menu(&mut self, session_id: &str, ctx: &mut ViewContext<Self>) {
        if self.menu_open_for.as_deref() == Some(session_id) {
            self.menu_open_for = None;
            ctx.notify();
            return;
        }
        let items = self.thread_menu_items(session_id, ctx);
        self.thread_menu
            .update(ctx, |menu, ctx| menu.set_items(items, ctx));
        self.pending_delete = None;
        self.keep_menu_open = false;
        self.menu_open_for = Some(session_id.to_owned());
        ctx.notify();
    }

    fn thread_menu_items(
        &self,
        session_id: &str,
        app: &AppContext,
    ) -> Vec<MenuItem<ClaudeThreadsAction>> {
        let id = session_id.to_owned();
        let thread = self.find_thread(session_id);
        let mut items = Vec::new();
        // Threads that haven't written a transcript yet have nothing to store a name in.
        if thread.is_some_and(|thread| !thread.transcript.as_os_str().is_empty()) {
            items.push(
                MenuItemFields::new("Renomear")
                    .with_icon(WarpIcon::Pencil)
                    .with_on_select_action(ClaudeThreadsAction::StartRenameThread(id.clone()))
                    .into_item(),
            );
        }
        let pin_label = if self.config.is_pinned(session_id) {
            "Desafixar"
        } else {
            "Fixar"
        };
        items.push(
            MenuItemFields::new(pin_label)
                .with_icon(WarpIcon::Pin)
                .with_on_select_action(ClaudeThreadsAction::TogglePinThread(id.clone()))
                .into_item(),
        );
        if !self.move_destinations(session_id).is_empty()
            || self.config.moved.contains_key(session_id)
        {
            items.push(
                MenuItemFields::new("Mover para projeto")
                    .with_icon(WarpIcon::Folder)
                    .with_right_side_icon(WarpIcon::ChevronRight)
                    .with_on_select_action(ClaudeThreadsAction::ShowThreadMenuPage {
                        session_id: id.clone(),
                        page: ThreadMenuPage::Move,
                    })
                    .into_item(),
            );
        }
        items.push(MenuItem::Separator);
        let red = Appearance::as_ref(app).theme().ansi_fg_red();
        items.push(
            MenuItemFields::new("Excluir")
                .with_icon(WarpIcon::Trash)
                .with_override_text_color(red)
                .with_override_icon_color(WarpThemeFill::Solid(red))
                .with_on_select_action(ClaudeThreadsAction::AskDeleteThread(id))
                .into_item(),
        );
        items
    }

    /// The projects a thread can move to: every project but the one it's listed under.
    fn move_destinations(&self, session_id: &str) -> Vec<&ThreadProject> {
        let current_project = self
            .find_thread(session_id)
            .and_then(|thread| self.config.project_of(thread));
        self.config
            .projects
            .iter()
            .filter(|project| Some(project.path.as_path()) != current_project)
            .collect()
    }

    /// The second page of the "⋯" menu: back to the first page, then the projects.
    fn move_menu_items(&self, session_id: &str) -> Vec<MenuItem<ClaudeThreadsAction>> {
        let id = session_id.to_owned();
        let mut items = vec![
            MenuItemFields::new("Mover para projeto")
                .with_icon(WarpIcon::ChevronLeft)
                .with_on_select_action(ClaudeThreadsAction::ShowThreadMenuPage {
                    session_id: id.clone(),
                    page: ThreadMenuPage::Actions,
                })
                .into_item(),
            MenuItem::Separator,
        ];
        for project in self.move_destinations(session_id) {
            items.push(
                MenuItemFields::new(project.name())
                    .with_icon(WarpIcon::Folder)
                    .with_on_select_action(ClaudeThreadsAction::MoveThreadToProject {
                        session_id: id.clone(),
                        project: project.path.clone(),
                    })
                    .into_item(),
            );
        }
        if self.config.moved.contains_key(session_id) {
            items.push(MenuItem::Separator);
            items.push(
                MenuItemFields::new("Voltar para a pasta de origem")
                    .with_icon(WarpIcon::ReverseLeft)
                    .with_on_select_action(ClaudeThreadsAction::ResetThreadProject(id))
                    .into_item(),
            );
        }
        items
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
        let projects = &mut self.config.projects;
        let Some(from) = projects.iter().position(|p| p.path == path) else {
            return;
        };
        let cursor_y = position.center().y();
        let to = projects.iter().position(|project| {
            ctx.element_position_by_id(project_block_position_id(&project.path))
                .is_some_and(|rect| rect.min_y() <= cursor_y && cursor_y < rect.max_y())
        });
        if let Some(to) = to
            && to != from
        {
            let project = projects.remove(from);
            projects.insert(to, project);
            ctx.notify();
        }
    }
}

/// Save-position id of a project's block (header and threads), used to find
/// where a dragged project would land.
fn project_block_position_id(path: &Path) -> String {
    format!("claude_threads_project_block:{}", path.display())
}

/// Save-position id of a thread's "⋯" button, which its menu hangs from.
fn thread_menu_button_position_id(session_id: &str) -> String {
    format!("claude_threads_menu_button:{session_id}")
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
            ClaudeThreadsAction::ToggleSection(section) => self.toggle_section(*section, ctx),
            ClaudeThreadsAction::ToggleShowAllThreads(Some(path)) => {
                if !self.show_all_threads.remove(path) {
                    self.show_all_threads.insert(path.clone());
                }
                ctx.notify();
            }
            ClaudeThreadsAction::ToggleShowAllThreads(None) => {
                self.show_all_recent = !self.show_all_recent;
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
            ClaudeThreadsAction::ToggleThreadMenu(session_id) => {
                self.toggle_thread_menu(session_id, ctx);
            }
            ClaudeThreadsAction::ShowThreadMenuPage { session_id, page } => {
                let items = match page {
                    ThreadMenuPage::Actions => self.thread_menu_items(session_id, ctx),
                    ThreadMenuPage::Move => self.move_menu_items(session_id),
                };
                self.thread_menu
                    .update(ctx, |menu, ctx| menu.set_items(items, ctx));
                self.keep_menu_open = true;
                ctx.notify();
            }
            ClaudeThreadsAction::TogglePinThread(session_id) => self.toggle_pin(session_id, ctx),
            ClaudeThreadsAction::MoveThreadToProject {
                session_id,
                project,
            } => self.move_to_project(session_id, project, ctx),
            ClaudeThreadsAction::ResetThreadProject(session_id) => {
                self.reset_project(session_id, ctx);
            }
            ClaudeThreadsAction::StartRenameThread(session_id) => {
                self.start_rename(session_id, ctx);
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
                self.save_config();
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
    /// Titles of threads that aren't open, a step behind the open ones.
    idle_text: WarpThemeFill,
    sub_text: WarpThemeFill,
    /// Branch and folder line under a thread's title.
    detail_text: WarpThemeFill,
    row_hover: WarpThemeFill,
    /// The thread open in the focused pane.
    row_active: WarpThemeFill,
    button_hover: WarpThemeFill,
    working: WarpThemeFill,
    claude: WarpThemeFill,
}

impl Palette {
    fn new(appearance: &Appearance) -> Self {
        let theme = appearance.theme();
        let main_text = theme.main_text_color(theme.background());
        let sub_text = theme.sub_text_color(theme.background());
        Self {
            main_text,
            idle_text: main_text.with_opacity(82),
            sub_text,
            detail_text: sub_text.with_opacity(75),
            row_hover: internal_colors::fg_overlay_1(theme),
            row_active: internal_colors::fg_overlay_2(theme),
            button_hover: internal_colors::fg_overlay_3(theme),
            working: theme.accent(),
            claude: CLIAgent::Claude
                .brand_color()
                .map(WarpThemeFill::Solid)
                .unwrap_or_else(|| theme.accent()),
        }
    }
}

/// Fonts used by the rows, resolved once per render.
#[derive(Clone, Copy)]
struct Fonts {
    family: FamilyId,
    medium: Properties,
}

impl Fonts {
    fn new(appearance: &Appearance) -> Self {
        Self {
            family: appearance.ui_font_family(),
            medium: Properties::default().weight(Weight::Medium),
        }
    }
}

/// What every row of a render needs.
#[derive(Clone, Copy)]
struct RowContext<'a> {
    now: SystemTime,
    active_terminal_id: Option<EntityId>,
    palette: Palette,
    fonts: Fonts,
    appearance: &'a Appearance,
    app: &'a AppContext,
}

impl View for ClaudeThreadsView {
    fn ui_name() -> &'static str {
        "ClaudeThreadsView"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let rows = RowContext {
            now: SystemTime::now(),
            active_terminal_id: get_context_target_terminal_view(self.window_id, app)
                .map(|view| view.id()),
            palette: Palette::new(appearance),
            fonts: Fonts::new(appearance),
            appearance,
            app,
        };
        let groups =
            claude_threads::group_threads(self.listed_threads(rows.now, app), &self.config);
        let is_searching = !self.search_query.is_empty();

        let mut list = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(ROW_SPACING);
        let mut is_first_section = true;
        let mut add_section_header = |list: &mut Flex, section| {
            list.add_child(self.render_section_header(section, is_first_section, rows));
            is_first_section = false;
        };

        if !groups.pinned.is_empty() {
            add_section_header(&mut list, ThreadSection::Pinned);
            if !self.is_section_collapsed(ThreadSection::Pinned) {
                for thread in &groups.pinned {
                    let project = self.config.project_of(thread);
                    list.add_child(self.render_thread_row(thread, project, 0., rows));
                }
            }
        }

        let has_project_matches = groups
            .by_project
            .values()
            .any(|threads| !threads.is_empty());
        if !is_searching || has_project_matches {
            add_section_header(&mut list, ThreadSection::Projects);
            if !self.is_section_collapsed(ThreadSection::Projects) {
                list.add_child(self.render_projects(&groups, rows));
            }
        }

        if !groups.recent.is_empty() {
            add_section_header(&mut list, ThreadSection::Recent);
            if !self.is_section_collapsed(ThreadSection::Recent) {
                self.add_thread_rows(&mut list, &groups.recent, None, 0., rows);
            }
        }

        let has_matches =
            !groups.pinned.is_empty() || has_project_matches || !groups.recent.is_empty();
        if is_searching && !has_matches {
            list.add_child(render_note("Nenhuma thread encontrada", 0., rows));
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

        let panel = Flex::column()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(self.render_brand(rows.fonts))
            .with_child(self.render_search_bar(rows.palette))
            .with_child(Shrinkable::new(1., scrollable).finish())
            .finish();
        let mut stack = Stack::new().with_child(panel);
        if let Some(session_id) = &self.menu_open_for {
            stack.add_positioned_overlay_child(
                ChildView::new(&self.thread_menu).finish(),
                OffsetPositioning::offset_from_save_position_element(
                    thread_menu_button_position_id(session_id),
                    vec2f(MENU_OFFSET, 0.),
                    PositionedElementOffsetBounds::WindowByPosition,
                    PositionedElementAnchor::TopRight,
                    ChildAnchor::TopLeft,
                ),
            );
        }
        stack.finish()
    }
}

impl ClaudeThreadsView {
    /// The scanned threads narrowed down to the search query, minus the ones being deleted.
    /// Claude sessions that just started come first, before Claude writes their transcript, so a
    /// new thread is in the list right away.
    fn listed_threads(&self, now: SystemTime, app: &AppContext) -> Vec<ClaudeThread> {
        let mut threads = self.pending_threads(now, app);
        threads.extend(self.threads.iter().cloned());
        threads.retain(|thread| !self.deleting.contains(&thread.session_id));
        if !self.search_query.is_empty() {
            threads.retain(|thread| {
                thread.title.to_lowercase().contains(&self.search_query)
                    || thread
                        .git_branch
                        .as_ref()
                        .is_some_and(|branch| branch.to_lowercase().contains(&self.search_query))
            });
        }
        threads
    }

    /// Claude sessions running in a tab that no scanned thread covers yet.
    fn pending_threads(&self, now: SystemTime, app: &AppContext) -> Vec<ClaudeThread> {
        CLIAgentSessionsModel::as_ref(app)
            .sessions()
            .filter(|(_, session)| session.agent == CLIAgent::Claude)
            .filter_map(|(_, session)| {
                let context = &session.session_context;
                let session_id = context.session_id.clone()?;
                let cwd = PathBuf::from(context.cwd.as_deref()?);
                let is_scanned = self.find_thread(&session_id).is_some();
                (!is_scanned).then(|| ClaudeThread {
                    title: context
                        .query
                        .as_deref()
                        .map(str::trim)
                        .filter(|query| !query.is_empty())
                        .unwrap_or(NEW_THREAD_TITLE)
                        .to_owned(),
                    session_id,
                    cwd,
                    updated_at: now,
                    transcript: PathBuf::new(),
                    git_branch: None,
                })
            })
            .collect()
    }

    fn render_search_bar(&self, palette: Palette) -> Box<dyn Element> {
        let row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(ICON_TEXT_GAP)
            .with_child(render_icon(
                WarpIcon::Search,
                palette.sub_text,
                ACTION_ICON_SIZE,
            ))
            .with_child(
                Shrinkable::new(
                    1.,
                    TextInput::new(
                        self.search_editor.clone(),
                        UiComponentStyles::default()
                            .set_background(ElementFill::None)
                            .set_border_radius(CornerRadius::with_all(Radius::Pixels(0.)))
                            .set_border_width(0.),
                    )
                    .build()
                    .finish(),
                )
                .finish(),
            )
            .finish();
        Container::new(
            Container::new(row)
                .with_horizontal_padding(ROW_HORIZONTAL_PADDING)
                .with_vertical_padding(5.)
                .with_background(palette.row_hover)
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(ROW_CORNER_RADIUS)))
                .finish(),
        )
        .with_horizontal_padding(PANEL_PADDING)
        .with_padding_bottom(PANEL_PADDING)
        .finish()
    }

    /// The name of a section with a chevron after it; clicking it collapses or expands the
    /// section. "Projetos" also gets the button that adds a project folder, shown on hover.
    fn render_section_header(
        &self,
        section: ThreadSection,
        is_first: bool,
        rows: RowContext<'_>,
    ) -> Box<dyn Element> {
        let RowContext {
            palette,
            fonts,
            appearance,
            ..
        } = rows;
        let chevron = if self.is_section_collapsed(section) {
            WarpIcon::ChevronRight
        } else {
            WarpIcon::ChevronDown
        };
        let add_button = (section == ThreadSection::Projects).then(|| {
            render_icon_button(
                WarpIcon::Plus,
                "Adicionar pasta",
                self.mouse_state("header:add".to_owned()),
                ClaudeThreadsAction::AddProject,
                palette,
                appearance,
            )
        });
        let header = Hoverable::new(
            self.mouse_state(format!("section:{section:?}")),
            move |hover_state| {
                let color = if hover_state.is_hovered() {
                    palette.main_text
                } else {
                    palette.sub_text
                };
                let label = Flex::row()
                    .with_main_axis_size(MainAxisSize::Min)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(4.)
                    .with_child(
                        Text::new_inline(section.label(), fonts.family, SECTION_FONT_SIZE)
                            .with_style(fonts.medium)
                            .with_color(color.into())
                            .finish(),
                    )
                    .with_child(render_icon(chevron, color, SECTION_CHEVRON_SIZE))
                    .finish();
                let mut row = Flex::row()
                    .with_main_axis_size(MainAxisSize::Max)
                    .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_child(label);
                let trailing = add_button.filter(|_| hover_state.is_hovered());
                row.add_child(
                    ConstrainedBox::new(trailing.unwrap_or_else(|| Empty::new().finish()))
                        .with_height(META_HEIGHT)
                        .finish(),
                );
                Container::new(row.finish())
                    .with_horizontal_padding(ROW_HORIZONTAL_PADDING)
                    .with_vertical_padding(3.)
                    .finish()
            },
        )
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(ClaudeThreadsAction::ToggleSection(section));
        })
        .with_defer_events_to_children()
        .finish();
        Container::new(header)
            .with_padding_top(if is_first { 0. } else { SECTION_SPACING })
            .finish()
    }

    /// Every project folder with its threads; while searching, only the projects with matches,
    /// expanded.
    fn render_projects(&self, groups: &ThreadGroups, rows: RowContext<'_>) -> Box<dyn Element> {
        let is_searching = !self.search_query.is_empty();
        let mut list = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(PROJECT_SPACING);
        if self.config.projects.is_empty() {
            list.add_child(render_empty_state(rows));
        }
        for project in &self.config.projects {
            let threads = groups
                .by_project
                .get(&project.path)
                .map(Vec::as_slice)
                .unwrap_or_default();
            if is_searching && threads.is_empty() {
                continue;
            }
            let mut block = Flex::column()
                .with_main_axis_size(MainAxisSize::Min)
                .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
                .with_spacing(ROW_SPACING)
                .with_child(self.render_project_row(project, threads.len(), rows));
            if is_searching || !project.collapsed {
                if threads.is_empty() {
                    if self.has_scanned {
                        block.add_child(render_note("Nenhuma conversa ainda", THREAD_INDENT, rows));
                    }
                } else {
                    self.add_thread_rows(
                        &mut block,
                        threads,
                        Some(&project.path),
                        THREAD_INDENT,
                        rows,
                    );
                }
            }
            list.add_child(
                SavePosition::new(block.finish(), &project_block_position_id(&project.path))
                    .finish(),
            );
        }
        list.finish()
    }

    /// The button that hides the sidebar and the app's name, on the banner image across the top
    /// of the panel.
    fn render_brand(&self, fonts: Fonts) -> Box<dyn Element> {
        let toggle_button = Hoverable::new(self.mouse_state("brand:toggle".to_owned()), |state| {
            let (icon_opacity, background_opacity) = if state.is_hovered() {
                (100, 16)
            } else {
                (72, 0)
            };
            let icon = ConstrainedBox::new(
                WarpIcon::LeftSidebarClose
                    .to_warpui_icon(brand_color(BRAND_TEXT, icon_opacity))
                    .finish(),
            )
            .with_width(BRAND_ICON_SIZE)
            .with_height(BRAND_ICON_SIZE)
            .finish();
            ConstrainedBox::new(
                Container::new(Align::new(icon).finish())
                    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(
                        BUTTON_CORNER_RADIUS,
                    )))
                    .with_background(brand_color(BRAND_TEXT, background_opacity))
                    .finish(),
            )
            .with_width(BRAND_BUTTON_SIZE)
            .with_height(BRAND_BUTTON_SIZE)
            .finish()
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(|ctx, _, _| ctx.dispatch_typed_action(WorkspaceAction::ToggleClaudeThreadsPanel))
        .finish();

        let name = Flex::row()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(5.)
            .with_child(
                Text::new_inline("Picolas", fonts.family, BRAND_FONT_SIZE)
                    .with_style(Properties::default().weight(Weight::Semibold))
                    .with_color(BRAND_TEXT)
                    .finish(),
            )
            .with_child(
                Text::new_inline("Code", fonts.family, BRAND_FONT_SIZE)
                    .with_style(Properties::default().weight(Weight::Semibold))
                    .with_color(BRAND_BLUE)
                    .finish(),
            )
            .finish();
        let content = ConstrainedBox::new(
            Container::new(
                Flex::row()
                    .with_main_axis_size(MainAxisSize::Max)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(10.)
                    .with_child(toggle_button)
                    .with_child(name)
                    .finish(),
            )
            .with_padding_left(PANEL_PADDING + 4.)
            .with_padding_right(PANEL_PADDING)
            .finish(),
        )
        .with_height(BRAND_HEIGHT)
        .finish();

        let background = Image::new(
            bundled_or_fetched_asset!("png/picolas_banner.png"),
            CacheOption::Original,
        )
        .cover()
        .finish();
        let banner = Stack::new()
            .with_child(
                ConstrainedBox::new(background)
                    .with_height(BRAND_HEIGHT)
                    .finish(),
            )
            .with_child(content);

        Container::new(Clipped::new(banner.finish()).finish())
            .with_background(WarpThemeFill::Solid(BRAND_BACKGROUND))
            .with_margin_bottom(PANEL_PADDING)
            .finish()
    }

    /// Chevron, folder name and thread count. On hover the count gives way to
    /// buttons for a new thread and removing the project. Clicking the row
    /// collapses or expands it.
    fn render_project_row(
        &self,
        project: &ThreadProject,
        thread_count: usize,
        rows: RowContext<'_>,
    ) -> Box<dyn Element> {
        let RowContext {
            palette,
            fonts,
            appearance,
            ..
        } = rows;
        let path = project.path.clone();
        let key = path.display().to_string();
        let name = project.name();
        let chevron = if project.collapsed && self.search_query.is_empty() {
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
                let mut meta = Flex::row()
                    .with_main_axis_size(MainAxisSize::Min)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(2.);
                if hover_state.is_hovered() {
                    meta.add_child(remove_button);
                    meta.add_child(new_thread_button);
                } else if thread_count > 0 {
                    meta.add_child(
                        Text::new_inline(thread_count.to_string(), fonts.family, META_FONT_SIZE)
                            .with_color(palette.detail_text.into())
                            .finish(),
                    );
                }
                let meta = ConstrainedBox::new(Align::new(meta.finish()).right().finish())
                    .with_height(META_HEIGHT)
                    .finish();

                let label = Flex::row()
                    .with_main_axis_size(MainAxisSize::Max)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(ICON_TEXT_GAP)
                    .with_child(render_icon(chevron, palette.sub_text, CHEVRON_SIZE))
                    .with_child(
                        Shrinkable::new(
                            1.,
                            Text::new_inline(name, fonts.family, PROJECT_FONT_SIZE)
                                .with_style(fonts.medium)
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
                let background = hover_state.is_hovered().then_some(palette.row_hover);
                render_row_container(row, PROJECT_ROW_VERTICAL_PADDING, background)
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

    /// The latest `RECENT_THREAD_LIMIT` threads of a project (or of "Recentes", when `project` is
    /// `None`) and the toggle that shows the rest.
    fn add_thread_rows(
        &self,
        list: &mut Flex,
        threads: &[ClaudeThread],
        project: Option<&Path>,
        indent: f32,
        rows: RowContext<'_>,
    ) {
        let show_all = !self.search_query.is_empty()
            || match project {
                Some(path) => self.show_all_threads.contains(path),
                None => self.show_all_recent,
            };
        let shown = if show_all {
            threads.len()
        } else {
            threads.len().min(RECENT_THREAD_LIMIT)
        };
        for thread in &threads[..shown] {
            list.add_child(self.render_thread_row(thread, project, indent, rows));
        }

        if !self.search_query.is_empty() || threads.len() <= RECENT_THREAD_LIMIT {
            return;
        }
        let RowContext { palette, fonts, .. } = rows;
        let (label, icon) = if show_all {
            ("Mostrar menos".to_owned(), WarpIcon::ChevronUp)
        } else {
            (
                format!("Mostrar mais ({})", threads.len() - RECENT_THREAD_LIMIT),
                WarpIcon::ChevronDown,
            )
        };
        let key = project.map(PathBuf::from);
        let mouse_state = self.mouse_state(format!("show-all:{key:?}"));
        list.add_child(
            Hoverable::new(mouse_state, move |hover_state| {
                let color = if hover_state.is_hovered() {
                    palette.main_text
                } else {
                    palette.sub_text
                };
                let content = Flex::row()
                    .with_main_axis_size(MainAxisSize::Max)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(ICON_TEXT_GAP)
                    .with_child(render_icon(icon, color, CHEVRON_SIZE))
                    .with_child(
                        Text::new_inline(label, fonts.family, META_FONT_SIZE)
                            .with_color(color.into())
                            .finish(),
                    )
                    .finish();
                let content = Container::new(content).with_padding_left(indent).finish();
                render_row_container(content, PROJECT_ROW_VERTICAL_PADDING, None)
            })
            .with_cursor(Cursor::PointingHand)
            .on_click(move |ctx, _, _| {
                ctx.dispatch_typed_action(ClaudeThreadsAction::ToggleShowAllThreads(key.clone()));
            })
            .finish(),
        );
    }

    /// Two lines: the title with its age (or what the session is doing), and the branch it runs
    /// on. The thread open in the focused pane gets a raised card. On hover the age gives way to
    /// the "⋯" menu and the pin button; deleting asks for confirmation in the row.
    fn render_thread_row(
        &self,
        thread: &ClaudeThread,
        project: Option<&Path>,
        indent: f32,
        rows: RowContext<'_>,
    ) -> Box<dyn Element> {
        let RowContext {
            now,
            active_terminal_id,
            palette,
            fonts,
            appearance,
            app,
        } = rows;
        let running_session =
            CLIAgentSessionsModel::as_ref(app).session_by_agent_session_id(&thread.session_id);
        let is_active = running_session
            .is_some_and(|(terminal_view_id, _)| Some(terminal_view_id) == active_terminal_id);
        let status = running_session.map(|(_, session)| {
            if matches!(session.status, CLIAgentSessionStatus::Blocked { .. }) {
                ThreadStatus::Blocked
            } else if session.is_working_on_prompt() {
                ThreadStatus::Working
            } else {
                ThreadStatus::Open
            }
        });
        let title = thread.title.clone();
        let age = claude_threads::format_age(thread.updated_at, now);
        // Threads outside their project's folder, in a subfolder, or in a worktree say where they
        // run.
        let detail = match &thread.git_branch {
            Some(branch) => Some((WarpIcon::GitBranch, branch.clone())),
            None => (Some(thread.cwd.as_path()) != project)
                .then(|| ThreadProject::new(thread.cwd.clone()).name())
                .map(|folder| (WarpIcon::Folder, folder)),
        };
        let action = ClaudeThreadsAction::OpenThread {
            session_id: thread.session_id.clone(),
            cwd: thread.cwd.clone(),
        };
        let session_id = &thread.session_id;
        let is_confirming = self.pending_delete.as_deref() == Some(session_id.as_str());
        let is_menu_open = self.menu_open_for.as_deref() == Some(session_id.as_str());
        let is_pinned = self.config.is_pinned(session_id);
        let rename_input = (self.renaming.as_deref() == Some(session_id.as_str())).then(|| {
            TextInput::new(
                self.rename_editor.clone(),
                UiComponentStyles::default()
                    .set_background(ElementFill::None)
                    .set_border_radius(CornerRadius::with_all(Radius::Pixels(0.)))
                    .set_border_width(0.),
            )
            .build()
            .finish()
        });
        let menu_button = SavePosition::new(
            render_icon_button(
                WarpIcon::DotsHorizontal,
                "Mais opções",
                self.mouse_state(format!("thread-menu:{session_id}")),
                ClaudeThreadsAction::ToggleThreadMenu(session_id.clone()),
                palette,
                appearance,
            ),
            &thread_menu_button_position_id(session_id),
        )
        .finish();
        let pin_button = render_icon_button(
            if is_pinned {
                WarpIcon::PinFilled
            } else {
                WarpIcon::Pin
            },
            if is_pinned { "Desafixar" } else { "Fixar" },
            self.mouse_state(format!("thread-pin:{session_id}")),
            ClaudeThreadsAction::TogglePinThread(session_id.clone()),
            palette,
            appearance,
        );
        let confirm_buttons = is_confirming.then(|| {
            [
                render_icon_button(
                    WarpIcon::Check,
                    "Fechar e mover para a Lixeira",
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
                    .with_spacing(2.);
                if let Some(buttons) = confirm_buttons {
                    meta.add_child(
                        Container::new(
                            Text::new_inline("Excluir?", fonts.family, META_FONT_SIZE)
                                .with_color(palette.main_text.into())
                                .finish(),
                        )
                        .with_padding_right(2.)
                        .finish(),
                    );
                    for button in buttons {
                        meta.add_child(button);
                    }
                } else if rename_input.is_some() {
                    // The editor takes the whole line.
                } else if hover_state.is_hovered() || is_menu_open {
                    meta.add_child(menu_button);
                    meta.add_child(pin_button);
                } else {
                    match status {
                        Some(status @ (ThreadStatus::Working | ThreadStatus::Blocked)) => {
                            let color = status.color(palette);
                            meta.add_child(
                                Text::new_inline("● ", fonts.family, META_FONT_SIZE)
                                    .with_color(color.into())
                                    .finish(),
                            );
                            meta.add_child(
                                Text::new_inline(status.label(), fonts.family, META_FONT_SIZE)
                                    .with_style(fonts.medium)
                                    .with_color(color.into())
                                    .finish(),
                            );
                        }
                        Some(ThreadStatus::Open) | None => {
                            if status.is_some() {
                                meta.add_child(
                                    Text::new_inline("● ", fonts.family, META_FONT_SIZE)
                                        .with_color(palette.sub_text.into())
                                        .finish(),
                                );
                            }
                            meta.add_child(
                                Text::new_inline(age, fonts.family, META_FONT_SIZE)
                                    .with_color(palette.sub_text.into())
                                    .finish(),
                            );
                        }
                    }
                }
                let meta = ConstrainedBox::new(meta.finish())
                    .with_height(META_HEIGHT)
                    .finish();

                let title_color = if is_running || is_active {
                    palette.main_text
                } else {
                    palette.idle_text
                };
                let title_line = Flex::row()
                    .with_main_axis_size(MainAxisSize::Max)
                    .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(ICON_TEXT_GAP)
                    .with_child(
                        Shrinkable::new(
                            1.,
                            rename_input.unwrap_or_else(|| {
                                Text::new_inline(title, fonts.family, TITLE_FONT_SIZE)
                                    .with_style(fonts.medium)
                                    .with_clip(ClipConfig::ellipsis())
                                    .with_color(title_color.into())
                                    .finish()
                            }),
                        )
                        .finish(),
                    )
                    .with_child(meta)
                    .finish();

                let mut details = Flex::row()
                    .with_main_axis_size(MainAxisSize::Min)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(DETAIL_ICON_GAP);
                if let Some((icon, text)) = detail {
                    details.add_child(render_icon(icon, palette.detail_text, DETAIL_ICON_SIZE));
                    details.add_child(
                        Shrinkable::new(
                            1.,
                            Text::new_inline(text, fonts.family, META_FONT_SIZE)
                                .with_clip(ClipConfig::ellipsis())
                                .with_color(palette.detail_text.into())
                                .finish(),
                        )
                        .finish(),
                    );
                }
                // The agent's logo closes the branch line, under the age or status.
                let detail_line = Flex::row()
                    .with_main_axis_size(MainAxisSize::Max)
                    .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(ICON_TEXT_GAP)
                    .with_child(Shrinkable::new(1., details.finish()).finish())
                    .with_child(render_icon(WarpIcon::ClaudeLogo, palette.claude, LOGO_SIZE))
                    .finish();
                let content = Flex::column()
                    .with_main_axis_size(MainAxisSize::Min)
                    .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
                    .with_spacing(LINE_SPACING)
                    .with_child(title_line)
                    .with_child(detail_line)
                    .finish();
                let content = Container::new(content).with_padding_left(indent).finish();
                let background = if is_active {
                    Some(palette.row_active)
                } else {
                    (hover_state.is_hovered() || is_menu_open).then_some(palette.row_hover)
                };
                render_row_container(content, THREAD_ROW_VERTICAL_PADDING, background)
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

/// What the Claude session of a thread open in a tab is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThreadStatus {
    Working,
    /// Waiting for the user to answer or approve something.
    Blocked,
    Open,
}

impl ThreadStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Working => "Trabalhando",
            Self::Blocked => "Aguardando",
            Self::Open => "Aberta",
        }
    }

    fn color(self, palette: Palette) -> WarpThemeFill {
        match self {
            Self::Working => palette.working,
            Self::Blocked => palette.claude,
            Self::Open => palette.sub_text,
        }
    }
}

/// `color` at `opacity` percent.
fn brand_color(color: ColorU, opacity: u8) -> WarpThemeFill {
    WarpThemeFill::Solid(color).with_opacity(opacity)
}

fn render_empty_state(rows: RowContext<'_>) -> Box<dyn Element> {
    let hint = Text::new(
        "Adicione uma pasta com o + de Projetos para ver as conversas do Claude Code feitas nela.",
        rows.fonts.family,
        META_FONT_SIZE,
    )
    .with_color(rows.palette.sub_text.into())
    .finish();
    Container::new(hint)
        .with_horizontal_padding(ROW_HORIZONTAL_PADDING)
        .with_vertical_padding(THREAD_ROW_VERTICAL_PADDING)
        .finish()
}

fn render_note(text: &'static str, indent: f32, rows: RowContext<'_>) -> Box<dyn Element> {
    Container::new(
        Text::new_inline(text, rows.fonts.family, META_FONT_SIZE)
            .with_color(rows.palette.detail_text.into())
            .finish(),
    )
    .with_padding_left(indent + ROW_HORIZONTAL_PADDING)
    .with_vertical_padding(PROJECT_ROW_VERTICAL_PADDING)
    .finish()
}

fn render_icon(icon: WarpIcon, color: WarpThemeFill, size: f32) -> Box<dyn Element> {
    ConstrainedBox::new(icon.to_warpui_icon(color).finish())
        .with_width(size)
        .with_height(size)
        .finish()
}

/// Padding and rounded corners shared by every row.
fn render_row_container(
    child: Box<dyn Element>,
    vertical_padding: f32,
    background: Option<WarpThemeFill>,
) -> Box<dyn Element> {
    let mut container = Container::new(child)
        .with_horizontal_padding(ROW_HORIZONTAL_PADDING)
        .with_vertical_padding(vertical_padding)
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(ROW_CORNER_RADIUS)));
    if let Some(background) = background {
        container = container.with_background(background);
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
        let color = if hover_state.is_hovered() {
            palette.main_text
        } else {
            palette.sub_text
        };
        let icon = ConstrainedBox::new(icon.to_warpui_icon(color).finish())
            .with_width(ACTION_ICON_SIZE)
            .with_height(ACTION_ICON_SIZE)
            .finish();
        let button = Container::new(icon)
            .with_padding(Padding::uniform(ACTION_BUTTON_PADDING))
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(BUTTON_CORNER_RADIUS)));
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
