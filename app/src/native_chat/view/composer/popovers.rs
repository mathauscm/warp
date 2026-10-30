//! What floats over the composer: the "/" command and "@" file menus, the model and effort
//! picker (with its Claude / Codex switch), the permission mode menu and the list of a
//! multi-folder chat's repositories.

use claude_chat::codex::INSTALL_HINT as CODEX_INSTALL_HINT;
use warp_core::ui::Icon as WarpIcon;
use warpui::elements::MouseStateHandle;
use warpui::elements::{
    Align, Border, ChildAnchor, ClippedScrollable, ConstrainedBox, Container, CrossAxisAlignment,
    Dismiss, DropShadow, Element, Expanded, Fill as ElementFill, Flex, Hoverable,
    MainAxisAlignment, MainAxisSize, OffsetPositioning, ParentAnchor, ParentElement,
    ParentOffsetBounds, ScrollbarWidth, Shrinkable, Stack,
};
use warpui::fonts::{Properties, Weight};
use warpui::geometry::vector::vec2f;
use warpui::platform::Cursor;
use warpui::{AppContext, ViewContext};

use super::widgets::{Look, icon, svg, with_alpha};
use super::{
    ComposerAction, ComposerEvent, ComposerView, MENU_VISIBLE_ROWS, Menu, PickerPage, git_status,
    logic,
};
use crate::native_chat::provider::{self, ChatProvider};

const PICKER_WIDTH: f32 = 260.;
const PICKER_PADDING: f32 = 12.;
const MODELS_MAX_HEIGHT: f32 = 320.;
const MODE_MENU_WIDTH: f32 = 330.;
const GIT_MENU_WIDTH: f32 = 300.;
const SLIDER_HEIGHT: f32 = 18.;
const SLIDER_WIDTH: f32 = PICKER_WIDTH - 2. * (PICKER_PADDING + 1.);
const SLIDER_KNOB_SIZE: f32 = 14.;
const SLIDER_DOT_SIZE: f32 = 3.;
const SLIDER_INSET: f32 = SLIDER_HEIGHT / 2.;
const RESET_ICON: &str = "bundled/svg/picolas-reset.svg";
const CHEVRON_RIGHT_ICON: &str = "bundled/svg/picolas-chevron-right.svg";
const CHECK_ICON: &str = "bundled/svg/picolas-check.svg";
/// Claude Code's model aliases, offered from a Codex chat before any Claude chat reported its list.
const CLAUDE_FALLBACK_MODELS: [(&str, &str, &str); 4] = [
    ("default", "Default", "O modelo padrão do Claude Code"),
    ("opus", "Opus", "O mais capaz, para tarefas complexas"),
    (
        "sonnet",
        "Sonnet",
        "Equilíbrio entre capacidade e velocidade",
    ),
    ("haiku", "Haiku", "O mais rápido"),
];

/// The model picker's Claude / Codex switch.
#[derive(Default)]
pub(super) struct ProviderPicker {
    /// The provider whose models the picker lists, when it isn't the chat's own.
    browsing: Option<ChatProvider>,
    tabs: [MouseStateHandle; 2],
}

impl ProviderPicker {
    pub(super) fn reset(&mut self) {
        self.browsing = None;
    }
}

/// A model the picker lists for a provider other than the chat's.
struct ModelChoice {
    /// `None` for the provider's default model.
    value: Option<String>,
    name: String,
    description: String,
}

/// The models of `provider` the picker offers from a chat of the other provider.
fn model_choices(provider: ChatProvider) -> Vec<ModelChoice> {
    match provider {
        ChatProvider::Claude => {
            let known = provider::claude_models();
            if known.is_empty() {
                CLAUDE_FALLBACK_MODELS
                    .iter()
                    .map(|(value, name, description)| ModelChoice {
                        value: Some((*value).to_owned()),
                        name: (*name).to_owned(),
                        description: (*description).to_owned(),
                    })
                    .collect()
            } else {
                known
                    .into_iter()
                    .map(|model| ModelChoice {
                        name: if model.display_name.is_empty() {
                            model.value.clone()
                        } else {
                            model.display_name.clone()
                        },
                        description: model.description,
                        value: Some(model.value),
                    })
                    .collect()
            }
        }
        ChatProvider::Codex => std::iter::once(ModelChoice {
            value: None,
            name: "Padrão".to_owned(),
            description: "O modelo configurado no Codex CLI".to_owned(),
        })
        .chain(
            provider::codex_models()
                .into_iter()
                .map(|model| ModelChoice {
                    value: Some(model.id),
                    name: model.display_name,
                    description: model.description,
                }),
        )
        .collect(),
    }
}

impl ComposerView {
    /// A floating surface with a soft shadow; clicking outside it closes the popovers.
    fn popover(&self, look: &Look, content: Box<dyn Element>, dismiss: bool) -> Box<dyn Element> {
        let palette = &look.palette;
        let card = Container::new(content)
            .with_uniform_padding(6.)
            .with_background_color(palette.surface.0)
            .with_border(Border::all(1.).with_border_color(palette.border_strong.0))
            .with_corner_radius(look.radius(look.sizes.card_radius))
            .with_drop_shadow(DropShadow {
                color: palette.shadow.0,
                offset: vec2f(0., 8.),
                blur_radius: look.sizes.shadow_blur + 6.,
                spread_radius: 0.,
            })
            .finish();
        if dismiss {
            Dismiss::new(card)
                .on_dismiss(|ctx, _| ctx.dispatch_typed_action(ComposerAction::ClosePopovers))
                .finish()
        } else {
            card
        }
    }

    pub(super) fn render_menu(&self, look: &Look) -> Option<Box<dyn Element>> {
        let menu = self.menu.as_ref()?;
        let palette = &look.palette;
        let sizes = &look.sizes;
        let mut rows = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(1.);
        let selected = menu.selected();
        let len = menu.len();
        // The visible window follows the selection.
        let first = selected
            .saturating_sub(MENU_VISIBLE_ROWS - 1)
            .min(len.saturating_sub(MENU_VISIBLE_ROWS));
        let title = match menu {
            Menu::Commands { .. } => "Comandos",
            Menu::Files { .. } => "Arquivos",
        };
        rows.add_child(
            Container::new(look.text(title, sizes.tiny_font_size, palette.text_faint.0))
                .with_horizontal_padding(10.)
                .with_padding_top(4.)
                .with_padding_bottom(4.)
                .finish(),
        );
        for (slot, index) in (first..len).take(MENU_VISIBLE_ROWS).enumerate() {
            let (label, detail, hint, warp_icon) = match menu {
                Menu::Commands {
                    entries, matches, ..
                } => {
                    let entry = &entries[matches[index]];
                    (
                        format!("/{}", entry.name),
                        entry.description.clone(),
                        entry.argument_hint.clone(),
                        if logic::ui_command(&entry.name).is_some() {
                            WarpIcon::Sliders
                        } else if entry.builtin {
                            WarpIcon::SlashCommands
                        } else {
                            WarpIcon::Stars
                        },
                    )
                }
                Menu::Files { paths, .. } => {
                    let path = &paths[index];
                    let (directory, name) = match path.trim_end_matches('/').rsplit_once('/') {
                        Some((directory, name)) => (directory.to_owned(), name.to_owned()),
                        None => (String::new(), path.clone()),
                    };
                    let warp_icon = if path.ends_with('/') {
                        WarpIcon::Folder
                    } else {
                        WarpIcon::File
                    };
                    (name, directory, String::new(), warp_icon)
                }
            };
            let is_selected = index == selected;
            let row_look = look.clone();
            rows.add_child(
                Hoverable::new(self.mouse.menu_rows[slot].clone(), move |state| {
                    let look = &row_look;
                    let palette = &look.palette;
                    let sizes = &look.sizes;
                    let mut row = Flex::row()
                        .with_main_axis_size(MainAxisSize::Max)
                        .with_cross_axis_alignment(CrossAxisAlignment::Center)
                        .with_spacing(10.)
                        .with_child(icon(
                            warp_icon,
                            if is_selected {
                                palette.text.0
                            } else {
                                palette.text_muted.0
                            },
                            sizes.small_icon_size,
                        ))
                        .with_child(look.medium_text(
                            label.clone(),
                            sizes.small_font_size + 0.5,
                            palette.text.0,
                        ));
                    if !hint.is_empty() {
                        row.add_child(look.text(
                            hint.clone(),
                            sizes.small_font_size,
                            palette.text_faint.0,
                        ));
                    }
                    row.add_child(
                        Shrinkable::new(
                            1.,
                            look.text(detail.clone(), sizes.small_font_size, palette.text_muted.0),
                        )
                        .finish(),
                    );
                    let mut container = Container::new(
                        ConstrainedBox::new(row.finish())
                            .with_height(sizes.chip_height + 4.)
                            .finish(),
                    )
                    .with_horizontal_padding(10.)
                    .with_corner_radius(look.radius(sizes.chip_radius));
                    if is_selected {
                        container = container.with_background_color(palette.surface_strong.0);
                    } else if state.is_hovered() {
                        container = container.with_background_color(palette.surface_hover.0);
                    }
                    container.finish()
                })
                .with_cursor(Cursor::PointingHand)
                .on_click(move |ctx, _, _| {
                    ctx.dispatch_typed_action(ComposerAction::AcceptMenuRow(index))
                })
                .finish(),
            );
        }
        if len == 0
            && let Menu::Files { is_loading, .. } = menu
        {
            let text = if *is_loading {
                "Buscando arquivos…"
            } else {
                "Nenhum arquivo encontrado"
            };
            rows.add_child(
                Container::new(look.text(text, sizes.small_font_size, palette.text_muted.0))
                    .with_horizontal_padding(10.)
                    .with_vertical_padding(8.)
                    .finish(),
            );
        }
        rows.add_child(
            Container::new(look.text(
                "↑↓ para navegar · ↵ para escolher · tab para completar · esc para fechar",
                sizes.tiny_font_size,
                palette.text_faint.0,
            ))
            .with_horizontal_padding(10.)
            .with_padding_top(6.)
            .with_padding_bottom(2.)
            .finish(),
        );
        Some(self.popover(look, rows.finish(), false))
    }

    pub(super) fn render_picker(
        &self,
        page: PickerPage,
        look: &Look,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let content = match page {
            PickerPage::Effort => self.render_effort_page(look, app),
            PickerPage::Models => self.render_models_page(look, app),
        };
        ConstrainedBox::new(
            self.popover(
                look,
                Container::new(content)
                    .with_uniform_padding(PICKER_PADDING - 6.)
                    .finish(),
                true,
            ),
        )
        .with_width(PICKER_WIDTH)
        .finish()
    }

    /// Where the knob is drawn: under the cursor while held, gliding while it settles, else on the
    /// current level.
    fn slider_knob(&self, current: Option<usize>, count: usize) -> Option<f32> {
        self.slider_settle
            .and_then(|settle| settle.position())
            .or_else(|| current.map(|index| logic::effort_position(index, count)))
    }

    fn render_effort_page(&self, look: &Look, app: &AppContext) -> Box<dyn Element> {
        let palette = look.palette.clone();
        let sizes = &look.sizes;
        let session = self.session.as_ref(app);
        let conversation = session.conversation();
        let model_name = logic::model_display_name(
            conversation.models(),
            session.chosen_model(),
            conversation.session().model.as_deref(),
        );
        let (levels, current) = self.effort_state(app);
        let title = if levels.is_empty() {
            "Sem níveis de esforço".to_owned()
        } else {
            current
                .map(|index| logic::effort_label(&levels[index]))
                .unwrap_or_else(|| "Padrão".to_owned())
        };

        let link_look = look.clone();
        let model_link = Hoverable::new(self.mouse.model_link.clone(), move |state| {
            let color = if state.is_hovered() {
                link_look.palette.text.0
            } else {
                link_look.palette.text_muted.0
            };
            Flex::row()
                .with_main_axis_size(MainAxisSize::Min)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(2.)
                .with_child(link_look.text(
                    model_name.clone(),
                    link_look.sizes.small_font_size,
                    color,
                ))
                .with_child(svg(CHEVRON_RIGHT_ICON, color, 10.))
                .finish()
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(|ctx, _, _| ctx.dispatch_typed_action(ComposerAction::ShowModels))
        .finish();
        let heading = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(2.)
            .with_child(
                warpui::elements::Text::new_inline(title, look.ui_font, sizes.title_font_size - 1.)
                    .with_style(Properties::default().weight(Weight::Medium))
                    .with_color(palette.accent.0)
                    .finish(),
            )
            .with_child(model_link)
            .finish();
        let reset_look = look.clone();
        let reset = Hoverable::new(self.mouse.reset_effort.clone(), move |state| {
            let color = if state.is_hovered() {
                reset_look.palette.text.0
            } else {
                reset_look.palette.text_muted.0
            };
            svg(RESET_ICON, color, reset_look.sizes.small_icon_size)
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(|ctx, _, _| ctx.dispatch_typed_action(ComposerAction::ResetEffort))
        .finish();
        let mut header = Stack::new().with_child(Align::new(heading).finish());
        header.add_positioned_child(
            reset,
            OffsetPositioning::offset_from_parent(
                vec2f(0., 0.),
                ParentOffsetBounds::ParentByPosition,
                ParentAnchor::TopRight,
                ChildAnchor::TopRight,
            ),
        );

        let mut column = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(12.)
            .with_child(header.finish());
        if !levels.is_empty() {
            let knob = self.slider_knob(current, levels.len());
            column.add_child(Align::new(self.render_slider(look, knob, levels.len())).finish());
        }
        column.finish()
    }

    /// A track with a dot per effort level, filled up to the knob.
    fn render_slider(&self, look: &Look, knob: Option<f32>, count: usize) -> Box<dyn Element> {
        let palette = &look.palette;
        let span = SLIDER_WIDTH - 2. * SLIDER_INSET;
        let knob_middle = knob.map(|knob| SLIDER_INSET + knob.clamp(0., 1.) * span);
        let pill = |width: f32, height: f32, color| {
            super::widgets::block(width, height, height / 2., color)
        };

        let mut layers =
            Stack::new().with_child(pill(SLIDER_WIDTH, SLIDER_HEIGHT, palette.surface_strong.0));
        if let Some(knob) = knob {
            let fill = 2. * SLIDER_INSET + knob.clamp(0., 1.) * span;
            layers.add_child(pill(fill, SLIDER_HEIGHT, palette.accent.0));
        }
        for index in 0..count {
            let x = SLIDER_INSET + logic::effort_position(index, count) * span;
            let is_filled = knob_middle.is_some_and(|middle| x < middle);
            let color = if is_filled {
                with_alpha(palette.on_accent.0, 0x99)
            } else {
                palette.text_faint.0
            };
            layers.add_child(
                Container::new(pill(SLIDER_DOT_SIZE, SLIDER_DOT_SIZE, color))
                    .with_padding_left(x - SLIDER_DOT_SIZE / 2.)
                    .with_padding_top((SLIDER_HEIGHT - SLIDER_DOT_SIZE) / 2.)
                    .finish(),
            );
        }
        if let Some(middle) = knob_middle {
            layers.add_child(
                Container::new(
                    Container::new(super::widgets::spacer(
                        SLIDER_KNOB_SIZE - 4.,
                        SLIDER_KNOB_SIZE - 4.,
                    ))
                    .with_background_color(palette.surface.0)
                    .with_border(Border::all(2.).with_border_color(palette.accent.0))
                    .with_corner_radius(look.radius(SLIDER_KNOB_SIZE / 2.))
                    .finish(),
                )
                .with_padding_left(middle - SLIDER_KNOB_SIZE / 2.)
                .with_padding_top((SLIDER_HEIGHT - SLIDER_KNOB_SIZE) / 2.)
                .finish(),
            );
        }
        // A click anywhere on a level's share of the track picks that level.
        let mut cells = Flex::row()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch);
        let cell_width = SLIDER_WIDTH / count.max(1) as f32;
        for (index, mouse_state) in self.mouse.effort_levels.iter().take(count).enumerate() {
            cells.add_child(
                Hoverable::new(mouse_state.clone(), move |_| {
                    super::widgets::spacer(cell_width, SLIDER_HEIGHT)
                })
                .with_cursor(Cursor::PointingHand)
                .on_click(move |ctx, _, _| {
                    ctx.dispatch_typed_action(ComposerAction::SetEffort(index))
                })
                .finish(),
            );
        }
        layers.add_child(cells.finish());
        ConstrainedBox::new(layers.finish())
            .with_width(SLIDER_WIDTH)
            .with_height(SLIDER_HEIGHT)
            .finish()
    }

    /// "Remover <tipo>", under the artifact kind chip.
    pub(super) fn render_artifact_kind_menu(
        &self,
        kind: claude_chat::artifacts::ArtifactKind,
        look: &Look,
    ) -> Box<dyn Element> {
        let row_look = look.clone();
        let label = format!("Remover {}", claude_chat::artifacts::kind_label_pt(kind));
        let row = Hoverable::new(self.mouse.remove_artifact_kind.clone(), move |state| {
            let look = &row_look;
            let palette = &look.palette;
            let sizes = &look.sizes;
            let mut container = Container::new(
                Flex::row()
                    .with_main_axis_size(MainAxisSize::Min)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(8.)
                    .with_child(icon(
                        WarpIcon::X,
                        palette.text_muted.0,
                        sizes.small_icon_size,
                    ))
                    .with_child(look.text(
                        label.clone(),
                        sizes.small_font_size + 0.5,
                        palette.text.0,
                    ))
                    .finish(),
            )
            .with_horizontal_padding(10.)
            .with_vertical_padding(7.)
            .with_corner_radius(look.radius(sizes.chip_radius));
            if state.is_hovered() {
                container = container.with_background_color(palette.surface_hover.0);
            }
            container.finish()
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(|ctx, _, _| ctx.dispatch_typed_action(ComposerAction::RemoveArtifactKind))
        .finish();
        self.popover(look, row, true)
    }

    /// Lists the models of `provider` in the picker.
    pub(super) fn browse_provider(&mut self, provider: ChatProvider, ctx: &mut ViewContext<Self>) {
        let own = self.session.as_ref(ctx).provider();
        self.provider_picker.browsing = (provider != own).then_some(provider);
        ctx.notify();
    }

    /// Switches to the model at `index` of another provider's list: in place for a chat that
    /// hasn't started its conversation, else in a new chat that takes the draft along.
    pub(super) fn choose_provider_model(
        &mut self,
        provider: ChatProvider,
        index: usize,
        ctx: &mut ViewContext<Self>,
    ) {
        let Some(choice) = model_choices(provider).into_iter().nth(index) else {
            return;
        };
        self.provider_picker.reset();
        let model = choice.value;
        let switched = self.session.update(ctx, |session, ctx| {
            session.switch_provider(provider, model.clone(), ctx)
        });
        if !switched {
            let draft = self.buffer_text(ctx);
            let draft = (!draft.trim().is_empty()).then_some(draft);
            if draft.is_some() {
                self.set_text("", ctx);
            }
            ctx.emit(ComposerEvent::NewChatWithProvider {
                provider,
                model,
                draft,
            });
        }
        self.close_popovers(ctx);
    }

    /// The Claude / Codex switch above the model list.
    fn render_provider_tabs(&self, look: &Look, shown: ChatProvider) -> Box<dyn Element> {
        let mut row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(4.);
        for (provider, mouse_state) in ChatProvider::ALL
            .into_iter()
            .zip(&self.provider_picker.tabs)
        {
            let is_shown = provider == shown;
            let tab_look = look.clone();
            row.add_child(
                Expanded::new(
                    1.,
                    Hoverable::new(mouse_state.clone(), move |state| {
                        let look = &tab_look;
                        let palette = &look.palette;
                        let sizes = &look.sizes;
                        let color = if is_shown {
                            palette.text.0
                        } else {
                            palette.text_muted.0
                        };
                        let mut tab = Container::new(
                            Align::new(look.medium_text(
                                provider.label(),
                                sizes.small_font_size,
                                color,
                            ))
                            .finish(),
                        )
                        .with_vertical_padding(4.)
                        .with_corner_radius(look.radius(sizes.chip_radius));
                        if is_shown {
                            tab = tab.with_background_color(palette.surface_strong.0);
                        } else if state.is_hovered() {
                            tab = tab.with_background_color(palette.surface_hover.0);
                        }
                        tab.finish()
                    })
                    .with_cursor(Cursor::PointingHand)
                    .on_click(move |ctx, _, _| {
                        ctx.dispatch_typed_action(ComposerAction::BrowseProvider(provider))
                    })
                    .finish(),
                )
                .finish(),
            );
        }
        Container::new(row.finish())
            .with_padding_bottom(4.)
            .finish()
    }

    /// The models of a provider other than the chat's.
    fn render_other_provider_models(
        &self,
        provider: ChatProvider,
        look: &Look,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let palette = &look.palette;
        let sizes = &look.sizes;
        let mut list = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(1.);
        let codex_missing = provider == ChatProvider::Codex && provider::codex_cli_path().is_none();
        let hint = if codex_missing {
            format!("Codex CLI não encontrado — instale com {CODEX_INSTALL_HINT}")
        } else if self.session.as_ref(app).has_transcript() {
            format!("Abre um novo chat com o {}", provider.label())
        } else {
            format!("Este chat passa a usar o {}", provider.label())
        };
        list.add_child(
            Container::new(look.paragraph(hint, sizes.tiny_font_size, palette.text_muted.0))
                .with_horizontal_padding(8.)
                .with_padding_bottom(4.)
                .finish(),
        );
        if !codex_missing {
            for (index, choice) in model_choices(provider)
                .into_iter()
                .enumerate()
                .take(self.mouse.models.len())
            {
                list.add_child(self.render_model_row(
                    look,
                    index,
                    choice.name,
                    choice.description,
                    false,
                    ComposerAction::ChooseProviderModel { provider, index },
                ));
            }
        }
        list.finish()
    }

    /// One row of the model list.
    fn render_model_row(
        &self,
        look: &Look,
        index: usize,
        name: String,
        description: String,
        is_current: bool,
        action: ComposerAction,
    ) -> Box<dyn Element> {
        let row_look = look.clone();
        Hoverable::new(self.mouse.models[index].clone(), move |state| {
            let look = &row_look;
            let palette = &look.palette;
            let sizes = &look.sizes;
            let mut text = Flex::column()
                .with_main_axis_size(MainAxisSize::Min)
                .with_cross_axis_alignment(CrossAxisAlignment::Start)
                .with_spacing(1.)
                .with_child(look.medium_text(
                    name.clone(),
                    sizes.small_font_size + 0.5,
                    palette.text.0,
                ));
            if !description.is_empty() {
                text.add_child(look.text(
                    description.clone(),
                    sizes.tiny_font_size,
                    palette.text_muted.0,
                ));
            }
            let mut row = Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(8.)
                .with_child(Expanded::new(1., text.finish()).finish());
            if is_current {
                row.add_child(svg(CHECK_ICON, palette.accent.0, sizes.small_icon_size));
            }
            let mut container = Container::new(row.finish())
                .with_horizontal_padding(8.)
                .with_vertical_padding(6.)
                .with_corner_radius(look.radius(sizes.chip_radius));
            if state.is_hovered() {
                container = container.with_background_color(palette.surface_hover.0);
            }
            container.finish()
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
        .finish()
    }

    fn render_models_page(&self, look: &Look, app: &AppContext) -> Box<dyn Element> {
        let palette = &look.palette;
        let sizes = &look.sizes;
        let session = self.session.as_ref(app);
        let own_provider = session.provider();
        let shown_provider = self.provider_picker.browsing.unwrap_or(own_provider);
        if shown_provider != own_provider {
            let scroll = ConstrainedBox::new(
                ClippedScrollable::vertical(
                    self.models_scroll.clone(),
                    self.render_other_provider_models(shown_provider, look, app),
                    ScrollbarWidth::Custom(4.),
                    palette.border_strong.0.into(),
                    palette.text_faint.0.into(),
                    ElementFill::None,
                )
                .finish(),
            )
            .with_max_height(MODELS_MAX_HEIGHT)
            .finish();
            return Flex::column()
                .with_main_axis_size(MainAxisSize::Min)
                .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
                .with_spacing(4.)
                .with_child(self.render_provider_tabs(look, shown_provider))
                .with_child(
                    Container::new(look.text(
                        "Selecionar modelo",
                        sizes.tiny_font_size,
                        palette.text_faint.0,
                    ))
                    .with_horizontal_padding(8.)
                    .with_padding_bottom(2.)
                    .finish(),
                )
                .with_child(scroll)
                .finish();
        }
        let conversation = session.conversation();
        let models = conversation.models();
        let current = logic::current_model(
            models,
            session.chosen_model(),
            conversation.session().model.as_deref(),
        )
        .map(|model| model.value.clone());

        let mut list = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(1.);
        if models.is_empty() {
            list.add_child(
                Container::new(look.text(
                    "Carregando modelos…",
                    sizes.small_font_size,
                    palette.text_muted.0,
                ))
                .with_uniform_padding(8.)
                .finish(),
            );
        }
        for (index, model) in models.iter().enumerate().take(self.mouse.models.len()) {
            let is_current = current.as_deref() == Some(model.value.as_str());
            let name = if model.display_name.is_empty() {
                model.value.clone()
            } else {
                model.display_name.clone()
            };
            list.add_child(self.render_model_row(
                look,
                index,
                name,
                model.description.clone(),
                is_current,
                ComposerAction::ChooseModel(index),
            ));
        }
        let scroll = ConstrainedBox::new(
            ClippedScrollable::vertical(
                self.models_scroll.clone(),
                list.finish(),
                ScrollbarWidth::Custom(4.),
                palette.border_strong.0.into(),
                palette.text_faint.0.into(),
                ElementFill::None,
            )
            .finish(),
        )
        .with_max_height(MODELS_MAX_HEIGHT)
        .finish();
        Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(4.)
            .with_child(self.render_provider_tabs(look, own_provider))
            .with_child(
                Container::new(look.text(
                    "Selecionar modelo",
                    sizes.tiny_font_size,
                    palette.text_faint.0,
                ))
                .with_horizontal_padding(8.)
                .with_padding_bottom(2.)
                .finish(),
            )
            .with_child(scroll)
            .finish()
    }

    pub(super) fn render_mode_menu(&self, look: &Look, app: &AppContext) -> Box<dyn Element> {
        let palette = &look.palette;
        let sizes = &look.sizes;
        let current = logic::permission_mode_index(
            self.session
                .as_ref(app)
                .conversation()
                .session()
                .permission_mode
                .as_deref(),
        );
        let mut list = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(1.)
            .with_child(
                Container::new(look.text(
                    "Permissões  ·  shift+tab alterna",
                    sizes.tiny_font_size,
                    palette.text_faint.0,
                ))
                .with_horizontal_padding(8.)
                .with_vertical_padding(4.)
                .finish(),
            );
        for (index, mode) in logic::PERMISSION_MODES.iter().enumerate() {
            let is_current = index == current;
            let row_look = look.clone();
            list.add_child(
                Hoverable::new(self.mouse.modes[index].clone(), move |state| {
                    let look = &row_look;
                    let palette = &look.palette;
                    let sizes = &look.sizes;
                    let text = Flex::column()
                        .with_main_axis_size(MainAxisSize::Min)
                        .with_cross_axis_alignment(CrossAxisAlignment::Start)
                        .with_spacing(1.)
                        .with_child(look.medium_text(
                            mode.label,
                            sizes.small_font_size + 0.5,
                            palette.text.0,
                        ))
                        .with_child(look.paragraph(
                            mode.description,
                            sizes.tiny_font_size,
                            palette.text_muted.0,
                        ))
                        .finish();
                    let mut row = Flex::row()
                        .with_main_axis_size(MainAxisSize::Max)
                        .with_cross_axis_alignment(CrossAxisAlignment::Center)
                        .with_spacing(8.)
                        .with_child(Expanded::new(1., text).finish());
                    if is_current {
                        row.add_child(svg(CHECK_ICON, palette.accent.0, sizes.small_icon_size));
                    }
                    let mut container = Container::new(row.finish())
                        .with_horizontal_padding(8.)
                        .with_vertical_padding(6.)
                        .with_corner_radius(look.radius(sizes.chip_radius));
                    if state.is_hovered() {
                        container = container.with_background_color(palette.surface_hover.0);
                    }
                    container.finish()
                })
                .with_cursor(Cursor::PointingHand)
                .on_click(move |ctx, _, _| {
                    ctx.dispatch_typed_action(ComposerAction::SetMode(index))
                })
                .finish(),
            );
        }
        ConstrainedBox::new(self.popover(look, list.finish(), true))
            .with_width(MODE_MENU_WIDTH)
            .finish()
    }

    /// The repositories of a chat with several folders, each opening its diff on the right.
    pub(super) fn render_git_menu(&self, look: &Look) -> Box<dyn Element> {
        let palette = &look.palette;
        let sizes = &look.sizes;
        let mut list = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(1.)
            .with_child(
                Container::new(look.text(
                    "Ver alterações de",
                    sizes.tiny_font_size,
                    palette.text_faint.0,
                ))
                .with_horizontal_padding(8.)
                .with_vertical_padding(4.)
                .finish(),
            );
        for (index, (status, mouse_state)) in self.git.iter().zip(&self.mouse.git_repos).enumerate()
        {
            let row_look = look.clone();
            let name = status.repo_name();
            let branch = status.branch.clone();
            let (added, removed) = git_status::diff_parts(status.lines_added, status.lines_removed);
            list.add_child(
                Hoverable::new(mouse_state.clone(), move |state| {
                    let look = &row_look;
                    let palette = &look.palette;
                    let sizes = &look.sizes;
                    let text = Flex::column()
                        .with_main_axis_size(MainAxisSize::Min)
                        .with_cross_axis_alignment(CrossAxisAlignment::Start)
                        .with_spacing(1.)
                        .with_child(look.medium_text(
                            name.clone(),
                            sizes.small_font_size + 0.5,
                            palette.text.0,
                        ))
                        .with_child(
                            Flex::row()
                                .with_main_axis_size(MainAxisSize::Min)
                                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                                .with_spacing(4.)
                                .with_child(icon(
                                    WarpIcon::GitBranch,
                                    palette.text_faint.0,
                                    sizes.small_icon_size - 2.,
                                ))
                                .with_child(look.text(
                                    branch.clone(),
                                    sizes.tiny_font_size,
                                    palette.text_muted.0,
                                ))
                                .finish(),
                        )
                        .finish();
                    let mut row = Flex::row()
                        .with_main_axis_size(MainAxisSize::Max)
                        .with_cross_axis_alignment(CrossAxisAlignment::Center)
                        .with_spacing(6.)
                        .with_child(Expanded::new(1., text).finish());
                    if let Some(added) = &added {
                        row.add_child(look.medium_text(
                            added.clone(),
                            sizes.small_font_size,
                            palette.diff_added.0,
                        ));
                    }
                    if let Some(removed) = &removed {
                        row.add_child(look.medium_text(
                            removed.clone(),
                            sizes.small_font_size,
                            palette.diff_removed.0,
                        ));
                    }
                    let mut container = Container::new(row.finish())
                        .with_horizontal_padding(8.)
                        .with_vertical_padding(6.)
                        .with_corner_radius(look.radius(sizes.chip_radius));
                    if state.is_hovered() {
                        container = container.with_background_color(palette.surface_hover.0);
                    }
                    container.finish()
                })
                .with_cursor(Cursor::PointingHand)
                .on_click(move |ctx, _, _| {
                    ctx.dispatch_typed_action(ComposerAction::OpenRepoDiff(index))
                })
                .finish(),
            );
        }
        ConstrainedBox::new(self.popover(look, list.finish(), true))
            .with_width(GIT_MENU_WIDTH)
            .finish()
    }
}
