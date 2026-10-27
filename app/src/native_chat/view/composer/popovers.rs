//! What floats over the composer: the "/" command and "@" file menus, the model and effort
//! picker and the permission mode menu.

use warp_core::ui::Icon as WarpIcon;
use warpui::AppContext;
use warpui::elements::{
    Align, Border, ChildAnchor, ClippedScrollable, ConstrainedBox, Container, CrossAxisAlignment,
    Dismiss, DropShadow, Element, Expanded, Fill as ElementFill, Flex, Hoverable,
    MainAxisAlignment, MainAxisSize, OffsetPositioning, ParentAnchor, ParentElement,
    ParentOffsetBounds, ScrollbarWidth, Shrinkable, Stack,
};
use warpui::fonts::{Properties, Weight};
use warpui::geometry::vector::vec2f;
use warpui::platform::Cursor;

use super::logic;
use super::widgets::{Look, icon, svg, with_alpha};
use super::{ComposerAction, ComposerView, MENU_VISIBLE_ROWS, Menu, PickerPage};

const PICKER_WIDTH: f32 = 260.;
const PICKER_PADDING: f32 = 12.;
const MODELS_MAX_HEIGHT: f32 = 320.;
const MODE_MENU_WIDTH: f32 = 330.;
const SLIDER_HEIGHT: f32 = 18.;
const SLIDER_WIDTH: f32 = PICKER_WIDTH - 2. * (PICKER_PADDING + 1.);
const SLIDER_KNOB_SIZE: f32 = 14.;
const SLIDER_DOT_SIZE: f32 = 3.;
const SLIDER_INSET: f32 = SLIDER_HEIGHT / 2.;
const RESET_ICON: &str = "bundled/svg/picolas-reset.svg";
const CHEVRON_RIGHT_ICON: &str = "bundled/svg/picolas-chevron-right.svg";
const CHECK_ICON: &str = "bundled/svg/picolas-check.svg";

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

    fn render_models_page(&self, look: &Look, app: &AppContext) -> Box<dyn Element> {
        let palette = &look.palette;
        let sizes = &look.sizes;
        let session = self.session.as_ref(app);
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
            let description = model.description.clone();
            let row_look = look.clone();
            list.add_child(
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
                .on_click(move |ctx, _, _| {
                    ctx.dispatch_typed_action(ComposerAction::ChooseModel(index))
                })
                .finish(),
            );
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
}
