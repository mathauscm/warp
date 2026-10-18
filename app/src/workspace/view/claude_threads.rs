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

use pathfinder_color::ColorU;
use pathfinder_geometry::vector::vec2f;
use warp_core::ui::Icon as WarpIcon;
use warp_core::ui::theme::color::internal_colors;
use warp_core::ui::theme::{Fill as WarpThemeFill, HorizontalGradient};
use warpui::r#async::Timer;
use warpui::elements::{
    ChildAnchor, Clipped, ClippedScrollStateHandle, ClippedScrollable, ConstrainedBox, Container,
    CornerRadius, CrossAxisAlignment, DragAxis, Draggable, DraggableState, Element, Empty,
    Fill as ElementFill, Flex, Hoverable, MainAxisAlignment, MainAxisSize, MouseStateHandle,
    OffsetPositioning, Padding, ParentAnchor, ParentElement, ParentOffsetBounds, Radius,
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
    self, ClaudeThread, NEW_THREAD_COMMAND, ProjectThreads, ThreadProject, TranscriptCache,
};
use crate::editor::{EditorView, Event as EditorEvent, SingleLineEditorOptions, TextOptions};
use crate::terminal::CLIAgent;
use crate::terminal::cli_agent_sessions::{CLIAgentSessionStatus, CLIAgentSessionsModel};
use crate::workspace::WorkspaceAction;
use crate::workspace::util::get_context_target_terminal_view;

/// How often the transcripts are re-read while the panel is visible, so new
/// conversations and title changes show up.
const POLL_INTERVAL: Duration = Duration::from_secs(4);
/// Threads written to this recently may be open in another Warp window or
/// app, so they can't be deleted.
const RECENTLY_ACTIVE: Duration = Duration::from_secs(120);
/// Threads listed per project until "Mostrar mais" is clicked.
const RECENT_THREAD_LIMIT: usize = 10;
const PANEL_PADDING: f32 = 8.;
const PROJECT_SPACING: f32 = 6.;
const ROW_SPACING: f32 = 1.;
const ROW_HORIZONTAL_PADDING: f32 = 8.;
const PROJECT_ROW_VERTICAL_PADDING: f32 = 5.;
const THREAD_ROW_VERTICAL_PADDING: f32 = 7.;
const ROW_CORNER_RADIUS: f32 = 8.;
const BUTTON_CORNER_RADIUS: f32 = 6.;
/// Lines a thread's title up with its project's name, past the chevron.
const THREAD_INDENT: f32 = CHEVRON_SIZE + ICON_TEXT_GAP;
const CHEVRON_SIZE: f32 = 12.;
const DETAIL_ICON_SIZE: f32 = 11.;
const LOGO_SIZE: f32 = 13.;
/// Centers the logo on the title line's text.
const LOGO_TOP_OFFSET: f32 = 3.;
const DETAIL_ICON_GAP: f32 = 5.;
const ACTION_ICON_SIZE: f32 = 14.;
const ACTION_BUTTON_PADDING: f32 = 3.;
const ICON_TEXT_GAP: f32 = 6.;
/// Space between a thread's title and its branch line.
const LINE_SPACING: f32 = 3.;
const SECTION_FONT_SIZE: f32 = 11.;
const BRAND_FONT_SIZE: f32 = 15.;
const BRAND_HEIGHT: f32 = 46.;
const BRAND_ICON_SIZE: f32 = 16.;
const BRAND_CORNER_RADIUS: f32 = 10.;
const BRAND_TEXT: ColorU = ColorU {
    r: 0xf5,
    g: 0xf7,
    b: 0xff,
    a: 0xff,
};
const BRAND_GRADIENT_LEFT: ColorU = ColorU {
    r: 0x14,
    g: 0x22,
    b: 0x5a,
    a: 0xff,
};
const BRAND_GRADIENT_RIGHT: ColorU = ColorU {
    r: 0x3a,
    g: 0x6c,
    b: 0xf4,
    a: 0xff,
};
/// Distances of the banner's vertical drafting lines from its right edge.
const BRAND_VERTICAL_LINE_OFFSETS: [f32; 2] = [28., 84.];
const BRAND_HORIZONTAL_LINE_WIDTH: f32 = 140.;
const BRAND_HORIZONTAL_LINE_INSET: f32 = 12.;
const PROJECT_FONT_SIZE: f32 = 13.;
const TITLE_FONT_SIZE: f32 = 13.;
const META_FONT_SIZE: f32 = 11.;
/// Height of a row's right side, which swaps between the age, status, and
/// buttons on hover; fixed so the swap doesn't move the row.
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
    /// The window whose focused pane gets its thread highlighted.
    window_id: WindowId,
    search_editor: ViewHandle<EditorView>,
    /// Lowercased; while set, only the threads whose title or branch contain it are listed, with
    /// every project expanded.
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
            window_id: ctx.window_id(),
            search_editor,
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

impl View for ClaudeThreadsView {
    fn ui_name() -> &'static str {
        "ClaudeThreadsView"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let palette = Palette::new(appearance);
        let fonts = Fonts::new(appearance);
        let active_terminal_id =
            get_context_target_terminal_view(self.window_id, app).map(|view| view.id());

        let mut list = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(PROJECT_SPACING);
        if self.projects.is_empty() {
            list.add_child(render_empty_state(palette, fonts));
        }
        let now = SystemTime::now();
        let is_searching = !self.search_query.is_empty();
        let mut has_matches = false;
        for project in &self.projects {
            let matches = self.matching_threads(project);
            if is_searching && matches.is_empty() {
                continue;
            }
            has_matches = true;
            let mut block = Flex::column()
                .with_main_axis_size(MainAxisSize::Min)
                .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
                .with_spacing(ROW_SPACING)
                .with_child(self.render_project_row(project, palette, fonts, appearance));
            if is_searching || !project.collapsed {
                self.add_thread_rows(
                    &mut block,
                    project,
                    &matches,
                    now,
                    active_terminal_id,
                    palette,
                    fonts,
                    appearance,
                    app,
                );
            }
            list.add_child(
                SavePosition::new(block.finish(), &project_block_position_id(&project.path))
                    .finish(),
            );
        }
        if is_searching && !has_matches {
            list.add_child(render_note("Nenhuma thread encontrada", palette, fonts));
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
            .with_child(self.render_brand(fonts))
            .with_child(self.render_search_bar(palette))
            .with_child(self.render_header(palette, fonts, appearance))
            .with_child(Shrinkable::new(1., scrollable).finish())
            .finish()
    }
}

impl ClaudeThreadsView {
    /// A project's threads, narrowed down to the search query.
    fn matching_threads(&self, project: &ThreadProject) -> Vec<&ClaudeThread> {
        let threads = self
            .threads
            .get(&project.path)
            .map(Vec::as_slice)
            .unwrap_or_default();
        if self.search_query.is_empty() {
            return threads.iter().collect();
        }
        threads
            .iter()
            .filter(|thread| {
                thread.title.to_lowercase().contains(&self.search_query)
                    || thread
                        .git_branch
                        .as_ref()
                        .is_some_and(|branch| branch.to_lowercase().contains(&self.search_query))
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

    /// "Threads" title with the button that adds a project folder.
    fn render_header(
        &self,
        palette: Palette,
        fonts: Fonts,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let title = Text::new_inline("THREADS", fonts.family, SECTION_FONT_SIZE)
            .with_style(fonts.medium)
            .with_color(palette.detail_text.into())
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
        .with_padding_bottom(4.)
        .with_horizontal_padding(PANEL_PADDING + ROW_HORIZONTAL_PADDING)
        .finish()
    }

    /// The app's name on a blueprint-blue banner, with the button that hides the sidebar.
    fn render_brand(&self, fonts: Fonts) -> Box<dyn Element> {
        let toggle_button = Hoverable::new(self.mouse_state("brand:toggle".to_owned()), |state| {
            let alpha = if state.is_hovered() { 100 } else { 75 };
            let icon = ConstrainedBox::new(
                WarpIcon::LeftSidebarClose
                    .to_warpui_icon(WarpThemeFill::Solid(BRAND_TEXT).with_opacity(alpha))
                    .finish(),
            )
            .with_width(BRAND_ICON_SIZE)
            .with_height(BRAND_ICON_SIZE)
            .finish();
            let mut button = Container::new(icon)
                .with_padding(Padding::uniform(4.))
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(BUTTON_CORNER_RADIUS)));
            if state.is_hovered() {
                button = button.with_background(WarpThemeFill::Solid(BRAND_TEXT).with_opacity(14));
            }
            button.finish()
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(|ctx, _, _| ctx.dispatch_typed_action(WorkspaceAction::ToggleVerticalTabsPanel))
        .finish();

        let name = Flex::row()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(5.)
            .with_child(
                Text::new_inline("Picolas", fonts.family, BRAND_FONT_SIZE)
                    .with_style(Properties::default().weight(Weight::Bold))
                    .with_color(BRAND_TEXT)
                    .finish(),
            )
            .with_child(
                Text::new_inline("Code", fonts.family, BRAND_FONT_SIZE)
                    .with_color(WarpThemeFill::Solid(BRAND_TEXT).with_opacity(78).into())
                    .finish(),
            )
            .finish();
        let content = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(10.)
            .with_child(toggle_button)
            .with_child(name)
            .finish();
        let content = ConstrainedBox::new(
            Container::new(content)
                .with_horizontal_padding(10.)
                .finish(),
        )
        .with_height(BRAND_HEIGHT)
        .finish();

        // Faint drafting lines over the gradient, like a blueprint.
        let line = WarpThemeFill::Solid(BRAND_TEXT).with_opacity(10);
        let mut banner = Stack::new().with_child(content);
        for offset in BRAND_VERTICAL_LINE_OFFSETS {
            banner.add_positioned_child(
                ConstrainedBox::new(
                    Container::new(Empty::new().finish())
                        .with_background(line)
                        .finish(),
                )
                .with_width(1.)
                .with_height(BRAND_HEIGHT)
                .finish(),
                OffsetPositioning::offset_from_parent(
                    vec2f(-offset, 0.),
                    ParentOffsetBounds::Unbounded,
                    ParentAnchor::TopRight,
                    ChildAnchor::TopRight,
                ),
            );
        }
        banner.add_positioned_child(
            ConstrainedBox::new(
                Container::new(Empty::new().finish())
                    .with_background(line)
                    .finish(),
            )
            .with_width(BRAND_HORIZONTAL_LINE_WIDTH)
            .with_height(1.)
            .finish(),
            OffsetPositioning::offset_from_parent(
                vec2f(0., -BRAND_HORIZONTAL_LINE_INSET),
                ParentOffsetBounds::Unbounded,
                ParentAnchor::BottomRight,
                ChildAnchor::BottomRight,
            ),
        );

        let banner = Container::new(Clipped::new(banner.finish()).finish())
            .with_background(WarpThemeFill::HorizontalGradient(HorizontalGradient::new(
                BRAND_GRADIENT_LEFT,
                BRAND_GRADIENT_RIGHT,
            )))
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(BRAND_CORNER_RADIUS)))
            .finish();
        Container::new(banner)
            .with_uniform_padding(PANEL_PADDING)
            .finish()
    }

    /// Chevron, folder name and thread count. On hover the count gives way to
    /// buttons for a new thread and removing the project. Clicking the row
    /// collapses or expands it.
    fn render_project_row(
        &self,
        project: &ThreadProject,
        palette: Palette,
        fonts: Fonts,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let path = project.path.clone();
        let key = path.display().to_string();
        let name = project.name();
        let thread_count = self.threads.get(&project.path).map_or(0, Vec::len);
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

    #[allow(clippy::too_many_arguments)]
    fn add_thread_rows(
        &self,
        list: &mut Flex,
        project: &ThreadProject,
        threads: &[&ClaudeThread],
        now: SystemTime,
        active_terminal_id: Option<EntityId>,
        palette: Palette,
        fonts: Fonts,
        appearance: &Appearance,
        app: &AppContext,
    ) {
        if threads.is_empty() {
            if self.has_scanned {
                list.add_child(render_note("Nenhuma conversa ainda", palette, fonts));
            }
            return;
        }

        let show_all =
            !self.search_query.is_empty() || self.show_all_threads.contains(&project.path);
        let shown = if show_all {
            threads.len()
        } else {
            threads.len().min(RECENT_THREAD_LIMIT)
        };
        for thread in &threads[..shown] {
            list.add_child(self.render_thread_row(
                thread,
                project,
                now,
                active_terminal_id,
                palette,
                fonts,
                appearance,
                app,
            ));
        }

        if self.search_query.is_empty() && threads.len() > RECENT_THREAD_LIMIT {
            let (label, icon) = if show_all {
                ("Mostrar menos".to_owned(), WarpIcon::ChevronUp)
            } else {
                (
                    format!("Mostrar mais ({})", threads.len() - RECENT_THREAD_LIMIT),
                    WarpIcon::ChevronDown,
                )
            };
            let path = project.path.clone();
            let mouse_state = self.mouse_state(format!("show-all:{}", path.display()));
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
                    let content = Container::new(content)
                        .with_padding_left(THREAD_INDENT)
                        .finish();
                    render_row_container(content, PROJECT_ROW_VERTICAL_PADDING, None)
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

    /// Two lines: the title with its age (or what the session is doing), and
    /// the branch it runs on. The thread open in the focused pane gets a
    /// raised card. On hover the age gives way to a trash button, which asks
    /// for confirmation in the row before moving the conversation to the
    /// Trash.
    #[allow(clippy::too_many_arguments)]
    fn render_thread_row(
        &self,
        thread: &ClaudeThread,
        project: &ThreadProject,
        now: SystemTime,
        active_terminal_id: Option<EntityId>,
        palette: Palette,
        fonts: Fonts,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let running_session =
            CLIAgentSessionsModel::as_ref(app).session_by_agent_session_id(&thread.session_id);
        // A thread open in a tab, or written to moments ago (maybe by another
        // app), is in use and can't be deleted.
        let recently_active = now
            .duration_since(thread.updated_at)
            .is_ok_and(|age| age < RECENTLY_ACTIVE)
            || thread.updated_at > now;
        let can_delete = running_session.is_none() && !recently_active;
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
        // Threads in a subfolder or a worktree of the project say where they run.
        let detail = match &thread.git_branch {
            Some(branch) => Some((WarpIcon::GitBranch, branch.clone())),
            None => (thread.cwd != project.path)
                .then(|| ThreadProject::new(thread.cwd.clone()).name())
                .map(|folder| (WarpIcon::Folder, folder)),
        };
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
                        Text::new_inline("Apagar?", fonts.family, META_FONT_SIZE)
                            .with_color(palette.main_text.into())
                            .finish(),
                    );
                    for button in buttons {
                        meta.add_child(button);
                    }
                } else if let Some(button) = trash_button.filter(|_| hover_state.is_hovered()) {
                    meta.add_child(button);
                } else {
                    match status {
                        Some(status @ (ThreadStatus::Working | ThreadStatus::Blocked)) => {
                            let color = status.color(palette);
                            meta.add_child(
                                Text::new_inline("●", fonts.family, META_FONT_SIZE)
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
                                    Text::new_inline("●", fonts.family, META_FONT_SIZE)
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
                            Text::new_inline(title, fonts.family, TITLE_FONT_SIZE)
                                .with_style(fonts.medium)
                                .with_clip(ClipConfig::ellipsis())
                                .with_color(title_color.into())
                                .finish(),
                        )
                        .finish(),
                    )
                    .with_child(meta)
                    .finish();

                let mut detail_line = Flex::row()
                    .with_main_axis_size(MainAxisSize::Max)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(DETAIL_ICON_GAP);
                if let Some((icon, text)) = detail {
                    detail_line.add_child(render_icon(icon, palette.detail_text, DETAIL_ICON_SIZE));
                    detail_line.add_child(
                        Shrinkable::new(
                            1.,
                            Text::new_inline(text, fonts.family, META_FONT_SIZE)
                                .with_clip(ClipConfig::ellipsis())
                                .with_color(palette.detail_text.into())
                                .finish(),
                        )
                        .finish(),
                    );
                } else {
                    detail_line.add_child(
                        Text::new_inline("Claude Code", fonts.family, META_FONT_SIZE)
                            .with_color(palette.detail_text.into())
                            .finish(),
                    );
                }

                let lines = Flex::column()
                    .with_main_axis_size(MainAxisSize::Min)
                    .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
                    .with_spacing(LINE_SPACING)
                    .with_child(title_line)
                    .with_child(detail_line.finish())
                    .finish();
                // The logo sits in the indent, under the project's chevron, level with the title.
                let logo = ConstrainedBox::new(
                    Container::new(render_icon(WarpIcon::ClaudeLogo, palette.claude, LOGO_SIZE))
                        .with_padding_top(LOGO_TOP_OFFSET)
                        .finish(),
                )
                .with_width(THREAD_INDENT)
                .finish();
                let content = Flex::row()
                    .with_main_axis_size(MainAxisSize::Max)
                    .with_cross_axis_alignment(CrossAxisAlignment::Start)
                    .with_child(logo)
                    .with_child(Shrinkable::new(1., lines).finish())
                    .finish();
                let background = if is_active {
                    Some(palette.row_active)
                } else {
                    hover_state.is_hovered().then_some(palette.row_hover)
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

fn render_empty_state(palette: Palette, fonts: Fonts) -> Box<dyn Element> {
    let hint = Text::new(
        "Adicione uma pasta com o botão acima para ver as conversas do Claude Code feitas nela.",
        fonts.family,
        META_FONT_SIZE,
    )
    .with_color(palette.sub_text.into())
    .finish();
    Container::new(hint)
        .with_horizontal_padding(ROW_HORIZONTAL_PADDING)
        .with_vertical_padding(THREAD_ROW_VERTICAL_PADDING)
        .finish()
}

fn render_note(text: &'static str, palette: Palette, fonts: Fonts) -> Box<dyn Element> {
    Container::new(
        Text::new_inline(text, fonts.family, META_FONT_SIZE)
            .with_color(palette.detail_text.into())
            .finish(),
    )
    .with_padding_left(THREAD_INDENT + ROW_HORIZONTAL_PADDING)
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
