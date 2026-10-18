use std::sync::Arc;

use pathfinder_geometry::vector::vec2f;
use settings::Setting;
use warp_errors::report_if_error;
use warpui::elements::{
    ChildAnchor, ChildView, ConstrainedBox, OffsetPositioning, ParentAnchor, ParentElement,
    ParentOffsetBounds, Stack,
};
use warpui::{
    AppContext, Element, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle,
};

use super::AgentInputButtonTheme;
use crate::appearance::Appearance;
use crate::context_chips::display_menu::{
    ChipMenuType, DisplayChipMenu, GenericMenuItem, PromptDisplayMenuEvent,
};
use crate::settings::{ClaudeTheme, ThemeSettings, ThemeSettingsChangedEvent};
use crate::terminal::input::{MenuPositioning, MenuPositioningProvider};
use crate::ui_components::icons::Icon;
use crate::view_components::action_button::{ActionButton, ButtonSize, TooltipAlignment};

const LABEL: &str = "Tema";
const TOOLTIP: &str = "Tema do Claude Code";
const CHECK_ICON_SIZE: f32 = 14.;
const THEMES: [ClaudeTheme; 4] = [
    ClaudeTheme::ClaudeDark,
    ClaudeTheme::ClaudeLight,
    ClaudeTheme::System,
    ClaudeTheme::Warp,
];

/// Footer button of Claude Code panes that picks the panes' colors.
pub struct ClaudeThemeSelector {
    button: ViewHandle<ActionButton>,
    menu: ViewHandle<DisplayChipMenu>,
    is_menu_open: bool,
    menu_positioning_provider: Arc<dyn MenuPositioningProvider>,
}

pub enum ClaudeThemeSelectorEvent {
    MenuVisibilityChanged { open: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudeThemeSelectorAction {
    ToggleMenu,
}

#[derive(Debug, Clone, Copy)]
struct ThemeMenuItem {
    theme: ClaudeTheme,
    is_selected: bool,
}

impl GenericMenuItem for ThemeMenuItem {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn name(&self) -> String {
        self.theme.label().to_owned()
    }

    fn icon(&self, _app: &AppContext) -> Option<Icon> {
        Some(match self.theme {
            ClaudeTheme::ClaudeDark | ClaudeTheme::ClaudeLight => Icon::PaintBrush,
            ClaudeTheme::System => Icon::Laptop,
            ClaudeTheme::Warp => Icon::Warp,
        })
    }

    fn action_data(&self) -> String {
        self.name()
    }

    fn right_side_element(&self, app: &AppContext) -> Option<Box<dyn Element>> {
        if !self.is_selected {
            return None;
        }
        let theme = Appearance::as_ref(app).theme();
        let color = theme.sub_text_color(theme.surface_2());
        Some(
            ConstrainedBox::new(Icon::Check.to_warpui_icon(color).finish())
                .with_width(CHECK_ICON_SIZE)
                .with_height(CHECK_ICON_SIZE)
                .finish(),
        )
    }
}

impl ClaudeThemeSelector {
    pub fn new(
        menu_positioning_provider: Arc<dyn MenuPositioningProvider>,
        ctx: &mut ViewContext<Self>,
    ) -> Self {
        let current = *ThemeSettings::as_ref(ctx).claude_theme.value();
        let button = ctx.add_typed_action_view(|_ctx| {
            ActionButton::new(LABEL, AgentInputButtonTheme)
                .with_icon(Icon::PaintBrush)
                .with_tooltip(TOOLTIP)
                .with_tooltip_alignment(TooltipAlignment::Left)
                .with_size(ButtonSize::AgentInputButton)
                .on_click(|ctx| {
                    ctx.dispatch_typed_action(ClaudeThemeSelectorAction::ToggleMenu);
                })
        });

        // `CodeReview` is the menu type without a search input.
        let menu = ctx.add_typed_action_view(|ctx| {
            DisplayChipMenu::new(menu_items(current), None, ChipMenuType::CodeReview, ctx)
        });
        ctx.subscribe_to_view(&menu, |me, _, event, ctx| me.handle_menu_event(event, ctx));

        ctx.subscribe_to_model(&ThemeSettings::handle(ctx), |me, _, event, ctx| {
            if matches!(event, ThemeSettingsChangedEvent::ClaudeThemeSetting { .. }) {
                me.refresh(ctx);
            }
        });

        Self {
            button,
            menu,
            is_menu_open: false,
            menu_positioning_provider,
        }
    }

    pub fn is_menu_open(&self) -> bool {
        self.is_menu_open
    }

    fn refresh(&mut self, ctx: &mut ViewContext<Self>) {
        let current = *ThemeSettings::as_ref(ctx).claude_theme.value();
        self.menu.update(ctx, |menu, ctx| {
            menu.update_menu_items(menu_items(current), ctx);
        });
        ctx.notify();
    }

    fn handle_menu_event(&mut self, event: &PromptDisplayMenuEvent, ctx: &mut ViewContext<Self>) {
        match event {
            PromptDisplayMenuEvent::MenuAction(menu_event) => {
                if let Some(item) = menu_event
                    .action_item
                    .as_any()
                    .downcast_ref::<ThemeMenuItem>()
                {
                    let theme = item.theme;
                    ThemeSettings::handle(ctx).update(ctx, |settings, ctx| {
                        report_if_error!(settings.claude_theme.set_value(theme, ctx));
                    });
                }
                self.set_menu_open(false, ctx);
            }
            PromptDisplayMenuEvent::CloseMenu => self.set_menu_open(false, ctx),
        }
    }

    fn set_menu_open(&mut self, open: bool, ctx: &mut ViewContext<Self>) {
        if self.is_menu_open == open {
            return;
        }
        self.is_menu_open = open;
        if open {
            self.menu.update(ctx, |menu, ctx| {
                menu.reset_selected_index();
                ctx.notify();
            });
            ctx.focus(&self.menu);
        }
        ctx.emit(ClaudeThemeSelectorEvent::MenuVisibilityChanged { open });
        ctx.notify();
    }

    fn menu_positioning(&self, app: &AppContext) -> OffsetPositioning {
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

fn menu_items(current: ClaudeTheme) -> Vec<ThemeMenuItem> {
    THEMES
        .into_iter()
        .map(|theme| ThemeMenuItem {
            theme,
            is_selected: theme == current,
        })
        .collect()
}

impl TypedActionView for ClaudeThemeSelector {
    type Action = ClaudeThemeSelectorAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            ClaudeThemeSelectorAction::ToggleMenu => {
                let open = !self.is_menu_open;
                self.set_menu_open(open, ctx);
            }
        }
    }
}

impl View for ClaudeThemeSelector {
    fn ui_name() -> &'static str {
        "ClaudeThemeSelector"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let mut stack = Stack::new();
        stack.add_child(ChildView::new(&self.button).finish());
        if self.is_menu_open {
            stack.add_positioned_overlay_child(
                ChildView::new(&self.menu).finish(),
                self.menu_positioning(app),
            );
        }
        stack.finish()
    }
}

impl Entity for ClaudeThemeSelector {
    type Event = ClaudeThemeSelectorEvent;
}
