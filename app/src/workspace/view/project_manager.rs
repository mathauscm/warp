//! Projects panel of the vertical tabs sidebar: saved project folders grouped
//! by tag, like the "Project Manager" VS Code extension (see
//! `crate::project_manager`). It takes the place of the tab list while the
//! header "Projetos" button is active.
//!
//! Clicking a project opens a new tab in its folder. On hover a project offers
//! Claude Code in its folder, editing its tags, renaming, revealing it in
//! Finder and removing it from the list.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use pathfinder_geometry::vector::vec2f;
use warp_core::ui::Icon as WarpIcon;
use warp_core::ui::theme::Fill as WarpThemeFill;
use warp_core::ui::theme::color::internal_colors;
use warpui::r#async::Timer;
use warpui::elements::{
    ChildAnchor, ClippedScrollStateHandle, ClippedScrollable, ConstrainedBox, Container,
    CornerRadius, CrossAxisAlignment, Element, Fill as ElementFill, Flex, Hoverable,
    MainAxisAlignment, MainAxisSize, MouseStateHandle, OffsetPositioning, Padding, ParentAnchor,
    ParentElement, ParentOffsetBounds, Radius, ScrollbarWidth, Shrinkable, Stack, Text, Wrap,
};
use warpui::platform::{Cursor, FilePickerConfiguration};
use warpui::text_layout::ClipConfig;
use warpui::ui_components::components::{UiComponent, UiComponentStyles};
use warpui::ui_components::text_input::TextInput;
use warpui::{AppContext, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle};

use crate::appearance::Appearance;
use crate::claude_threads::NEW_THREAD_COMMAND;
use crate::editor::{EditorView, Event as EditorEvent, SingleLineEditorOptions, TextOptions};
use crate::project_manager::{self, NO_TAG_LABEL, Project, ViewPrefs};
use crate::terminal::CLIAgent;
use crate::workspace::WorkspaceAction;

/// How often `projects.json` is checked for edits while the panel is visible.
const POLL_INTERVAL: Duration = Duration::from_secs(2);
const PANEL_PADDING: f32 = 8.;
const ROW_VERTICAL_PADDING: f32 = 5.;
const ROW_HORIZONTAL_PADDING: f32 = 6.;
const ROW_CORNER_RADIUS: f32 = 4.;
const ROW_SPACING: f32 = 2.;
const PROJECT_INDENT: f32 = 18.;
const ICON_SIZE: f32 = 14.;
const ACTION_ICON_SIZE: f32 = 14.;
const ACTION_BUTTON_PADDING: f32 = 2.;
const ICON_TEXT_GAP: f32 = 8.;
const TITLE_FONT_SIZE: f32 = 12.;
const META_FONT_SIZE: f32 = 10.;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectManagerAction {
    /// Pick a folder in the macOS picker and save it as a project.
    AddFolder,
    /// Save the active tab's folder as a project.
    SaveActiveDirectory,
    /// Open `projects.json` in the editor.
    EditProjectsFile,
    ToggleViewAsList,
    CycleSort,
    ToggleTagFilterBar,
    ToggleTagFilter(String),
    ToggleTagCollapsed(String),
    OpenProject(usize),
    OpenClaude(usize),
    RevealInFinder(usize),
    StartRename(usize),
    StartEditTags(usize),
    RemoveProject(usize),
}

/// What the inline editor of a project row is changing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditField {
    Name,
    Tags,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProjectEdit {
    index: usize,
    field: EditField,
}

pub struct ProjectManagerView {
    projects: Vec<Project>,
    load_error: Option<String>,
    prefs: ViewPrefs,
    /// Modification time of `projects.json` at the last load or save, so
    /// edits made in the editor are picked up.
    file_modified: Option<SystemTime>,
    is_visible: bool,
    /// Bumped on every scheduled check; older checks are dropped.
    poll_epoch: usize,
    search_editor: ViewHandle<EditorView>,
    search_query: String,
    show_tag_filter: bool,
    tag_filter: Vec<String>,
    /// Inline editor for renaming a project or editing its tags.
    edit_editor: ViewHandle<EditorView>,
    editing: Option<ProjectEdit>,
    scroll_state: ClippedScrollStateHandle,
    /// Hover state of each row and button, keyed by what it belongs to.
    mouse_states: RefCell<HashMap<String, MouseStateHandle>>,
}

impl ProjectManagerView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let search_editor = single_line_editor(ctx);
        search_editor.update(ctx, |editor, ctx| {
            editor.set_placeholder_text("Buscar projetos...", ctx);
        });
        ctx.subscribe_to_view(&search_editor, |me, editor, event, ctx| {
            if let EditorEvent::Edited(_) = event {
                me.search_query = editor.as_ref(ctx).buffer_text(ctx);
                ctx.notify();
            }
        });

        let edit_editor = single_line_editor(ctx);
        ctx.subscribe_to_view(&edit_editor, |me, _, event, ctx| match event {
            EditorEvent::Enter => me.finish_edit(ctx),
            // Like renaming a tab group, leaving the editor discards the edit.
            EditorEvent::Escape | EditorEvent::Blurred => me.cancel_edit(ctx),
            _ => {}
        });

        let mut view = Self {
            projects: Vec::new(),
            load_error: None,
            prefs: project_manager::load_prefs(),
            file_modified: None,
            is_visible: false,
            poll_epoch: 0,
            search_editor,
            search_query: String::new(),
            show_tag_filter: false,
            tag_filter: Vec::new(),
            edit_editor,
            editing: None,
            scroll_state: ClippedScrollStateHandle::default(),
            mouse_states: RefCell::new(HashMap::new()),
        };
        view.reload();
        view
    }

    /// Called when the panel is shown or hidden; `projects.json` is only
    /// watched while it's visible.
    pub fn set_visible(&mut self, visible: bool, ctx: &mut ViewContext<Self>) {
        if self.is_visible == visible {
            return;
        }
        self.is_visible = visible;
        if visible {
            self.reload_if_changed(ctx);
            self.schedule_poll(ctx);
        } else {
            self.cancel_edit(ctx);
        }
    }

    /// Saves `path` as a project, unless it's already saved, and opens the
    /// tags editor so it can be filed under a tag right away.
    pub fn add_project(&mut self, path: PathBuf, ctx: &mut ViewContext<Self>) {
        let index = match self
            .projects
            .iter()
            .position(|project| project.root() == path)
        {
            Some(index) => {
                self.projects[index].enabled = true;
                index
            }
            None => {
                self.projects.push(Project::new(&path));
                self.projects.len() - 1
            }
        };
        self.save();
        self.start_edit(index, EditField::Tags, ctx);
    }

    fn reload(&mut self) {
        match project_manager::load_projects() {
            Ok(projects) => {
                self.projects = projects;
                self.load_error = None;
            }
            Err(err) => self.load_error = Some(err),
        }
        self.file_modified = projects_file_modified();
    }

    fn reload_if_changed(&mut self, ctx: &mut ViewContext<Self>) {
        if self.editing.is_some() || projects_file_modified() == self.file_modified {
            return;
        }
        self.reload();
        ctx.notify();
    }

    fn schedule_poll(&mut self, ctx: &mut ViewContext<Self>) {
        self.poll_epoch += 1;
        let epoch = self.poll_epoch;
        ctx.spawn(
            async { Timer::after(POLL_INTERVAL).await },
            move |me, _, ctx| {
                if me.poll_epoch != epoch || !me.is_visible {
                    return;
                }
                me.reload_if_changed(ctx);
                me.schedule_poll(ctx);
            },
        );
    }

    fn save(&mut self) {
        if let Err(err) = project_manager::save_projects(&self.projects) {
            log::warn!("Failed to save projects: {err}");
        }
        self.file_modified = projects_file_modified();
    }

    fn save_prefs(&self) {
        if let Err(err) = project_manager::save_prefs(&self.prefs) {
            log::warn!("Failed to save project manager preferences: {err}");
        }
    }

    fn pick_folder(&mut self, ctx: &mut ViewContext<Self>) {
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

    fn edit_projects_file(&mut self, ctx: &mut ViewContext<Self>) {
        match project_manager::ensure_projects_file(&self.projects) {
            Ok(path) => {
                ctx.dispatch_typed_action_deferred(WorkspaceAction::OpenFileInSplitPane { path });
            }
            Err(err) => log::warn!("Failed to create projects.json: {err}"),
        }
    }

    fn start_edit(&mut self, index: usize, field: EditField, ctx: &mut ViewContext<Self>) {
        let Some(project) = self.projects.get(index) else {
            return;
        };
        let seed = match field {
            EditField::Name => project.name.clone(),
            EditField::Tags => project.tags.join(", "),
        };
        let placeholder = match field {
            EditField::Name => "Nome do projeto",
            EditField::Tags => "Tags separadas por vírgula",
        };
        self.editing = Some(ProjectEdit { index, field });
        self.edit_editor.update(ctx, move |editor, ctx| {
            editor.clear_buffer_and_reset_undo_stack(ctx);
            editor.set_placeholder_text(placeholder, ctx);
            editor.insert_selected_text(&seed, ctx);
        });
        ctx.focus(&self.edit_editor);
        ctx.notify();
    }

    fn finish_edit(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(edit) = self.editing.take() else {
            return;
        };
        let text = self.edit_editor.as_ref(ctx).buffer_text(ctx);
        if let Some(project) = self.projects.get_mut(edit.index) {
            match edit.field {
                EditField::Name => {
                    let name = text.trim();
                    if !name.is_empty() {
                        project.name = name.to_owned();
                    }
                }
                EditField::Tags => project.tags = project_manager::parse_tags(&text),
            }
            self.save();
        }
        ctx.notify();
    }

    fn cancel_edit(&mut self, ctx: &mut ViewContext<Self>) {
        if self.editing.take().is_some() {
            ctx.notify();
        }
    }

    fn project_root(&self, index: usize) -> Option<PathBuf> {
        self.projects.get(index).map(Project::root)
    }

    fn mouse_state(&self, key: String) -> MouseStateHandle {
        self.mouse_states
            .borrow_mut()
            .entry(key)
            .or_default()
            .clone()
    }
}

fn single_line_editor(ctx: &mut ViewContext<ProjectManagerView>) -> ViewHandle<EditorView> {
    ctx.add_typed_action_view(|ctx| {
        let appearance = Appearance::as_ref(ctx);
        let options = SingleLineEditorOptions {
            text: TextOptions::ui_text(Some(TITLE_FONT_SIZE), appearance),
            ..Default::default()
        };
        EditorView::single_line(options, ctx)
    })
}

fn projects_file_modified() -> Option<SystemTime> {
    std::fs::metadata(project_manager::projects_file())
        .and_then(|metadata| metadata.modified())
        .ok()
}

impl Entity for ProjectManagerView {
    type Event = ();
}

impl TypedActionView for ProjectManagerView {
    type Action = ProjectManagerAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            ProjectManagerAction::AddFolder => self.pick_folder(ctx),
            ProjectManagerAction::SaveActiveDirectory => {
                ctx.dispatch_typed_action_deferred(WorkspaceAction::SaveActiveDirectoryAsProject);
            }
            ProjectManagerAction::EditProjectsFile => self.edit_projects_file(ctx),
            ProjectManagerAction::ToggleViewAsList => {
                self.prefs.view_as_list = !self.prefs.view_as_list;
                self.save_prefs();
                ctx.notify();
            }
            ProjectManagerAction::CycleSort => {
                self.prefs.sort = self.prefs.sort.next();
                self.save_prefs();
                ctx.notify();
            }
            ProjectManagerAction::ToggleTagFilterBar => {
                self.show_tag_filter = !self.show_tag_filter;
                if !self.show_tag_filter {
                    self.tag_filter.clear();
                }
                ctx.notify();
            }
            ProjectManagerAction::ToggleTagFilter(tag) => {
                toggle_in(&mut self.tag_filter, tag);
                ctx.notify();
            }
            ProjectManagerAction::ToggleTagCollapsed(tag) => {
                toggle_in(&mut self.prefs.collapsed_tags, tag);
                self.save_prefs();
                ctx.notify();
            }
            ProjectManagerAction::OpenProject(index) => {
                if let Some(path) = self.project_root(*index) {
                    ctx.dispatch_typed_action_deferred(WorkspaceAction::OpenDirectoryInNewTab {
                        path,
                    });
                }
            }
            ProjectManagerAction::OpenClaude(index) => {
                if let Some(project) = self.projects.get(*index) {
                    ctx.dispatch_typed_action_deferred(WorkspaceAction::RunCommandInNewTab {
                        directory: project.root(),
                        command: NEW_THREAD_COMMAND.to_owned(),
                        group: Some(project.name.clone()),
                    });
                }
            }
            ProjectManagerAction::RevealInFinder(index) => {
                if let Some(path) = self.project_root(*index) {
                    ctx.dispatch_typed_action_deferred(WorkspaceAction::OpenInExplorer { path });
                }
            }
            ProjectManagerAction::StartRename(index) => {
                self.start_edit(*index, EditField::Name, ctx);
            }
            ProjectManagerAction::StartEditTags(index) => {
                self.start_edit(*index, EditField::Tags, ctx);
            }
            ProjectManagerAction::RemoveProject(index) => {
                if *index < self.projects.len() {
                    self.editing = None;
                    self.projects.remove(*index);
                    self.save();
                    ctx.notify();
                }
            }
        }
    }
}

fn toggle_in(list: &mut Vec<String>, value: &str) {
    if let Some(position) = list.iter().position(|item| item == value) {
        list.remove(position);
    } else {
        list.push(value.to_owned());
    }
}

/// Theme colors used by the rows, resolved once per render.
#[derive(Clone, Copy)]
struct Palette {
    main_text: WarpThemeFill,
    sub_text: WarpThemeFill,
    row_hover: WarpThemeFill,
    button_hover: WarpThemeFill,
    selected: WarpThemeFill,
    folder: WarpThemeFill,
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
            selected: internal_colors::fg_overlay_3(theme),
            folder: theme.accent(),
            claude: CLIAgent::Claude
                .brand_color()
                .map(WarpThemeFill::Solid)
                .unwrap_or_else(|| theme.accent()),
        }
    }
}

impl View for ProjectManagerView {
    fn ui_name() -> &'static str {
        "ProjectManagerView"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let palette = Palette::new(appearance);

        let visible = project_manager::visible_projects(
            &self.projects,
            &self.search_query,
            &self.tag_filter,
            self.prefs.sort,
        );

        let mut list = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(ROW_SPACING);
        if let Some(error) = &self.load_error {
            list.add_child(render_note(error.clone(), palette, appearance));
        } else if self.projects.is_empty() {
            list.add_child(render_note(
                "Salve a pasta da aba atual ou adicione uma com o + para montar sua lista."
                    .to_owned(),
                palette,
                appearance,
            ));
        } else if visible.is_empty() {
            list.add_child(render_note(
                "Nenhum projeto encontrado".to_owned(),
                palette,
                appearance,
            ));
        }

        // A project with several tags is listed once per tag, but the inline
        // editor can only be placed in one of those rows.
        let mut editor_placed = false;
        let mut place_editor = |index: usize| {
            let place = !editor_placed && self.editing.is_some_and(|edit| edit.index == index);
            editor_placed |= place;
            place
        };
        if self.prefs.view_as_list {
            for &index in &visible {
                let row = ProjectRow {
                    index,
                    group: "",
                    indent: 0.,
                    show_editor: place_editor(index),
                };
                list.add_child(self.render_project_row(row, palette, appearance));
            }
        } else {
            for (tag, members) in project_manager::group_by_tag(&self.projects, &visible) {
                let collapsed = self.prefs.collapsed_tags.contains(&tag);
                list.add_child(self.render_tag_row(
                    &tag,
                    members.len(),
                    collapsed,
                    palette,
                    appearance,
                ));
                if collapsed {
                    continue;
                }
                for index in members {
                    let row = ProjectRow {
                        index,
                        group: &tag,
                        indent: PROJECT_INDENT,
                        show_editor: place_editor(index),
                    };
                    list.add_child(self.render_project_row(row, palette, appearance));
                }
            }
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

        let mut panel = Flex::column()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(self.render_header(palette, appearance))
            .with_child(self.render_search_bar(palette));
        if self.show_tag_filter {
            panel.add_child(self.render_tag_filter(palette, appearance));
        }
        panel
            .with_child(Shrinkable::new(1., scrollable).finish())
            .finish()
    }
}

impl ProjectManagerView {
    /// "Projetos (N)" with the panel actions, in the extension's order.
    fn render_header(&self, palette: Palette, appearance: &Appearance) -> Box<dyn Element> {
        let enabled = self.projects.iter().filter(|p| p.enabled).count();
        let title = Text::new_inline(
            format!("Projetos ({enabled})"),
            appearance.ui_font_family(),
            TITLE_FONT_SIZE,
        )
        .with_clip(ClipConfig::ellipsis())
        .with_color(palette.sub_text.into())
        .finish();

        let (view_icon, view_tooltip) = if self.prefs.view_as_list {
            (WarpIcon::Dataflow, "Agrupar por tags")
        } else {
            (WarpIcon::ListCollapsed, "Ver como lista")
        };
        let filter_icon = if self.tag_filter.is_empty() {
            WarpIcon::FilterFunnel
        } else {
            WarpIcon::FilterFunnelFilled
        };
        let buttons = [
            (
                WarpIcon::Save,
                "Salvar a pasta da aba atual",
                ProjectManagerAction::SaveActiveDirectory,
            ),
            (
                WarpIcon::Plus,
                "Adicionar pasta",
                ProjectManagerAction::AddFolder,
            ),
            (
                WarpIcon::Edit,
                "Editar projects.json",
                ProjectManagerAction::EditProjectsFile,
            ),
            (
                view_icon,
                view_tooltip,
                ProjectManagerAction::ToggleViewAsList,
            ),
            (
                WarpIcon::Sort,
                self.prefs.sort.label(),
                ProjectManagerAction::CycleSort,
            ),
            (
                filter_icon,
                "Filtrar por tag",
                ProjectManagerAction::ToggleTagFilterBar,
            ),
        ];
        let mut button_row = Flex::row()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(2.);
        for (index, (icon, tooltip, action)) in buttons.into_iter().enumerate() {
            button_row.add_child(render_icon_button(
                icon,
                tooltip,
                self.mouse_state(format!("header:{index}")),
                action,
                palette,
                appearance,
            ));
        }

        Container::new(
            Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(ICON_TEXT_GAP)
                .with_child(Shrinkable::new(1., title).finish())
                .with_child(button_row.finish())
                .finish(),
        )
        .with_vertical_padding(PANEL_PADDING)
        .with_horizontal_padding(PANEL_PADDING + ROW_HORIZONTAL_PADDING)
        .finish()
    }

    fn render_search_bar(&self, palette: Palette) -> Box<dyn Element> {
        let search_icon =
            ConstrainedBox::new(WarpIcon::Search.to_warpui_icon(palette.sub_text).finish())
                .with_width(ICON_SIZE)
                .with_height(ICON_SIZE)
                .finish();
        let row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(6.)
            .with_child(search_icon)
            .with_child(Shrinkable::new(1., render_text_input(&self.search_editor)).finish())
            .finish();
        Container::new(
            Container::new(row)
                .with_horizontal_padding(ROW_HORIZONTAL_PADDING)
                .with_vertical_padding(4.)
                .with_background(palette.row_hover)
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(ROW_CORNER_RADIUS)))
                .finish(),
        )
        .with_horizontal_padding(PANEL_PADDING)
        .with_padding_bottom(PANEL_PADDING)
        .finish()
    }

    /// Chips with every tag (plus "Sem tag"); selected chips filter the list.
    fn render_tag_filter(&self, palette: Palette, appearance: &Appearance) -> Box<dyn Element> {
        let font_family = appearance.ui_font_family();
        let mut tags = project_manager::all_tags(&self.projects);
        tags.push(NO_TAG_LABEL.to_owned());

        let mut chips = Wrap::row().with_spacing(4.).with_run_spacing(4.);
        for tag in tags {
            let is_selected = self.tag_filter.contains(&tag);
            let mouse_state = self.mouse_state(format!("filter:{tag}"));
            let label = tag.clone();
            chips.add_child(
                Hoverable::new(mouse_state, move |hover_state| {
                    let text_color = if is_selected {
                        palette.main_text
                    } else {
                        palette.sub_text
                    };
                    let mut chip = Container::new(
                        Text::new_inline(label, font_family, META_FONT_SIZE)
                            .with_color(text_color.into())
                            .finish(),
                    )
                    .with_horizontal_padding(6.)
                    .with_vertical_padding(2.)
                    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(8.)));
                    if is_selected {
                        chip = chip.with_background(palette.selected);
                    } else if hover_state.is_hovered() {
                        chip = chip.with_background(palette.row_hover);
                    }
                    chip.finish()
                })
                .with_cursor(Cursor::PointingHand)
                .on_click(move |ctx, _, _| {
                    ctx.dispatch_typed_action(ProjectManagerAction::ToggleTagFilter(tag.clone()));
                })
                .finish(),
            );
        }
        Container::new(chips.finish())
            .with_horizontal_padding(PANEL_PADDING + ROW_HORIZONTAL_PADDING)
            .with_padding_bottom(PANEL_PADDING)
            .finish()
    }

    /// Chevron, tag name and how many projects it holds; a click collapses it.
    fn render_tag_row(
        &self,
        tag: &str,
        count: usize,
        collapsed: bool,
        palette: Palette,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let font_family = appearance.ui_font_family();
        let chevron = if collapsed {
            WarpIcon::ChevronRight
        } else {
            WarpIcon::ChevronDown
        };
        let name = tag.to_owned();
        let tag = tag.to_owned();
        Hoverable::new(self.mouse_state(format!("tag:{tag}")), move |hover_state| {
            let row = Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(ICON_TEXT_GAP)
                .with_child(render_icon(chevron, palette.sub_text, 12.))
                .with_child(render_icon(WarpIcon::Tag, palette.sub_text, 12.))
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
                .with_child(
                    Text::new_inline(count.to_string(), font_family, META_FONT_SIZE)
                        .with_color(palette.sub_text.into())
                        .finish(),
                )
                .finish();
            render_row_container(row, hover_state.is_hovered(), palette)
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(ProjectManagerAction::ToggleTagCollapsed(tag.clone()));
        })
        .finish()
    }

    /// Folder icon and name (or the inline editor while it's being renamed or
    /// tagged), with the project actions on hover. A click opens a tab there.
    fn render_project_row(
        &self,
        row: ProjectRow<'_>,
        palette: Palette,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let ProjectRow {
            index,
            group,
            indent,
            show_editor,
        } = row;
        let project = &self.projects[index];
        let font_family = appearance.ui_font_family();
        let key = format!("project:{group}:{index}");
        let editing = self
            .editing
            .filter(|edit| show_editor && edit.index == index);
        let exists = project.root().is_dir();

        let name_element: Box<dyn Element> = match editing {
            Some(edit) if edit.field == EditField::Name => render_text_input(&self.edit_editor),
            _ => {
                let color = if exists {
                    palette.main_text
                } else {
                    palette.sub_text
                };
                Text::new_inline(project.name.clone(), font_family, TITLE_FONT_SIZE)
                    .with_clip(ClipConfig::ellipsis())
                    .with_color(color.into())
                    .finish()
            }
        };
        // Below the name: the tags editor, or a hint when the folder is gone.
        let detail: Option<Box<dyn Element>> = match editing {
            Some(edit) if edit.field == EditField::Tags => {
                Some(render_text_input(&self.edit_editor))
            }
            _ if !exists => Some(
                Text::new_inline("pasta não encontrada", font_family, META_FONT_SIZE)
                    .with_color(palette.sub_text.into())
                    .finish(),
            ),
            _ => None,
        };

        let actions = [
            (
                WarpIcon::ClaudeLogo,
                "Abrir Claude Code aqui",
                ProjectManagerAction::OpenClaude(index),
            ),
            (
                WarpIcon::Tag,
                "Editar tags",
                ProjectManagerAction::StartEditTags(index),
            ),
            (
                WarpIcon::Pencil,
                "Renomear",
                ProjectManagerAction::StartRename(index),
            ),
            (
                WarpIcon::LinkExternal,
                "Revelar no Finder",
                ProjectManagerAction::RevealInFinder(index),
            ),
            (
                WarpIcon::X,
                "Remover da lista",
                ProjectManagerAction::RemoveProject(index),
            ),
        ];
        let action_buttons: Vec<Box<dyn Element>> = actions
            .into_iter()
            .map(|(icon, tooltip, action)| {
                render_icon_button(
                    icon,
                    tooltip,
                    self.mouse_state(format!("{key}:{tooltip}")),
                    action,
                    palette,
                    appearance,
                )
            })
            .collect();
        let is_editing = editing.is_some();

        Hoverable::new(self.mouse_state(key), move |hover_state| {
            let mut text_column = Flex::column()
                .with_main_axis_size(MainAxisSize::Min)
                .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
                .with_spacing(2.)
                .with_child(name_element);
            if let Some(detail) = detail {
                text_column.add_child(detail);
            }
            let label = Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(ICON_TEXT_GAP)
                .with_child(render_icon(WarpIcon::Folder, palette.folder, ICON_SIZE))
                .with_child(Shrinkable::new(1., text_column.finish()).finish())
                .finish();

            let mut row = Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(ICON_TEXT_GAP)
                .with_child(Shrinkable::new(1., label).finish());
            if hover_state.is_hovered() && !is_editing {
                let mut buttons = Flex::row()
                    .with_main_axis_size(MainAxisSize::Min)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(2.);
                for button in action_buttons {
                    buttons.add_child(button);
                }
                row.add_child(buttons.finish());
            }
            let row = Container::new(row.finish())
                .with_padding_left(indent)
                .finish();
            render_row_container(row, hover_state.is_hovered() && !is_editing, palette)
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, _, _| {
            if !is_editing {
                ctx.dispatch_typed_action(ProjectManagerAction::OpenProject(index));
            }
        })
        .with_defer_events_to_children()
        .finish()
    }
}

/// Where a project row sits in the list.
struct ProjectRow<'a> {
    index: usize,
    /// The tag group it's listed under (empty in the list view).
    group: &'a str,
    indent: f32,
    /// Whether this row may hold the inline editor.
    show_editor: bool,
}

fn render_text_input(editor: &ViewHandle<EditorView>) -> Box<dyn Element> {
    TextInput::new(
        editor.clone(),
        UiComponentStyles::default()
            .set_background(ElementFill::None)
            .set_border_radius(CornerRadius::with_all(Radius::Pixels(0.)))
            .set_border_width(0.),
    )
    .build()
    .finish()
}

fn render_note(text: String, palette: Palette, appearance: &Appearance) -> Box<dyn Element> {
    Container::new(
        Text::new(text, appearance.ui_font_family(), TITLE_FONT_SIZE)
            .with_color(palette.sub_text.into())
            .finish(),
    )
    .with_horizontal_padding(ROW_HORIZONTAL_PADDING)
    .with_vertical_padding(ROW_VERTICAL_PADDING)
    .finish()
}

fn render_icon(icon: WarpIcon, color: WarpThemeFill, size: f32) -> Box<dyn Element> {
    ConstrainedBox::new(icon.to_warpui_icon(color).finish())
        .with_width(size)
        .with_height(size)
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

/// Small icon button with a tooltip below it. The Claude button keeps the
/// Claude color.
fn render_icon_button(
    icon: WarpIcon,
    tooltip: &'static str,
    mouse_state: MouseStateHandle,
    action: ProjectManagerAction,
    palette: Palette,
    appearance: &Appearance,
) -> Box<dyn Element> {
    let ui_builder = appearance.ui_builder().clone();
    let icon_color = if icon == WarpIcon::ClaudeLogo {
        palette.claude
    } else {
        palette.sub_text
    };
    Hoverable::new(mouse_state, move |hover_state| {
        let icon = ConstrainedBox::new(icon.to_warpui_icon(icon_color).finish())
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
