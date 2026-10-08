use std::path::PathBuf;
use std::sync::Arc;

use pathfinder_geometry::vector::vec2f;
use warpui::elements::{
    ChildAnchor, ChildView, OffsetPositioning, ParentAnchor, ParentElement, ParentOffsetBounds,
    Stack, Text,
};
use warpui::{
    AppContext, Element, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle,
};

use super::AgentInputButtonTheme;
use crate::appearance::Appearance;
use crate::context_chips::display_menu::{
    ChipMenuType, DisplayChipMenu, GenericMenuItem, PromptDisplayMenuEvent,
};
use crate::terminal::input::{MenuPositioning, MenuPositioningProvider};
use crate::ui_components::icons::Icon;
use crate::view_components::DismissibleToast;
use crate::view_components::action_button::{ActionButton, ButtonSize, TooltipAlignment};
use crate::workspace::{ToastStack, WorkspaceAction};
use crate::worktrees::{
    WorktreeBranch, WorktreeOutcome, WorktreeWorkspace, create_branch, list_branches,
    resolve_workspace, switch_branch,
};

const BUTTON_LABEL: &str = "Worktree";
const SEARCH_PLACEHOLDER: &str = "Buscar branch...";
const CREATE_PLACEHOLDER: &str = "Nome da nova branch (Enter para criar)";
const LOADING_TEXT: &str = "Carregando branches...";
const TOOLTIP: &str = "Criar ou buscar branch no worktree";

/// Returns the directory the worktree workspace is resolved from.
pub type CwdSource = Arc<dyn Fn(&AppContext) -> Option<PathBuf>>;

/// Where the button lives, which decides how it looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeButtonStyle {
    /// Labeled chip in the agent input footer.
    Footer,
    /// Icon-only button in the tab bar.
    Toolbar,
}

/// Opens the menu below the button, for buttons at the top of the window.
pub struct BelowButtonPositioning;

impl MenuPositioningProvider for BelowButtonPositioning {
    fn menu_position(&self, _app: &AppContext) -> MenuPositioning {
        MenuPositioning::BelowInputBox
    }
}

/// Which menu is showing. The button opens [`MenuMode::Options`], from which
/// the user picks between creating a branch and searching the existing ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuMode {
    Closed,
    Options,
    Search,
    Create,
}

/// Footer button that switches the workspace's worktree slot to a branch, or
/// creates a new branch there, and then opens a new tab in the worktree.
/// The git work lives in [`crate::worktrees`].
pub struct WorktreeSelector {
    button: ViewHandle<ActionButton>,
    options_menu: ViewHandle<DisplayChipMenu>,
    search_menu: ViewHandle<DisplayChipMenu>,
    create_menu: ViewHandle<DisplayChipMenu>,
    mode: MenuMode,
    menu_positioning_provider: Arc<dyn MenuPositioningProvider>,
    cwd_source: CwdSource,
    style: WorktreeButtonStyle,
    /// The terminal's directory when the menu was opened.
    cwd: Option<PathBuf>,
    /// Resolved in the background when the menu opens.
    workspace: Option<WorktreeWorkspace>,
    /// Discards branch lists from a previous opening of the menu.
    load_epoch: usize,
    /// True while a switch or create runs; the button is disabled meanwhile.
    is_busy: bool,
}

pub enum WorktreeSelectorEvent {
    MenuVisibilityChanged { open: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeSelectorAction {
    ToggleMenu,
}

#[derive(Debug, Clone, Copy)]
enum OptionMenuItem {
    CreateBranch,
    SearchBranch,
}

impl GenericMenuItem for OptionMenuItem {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn name(&self) -> String {
        match self {
            Self::CreateBranch => "Criar branch".to_owned(),
            Self::SearchBranch => "Buscar branch".to_owned(),
        }
    }

    fn icon(&self, _app: &AppContext) -> Option<Icon> {
        Some(match self {
            Self::CreateBranch => Icon::Plus,
            Self::SearchBranch => Icon::Search,
        })
    }

    fn action_data(&self) -> String {
        self.name()
    }
}

#[derive(Debug, Clone)]
struct BranchMenuItem {
    branch: WorktreeBranch,
    /// Set when only some of the workspace repos have this branch.
    partial_note: Option<String>,
}

impl GenericMenuItem for BranchMenuItem {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn name(&self) -> String {
        self.branch.name.clone()
    }

    fn icon(&self, _app: &AppContext) -> Option<Icon> {
        Some(if self.branch.is_current {
            Icon::Check
        } else {
            Icon::GitBranch
        })
    }

    fn action_data(&self) -> String {
        self.branch.name.clone()
    }

    fn right_side_element(&self, app: &AppContext) -> Option<Box<dyn Element>> {
        let note = self.partial_note.clone()?;
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        Some(
            Text::new_inline(note, appearance.ui_font_family(), appearance.ui_font_size())
                .with_color(theme.sub_text_color(theme.surface_2()).into_solid())
                .finish(),
        )
    }
}

/// A non-selectable line shown while loading or when there is nothing to list.
#[derive(Debug, Clone)]
struct StatusMenuItem(String);

impl GenericMenuItem for StatusMenuItem {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn name(&self) -> String {
        self.0.clone()
    }

    fn icon(&self, _app: &AppContext) -> Option<Icon> {
        None
    }

    fn action_data(&self) -> String {
        String::new()
    }
}

/// Built from what the user types in the create menu.
#[derive(Debug, Clone)]
struct CreateBranchMenuItem {
    name: String,
}

impl GenericMenuItem for CreateBranchMenuItem {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn name(&self) -> String {
        format!("Criar branch \"{}\"", self.name)
    }

    fn icon(&self, _app: &AppContext) -> Option<Icon> {
        Some(Icon::Plus)
    }

    fn action_data(&self) -> String {
        self.name.clone()
    }
}

impl WorktreeSelector {
    pub fn new(
        menu_positioning_provider: Arc<dyn MenuPositioningProvider>,
        cwd_source: CwdSource,
        style: WorktreeButtonStyle,
        ctx: &mut ViewContext<Self>,
    ) -> Self {
        let button = ctx.add_typed_action_view(|_ctx| {
            ActionButton::new(idle_label(style), AgentInputButtonTheme)
                .with_icon(Icon::GitBranch)
                .with_tooltip(TOOLTIP)
                .with_tooltip_alignment(TooltipAlignment::Left)
                .with_size(ButtonSize::AgentInputButton)
                .on_click(|ctx| {
                    ctx.dispatch_typed_action(WorktreeSelectorAction::ToggleMenu);
                })
        });

        // `CodeReview` is the menu type without a search input.
        let options_menu = ctx.add_typed_action_view(|ctx| {
            DisplayChipMenu::new(
                vec![OptionMenuItem::CreateBranch, OptionMenuItem::SearchBranch],
                None,
                ChipMenuType::CodeReview,
                ctx,
            )
        });

        let search_menu = ctx.add_typed_action_view(|ctx| {
            let mut menu = DisplayChipMenu::new(
                Vec::<StatusMenuItem>::new(),
                None,
                ChipMenuType::Branches,
                ctx,
            );
            menu.set_search_placeholder(SEARCH_PLACEHOLDER, ctx);
            menu
        });

        let create_menu = ctx.add_typed_action_view(|ctx| {
            let mut menu = DisplayChipMenu::new(
                Vec::<StatusMenuItem>::new(),
                None,
                ChipMenuType::Branches,
                ctx,
            )
            .with_create_item_from_query(Arc::new(|query: &str| {
                if query.is_empty() || query.contains(char::is_whitespace) {
                    return None;
                }
                let item: Arc<dyn GenericMenuItem> = Arc::new(CreateBranchMenuItem {
                    name: query.to_owned(),
                });
                Some(item)
            }));
            menu.set_search_placeholder(CREATE_PLACEHOLDER, ctx);
            menu
        });

        for menu in [&options_menu, &search_menu, &create_menu] {
            ctx.subscribe_to_view(menu, |me, _, event, ctx| me.handle_menu_event(event, ctx));
        }

        Self {
            button,
            options_menu,
            search_menu,
            create_menu,
            mode: MenuMode::Closed,
            menu_positioning_provider,
            cwd_source,
            style,
            cwd: None,
            workspace: None,
            load_epoch: 0,
            is_busy: false,
        }
    }

    pub fn is_menu_open(&self) -> bool {
        self.mode != MenuMode::Closed
    }

    fn handle_menu_event(&mut self, event: &PromptDisplayMenuEvent, ctx: &mut ViewContext<Self>) {
        match event {
            PromptDisplayMenuEvent::MenuAction(menu_event) => {
                let item = menu_event.action_item.as_any();
                if let Some(option) = item.downcast_ref::<OptionMenuItem>() {
                    let mode = match option {
                        OptionMenuItem::CreateBranch => MenuMode::Create,
                        OptionMenuItem::SearchBranch => MenuMode::Search,
                    };
                    self.set_mode(mode, ctx);
                } else if let Some(branch_item) = item.downcast_ref::<BranchMenuItem>() {
                    let branch = branch_item.branch.clone();
                    self.set_mode(MenuMode::Closed, ctx);
                    self.run_switch(branch, ctx);
                } else if let Some(create_item) = item.downcast_ref::<CreateBranchMenuItem>() {
                    let name = create_item.name.clone();
                    self.set_mode(MenuMode::Closed, ctx);
                    self.run_create(name, ctx);
                }
            }
            // Escape steps back to the options before closing the menu.
            PromptDisplayMenuEvent::CloseMenu => {
                let mode = match self.mode {
                    MenuMode::Search | MenuMode::Create => MenuMode::Options,
                    MenuMode::Options | MenuMode::Closed => MenuMode::Closed,
                };
                self.set_mode(mode, ctx);
            }
        }
    }

    fn set_mode(&mut self, mode: MenuMode, ctx: &mut ViewContext<Self>) {
        if self.mode == mode {
            return;
        }
        let was_open = self.is_menu_open();
        self.mode = mode;

        match mode {
            MenuMode::Closed => {}
            MenuMode::Options => {
                self.options_menu.update(ctx, |menu, ctx| {
                    menu.reset_selected_index();
                    ctx.notify();
                });
                ctx.focus(&self.options_menu);
                if !was_open {
                    self.load_branches(ctx);
                }
            }
            MenuMode::Search => {
                self.search_menu
                    .update(ctx, |menu, ctx| menu.clear_search(ctx));
                ctx.focus(&self.search_menu);
            }
            MenuMode::Create => {
                self.create_menu
                    .update(ctx, |menu, ctx| menu.clear_search(ctx));
                ctx.focus(&self.create_menu);
            }
        }

        let is_open = self.is_menu_open();
        if was_open != is_open {
            ctx.emit(WorktreeSelectorEvent::MenuVisibilityChanged { open: is_open });
        }
        ctx.notify();
    }

    /// Resolves the workspace and fills the search menu while the user is
    /// still looking at the options.
    fn load_branches(&mut self, ctx: &mut ViewContext<Self>) {
        self.load_epoch += 1;
        let epoch = self.load_epoch;
        self.workspace = None;
        self.cwd = (self.cwd_source)(ctx);
        self.show_search_status(LOADING_TEXT, ctx);

        let Some(cwd) = self.cwd.clone() else {
            self.show_search_status("Não consegui identificar a pasta atual do terminal", ctx);
            return;
        };

        ctx.spawn(
            async move {
                let workspace = resolve_workspace(cwd).await?;
                let branches = list_branches(&workspace).await?;
                Ok::<_, String>((workspace, branches))
            },
            move |me, result, ctx| {
                if me.load_epoch != epoch {
                    return;
                }
                match result {
                    Ok((workspace, branches)) => {
                        let items = branch_menu_items(&workspace, branches);
                        me.workspace = Some(workspace);
                        if items.is_empty() {
                            me.show_search_status("Nenhuma branch para abrir", ctx);
                        } else {
                            me.search_menu.update(ctx, |menu, ctx| {
                                menu.update_menu_items(items, ctx);
                            });
                        }
                    }
                    Err(err) => me.show_search_status(&err, ctx),
                }
            },
        );
    }

    fn show_search_status(&mut self, text: &str, ctx: &mut ViewContext<Self>) {
        let item = StatusMenuItem(text.to_owned());
        self.search_menu.update(ctx, |menu, ctx| {
            menu.update_menu_items(vec![item], ctx);
        });
    }

    fn run_switch(&mut self, branch: WorktreeBranch, ctx: &mut ViewContext<Self>) {
        let Some(workspace) = self.workspace.clone() else {
            return;
        };
        let label = format!("Trocando para {}...", branch.name);
        self.run(label, switch_branch(workspace, branch), ctx);
    }

    fn run_create(&mut self, name: String, ctx: &mut ViewContext<Self>) {
        // The create menu can be used before the branch list finishes loading,
        // so resolve the workspace here when it isn't known yet.
        let workspace = self.workspace.clone();
        let Some(cwd) = self.cwd.clone() else {
            show_toast(
                DismissibleToast::error(
                    "Worktree: não consegui identificar a pasta atual do terminal".to_owned(),
                ),
                ctx,
            );
            return;
        };
        let label = format!("Criando {name}...");
        let work = async move {
            let workspace = match workspace {
                Some(workspace) => workspace,
                None => resolve_workspace(cwd).await?,
            };
            create_branch(workspace, name).await
        };
        self.run(label, work, ctx);
    }

    fn run(
        &mut self,
        label: String,
        work: impl std::future::Future<Output = Result<WorktreeOutcome, String>> + Send + 'static,
        ctx: &mut ViewContext<Self>,
    ) {
        if self.is_busy {
            return;
        }
        self.set_busy(Some(label), ctx);
        ctx.spawn(work, |me, result, ctx| {
            me.set_busy(None, ctx);
            match result {
                Ok(outcome) => {
                    show_toast(DismissibleToast::success(outcome.message), ctx);
                    ctx.dispatch_typed_action_deferred(WorkspaceAction::OpenDirectoryInNewTab {
                        path: outcome.worktree_root,
                    });
                }
                Err(err) => {
                    show_toast(DismissibleToast::error(format!("Worktree: {err}")), ctx);
                }
            }
        });
    }

    fn set_busy(&mut self, label: Option<String>, ctx: &mut ViewContext<Self>) {
        let is_busy = label.is_some();
        self.is_busy = is_busy;
        let label = label.unwrap_or_else(|| idle_label(self.style).to_owned());
        self.button.update(ctx, |button, ctx| {
            button.set_label(label, ctx);
            button.set_disabled(is_busy, ctx);
        });
        ctx.notify();
    }

    fn get_menu_positioning(&self, app: &AppContext) -> OffsetPositioning {
        match self.menu_positioning_provider.menu_position(app) {
            MenuPositioning::BelowInputBox => OffsetPositioning::offset_from_parent(
                vec2f(0., 4.),
                ParentOffsetBounds::WindowByPosition,
                ParentAnchor::BottomLeft,
                ChildAnchor::TopLeft,
            ),
            MenuPositioning::AboveInputBox => OffsetPositioning::offset_from_parent(
                vec2f(0., -4.),
                ParentOffsetBounds::WindowByPosition,
                ParentAnchor::TopLeft,
                ChildAnchor::BottomLeft,
            ),
        }
    }
}

fn idle_label(style: WorktreeButtonStyle) -> &'static str {
    match style {
        WorktreeButtonStyle::Footer => BUTTON_LABEL,
        WorktreeButtonStyle::Toolbar => "",
    }
}

fn branch_menu_items(
    workspace: &WorktreeWorkspace,
    branches: Vec<WorktreeBranch>,
) -> Vec<BranchMenuItem> {
    let repo_count = workspace.repos.len();
    branches
        .into_iter()
        .map(|branch| {
            let partial_note = (repo_count > 1 && branch.repos.len() < repo_count)
                .then(|| format!("só {}", branch.repos.join(", ")));
            BranchMenuItem {
                branch,
                partial_note,
            }
        })
        .collect()
}

fn show_toast(toast: DismissibleToast<WorkspaceAction>, ctx: &mut ViewContext<WorktreeSelector>) {
    let window_id = ctx.window_id();
    ToastStack::handle(ctx).update(ctx, |toast_stack, ctx| {
        toast_stack.add_ephemeral_toast(toast, window_id, ctx);
    });
}

impl TypedActionView for WorktreeSelector {
    type Action = WorktreeSelectorAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            WorktreeSelectorAction::ToggleMenu => {
                if self.is_busy {
                    return;
                }
                let mode = if self.is_menu_open() {
                    MenuMode::Closed
                } else {
                    MenuMode::Options
                };
                self.set_mode(mode, ctx);
            }
        }
    }
}

impl View for WorktreeSelector {
    fn ui_name() -> &'static str {
        "WorktreeSelector"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let mut stack = Stack::new();
        stack.add_child(ChildView::new(&self.button).finish());

        let menu = match self.mode {
            MenuMode::Closed => None,
            MenuMode::Options => Some(&self.options_menu),
            MenuMode::Search => Some(&self.search_menu),
            MenuMode::Create => Some(&self.create_menu),
        };
        if let Some(menu) = menu {
            stack.add_positioned_overlay_child(
                ChildView::new(menu).finish(),
                self.get_menu_positioning(app),
            );
        }

        stack.finish()
    }
}

impl Entity for WorktreeSelector {
    type Event = WorktreeSelectorEvent;
}
