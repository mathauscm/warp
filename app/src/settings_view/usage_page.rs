use warpui::elements::{Align, Container, CrossAxisAlignment, Flex, ParentElement, Text};
use warpui::fonts::{Properties, Weight};
use warpui::{AppContext, Element, Entity, TypedActionView, View, ViewContext, ViewHandle};

use super::SettingsSection;
use super::settings_page::{
    MatchData, PageType, SettingsPageEvent, SettingsPageMeta, SettingsPageViewHandle,
    SettingsWidget,
};
use crate::appearance::Appearance;

/// Placeholder for the upcoming Claude and Codex usage page.
pub struct UsagePageView {
    page: PageType<Self>,
}

impl UsagePageView {
    pub fn new(_ctx: &mut ViewContext<UsagePageView>) -> Self {
        UsagePageView {
            page: PageType::new_monolith(UsagePlaceholderWidget, Some("Uso"), false),
        }
    }
}

impl Entity for UsagePageView {
    type Event = SettingsPageEvent;
}

impl TypedActionView for UsagePageView {
    type Action = ();
}

impl View for UsagePageView {
    fn ui_name() -> &'static str {
        "UsagePage"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        self.page.render(self, app)
    }
}

struct UsagePlaceholderWidget;

impl SettingsWidget for UsagePlaceholderWidget {
    type View = UsagePageView;

    fn search_terms(&self) -> &str {
        "uso usage billing claude codex"
    }

    fn render(
        &self,
        _view: &UsagePageView,
        appearance: &Appearance,
        _app: &AppContext,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let font_family = appearance.ui_font_family();
        let background = theme.background();

        Align::new(
            Flex::column()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(
                    Text::new_inline("Em breve no Picolas", font_family, 18.)
                        .with_style(Properties::default().weight(Weight::Semibold))
                        .with_color(theme.main_text_color(background).into_solid())
                        .finish(),
                )
                .with_child(
                    Container::new(
                        Text::new(
                            "Aqui você vai acompanhar o uso do Claude e do Codex.",
                            font_family,
                            appearance.ui_font_size(),
                        )
                        .with_color(theme.sub_text_color(background).into_solid())
                        .finish(),
                    )
                    .with_margin_top(8.)
                    .finish(),
                )
                .finish(),
        )
        .finish()
    }
}

impl SettingsPageMeta for UsagePageView {
    fn section() -> SettingsSection {
        SettingsSection::BillingAndUsage
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

impl From<ViewHandle<UsagePageView>> for SettingsPageViewHandle {
    fn from(view_handle: ViewHandle<UsagePageView>) -> Self {
        SettingsPageViewHandle::BillingAndUsage(view_handle)
    }
}
