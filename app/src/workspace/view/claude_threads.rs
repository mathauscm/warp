//! Threads panel of the vertical tabs sidebar: Claude Code conversations sorted into pinned
//! threads, project folders, workspaces, the recent threads outside every project and the archived
//! ones (see `crate::claude_threads`). It takes the place of the tab list while the header "Threads" button
//! is active.
//!
//! Clicking a thread focuses the pane already showing it, or opens it in the native chat (see
//! `super::chat_tabs` for which tab); the `+` of a project starts a new thread in its folder, and
//! the `+` of a workspace one that works across all its folders.

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
    ChildAnchor, ChildView, ClippedScrollStateHandle, ClippedScrollable, ConstrainedBox, Container,
    CornerRadius, CrossAxisAlignment, DragAxis, Draggable, DraggableState, Element, Empty,
    Fill as ElementFill, Flex, Hoverable, Icon as SvgIcon, MainAxisAlignment, MainAxisSize,
    MouseStateHandle, OffsetPositioning, Padding, ParentAnchor, ParentElement, ParentOffsetBounds,
    PositionedElementAnchor, PositionedElementOffsetBounds, Radius, SavePosition, ScrollbarWidth,
    Shrinkable, Stack, Text,
};
use warpui::fonts::{FamilyId, Properties, Weight};
use warpui::geometry::rect::RectF;
use warpui::platform::{Cursor, FilePickerConfiguration};
use warpui::prelude::Align;
use warpui::text_layout::ClipConfig;
use warpui::ui_components::components::{UiComponent, UiComponentStyles};
use warpui::ui_components::text_input::TextInput;
use warpui::{
    AppContext, Entity, EntityId, EventContext, SingletonEntity, TypedActionView, View,
    ViewContext, ViewHandle, WindowId,
};

use super::project_manager::{
    add_folder_menu_items, add_menu_positioning, create_project_folder, new_project_folder_picker,
};
use crate::appearance::Appearance;
use crate::claude_threads::{
    self, ClaudeThread, ThreadGroups, ThreadProject, ThreadWorkspace, ThreadsConfig,
    TranscriptCache,
};
use crate::editor::{EditorView, Event as EditorEvent, SingleLineEditorOptions, TextOptions};
use crate::menu::{Event as MenuEvent, Menu, MenuItem, MenuItemFields};
use crate::native_chat::ChatProvider;
use crate::native_chat::registry::{NativeChatRegistry, NativeChatStatus};
use crate::terminal::CLIAgent;
use crate::terminal::cli_agent_sessions::{CLIAgentSessionStatus, CLIAgentSessionsModel};
use crate::workspace::WorkspaceAction;
use crate::workspace::util::{
    get_context_target_terminal_view, get_focused_claude_chat_session_id,
};

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
const PROJECT_ROW_VERTICAL_PADDING: f32 = 6.;
const THREAD_ROW_VERTICAL_PADDING: f32 = 6.;
const ROW_CORNER_RADIUS: f32 = 8.;
const BUTTON_CORNER_RADIUS: f32 = 6.;
/// Lines a thread's title up with its project's name, past the folder icon.
const THREAD_INDENT: f32 = FOLDER_ICON_SIZE + ICON_TEXT_GAP;
const CHEVRON_SIZE: f32 = 12.;
const SECTION_CHEVRON_SIZE: f32 = 10.;
const ACTION_ICON_SIZE: f32 = 14.;
const ACTION_BUTTON_PADDING: f32 = 3.;
const ICON_TEXT_GAP: f32 = 9.;
const SECTION_FONT_SIZE: f32 = 13.;
const HEADER_FONT_SIZE: f32 = 17.;
const HEADER_ICON_SIZE: f32 = 16.;
const FOLDER_ICON_SIZE: f32 = 16.;
const NEW_CHAT_ICON_SIZE: f32 = 16.;
const FOLDER_ICON: &str = "bundled/svg/picolas-folder.svg";
const FOLDER_OPEN_ICON: &str = "bundled/svg/picolas-folder-open.svg";
const NEW_CHAT_ICON: &str = "bundled/svg/picolas-new-chat.svg";
const ARTIFACTS_ICON: &str = "bundled/svg/picolas-artifact-other.svg";
const SEARCH_ICON: &str = "bundled/svg/picolas-search.svg";
const SIDEBAR_ICON: &str = "bundled/svg/picolas-sidebar.svg";
const WORKSPACE_ICON: &str = "bundled/svg/picolas-workspace.svg";
const ARCHIVE_ICON: &str = "bundled/svg/picolas-archive.svg";
const DIR_ICON_SIZE: f32 = 13.;
const ADD_PROJECT_BUTTON_POSITION_ID: &str = "claude_threads_add_project_button";

const PROJECT_FONT_SIZE: f32 = 13.5;
const TITLE_FONT_SIZE: f32 = 13.5;
const META_FONT_SIZE: f32 = 11.5;
/// Height of a row's right side, which swaps between the age, status, and
/// buttons on hover; fixed so the swap doesn't move the row.
const META_HEIGHT: f32 = ACTION_ICON_SIZE + 2. * ACTION_BUTTON_PADDING;

/// The collapsible parts of the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadSection {
    Pinned,
    Projects,
    Workspaces,
    Recent,
    Archived,
}

impl ThreadSection {
    fn label(self) -> &'static str {
        match self {
            Self::Pinned => "Fixados",
            Self::Projects => "Projetos",
            Self::Workspaces => "Workspaces",
            Self::Recent => "Recentes",
            Self::Archived => "Arquivadas",
        }
    }
}

/// A list of threads cut at `RECENT_THREAD_LIMIT` until "Mostrar mais" is clicked.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ThreadList {
    Project(PathBuf),
    /// By workspace id.
    Workspace(String),
    Recent,
    Archived,
}

/// What the workspace name editor names.
#[derive(Debug, Clone, PartialEq, Eq)]
enum WorkspaceNaming {
    New,
    /// By workspace id.
    Rename(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClaudeThreadsAction {
    /// A project's header row is being dragged to reorder the projects.
    DragProject {
        path: PathBuf,
        position: RectF,
    },
    DropProject,
    /// Shows or hides the `+` menu of the projects: an existing folder or a new one.
    ToggleAddProjectMenu,
    AddProject,
    /// Names a new folder in the save panel, creates it, adds it as a project and starts a chat
    /// in it.
    CreateProjectFolder,
    RemoveProject(PathBuf),
    ToggleProjectCollapsed(PathBuf),
    ToggleSection(ThreadSection),
    /// Shows every thread of the list, or only the latest again.
    ToggleShowAllThreads(ThreadList),
    NewThread(PathBuf),
    /// Starts a thread in the home folder.
    NewChat,
    /// Opens the gallery of artifacts.
    OpenArtifacts,
    /// Shows or hides the search field.
    ToggleSearch,
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
    /// Archives the thread, or brings it back from "Arquivadas".
    ToggleArchiveThread(String),
    /// Shows the name editor of a new workspace, whose folders are picked next.
    NewWorkspace,
    StartRenameWorkspace(String),
    ToggleWorkspaceCollapsed(String),
    /// Starts a thread across the workspace's folders, or picks its first folders when it has
    /// none.
    NewWorkspaceThread(String),
    ToggleWorkspaceMenu(String),
    ShowWorkspaceMenuPage {
        workspace_id: String,
        page: WorkspaceMenuPage,
    },
    /// Picks folders (several at once) to add to the workspace.
    AddWorkspaceDirs(String),
    RemoveWorkspaceDir {
        workspace_id: String,
        dir: PathBuf,
    },
    /// Drops the workspace from the panel; its threads go back to their projects or "Recentes".
    DeleteWorkspace(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadMenuPage {
    Actions,
    Move,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceMenuPage {
    Actions,
    RemoveDir,
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
    /// Lists showing every thread instead of the latest `RECENT_THREAD_LIMIT`.
    show_all_threads: HashSet<ThreadList>,
    /// The thread whose row is asking "Excluir?".
    pending_delete: Option<String>,
    /// Threads on their way to the Trash, hidden from the list meanwhile.
    deleting: HashSet<String>,
    thread_menu: ViewHandle<Menu<ClaudeThreadsAction>>,
    /// The thread whose "⋯" menu is open.
    menu_open_for: Option<String>,
    /// The workspace whose "⋯" menu is open, by id.
    workspace_menu_open_for: Option<String>,
    /// Whether `thread_menu` shows the `+` menu of the projects instead.
    is_add_project_menu_open: bool,
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
    workspace_name_editor: ViewHandle<EditorView>,
    naming_workspace: Option<WorkspaceNaming>,
    /// The branch checked out in each workspace folder that is a git repository.
    git_branches: HashMap<PathBuf, String>,
    /// Lowercased; while set, only the threads whose title or branch contain it are listed, with
    /// every section and project expanded.
    search_query: String,
    is_search_open: bool,
    /// The system font, which the panel's text uses when it loads.
    font_family: Option<FamilyId>,
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
        let workspace_name_editor = ctx.add_typed_action_view(|ctx| {
            let appearance = Appearance::as_ref(ctx);
            let options = SingleLineEditorOptions {
                text: TextOptions::ui_text(Some(PROJECT_FONT_SIZE), appearance),
                ..Default::default()
            };
            let mut editor = EditorView::single_line(options, ctx);
            editor.set_placeholder_text("Nome do workspace", ctx);
            editor
        });
        ctx.subscribe_to_view(&workspace_name_editor, |me, _, event, ctx| match event {
            EditorEvent::Enter => me.finish_naming_workspace(ctx),
            EditorEvent::Escape | EditorEvent::Blurred => me.cancel_naming_workspace(ctx),
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
                me.workspace_menu_open_for = None;
                me.is_add_project_menu_open = false;
                ctx.notify();
            }
            MenuEvent::ItemSelected | MenuEvent::ItemHovered => {}
        });
        // Status dots follow the Claude sessions running in the tabs.
        ctx.subscribe_to_model(&NativeChatRegistry::handle(ctx), |me, _, _, ctx| {
            me.record_workspace_threads(ctx);
            ctx.notify()
        });
        ctx.subscribe_to_model(&CLIAgentSessionsModel::handle(ctx), |me, _, _, ctx| {
            if me.is_visible {
                ctx.notify();
            }
        });
        let mut config = claude_threads::load_config();
        seed_dev_config(&mut config);
        Self {
            config,
            threads: Vec::new(),
            has_scanned: false,
            transcript_cache: Arc::new(Mutex::new(TranscriptCache::new())),
            scan_epoch: 0,
            is_visible: false,
            show_all_threads: HashSet::new(),
            pending_delete: None,
            deleting: HashSet::new(),
            thread_menu,
            menu_open_for: None,
            workspace_menu_open_for: None,
            is_add_project_menu_open: false,
            keep_menu_open: false,
            scroll_state: ClippedScrollStateHandle::default(),
            mouse_states: RefCell::new(HashMap::new()),
            drag_states: RefCell::new(HashMap::new()),
            window_id: ctx.window_id(),
            search_editor,
            rename_editor,
            renaming: None,
            workspace_name_editor,
            naming_workspace: None,
            git_branches: HashMap::new(),
            search_query: String::new(),
            is_search_open: false,
            font_family: crate::appearance::system_ui_font_family(ctx),
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
            if take_dev_open_add_menu() {
                self.toggle_add_project_menu(ctx);
            }
        }
    }

    fn schedule_scan(&mut self, delay: Duration, ctx: &mut ViewContext<Self>) {
        self.scan_epoch += 1;
        let epoch = self.scan_epoch;
        let cache = self.transcript_cache.clone();
        let branch_dirs = self.expanded_workspace_dirs();
        ctx.spawn(
            async move {
                Timer::after(delay).await;
                let threads = {
                    let mut cache = cache.lock().unwrap_or_else(PoisonError::into_inner);
                    claude_threads::scan(&mut cache)
                };
                (threads, read_git_branches(branch_dirs))
            },
            move |me, (threads, branches), ctx| {
                if me.scan_epoch != epoch {
                    return;
                }
                me.threads = threads;
                me.apply_git_branches(branches);
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

    fn toggle_add_project_menu(&mut self, ctx: &mut ViewContext<Self>) {
        self.is_add_project_menu_open = !self.is_add_project_menu_open;
        if self.is_add_project_menu_open {
            self.menu_open_for = None;
            self.workspace_menu_open_for = None;
            self.keep_menu_open = false;
            let items = add_folder_menu_items(
                ClaudeThreadsAction::AddProject,
                ClaudeThreadsAction::CreateProjectFolder,
            );
            self.thread_menu
                .update(ctx, |menu, ctx| menu.set_items(items, ctx));
        }
        ctx.notify();
    }

    /// Creates a folder named in the save panel, adds it as a project and starts a chat in it.
    fn create_project_folder(&mut self, ctx: &mut ViewContext<Self>) {
        let window_id = ctx.window_id();
        ctx.open_save_file_picker(
            move |path, me, ctx| {
                let Some(path) = path.and_then(|path| create_project_folder(&path, window_id, ctx))
                else {
                    return;
                };
                me.add_project(path.clone(), ctx);
                ctx.dispatch_typed_action_deferred(WorkspaceAction::OpenClaudeChat {
                    group: Some(project_group_name(&path)),
                    cwd: path,
                    session_id: None,
                    draft: None,
                    artifact_kind: None,
                });
            },
            new_project_folder_picker(),
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
        self.show_all_threads
            .remove(&ThreadList::Project(path.to_path_buf()));
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
            ThreadSection::Workspaces => &mut self.config.workspaces_collapsed,
            ThreadSection::Recent => &mut self.config.recent_collapsed,
            ThreadSection::Archived => &mut self.config.archived_expanded,
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
                ThreadSection::Workspaces => self.config.workspaces_collapsed,
                ThreadSection::Recent => self.config.recent_collapsed,
                ThreadSection::Archived => !self.config.archived_expanded,
            }
    }

    fn toggle_pin(&mut self, session_id: &str, ctx: &mut ViewContext<Self>) {
        self.config.toggle_pinned(session_id);
        if self.config.is_pinned(session_id) {
            self.config.pinned_collapsed = false;
        }
        self.save_config();
        ctx.notify();
    }

    /// Archiving leaves the thread open in its chat, if it is.
    fn toggle_archive(&mut self, session_id: &str, ctx: &mut ViewContext<Self>) {
        self.config.toggle_archived(session_id);
        self.save_config();
        ctx.notify();
    }

    fn move_to_project(&mut self, session_id: &str, project: &Path, ctx: &mut ViewContext<Self>) {
        self.config
            .moved
            .insert(session_id.to_owned(), project.to_path_buf());
        self.config.workspace_threads.remove(session_id);
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

    /// Focuses the terminal already running the thread, or opens it in the native chat.
    fn open_thread(&mut self, session_id: &str, cwd: &Path, ctx: &mut ViewContext<Self>) {
        if self.focus_running_thread(session_id, ctx) {
            return;
        }
        if let Some(workspace) = self.config.workspace_of(session_id) {
            ctx.dispatch_typed_action_deferred(WorkspaceAction::OpenClaudeWorkspaceChat {
                cwd: cwd.to_path_buf(),
                workspace: workspace.chat_workspace(cwd),
                session_id: Some(session_id.to_owned()),
                group: Some(workspace.name.clone()),
            });
            return;
        }
        let group = self
            .find_thread(session_id)
            .and_then(|thread| self.config.project_of(thread))
            .map(project_group_name);
        ctx.dispatch_typed_action_deferred(WorkspaceAction::OpenClaudeChat {
            cwd: cwd.to_path_buf(),
            session_id: Some(session_id.to_owned()),
            draft: None,
            artifact_kind: None,
            group,
        });
    }

    /// Focuses the terminal tab where Claude Code is already running the thread, if any.
    fn focus_running_thread(&self, session_id: &str, ctx: &mut ViewContext<Self>) -> bool {
        let running_in = CLIAgentSessionsModel::as_ref(ctx)
            .session_by_agent_session_id(session_id)
            .map(|(terminal_view_id, _)| terminal_view_id);
        let Some(terminal_view_id) = running_in else {
            return false;
        };
        ctx.dispatch_typed_action_deferred(WorkspaceAction::FocusTerminalViewInWorkspace {
            terminal_view_id,
        });
        true
    }

    /// Closes the pane running the thread, if any, then moves its transcript to the Trash.
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
        let is_open_in_chat = NativeChatRegistry::as_ref(ctx)
            .status(session_id, ctx)
            .is_some();
        if is_open_in_chat {
            ctx.dispatch_typed_action_deferred(WorkspaceAction::CloseClaudeChat {
                session_id: session_id.to_owned(),
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
        let delay = if running_in.is_some() || is_open_in_chat {
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

    /// Whether the thread is a native chat that never sent a message, which deleting loses
    /// nothing of.
    fn is_unsent_chat(&self, session_id: &str, app: &AppContext) -> bool {
        self.find_thread(session_id).is_none()
            && NativeChatRegistry::as_ref(app).is_unsent(session_id, app)
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

    /// A Claude Code thread in the home folder, outside every project, like a new chat in a chat
    /// app.
    fn new_chat(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(cwd) = dirs::home_dir() else {
            return;
        };
        let unused = NativeChatRegistry::as_ref(ctx).unused_chat_in(&cwd, ctx);
        ctx.dispatch_typed_action_deferred(WorkspaceAction::OpenClaudeChat {
            cwd,
            session_id: unused,
            draft: None,
            artifact_kind: None,
            group: None,
        });
    }

    /// Opens the search field focused, or closes it and clears the search.
    fn toggle_search(&mut self, ctx: &mut ViewContext<Self>) {
        self.is_search_open = !self.is_search_open;
        if self.is_search_open {
            ctx.focus(&self.search_editor);
        } else {
            self.search_editor
                .update(ctx, |editor, ctx| editor.clear_buffer(ctx));
            self.search_query.clear();
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
        self.is_add_project_menu_open = false;
        self.workspace_menu_open_for = None;
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
        let is_unsent = self.is_unsent_chat(session_id, app);
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
            MenuItemFields::new(if is_unsent { "Descartar" } else { "Excluir" })
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

// --- workspaces -------------------------------------------------------------------------------

impl ClaudeThreadsView {
    /// The folders of the expanded workspaces, whose branches the panel shows.
    fn expanded_workspace_dirs(&self) -> Vec<PathBuf> {
        self.config
            .workspaces
            .iter()
            .filter(|workspace| !workspace.collapsed)
            .flat_map(|workspace| workspace.dirs.iter().cloned())
            .collect()
    }

    fn apply_git_branches(&mut self, branches: Vec<(PathBuf, Option<String>)>) {
        for (dir, branch) in branches {
            match branch {
                Some(branch) => self.git_branches.insert(dir, branch),
                None => self.git_branches.remove(&dir),
            };
        }
    }

    /// Reads the branches of `dirs` off the main thread.
    fn refresh_git_branches(&mut self, dirs: Vec<PathBuf>, ctx: &mut ViewContext<Self>) {
        if dirs.is_empty() {
            return;
        }
        ctx.spawn(
            async move { read_git_branches(dirs) },
            |me, branches, ctx| {
                me.apply_git_branches(branches);
                ctx.notify();
            },
        );
    }

    /// Lists the open chats of a workspace under it once they have a conversation.
    fn record_workspace_threads(&mut self, ctx: &mut ViewContext<Self>) {
        let mut changed = false;
        for chat in NativeChatRegistry::as_ref(ctx).open_chats(ctx) {
            if let Some(workspace_id) = chat.workspace_id.as_deref()
                && chat.has_transcript
            {
                changed |= self
                    .config
                    .assign_to_workspace(&chat.session_id, workspace_id);
            }
        }
        if changed {
            self.save_config();
        }
    }

    /// `self.config`, plus the open chats of a workspace that haven't sent anything yet, so they
    /// are listed under it right away.
    fn config_with_open_workspace_chats(&self, app: &AppContext) -> ThreadsConfig {
        let mut config = self.config.clone();
        for chat in NativeChatRegistry::as_ref(app).open_chats(app) {
            if let Some(workspace_id) = chat.workspace_id.as_deref() {
                config.assign_to_workspace(&chat.session_id, workspace_id);
            }
        }
        config
    }

    /// Shows the name editor for a new workspace (`None`) or for renaming one.
    fn start_naming_workspace(&mut self, workspace_id: Option<&str>, ctx: &mut ViewContext<Self>) {
        let (naming, name) = match workspace_id {
            Some(id) => {
                let Some(workspace) = self.config.workspace(id) else {
                    return;
                };
                (
                    WorkspaceNaming::Rename(id.to_owned()),
                    workspace.name.clone(),
                )
            }
            None => (WorkspaceNaming::New, String::new()),
        };
        self.naming_workspace = Some(naming);
        self.config.workspaces_collapsed = false;
        self.workspace_name_editor.update(ctx, |editor, ctx| {
            editor.clear_buffer_and_reset_undo_stack(ctx);
            if !name.is_empty() {
                editor.insert_selected_text(&name, ctx);
            }
        });
        ctx.focus(&self.workspace_name_editor);
        ctx.notify();
    }

    fn finish_naming_workspace(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(naming) = self.naming_workspace.take() else {
            return;
        };
        let name = self
            .workspace_name_editor
            .as_ref(ctx)
            .buffer_text(ctx)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if name.is_empty() {
            ctx.notify();
            return;
        }
        match naming {
            WorkspaceNaming::New => {
                let workspace = ThreadWorkspace::new(name);
                let id = workspace.id.clone();
                self.config.workspaces.push(workspace);
                self.save_config();
                self.pick_workspace_dirs(&id, ctx);
            }
            WorkspaceNaming::Rename(id) => {
                if let Some(workspace) = self.config.workspace_mut(&id) {
                    workspace.name = name;
                    self.save_config();
                }
            }
        }
        ctx.notify();
    }

    fn cancel_naming_workspace(&mut self, ctx: &mut ViewContext<Self>) {
        if self.naming_workspace.take().is_some() {
            ctx.notify();
        }
    }

    fn pick_workspace_dirs(&mut self, workspace_id: &str, ctx: &mut ViewContext<Self>) {
        let workspace_id = workspace_id.to_owned();
        ctx.open_file_picker(
            move |result, ctx| {
                let Ok(paths) = result else { return };
                let dirs: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
                if let Some(handle) = ctx.handle().upgrade(ctx) {
                    handle.update(ctx, |me, ctx| {
                        me.add_workspace_dirs(&workspace_id, dirs, ctx)
                    });
                }
            },
            FilePickerConfiguration::new()
                .folders_only()
                .allow_multi_select(),
        );
    }

    fn add_workspace_dirs(
        &mut self,
        workspace_id: &str,
        dirs: Vec<PathBuf>,
        ctx: &mut ViewContext<Self>,
    ) {
        let Some(workspace) = self.config.workspace_mut(workspace_id) else {
            return;
        };
        if !workspace.add_dirs(dirs.iter().cloned()) {
            return;
        }
        workspace.collapsed = false;
        self.config.workspaces_collapsed = false;
        self.save_config();
        self.refresh_git_branches(dirs, ctx);
        ctx.notify();
    }

    fn remove_workspace_dir(
        &mut self,
        workspace_id: &str,
        dir: &Path,
        ctx: &mut ViewContext<Self>,
    ) {
        if let Some(workspace) = self.config.workspace_mut(workspace_id) {
            workspace.dirs.retain(|existing| existing != dir);
            self.save_config();
            ctx.notify();
        }
    }

    fn delete_workspace(&mut self, workspace_id: &str, ctx: &mut ViewContext<Self>) {
        self.config.remove_workspace(workspace_id);
        self.show_all_threads
            .remove(&ThreadList::Workspace(workspace_id.to_owned()));
        self.save_config();
        ctx.notify();
    }

    fn toggle_workspace_collapsed(&mut self, workspace_id: &str, ctx: &mut ViewContext<Self>) {
        let Some(workspace) = self.config.workspace_mut(workspace_id) else {
            return;
        };
        workspace.collapsed = !workspace.collapsed;
        let dirs = (!workspace.collapsed).then(|| workspace.dirs.clone());
        self.save_config();
        if let Some(dirs) = dirs {
            self.refresh_git_branches(dirs, ctx);
        }
        ctx.notify();
    }

    fn new_workspace_thread(&mut self, workspace_id: &str, ctx: &mut ViewContext<Self>) {
        let Some(cwd) = self
            .config
            .workspace(workspace_id)
            .and_then(|workspace| workspace.primary_dir().map(Path::to_path_buf))
        else {
            self.pick_workspace_dirs(workspace_id, ctx);
            return;
        };
        let Some(workspace) = self.config.workspace(workspace_id) else {
            return;
        };
        let unused = NativeChatRegistry::as_ref(ctx).unused_chat_in_workspace(workspace_id, ctx);
        ctx.dispatch_typed_action_deferred(WorkspaceAction::OpenClaudeWorkspaceChat {
            workspace: workspace.chat_workspace(&cwd),
            group: Some(workspace.name.clone()),
            cwd,
            session_id: unused,
        });
    }

    fn toggle_workspace_menu(&mut self, workspace_id: &str, ctx: &mut ViewContext<Self>) {
        if self.workspace_menu_open_for.as_deref() == Some(workspace_id) {
            self.workspace_menu_open_for = None;
            ctx.notify();
            return;
        }
        let items = self.workspace_menu_items(workspace_id, ctx);
        self.thread_menu
            .update(ctx, |menu, ctx| menu.set_items(items, ctx));
        self.keep_menu_open = false;
        self.is_add_project_menu_open = false;
        self.menu_open_for = None;
        self.workspace_menu_open_for = Some(workspace_id.to_owned());
        ctx.notify();
    }

    fn workspace_menu_items(
        &self,
        workspace_id: &str,
        app: &AppContext,
    ) -> Vec<MenuItem<ClaudeThreadsAction>> {
        let Some(workspace) = self.config.workspace(workspace_id) else {
            return Vec::new();
        };
        let id = workspace_id.to_owned();
        let mut items = vec![
            MenuItemFields::new("Nova thread")
                .with_icon(WarpIcon::Plus)
                .with_on_select_action(ClaudeThreadsAction::NewWorkspaceThread(id.clone()))
                .into_item(),
            MenuItemFields::new("Renomear")
                .with_icon(WarpIcon::Pencil)
                .with_on_select_action(ClaudeThreadsAction::StartRenameWorkspace(id.clone()))
                .into_item(),
            MenuItemFields::new("Adicionar pasta…")
                .with_icon(WarpIcon::Folder)
                .with_on_select_action(ClaudeThreadsAction::AddWorkspaceDirs(id.clone()))
                .into_item(),
        ];
        if !workspace.dirs.is_empty() {
            items.push(
                MenuItemFields::new("Remover pasta")
                    .with_icon(WarpIcon::X)
                    .with_right_side_icon(WarpIcon::ChevronRight)
                    .with_on_select_action(ClaudeThreadsAction::ShowWorkspaceMenuPage {
                        workspace_id: id.clone(),
                        page: WorkspaceMenuPage::RemoveDir,
                    })
                    .into_item(),
            );
        }
        items.push(MenuItem::Separator);
        let red = Appearance::as_ref(app).theme().ansi_fg_red();
        items.push(
            MenuItemFields::new("Excluir workspace")
                .with_icon(WarpIcon::Trash)
                .with_override_text_color(red)
                .with_override_icon_color(WarpThemeFill::Solid(red))
                .with_on_select_action(ClaudeThreadsAction::DeleteWorkspace(id))
                .into_item(),
        );
        items
    }

    /// The second page of a workspace's "⋯" menu: back to the first page, then its folders.
    fn workspace_remove_dir_items(&self, workspace_id: &str) -> Vec<MenuItem<ClaudeThreadsAction>> {
        let id = workspace_id.to_owned();
        let mut items = vec![
            MenuItemFields::new("Remover pasta")
                .with_icon(WarpIcon::ChevronLeft)
                .with_on_select_action(ClaudeThreadsAction::ShowWorkspaceMenuPage {
                    workspace_id: id.clone(),
                    page: WorkspaceMenuPage::Actions,
                })
                .into_item(),
            MenuItem::Separator,
        ];
        let dirs = self
            .config
            .workspace(workspace_id)
            .map(|workspace| workspace.dirs.as_slice())
            .unwrap_or_default();
        for dir in dirs {
            items.push(
                MenuItemFields::new(dir_name(dir))
                    .with_icon(WarpIcon::Folder)
                    .with_on_select_action(ClaudeThreadsAction::RemoveWorkspaceDir {
                        workspace_id: id.clone(),
                        dir: dir.clone(),
                    })
                    .into_item(),
            );
        }
        items
    }
}

/// The branch of each of `dirs`, `None` for the ones outside a git repository.
fn read_git_branches(dirs: Vec<PathBuf>) -> Vec<(PathBuf, Option<String>)> {
    dirs.into_iter()
        .map(|dir| {
            let branch = claude_threads::git_branch(&dir);
            (dir, branch)
        })
        .collect()
}

fn dir_name(dir: &Path) -> String {
    ThreadProject::new(dir.to_path_buf()).name()
}

/// Save-position id of a workspace's "⋯" button, which its menu hangs from.
fn workspace_menu_button_position_id(workspace_id: &str) -> String {
    format!("claude_threads_workspace_menu_button:{workspace_id}")
}

/// For working on the panel: `PICOLAS_SEED_WORKSPACE=<name>=<folder>,<folder>...` lists that
/// workspace and `PICOLAS_SEED_ARCHIVED=<session id>,...` archives those threads, without saving
/// either. Debug builds only.
fn seed_dev_config(config: &mut ThreadsConfig) {
    if !cfg!(debug_assertions) {
        return;
    }
    if let Ok(spec) = std::env::var("PICOLAS_SEED_WORKSPACE")
        && let Some((name, dirs)) = spec.split_once('=')
        && !config
            .workspaces
            .iter()
            .any(|workspace| workspace.name == name)
    {
        let mut workspace = ThreadWorkspace::new(name.to_owned());
        workspace.id = crate::native_chat::DEV_WORKSPACE_ID.to_owned();
        workspace.add_dirs(
            dirs.split(',')
                .filter(|dir| !dir.is_empty())
                .map(PathBuf::from),
        );
        config.workspaces.push(workspace);
    }
    if let Ok(ids) = std::env::var("PICOLAS_SEED_ARCHIVED") {
        for id in ids.split(',').filter(|id| !id.is_empty()) {
            if !config.is_archived(id) {
                config.toggle_archived(id);
            }
        }
        config.archived_expanded = true;
    }
}

/// Save-position id of a project's block (header and threads), used to find
/// where a dragged project would land.
fn project_block_position_id(path: &Path) -> String {
    format!("claude_threads_project_block:{}", path.display())
}

/// Whether `PICOLAS_SIDEBAR=menu` asks for the projects' `+` menu, the first time the panel shows.
/// Debug builds only.
fn take_dev_open_add_menu() -> bool {
    use std::sync::atomic::{AtomicBool, Ordering};

    static TAKEN: AtomicBool = AtomicBool::new(false);
    cfg!(debug_assertions)
        && std::env::var("PICOLAS_SIDEBAR").is_ok_and(|value| value == "menu")
        && !TAKEN.swap(true, Ordering::Relaxed)
}

/// Save-position id of a thread's "⋯" button, which its menu hangs from.
fn thread_menu_button_position_id(session_id: &str) -> String {
    format!("claude_threads_menu_button:{session_id}")
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
            ClaudeThreadsAction::ToggleAddProjectMenu => self.toggle_add_project_menu(ctx),
            ClaudeThreadsAction::AddProject => self.pick_project_folder(ctx),
            ClaudeThreadsAction::CreateProjectFolder => self.create_project_folder(ctx),
            ClaudeThreadsAction::RemoveProject(path) => self.remove_project(path, ctx),
            ClaudeThreadsAction::ToggleProjectCollapsed(path) => {
                self.toggle_project_collapsed(path, ctx);
            }
            ClaudeThreadsAction::ToggleSection(section) => self.toggle_section(*section, ctx),
            ClaudeThreadsAction::ToggleShowAllThreads(list) => {
                if !self.show_all_threads.remove(list) {
                    self.show_all_threads.insert(list.clone());
                }
                ctx.notify();
            }
            ClaudeThreadsAction::NewThread(path) => {
                let unused = NativeChatRegistry::as_ref(ctx).unused_chat_in(path, ctx);
                ctx.dispatch_typed_action_deferred(WorkspaceAction::OpenClaudeChat {
                    cwd: path.clone(),
                    session_id: unused,
                    draft: None,
                    artifact_kind: None,
                    group: Some(project_group_name(path)),
                });
            }
            ClaudeThreadsAction::OpenArtifacts => {
                ctx.dispatch_typed_action_deferred(WorkspaceAction::OpenArtifacts {
                    artifact_id: None,
                });
            }
            ClaudeThreadsAction::NewChat => self.new_chat(ctx),
            ClaudeThreadsAction::ToggleSearch => self.toggle_search(ctx),
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
                if self.is_unsent_chat(session_id, ctx) {
                    self.delete_thread(session_id, ctx);
                } else {
                    self.pending_delete = Some(session_id.clone());
                    ctx.notify();
                }
            }
            ClaudeThreadsAction::ConfirmDeleteThread(session_id) => {
                self.delete_thread(session_id, ctx);
            }
            ClaudeThreadsAction::DragProject { path, position } => {
                // A grabbing hand while the project moves, like dragging anything on macOS.
                ctx.set_cursor_shape(Cursor::ClosedHand);
                self.drag_project(path, *position, ctx);
            }
            ClaudeThreadsAction::DropProject => {
                ctx.reset_cursor();
                self.save_config();
                ctx.notify();
            }
            ClaudeThreadsAction::CancelDeleteThread => {
                self.pending_delete = None;
                ctx.notify();
            }
            ClaudeThreadsAction::ToggleArchiveThread(session_id) => {
                self.toggle_archive(session_id, ctx);
            }
            ClaudeThreadsAction::NewWorkspace => self.start_naming_workspace(None, ctx),
            ClaudeThreadsAction::StartRenameWorkspace(workspace_id) => {
                self.start_naming_workspace(Some(workspace_id.as_str()), ctx);
            }
            ClaudeThreadsAction::ToggleWorkspaceCollapsed(workspace_id) => {
                self.toggle_workspace_collapsed(workspace_id, ctx);
            }
            ClaudeThreadsAction::NewWorkspaceThread(workspace_id) => {
                self.new_workspace_thread(workspace_id, ctx);
            }
            ClaudeThreadsAction::ToggleWorkspaceMenu(workspace_id) => {
                self.toggle_workspace_menu(workspace_id, ctx);
            }
            ClaudeThreadsAction::ShowWorkspaceMenuPage { workspace_id, page } => {
                let items = match page {
                    WorkspaceMenuPage::Actions => self.workspace_menu_items(workspace_id, ctx),
                    WorkspaceMenuPage::RemoveDir => self.workspace_remove_dir_items(workspace_id),
                };
                self.thread_menu
                    .update(ctx, |menu, ctx| menu.set_items(items, ctx));
                self.keep_menu_open = true;
                ctx.notify();
            }
            ClaudeThreadsAction::AddWorkspaceDirs(workspace_id) => {
                self.pick_workspace_dirs(workspace_id, ctx);
            }
            ClaudeThreadsAction::RemoveWorkspaceDir { workspace_id, dir } => {
                self.remove_workspace_dir(workspace_id, dir, ctx);
            }
            ClaudeThreadsAction::DeleteWorkspace(workspace_id) => {
                self.delete_workspace(workspace_id, ctx);
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
    /// Notes, counts and ages, a step behind `sub_text`.
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
    fn new(family: Option<FamilyId>, appearance: &Appearance) -> Self {
        Self {
            family: family.unwrap_or_else(|| appearance.ui_font_family()),
            medium: Properties::default().weight(Weight::Medium),
        }
    }
}

/// What every row of a render needs.
#[derive(Clone, Copy)]
struct RowContext<'a> {
    now: SystemTime,
    active_terminal_id: Option<EntityId>,
    /// The conversation of the focused native chat, if one is focused.
    active_chat_session_id: Option<&'a str>,
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
        let active_chat_session_id = get_focused_claude_chat_session_id(self.window_id, app);
        let rows = RowContext {
            now: SystemTime::now(),
            active_terminal_id: get_context_target_terminal_view(self.window_id, app)
                .map(|view| view.id()),
            active_chat_session_id: active_chat_session_id.as_deref(),
            palette: Palette::new(appearance),
            fonts: Fonts::new(self.font_family, appearance),
            appearance,
            app,
        };
        let groups = claude_threads::group_threads(
            self.listed_threads(rows.now, app),
            &self.config_with_open_workspace_chats(app),
        );
        let is_searching = !self.search_query.is_empty();

        let mut list = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(ROW_SPACING);
        let mut is_first_section = true;
        let mut add_section_header = |list: &mut Flex, section, count| {
            list.add_child(self.render_section_header(section, count, is_first_section, rows));
            is_first_section = false;
        };

        if !groups.pinned.is_empty() {
            add_section_header(&mut list, ThreadSection::Pinned, None);
            if !self.is_section_collapsed(ThreadSection::Pinned) {
                for thread in &groups.pinned {
                    list.add_child(self.render_thread_row(thread, 0., rows));
                }
            }
        }

        let has_project_matches = groups
            .by_project
            .values()
            .any(|threads| !threads.is_empty());
        if !is_searching || has_project_matches {
            add_section_header(&mut list, ThreadSection::Projects, None);
            if !self.is_section_collapsed(ThreadSection::Projects) {
                list.add_child(self.render_projects(&groups, rows));
            }
        }

        let has_workspace_matches = groups
            .by_workspace
            .values()
            .any(|threads| !threads.is_empty());
        if !is_searching || has_workspace_matches {
            add_section_header(&mut list, ThreadSection::Workspaces, None);
            if !self.is_section_collapsed(ThreadSection::Workspaces) {
                list.add_child(self.render_workspaces(&groups, rows));
            }
        }

        if !groups.recent.is_empty() {
            add_section_header(&mut list, ThreadSection::Recent, None);
            if !self.is_section_collapsed(ThreadSection::Recent) {
                self.add_thread_rows(&mut list, &groups.recent, ThreadList::Recent, 0., rows);
            }
        }

        if !groups.archived.is_empty() {
            add_section_header(
                &mut list,
                ThreadSection::Archived,
                Some(groups.archived.len()),
            );
            if !self.is_section_collapsed(ThreadSection::Archived) {
                self.add_thread_rows(&mut list, &groups.archived, ThreadList::Archived, 0., rows);
            }
        }

        let has_matches = !groups.pinned.is_empty()
            || has_project_matches
            || has_workspace_matches
            || !groups.recent.is_empty()
            || !groups.archived.is_empty();
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
            .with_child(self.render_header(rows))
            .with_child(self.render_new_chat_row(rows))
            .with_children(
                (self.is_search_open || is_searching).then(|| self.render_search_bar(rows.palette)),
            )
            .with_child(Shrinkable::new(1., scrollable).finish())
            .finish();
        let mut stack = Stack::new().with_child(panel);
        if self.is_add_project_menu_open {
            stack.add_positioned_overlay_child(
                ChildView::new(&self.thread_menu).finish(),
                add_menu_positioning(ADD_PROJECT_BUTTON_POSITION_ID),
            );
        } else if let Some(workspace_id) = &self.workspace_menu_open_for {
            stack.add_positioned_overlay_child(
                ChildView::new(&self.thread_menu).finish(),
                OffsetPositioning::offset_from_save_position_element(
                    workspace_menu_button_position_id(workspace_id),
                    vec2f(MENU_OFFSET, 0.),
                    PositionedElementOffsetBounds::WindowByPosition,
                    PositionedElementAnchor::TopRight,
                    ChildAnchor::TopLeft,
                ),
            );
        } else if let Some(session_id) = &self.menu_open_for {
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

    /// Claude sessions open in a tab (native chats, or Claude Code in a terminal) that no scanned
    /// thread covers yet.
    fn pending_threads(&self, now: SystemTime, app: &AppContext) -> Vec<ClaudeThread> {
        let native = NativeChatRegistry::as_ref(app)
            .open_chats(app)
            .into_iter()
            .filter(|chat| self.find_thread(&chat.session_id).is_none())
            .map(|chat| ClaudeThread {
                provider: chat.provider,
                title: chat.title.unwrap_or_else(|| NEW_THREAD_TITLE.to_owned()),
                session_id: chat.session_id,
                cwd: chat.cwd,
                updated_at: now,
                transcript: PathBuf::new(),
                git_branch: None,
            });
        let terminal = CLIAgentSessionsModel::as_ref(app)
            .sessions()
            .filter(|(_, session)| session.agent == CLIAgent::Claude)
            .filter_map(|(_, session)| {
                let context = &session.session_context;
                let session_id = context.session_id.clone()?;
                let cwd = PathBuf::from(context.cwd.as_deref()?);
                let is_scanned = self.find_thread(&session_id).is_some();
                (!is_scanned).then(|| ClaudeThread {
                    provider: ChatProvider::Claude,
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
            .collect::<Vec<_>>();
        native.chain(terminal).collect()
    }

    fn render_search_bar(&self, palette: Palette) -> Box<dyn Element> {
        let row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(ICON_TEXT_GAP)
            .with_child(render_svg(SEARCH_ICON, palette.sub_text, ACTION_ICON_SIZE))
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

    /// The name of a section, with `count` after it when given; clicking it collapses or expands
    /// the section, which a chevron shows on hover. "Projetos" and "Workspaces" also get the
    /// button that adds one, on hover.
    fn render_section_header(
        &self,
        section: ThreadSection,
        count: Option<usize>,
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
        let add_button = match section {
            ThreadSection::Projects => Some(
                SavePosition::new(
                    render_icon_button(
                        WarpIcon::Plus,
                        "Adicionar pasta",
                        self.mouse_state("header:add".to_owned()),
                        ClaudeThreadsAction::ToggleAddProjectMenu,
                        palette,
                        appearance,
                    ),
                    ADD_PROJECT_BUTTON_POSITION_ID,
                )
                .finish(),
            ),
            ThreadSection::Workspaces => Some(render_icon_button(
                WarpIcon::Plus,
                "Novo workspace…",
                self.mouse_state("header:add-workspace".to_owned()),
                ClaudeThreadsAction::NewWorkspace,
                palette,
                appearance,
            )),
            ThreadSection::Pinned | ThreadSection::Recent | ThreadSection::Archived => None,
        };
        let label = match count {
            Some(count) => format!("{} ({count})", section.label()),
            None => section.label().to_owned(),
        };
        // The menu hangs from the button, which has to stay while the pointer moves to the menu.
        let is_add_menu_open = section == ThreadSection::Projects && self.is_add_project_menu_open;
        let header = Hoverable::new(
            self.mouse_state(format!("section:{section:?}")),
            move |hover_state| {
                let color = if hover_state.is_hovered() {
                    palette.main_text
                } else {
                    palette.sub_text
                };
                let mut label = Flex::row()
                    .with_main_axis_size(MainAxisSize::Min)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(4.)
                    .with_child(
                        Text::new_inline(label, fonts.family, SECTION_FONT_SIZE)
                            .with_style(fonts.medium)
                            .with_color(color.into())
                            .finish(),
                    );
                if hover_state.is_hovered() {
                    label.add_child(render_icon(chevron, color, SECTION_CHEVRON_SIZE));
                }
                let label = label.finish();
                let mut row = Flex::row()
                    .with_main_axis_size(MainAxisSize::Max)
                    .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_child(label);
                let trailing = add_button.filter(|_| hover_state.is_hovered() || is_add_menu_open);
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
                .with_child(self.render_project_row(project, rows));
            if is_searching || !project.collapsed {
                if threads.is_empty() {
                    if self.has_scanned {
                        block.add_child(render_note("Nenhum chat", THREAD_INDENT, rows));
                    }
                } else {
                    self.add_thread_rows(
                        &mut block,
                        threads,
                        ThreadList::Project(project.path.clone()),
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

    /// Every workspace with its folders and threads; while searching, only the workspaces with
    /// matches, expanded, without their folders.
    fn render_workspaces(&self, groups: &ThreadGroups, rows: RowContext<'_>) -> Box<dyn Element> {
        let is_searching = !self.search_query.is_empty();
        let mut list = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(PROJECT_SPACING);
        if self.naming_workspace == Some(WorkspaceNaming::New) {
            list.add_child(self.render_workspace_name_input(rows));
        } else if self.config.workspaces.is_empty() {
            list.add_child(render_hint(
                "Crie um workspace com o + para conversar com várias pastas ao mesmo tempo.",
                rows,
            ));
        }
        for workspace in &self.config.workspaces {
            let threads = groups
                .by_workspace
                .get(&workspace.id)
                .map(Vec::as_slice)
                .unwrap_or_default();
            if is_searching && threads.is_empty() {
                continue;
            }
            let mut block = Flex::column()
                .with_main_axis_size(MainAxisSize::Min)
                .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
                .with_spacing(ROW_SPACING)
                .with_child(self.render_workspace_row(workspace, rows));
            if !is_searching && !workspace.collapsed {
                for dir in &workspace.dirs {
                    block.add_child(self.render_workspace_dir_row(&workspace.id, dir, rows));
                }
                block.add_child(self.render_add_workspace_dir_row(&workspace.id, rows));
            }
            if is_searching || !workspace.collapsed {
                if threads.is_empty() {
                    if self.has_scanned && !workspace.dirs.is_empty() {
                        block.add_child(render_note("Nenhum chat", THREAD_INDENT, rows));
                    }
                } else {
                    self.add_thread_rows(
                        &mut block,
                        threads,
                        ThreadList::Workspace(workspace.id.clone()),
                        THREAD_INDENT,
                        rows,
                    );
                }
            }
            list.add_child(block.finish());
        }
        list.finish()
    }

    /// The editor naming a new workspace, or renaming one, in place of its name.
    fn render_workspace_name_input(&self, rows: RowContext<'_>) -> Box<dyn Element> {
        let input = TextInput::new(
            self.workspace_name_editor.clone(),
            UiComponentStyles::default()
                .set_background(ElementFill::None)
                .set_border_radius(CornerRadius::with_all(Radius::Pixels(0.)))
                .set_border_width(0.),
        )
        .build()
        .finish();
        let row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(ICON_TEXT_GAP)
            .with_child(render_svg(
                WORKSPACE_ICON,
                rows.palette.main_text,
                FOLDER_ICON_SIZE,
            ))
            .with_child(Shrinkable::new(1., input).finish())
            .finish();
        render_row_container(
            row,
            PROJECT_ROW_VERTICAL_PADDING,
            Some(rows.palette.row_hover),
        )
    }

    /// The workspace's icon, name and how many folders it has. On hover come its "⋯" menu and
    /// the button for a new thread. Clicking the row collapses or expands it.
    fn render_workspace_row(
        &self,
        workspace: &ThreadWorkspace,
        rows: RowContext<'_>,
    ) -> Box<dyn Element> {
        if self.naming_workspace == Some(WorkspaceNaming::Rename(workspace.id.clone())) {
            return self.render_workspace_name_input(rows);
        }
        let RowContext {
            palette,
            fonts,
            appearance,
            ..
        } = rows;
        let id = workspace.id.clone();
        let name = workspace.name.clone();
        let dir_count = match workspace.dirs.len() {
            1 => "1 pasta".to_owned(),
            count => format!("{count} pastas"),
        };
        let is_menu_open = self.workspace_menu_open_for.as_deref() == Some(id.as_str());
        let menu_button = SavePosition::new(
            render_icon_button(
                WarpIcon::DotsHorizontal,
                "Mais opções",
                self.mouse_state(format!("workspace-menu:{id}")),
                ClaudeThreadsAction::ToggleWorkspaceMenu(id.clone()),
                palette,
                appearance,
            ),
            &workspace_menu_button_position_id(&id),
        )
        .finish();
        let new_thread_button = render_icon_button(
            WarpIcon::Plus,
            "Nova thread no workspace",
            self.mouse_state(format!("workspace-new:{id}")),
            ClaudeThreadsAction::NewWorkspaceThread(id.clone()),
            palette,
            appearance,
        );
        Hoverable::new(
            self.mouse_state(format!("workspace:{id}")),
            move |hover_state| {
                let mut meta = Flex::row()
                    .with_main_axis_size(MainAxisSize::Min)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(2.);
                if hover_state.is_hovered() || is_menu_open {
                    meta.add_child(menu_button);
                    meta.add_child(new_thread_button);
                } else {
                    meta.add_child(
                        Text::new_inline(dir_count, fonts.family, META_FONT_SIZE)
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
                    .with_child(render_svg(
                        WORKSPACE_ICON,
                        palette.main_text,
                        FOLDER_ICON_SIZE,
                    ))
                    .with_child(
                        Shrinkable::new(
                            1.,
                            Text::new_inline(name, fonts.family, PROJECT_FONT_SIZE)
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
                let background =
                    (hover_state.is_hovered() || is_menu_open).then_some(palette.row_hover);
                render_row_container(row, PROJECT_ROW_VERTICAL_PADDING, background)
            },
        )
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(ClaudeThreadsAction::ToggleWorkspaceCollapsed(id.clone()));
        })
        .with_defer_events_to_children()
        .finish()
    }

    /// A folder of a workspace with the branch checked out in it; on hover, the button that
    /// removes it from the workspace.
    fn render_workspace_dir_row(
        &self,
        workspace_id: &str,
        dir: &Path,
        rows: RowContext<'_>,
    ) -> Box<dyn Element> {
        let RowContext {
            palette,
            fonts,
            appearance,
            ..
        } = rows;
        let name = dir_name(dir);
        let branch = self.git_branches.get(dir).cloned();
        let key = format!("{workspace_id}:{}", dir.display());
        let remove_button = render_icon_button(
            WarpIcon::X,
            "Remover pasta do workspace",
            self.mouse_state(format!("workspace-dir-remove:{key}")),
            ClaudeThreadsAction::RemoveWorkspaceDir {
                workspace_id: workspace_id.to_owned(),
                dir: dir.to_path_buf(),
            },
            palette,
            appearance,
        );
        Hoverable::new(
            self.mouse_state(format!("workspace-dir:{key}")),
            move |hover_state| {
                let mut label = Flex::row()
                    .with_main_axis_size(MainAxisSize::Min)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(6.)
                    .with_child(render_svg(FOLDER_ICON, palette.sub_text, DIR_ICON_SIZE))
                    .with_child(
                        Shrinkable::new(
                            1.,
                            Text::new_inline(name, fonts.family, META_FONT_SIZE + 1.)
                                .with_clip(ClipConfig::ellipsis())
                                .with_color(palette.sub_text.into())
                                .finish(),
                        )
                        .finish(),
                    );
                if let Some(branch) = branch {
                    label.add_child(render_icon(
                        WarpIcon::GitBranch,
                        palette.detail_text,
                        DIR_ICON_SIZE - 2.,
                    ));
                    label.add_child(
                        Shrinkable::new(
                            1.,
                            Text::new_inline(branch, fonts.family, META_FONT_SIZE)
                                .with_clip(ClipConfig::ellipsis())
                                .with_color(palette.detail_text.into())
                                .finish(),
                        )
                        .finish(),
                    );
                }
                let trailing = if hover_state.is_hovered() {
                    remove_button
                } else {
                    Empty::new().finish()
                };
                let row = Flex::row()
                    .with_main_axis_size(MainAxisSize::Max)
                    .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(ICON_TEXT_GAP)
                    .with_child(Shrinkable::new(1., label.finish()).finish())
                    .with_child(
                        ConstrainedBox::new(trailing)
                            .with_height(META_HEIGHT)
                            .finish(),
                    )
                    .finish();
                let content = Container::new(row)
                    .with_padding_left(THREAD_INDENT)
                    .finish();
                render_row_container(content, 2., None)
            },
        )
        .with_defer_events_to_children()
        .finish()
    }

    /// "Adicionar pasta", below a workspace's folders.
    fn render_add_workspace_dir_row(
        &self,
        workspace_id: &str,
        rows: RowContext<'_>,
    ) -> Box<dyn Element> {
        let RowContext { palette, fonts, .. } = rows;
        let action = ClaudeThreadsAction::AddWorkspaceDirs(workspace_id.to_owned());
        Hoverable::new(
            self.mouse_state(format!("workspace-add-dir:{workspace_id}")),
            move |hover_state| {
                let color = if hover_state.is_hovered() {
                    palette.main_text
                } else {
                    palette.detail_text
                };
                let content = Flex::row()
                    .with_main_axis_size(MainAxisSize::Max)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(6.)
                    .with_child(render_icon(WarpIcon::Plus, color, DIR_ICON_SIZE))
                    .with_child(
                        Text::new_inline("Adicionar pasta", fonts.family, META_FONT_SIZE)
                            .with_color(color.into())
                            .finish(),
                    )
                    .finish();
                let content = Container::new(content)
                    .with_padding_left(THREAD_INDENT)
                    .finish();
                render_row_container(content, 3., None)
            },
        )
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
        .finish()
    }

    /// The app's name, with the buttons that show the search field and hide the sidebar.
    fn render_header(&self, rows: RowContext<'_>) -> Box<dyn Element> {
        let RowContext {
            palette,
            fonts,
            appearance,
            ..
        } = rows;
        let buttons = Flex::row()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(4.)
            .with_child(render_svg_button(
                SEARCH_ICON,
                "Buscar threads",
                self.mouse_state("header:search".to_owned()),
                HEADER_ICON_SIZE,
                palette,
                appearance,
                |ctx| ctx.dispatch_typed_action(ClaudeThreadsAction::ToggleSearch),
            ))
            .with_child(render_svg_button(
                SIDEBAR_ICON,
                "Recolher painel",
                self.mouse_state("header:sidebar".to_owned()),
                HEADER_ICON_SIZE,
                palette,
                appearance,
                |ctx| ctx.dispatch_typed_action(WorkspaceAction::ToggleClaudeThreadsPanel),
            ))
            .finish();
        let row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(
                Text::new_inline("Picolas Code", fonts.family, HEADER_FONT_SIZE)
                    .with_style(Properties::default().weight(Weight::Semibold))
                    .with_color(palette.main_text.into())
                    .finish(),
            )
            .with_child(buttons)
            .finish();
        Container::new(row)
            .with_padding_left(PANEL_PADDING + ROW_HORIZONTAL_PADDING)
            .with_padding_right(PANEL_PADDING + 2.)
            .with_padding_top(12.)
            .with_padding_bottom(10.)
            .finish()
    }

    /// "Novo chat" (a chat in the home folder) and "Artefatos" (the gallery of what Claude made).
    fn render_new_chat_row(&self, rows: RowContext<'_>) -> Box<dyn Element> {
        Container::new(
            Flex::column()
                .with_child(self.render_nav_row(
                    "new-chat",
                    NEW_CHAT_ICON,
                    "Novo chat",
                    ClaudeThreadsAction::NewChat,
                    rows,
                ))
                .with_child(self.render_nav_row(
                    "artifacts",
                    ARTIFACTS_ICON,
                    "Artefatos",
                    ClaudeThreadsAction::OpenArtifacts,
                    rows,
                ))
                .finish(),
        )
        .with_horizontal_padding(PANEL_PADDING)
        .with_padding_bottom(PANEL_PADDING)
        .finish()
    }

    fn render_nav_row(
        &self,
        key: &str,
        icon: &'static str,
        label: &'static str,
        action: ClaudeThreadsAction,
        rows: RowContext<'_>,
    ) -> Box<dyn Element> {
        let RowContext { palette, fonts, .. } = rows;
        Hoverable::new(self.mouse_state(key.to_owned()), move |state| {
            let content = Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(ICON_TEXT_GAP)
                .with_child(render_svg(icon, palette.main_text, NEW_CHAT_ICON_SIZE))
                .with_child(
                    Text::new_inline(label, fonts.family, TITLE_FONT_SIZE)
                        .with_color(palette.main_text.into())
                        .finish(),
                )
                .finish();
            let background = state.is_hovered().then_some(palette.row_hover);
            render_row_container(content, PROJECT_ROW_VERTICAL_PADDING, background)
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
        .finish()
    }

    /// A folder icon, open while the project is expanded, and the folder's name. On hover come
    /// the buttons for a new thread and removing the project. Clicking the row collapses or
    /// expands it.
    fn render_project_row(
        &self,
        project: &ThreadProject,
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
        let folder_icon = if project.collapsed && self.search_query.is_empty() {
            FOLDER_ICON
        } else {
            FOLDER_OPEN_ICON
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
                }
                let meta = ConstrainedBox::new(Align::new(meta.finish()).right().finish())
                    .with_height(META_HEIGHT)
                    .finish();

                let label = Flex::row()
                    .with_main_axis_size(MainAxisSize::Max)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(ICON_TEXT_GAP)
                    .with_child(render_svg(folder_icon, palette.main_text, FOLDER_ICON_SIZE))
                    .with_child(
                        Shrinkable::new(
                            1.,
                            Text::new_inline(name, fonts.family, PROJECT_FONT_SIZE)
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

    /// The latest `RECENT_THREAD_LIMIT` threads of `list` and the toggle that shows the rest.
    fn add_thread_rows(
        &self,
        list: &mut Flex,
        threads: &[ClaudeThread],
        thread_list: ThreadList,
        indent: f32,
        rows: RowContext<'_>,
    ) {
        let show_all =
            !self.search_query.is_empty() || self.show_all_threads.contains(&thread_list);
        let shown = if show_all {
            threads.len()
        } else {
            threads.len().min(RECENT_THREAD_LIMIT)
        };
        for thread in &threads[..shown] {
            list.add_child(self.render_thread_row(thread, indent, rows));
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
        let mouse_state = self.mouse_state(format!("show-all:{thread_list:?}"));
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
                ctx.dispatch_typed_action(ClaudeThreadsAction::ToggleShowAllThreads(
                    thread_list.clone(),
                ));
            })
            .finish(),
        );
    }

    /// The title with its age, or what the session is doing. The thread open in the focused pane
    /// gets a raised card. On hover the age gives way to the archive button and the "⋯" menu;
    /// deleting asks for confirmation in the row.
    fn render_thread_row(
        &self,
        thread: &ClaudeThread,
        indent: f32,
        rows: RowContext<'_>,
    ) -> Box<dyn Element> {
        let RowContext {
            now,
            active_terminal_id,
            active_chat_session_id,
            palette,
            fonts,
            appearance,
            app,
        } = rows;
        let running_session =
            CLIAgentSessionsModel::as_ref(app).session_by_agent_session_id(&thread.session_id);
        let is_active = running_session
            .is_some_and(|(terminal_view_id, _)| Some(terminal_view_id) == active_terminal_id)
            || active_chat_session_id == Some(thread.session_id.as_str());
        let status = running_session
            .map(|(_, session)| {
                if matches!(session.status, CLIAgentSessionStatus::Blocked { .. }) {
                    ThreadStatus::Blocked
                } else if session.is_working_on_prompt() {
                    ThreadStatus::Working
                } else {
                    ThreadStatus::Open
                }
            })
            .or_else(|| {
                NativeChatRegistry::as_ref(app)
                    .status(&thread.session_id, app)
                    .map(|status| match status {
                        NativeChatStatus::Working => ThreadStatus::Working,
                        NativeChatStatus::AwaitingUser => ThreadStatus::Blocked,
                        NativeChatStatus::Idle => ThreadStatus::Open,
                    })
            });
        let title = thread.title.clone();
        let age = claude_threads::format_age(thread.updated_at, now);
        let is_codex = thread.provider == ChatProvider::Codex;
        let action = ClaudeThreadsAction::OpenThread {
            session_id: thread.session_id.clone(),
            cwd: thread.cwd.clone(),
        };
        let session_id = &thread.session_id;
        let is_confirming = self.pending_delete.as_deref() == Some(session_id.as_str());
        let is_menu_open = self.menu_open_for.as_deref() == Some(session_id.as_str());
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
        // An unsent chat has no conversation to archive yet.
        let archive_button = (!self.is_unsent_chat(session_id, app)).then(|| {
            let archive_action = ClaudeThreadsAction::ToggleArchiveThread(session_id.clone());
            render_svg_button(
                ARCHIVE_ICON,
                if self.config.is_archived(session_id) {
                    "Desarquivar"
                } else {
                    "Arquivar"
                },
                self.mouse_state(format!("thread-archive:{session_id}")),
                ACTION_ICON_SIZE,
                palette,
                appearance,
                move |ctx| ctx.dispatch_typed_action(archive_action.clone()),
            )
        });
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
        // other opens in a native chat split.
        let can_drop = thread.resume_command().is_some();
        let drop_cwd = thread.cwd.clone();
        let is_running = running_session.is_some();
        let is_renaming = rename_input.is_some();

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
                    if let Some(archive_button) = archive_button {
                        meta.add_child(archive_button);
                    }
                    meta.add_child(menu_button);
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
                            if is_codex {
                                meta.add_child(
                                    Text::new_inline("Codex · ", fonts.family, META_FONT_SIZE)
                                        .with_color(palette.detail_text.into())
                                        .finish(),
                                );
                            }
                            meta.add_child(
                                Text::new_inline(age, fonts.family, META_FONT_SIZE)
                                    .with_color(palette.detail_text.into())
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
                                    .with_clip(ClipConfig::ellipsis())
                                    .with_color(title_color.into())
                                    .finish()
                            }),
                        )
                        .finish(),
                    )
                    .with_child(meta)
                    .finish();

                let content = title_line;
                let content = Container::new(content).with_padding_left(indent).finish();
                let background = if is_active {
                    Some(palette.row_active)
                } else {
                    (hover_state.is_hovered() || is_menu_open).then_some(palette.row_hover)
                };
                render_row_container(content, THREAD_ROW_VERTICAL_PADDING, background)
            },
        );
        // While renaming, pressing and dragging in the row selects the title's text.
        let row = if is_renaming {
            row
        } else {
            row.with_cursor(Cursor::PointingHand)
                .on_click(move |ctx, _, _| {
                    ctx.dispatch_typed_action(action.clone());
                })
        }
        .with_defer_events_to_children()
        .finish();

        // Dragging a thread onto a pane of the active tab splits it there (see
        // `tab_merge`): an open thread moves its pane, so the same conversation
        // never runs twice.
        if is_renaming || (!can_drop && !is_running) {
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
                    cwd: drop_cwd.clone(),
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

fn render_empty_state(rows: RowContext<'_>) -> Box<dyn Element> {
    render_hint(
        "Adicione uma pasta com o + de Projetos para ver as conversas do Claude Code feitas nela.",
        rows,
    )
}

fn render_hint(text: &'static str, rows: RowContext<'_>) -> Box<dyn Element> {
    let hint = Text::new(text, rows.fonts.family, META_FONT_SIZE)
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

/// One of the panel's own outline icons, from `bundled/svg`.
fn render_svg(path: &'static str, color: WarpThemeFill, size: f32) -> Box<dyn Element> {
    ConstrainedBox::new(SvgIcon::new(path, color.into_solid()).finish())
        .with_width(size)
        .with_height(size)
        .finish()
}

/// `render_icon_button` for the panel's own outline icons, with any click handler.
fn render_svg_button(
    path: &'static str,
    tooltip: &'static str,
    mouse_state: MouseStateHandle,
    size: f32,
    palette: Palette,
    appearance: &Appearance,
    on_click: impl Fn(&mut EventContext) + 'static,
) -> Box<dyn Element> {
    let ui_builder = appearance.ui_builder().clone();
    Hoverable::new(mouse_state, move |hover_state| {
        let color = if hover_state.is_hovered() {
            palette.main_text
        } else {
            palette.sub_text
        };
        let button = Container::new(render_svg(path, color, size))
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
    .on_click(move |ctx, _, _| on_click(ctx))
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
