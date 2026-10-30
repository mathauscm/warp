//! The "Provedores" settings page: which CLI new native chats drive (Claude Code or Codex), the
//! model new Codex chats start with, and whether the Codex CLI is installed.

use claude_chat::codex::INSTALL_HINT as CODEX_INSTALL_HINT;
use settings::Setting as _;
use warp_errors::report_if_error;
use warpui::elements::{Container, Element, Flex, ParentElement, Text};
use warpui::keymap::{ContextPredicate, FixedBinding};
use warpui::{
    Action, AppContext, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle,
};

use super::settings_page::{
    MatchData, PageTitle, PageType, SettingsPageEvent, SettingsPageMeta, SettingsPageViewHandle,
    SettingsWidget, render_dropdown_item,
};
use super::{LocalOnlyIconState, SettingsAction, SettingsSection};
use crate::appearance::Appearance;
use crate::native_chat::provider::{
    self, ChatProvider, ChatProviderSettings, CodexDefaultModel, DefaultChatProvider,
};
use crate::util::bindings;
use crate::view_components::{Dropdown, DropdownItem};

const PAGE_TITLE: &str = "Provedores";

pub struct ProvidersPageView {
    page: PageType<Self>,
    provider_dropdown: ViewHandle<Dropdown<ProvidersPageAction>>,
    codex_model_dropdown: ViewHandle<Dropdown<ProvidersPageAction>>,
    /// Where the Codex CLI was found when the page opened, if anywhere.
    codex_path: Option<String>,
    local_only_icon_tooltip_states:
        std::cell::RefCell<std::collections::HashMap<String, warpui::elements::MouseStateHandle>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProvidersPageAction {
    SetDefaultProvider(ChatProvider),
    /// An empty model uses the Codex CLI's own default.
    SetCodexDefaultModel(String),
}

impl ProvidersPageView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let provider_dropdown = ctx.add_typed_action_view(|ctx| {
            let mut dropdown = Dropdown::new(ctx);
            dropdown.set_items(
                ChatProvider::ALL
                    .into_iter()
                    .map(|provider| {
                        DropdownItem::new(
                            provider.label(),
                            ProvidersPageAction::SetDefaultProvider(provider),
                        )
                    })
                    .collect(),
                ctx,
            );
            dropdown
        });
        let codex_model_dropdown = ctx.add_typed_action_view(Dropdown::new);
        ctx.subscribe_to_model(&ChatProviderSettings::handle(ctx), |me, _, _, ctx| {
            me.sync_dropdowns(ctx);
        });
        let mut page = Self {
            page: PageType::new_uncategorized(
                vec![
                    Box::new(DefaultProviderWidget),
                    Box::new(CodexModelWidget),
                    Box::new(CodexCliWidget),
                ],
                Some(PageTitle::new(PAGE_TITLE)),
            ),
            provider_dropdown,
            codex_model_dropdown,
            codex_path: provider::codex_cli_path().map(|path| path.display().to_string()),
            local_only_icon_tooltip_states: Default::default(),
        };
        page.sync_dropdowns(ctx);
        page
    }

    /// Shows the saved choices, listing the Codex models known now.
    fn sync_dropdowns(&mut self, ctx: &mut ViewContext<Self>) {
        let settings = ChatProviderSettings::as_ref(ctx);
        let default_provider = *settings.default_provider;
        let codex_model = settings.codex_default_model.value().clone();
        self.provider_dropdown.update(ctx, |dropdown, ctx| {
            dropdown.set_selected_by_action(
                ProvidersPageAction::SetDefaultProvider(default_provider),
                ctx,
            );
        });
        let mut items = vec![DropdownItem::new(
            "Padrão do Codex CLI",
            ProvidersPageAction::SetCodexDefaultModel(String::new()),
        )];
        let mut known = false;
        for model in provider::codex_models() {
            known |= model.id == codex_model;
            items.push(DropdownItem::new(
                model.display_name,
                ProvidersPageAction::SetCodexDefaultModel(model.id),
            ));
        }
        if !known && !codex_model.is_empty() {
            items.push(DropdownItem::new(
                codex_model.clone(),
                ProvidersPageAction::SetCodexDefaultModel(codex_model.clone()),
            ));
        }
        self.codex_model_dropdown.update(ctx, |dropdown, ctx| {
            dropdown.set_items(items, ctx);
            dropdown.set_selected_by_action(
                ProvidersPageAction::SetCodexDefaultModel(codex_model),
                ctx,
            );
        });
        ctx.notify();
    }
}

impl Entity for ProvidersPageView {
    type Event = SettingsPageEvent;
}

impl View for ProvidersPageView {
    fn ui_name() -> &'static str {
        "ProvidersPage"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        self.page.render(self, app)
    }
}

impl TypedActionView for ProvidersPageView {
    type Action = ProvidersPageAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            ProvidersPageAction::SetDefaultProvider(provider) => {
                set_default_provider(*provider, ctx);
            }
            ProvidersPageAction::SetCodexDefaultModel(model) => {
                ChatProviderSettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.codex_default_model.set_value(model.clone(), ctx));
                });
            }
        }
        ctx.notify();
    }
}

/// Saves the provider new chats use.
pub fn set_default_provider(provider: ChatProvider, ctx: &mut AppContext) {
    ChatProviderSettings::handle(ctx).update(ctx, |settings, ctx| {
        report_if_error!(settings.default_provider.set_value(provider, ctx));
    });
}

struct DefaultProviderWidget;

impl SettingsWidget for DefaultProviderWidget {
    type View = ProvidersPageView;

    fn search_terms(&self) -> &str {
        "provedor provider padrão default claude codex openai chat modelo model cli"
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        render_dropdown_item(
            appearance,
            "Provedor padrão",
            Some("Novos chats usam este provedor; conversas já começadas continuam no delas."),
            None,
            LocalOnlyIconState::for_setting(
                DefaultChatProvider::storage_key(),
                DefaultChatProvider::sync_to_cloud(),
                &mut view.local_only_icon_tooltip_states.borrow_mut(),
                app,
            ),
            None,
            &view.provider_dropdown,
        )
    }
}

struct CodexModelWidget;

impl SettingsWidget for CodexModelWidget {
    type View = ProvidersPageView;

    fn search_terms(&self) -> &str {
        "codex modelo model padrão default gpt openai"
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        render_dropdown_item(
            appearance,
            "Modelo padrão do Codex",
            Some("O modelo com que novos chats do Codex começam."),
            None,
            LocalOnlyIconState::for_setting(
                CodexDefaultModel::storage_key(),
                CodexDefaultModel::sync_to_cloud(),
                &mut view.local_only_icon_tooltip_states.borrow_mut(),
                app,
            ),
            None,
            &view.codex_model_dropdown,
        )
    }
}

struct CodexCliWidget;

impl SettingsWidget for CodexCliWidget {
    type View = ProvidersPageView;

    fn search_terms(&self) -> &str {
        "codex cli instalar install npm caminho path openai"
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        _app: &AppContext,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let background = theme.background();
        let font = appearance.ui_font_family();
        let size = appearance.ui_font_size();
        let status = match &view.codex_path {
            Some(path) => format!("Instalado em {path}"),
            None => format!("Codex CLI não encontrado — instale com {CODEX_INSTALL_HINT}"),
        };
        let status_color = if view.codex_path.is_some() {
            theme.sub_text_color(background).into_solid()
        } else {
            theme.ui_error_color()
        };
        Container::new(
            Flex::column()
                .with_spacing(4.)
                .with_child(
                    Text::new_inline("Codex CLI", font, size)
                        .with_color(theme.main_text_color(background).into_solid())
                        .finish(),
                )
                .with_child(
                    Text::new(status, font, size - 1.)
                        .with_color(status_color)
                        .finish(),
                )
                .finish(),
        )
        .with_padding_bottom(12.)
        .finish()
    }
}

impl SettingsPageMeta for ProvidersPageView {
    fn section() -> SettingsSection {
        SettingsSection::Providers
    }

    fn should_render(&self, _ctx: &AppContext) -> bool {
        true
    }

    fn update_filter(&mut self, query: &str, ctx: &mut ViewContext<Self>) -> MatchData {
        self.page.update_filter(query, ctx)
    }

    fn scroll_to_widget(&mut self, widget_id: &'static str) {
        self.page.scroll_to_widget(widget_id)
    }

    fn clear_highlighted_widget(&mut self) {
        self.page.clear_highlighted_widget();
    }
}

impl From<ViewHandle<ProvidersPageView>> for SettingsPageViewHandle {
    fn from(view_handle: ViewHandle<ProvidersPageView>) -> Self {
        SettingsPageViewHandle::Providers(view_handle)
    }
}

/// Command Palette entries to pick the default provider.
pub fn init_actions_from_parent_view<T: Action + Clone>(
    app: &mut AppContext,
    context: &ContextPredicate,
    builder: fn(SettingsAction) -> T,
) {
    app.register_fixed_bindings(
        ChatProvider::ALL
            .into_iter()
            .map(|provider| {
                FixedBinding::empty(
                    format!("Provedor padrão do chat: {}", provider.label()),
                    builder(SettingsAction::SetDefaultChatProvider(provider)),
                    context.to_owned(),
                )
                .with_group(bindings::BindingGroup::Settings.as_str())
            })
            .collect::<Vec<_>>(),
    );
}
