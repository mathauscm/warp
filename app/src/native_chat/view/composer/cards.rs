//! The cards stacked above the composer: a crashed process, queued messages, Claude's task list
//! and the request waiting for an answer (a permission, a question or the plan).

use claude_chat::timeline::{PendingRequest, PendingRequestKind, Question, TodoItem, TodoStatus};
use serde_json::Value;
use warp_core::ui::Icon as WarpIcon;
use warpui::AppContext;
use warpui::elements::{
    Border, ClippedScrollable, ConstrainedBox, Container, CrossAxisAlignment, Element, Empty,
    Expanded, Fill as ElementFill, Flex, FormattedTextElement, HeadingFontSizeMultipliers,
    Hoverable, MainAxisAlignment, MainAxisSize, ParentElement, ScrollbarWidth, Shrinkable, Stack,
};
use warpui::platform::Cursor;

use super::logic::{self, PermissionPreview};
use super::widgets::{self, ButtonKind, Look, icon, with_alpha};
use super::{ComposerAction, ComposerView, PermissionChoice, PlanChoice, exit_error};

const PREVIEW_MAX_HEIGHT: f32 = 132.;
const PLAN_MAX_HEIGHT: f32 = 300.;
const TODO_SEGMENTS_MAX: usize = 12;

impl ComposerView {
    pub(super) fn render_cards(&self, look: &Look, app: &AppContext) -> Vec<Box<dyn Element>> {
        let session = self.session.as_ref(app);
        let conversation = session.conversation();
        let mut cards = Vec::new();
        if let Some(error) = exit_error(session.process_state()).or(conversation.exit_error()) {
            cards.push(self.render_error_banner(look, error));
        }
        if !self.queued.is_empty() {
            cards.push(self.render_queued(look));
        }
        if !conversation.todos().is_empty() {
            cards.push(self.render_todos(look, conversation.todos()));
        }
        let pending = conversation.pending_requests();
        if let Some(request) = pending.first() {
            cards.push(self.render_request(look, request, pending.len()));
        }
        cards
    }

    fn render_error_banner(&self, look: &Look, error: &str) -> Box<dyn Element> {
        let palette = &look.palette;
        let sizes = &look.sizes;
        let row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(10.)
            .with_child(icon(
                WarpIcon::AlertTriangle,
                palette.error.0,
                sizes.icon_size,
            ))
            .with_child(
                Expanded::new(
                    1.,
                    look.paragraph(error, sizes.small_font_size, palette.text.0),
                )
                .finish(),
            )
            .with_child(widgets::button(
                look,
                self.mouse.restart.clone(),
                "Reiniciar",
                None,
                ButtonKind::Secondary,
                ComposerAction::Restart,
            ))
            .finish();
        Container::new(row)
            .with_horizontal_padding(sizes.composer_padding)
            .with_vertical_padding(10.)
            .with_background_color(with_alpha(palette.error.0, 0x1f))
            .with_border(Border::all(1.).with_border_color(with_alpha(palette.error.0, 0x55)))
            .with_corner_radius(look.radius(sizes.card_radius))
            .finish()
    }

    fn render_queued(&self, look: &Look) -> Box<dyn Element> {
        let palette = &look.palette;
        let sizes = &look.sizes;
        let mut row = warpui::elements::Wrap::row()
            .with_spacing(6.)
            .with_run_spacing(6.)
            .with_child(
                ConstrainedBox::new(
                    Flex::row()
                        .with_main_axis_size(MainAxisSize::Min)
                        .with_cross_axis_alignment(CrossAxisAlignment::Center)
                        .with_spacing(6.)
                        .with_child(icon(
                            WarpIcon::Queued,
                            palette.text_faint.0,
                            sizes.small_icon_size,
                        ))
                        .with_child(look.text(
                            "Na fila",
                            sizes.small_font_size,
                            palette.text_faint.0,
                        ))
                        .finish(),
                )
                .with_height(sizes.chip_height)
                .finish(),
            );
        for message in &self.queued {
            row.add_child(
                Container::new(
                    ConstrainedBox::new(
                        Flex::row()
                            .with_main_axis_size(MainAxisSize::Min)
                            .with_cross_axis_alignment(CrossAxisAlignment::Center)
                            .with_child(look.text(
                                message.clone(),
                                sizes.small_font_size,
                                palette.text_muted.0,
                            ))
                            .finish(),
                    )
                    .with_height(sizes.chip_height)
                    .finish(),
                )
                .with_horizontal_padding(10.)
                .with_background_color(palette.surface.0)
                .with_border(Border::all(1.).with_border_color(palette.border.0))
                .with_corner_radius(look.radius(sizes.chip_height / 2.))
                .finish(),
            );
        }
        Container::new(row.finish())
            .with_horizontal_padding(4.)
            .finish()
    }

    /// "Tarefas 2/5" with a segmented bar and the current step; expands into the full list.
    fn render_todos(&self, look: &Look, todos: &[TodoItem]) -> Box<dyn Element> {
        let palette = look.palette.clone();
        let sizes = look.sizes.clone();
        let (done, total) = logic::todo_progress(todos);
        let current = logic::current_todo(todos).map(str::to_owned);
        let statuses: Vec<TodoStatus> = todos.iter().map(|todo| todo.status).collect();
        let is_expanded = self.is_todos_expanded;
        let header_look = look.clone();
        let header = Hoverable::new(self.mouse.todos.clone(), move |state| {
            let look = &header_look;
            let segments = statuses.len().min(TODO_SEGMENTS_MAX);
            let mut bar = Flex::row()
                .with_main_axis_size(MainAxisSize::Min)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(2.);
            for index in 0..segments {
                // With more tasks than segments, each segment stands for a share of them.
                let task = index * statuses.len() / segments;
                let color = match statuses[task] {
                    TodoStatus::Completed => palette.success.0,
                    TodoStatus::InProgress => palette.accent.0,
                    TodoStatus::Pending => palette.surface_strong.0,
                };
                bar.add_child(widgets::block(10., 3., 1.5, color));
            }
            let count_color = if done == total {
                palette.success.0
            } else {
                palette.text_muted.0
            };
            let mut row = Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(8.)
                .with_child(icon(
                    WarpIcon::TaskListBlock,
                    palette.text_muted.0,
                    sizes.small_icon_size,
                ))
                .with_child(look.medium_text("Tarefas", sizes.small_font_size, palette.text.0))
                .with_child(look.text(
                    format!("{done}/{total}"),
                    sizes.small_font_size,
                    count_color,
                ))
                .with_child(bar.finish());
            let detail = current.clone().unwrap_or_default();
            row.add_child(
                Shrinkable::new(
                    1.,
                    look.text(detail, sizes.small_font_size, palette.text_muted.0),
                )
                .finish(),
            );
            row.add_child(Expanded::new(1., Empty::new().finish()).finish());
            row.add_child(icon(
                if is_expanded {
                    WarpIcon::ChevronDown
                } else {
                    WarpIcon::ChevronUp
                },
                if state.is_hovered() {
                    palette.text.0
                } else {
                    palette.text_faint.0
                },
                sizes.small_icon_size - 2.,
            ));
            ConstrainedBox::new(row.finish())
                .with_height(sizes.chip_height)
                .finish()
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(|ctx, _, _| ctx.dispatch_typed_action(ComposerAction::ToggleTodos))
        .finish();

        let palette = &look.palette;
        let sizes = &look.sizes;
        let mut column = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(header);
        if is_expanded {
            let mut list = Flex::column()
                .with_main_axis_size(MainAxisSize::Min)
                .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
                .with_spacing(4.);
            for todo in todos {
                let (warp_icon, icon_color, text_color) = match todo.status {
                    TodoStatus::Completed => {
                        (WarpIcon::Check, palette.success.0, palette.text_faint.0)
                    }
                    TodoStatus::InProgress => {
                        (WarpIcon::CircleFilled, palette.accent.0, palette.text.0)
                    }
                    TodoStatus::Pending => {
                        (WarpIcon::Circle, palette.text_faint.0, palette.text_muted.0)
                    }
                };
                list.add_child(
                    Flex::row()
                        .with_main_axis_size(MainAxisSize::Max)
                        .with_cross_axis_alignment(CrossAxisAlignment::Start)
                        .with_spacing(8.)
                        .with_child(
                            Container::new(icon(warp_icon, icon_color, sizes.small_icon_size - 1.))
                                .with_padding_top(2.)
                                .finish(),
                        )
                        .with_child(
                            Expanded::new(
                                1.,
                                look.paragraph(
                                    todo.content.clone(),
                                    sizes.small_font_size,
                                    text_color,
                                ),
                            )
                            .finish(),
                        )
                        .finish(),
                );
            }
            column.add_child(
                Container::new(list.finish())
                    .with_padding_top(8.)
                    .with_padding_left(2.)
                    .finish(),
            );
        }
        Container::new(column.finish())
            .with_horizontal_padding(sizes.composer_padding)
            .with_vertical_padding(6.)
            .with_background_color(palette.surface.0)
            .with_border(Border::all(1.).with_border_color(palette.border.0))
            .with_corner_radius(look.radius(sizes.card_radius))
            .finish()
    }

    fn render_request(
        &self,
        look: &Look,
        request: &PendingRequest,
        count: usize,
    ) -> Box<dyn Element> {
        let content = match &request.kind {
            PendingRequestKind::Permission {
                tool_name,
                display_name,
                input,
                description,
                blocked_path,
                suggestions,
            } => self.render_permission(
                look,
                tool_name,
                display_name.as_deref(),
                input,
                description.as_deref(),
                blocked_path.as_deref(),
                suggestions.first(),
            ),
            PendingRequestKind::Question { questions, .. } => {
                self.render_questions(look, questions)
            }
            PendingRequestKind::PlanApproval { plan, .. } => self.render_plan(look, plan),
        };
        let palette = &look.palette;
        let sizes = &look.sizes;
        let mut column = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(content);
        if count > 1 {
            column.add_child(
                Container::new(look.text(
                    format!("+{} aguardando", count - 1),
                    sizes.tiny_font_size,
                    palette.text_faint.0,
                ))
                .with_padding_top(8.)
                .finish(),
            );
        }
        Container::new(column.finish())
            .with_uniform_padding(sizes.composer_padding + 2.)
            .with_background_color(palette.surface.0)
            .with_border(Border::all(1.).with_border_color(palette.border_strong.0))
            .with_corner_radius(look.radius(sizes.composer_radius - 2.))
            .finish()
    }

    /// A small header line: icon, title and an optional muted note on the right.
    fn render_card_header(
        look: &Look,
        warp_icon: WarpIcon,
        icon_color: pathfinder_color::ColorU,
        title: Vec<(String, bool)>,
    ) -> Box<dyn Element> {
        let palette = &look.palette;
        let sizes = &look.sizes;
        let mut row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(8.)
            .with_child(
                Container::new(icon(warp_icon, icon_color, sizes.small_icon_size))
                    .with_uniform_padding(5.)
                    .with_background_color(with_alpha(icon_color, 0x24))
                    .with_corner_radius(look.radius(7.))
                    .finish(),
            );
        let mut text = Flex::row()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(4.);
        for (part, is_strong) in title {
            text.add_child(if is_strong {
                look.medium_text(part, sizes.font_size - 0.5, palette.text.0)
            } else {
                look.text(part, sizes.font_size - 0.5, palette.text_muted.0)
            });
        }
        row.add_child(Shrinkable::new(1., text.finish()).finish());
        row.finish()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_permission(
        &self,
        look: &Look,
        tool_name: &str,
        display_name: Option<&str>,
        input: &Value,
        description: Option<&str>,
        blocked_path: Option<&str>,
        suggestion: Option<&Value>,
    ) -> Box<dyn Element> {
        let palette = &look.palette;
        let sizes = &look.sizes;
        let tool = logic::tool_display_name(tool_name, display_name);
        let mut column = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(10.)
            .with_child(Self::render_card_header(
                look,
                WarpIcon::Lock,
                palette.warning.0,
                vec![("Claude quer usar".to_owned(), false), (tool, true)],
            ));

        let preview = logic::permission_preview(tool_name, input);
        let description = description
            .filter(|text| !text.is_empty())
            .filter(|text| !matches!(&preview, PermissionPreview::Path(path) if path == text))
            .filter(|text| {
                !matches!(&preview, PermissionPreview::File(stats) if stats.path.ends_with(*text))
            });
        if let Some(description) = description {
            column.add_child(look.paragraph(
                description,
                sizes.small_font_size,
                palette.text_muted.0,
            ));
        }
        match preview {
            PermissionPreview::Command(command) => {
                column.add_child(self.render_code_box(look, look.mono(command, palette.text.0)));
            }
            PermissionPreview::File(stats) => {
                let row = Flex::row()
                    .with_main_axis_size(MainAxisSize::Max)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(8.)
                    .with_child(icon(
                        WarpIcon::File,
                        palette.text_muted.0,
                        sizes.small_icon_size,
                    ))
                    .with_child(
                        Shrinkable::new(
                            1.,
                            warpui::elements::Text::new_inline(
                                stats.path.clone(),
                                look.mono_font,
                                sizes.code_font_size,
                            )
                            .with_color(palette.text.0)
                            .with_clip(warpui::text_layout::ClipConfig::start())
                            .finish(),
                        )
                        .finish(),
                    )
                    .with_child(look.text(
                        format!("+{}", stats.added),
                        sizes.small_font_size,
                        palette.diff_added.0,
                    ))
                    .with_child(look.text(
                        format!("−{}", stats.removed),
                        sizes.small_font_size,
                        palette.diff_removed.0,
                    ))
                    .finish();
                column.add_child(self.render_code_box(look, row));
            }
            PermissionPreview::Path(path) | PermissionPreview::Text(path) => {
                column.add_child(self.render_code_box(look, look.mono(path, palette.text.0)));
            }
            PermissionPreview::None => {}
        }
        if let Some(path) = blocked_path
            && !matches!(tool_name, "Edit" | "MultiEdit" | "Write" | "Read")
        {
            column.add_child(look.text(
                format!("Fora da pasta do projeto: {path}"),
                sizes.tiny_font_size,
                palette.text_faint.0,
            ));
        }

        let mut buttons = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(8.);
        if let Some(suggestion) = suggestion {
            buttons.add_child(widgets::button(
                look,
                self.mouse.allow_always.clone(),
                logic::suggestion_label(suggestion),
                None,
                ButtonKind::Ghost,
                ComposerAction::Permission(PermissionChoice::AllowAlways),
            ));
        }
        buttons.add_child(Expanded::new(1., Empty::new().finish()).finish());
        buttons.add_child(widgets::button(
            look,
            self.mouse.deny.clone(),
            "Negar",
            Some("esc"),
            ButtonKind::Secondary,
            ComposerAction::Permission(PermissionChoice::Deny),
        ));
        buttons.add_child(widgets::button(
            look,
            self.mouse.allow.clone(),
            "Permitir",
            Some("↵"),
            ButtonKind::Primary,
            ComposerAction::Permission(PermissionChoice::Allow),
        ));
        column.add_child(
            Container::new(buttons.finish())
                .with_padding_top(2.)
                .finish(),
        );
        column.finish()
    }

    fn render_code_box(&self, look: &Look, content: Box<dyn Element>) -> Box<dyn Element> {
        let palette = &look.palette;
        ConstrainedBox::new(
            Container::new(warpui::elements::Clipped::new(content).finish())
                .with_horizontal_padding(10.)
                .with_vertical_padding(8.)
                .with_background_color(palette.code_background.0)
                .with_border(Border::all(1.).with_border_color(palette.border.0))
                .with_corner_radius(look.radius(look.sizes.chip_radius + 2.))
                .finish(),
        )
        .with_max_height(PREVIEW_MAX_HEIGHT)
        .finish()
    }

    fn render_questions(&self, look: &Look, questions: &[Question]) -> Box<dyn Element> {
        let palette = &look.palette;
        let sizes = &look.sizes;
        let mut column = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(10.);
        let mut option_states = self.mouse.options.iter();
        for (question_index, question) in questions.iter().enumerate() {
            let mut title = Vec::new();
            if let Some(header) = question.header.as_ref().filter(|header| !header.is_empty()) {
                title.push((header.clone(), false));
            } else {
                title.push(("Pergunta".to_owned(), false));
            }
            if questions.len() > 1 {
                title.push((format!("{}/{}", question_index + 1, questions.len()), false));
            }
            column.add_child(Self::render_card_header(
                look,
                WarpIcon::HelpCircle,
                palette.info.0,
                title,
            ));
            column.add_child(look.paragraph(
                question.question.clone(),
                sizes.font_size,
                palette.text.0,
            ));
            let selected = self
                .question
                .selected
                .get(question_index)
                .cloned()
                .unwrap_or_default();
            let is_active = question_index == self.question.active;
            let mut options = Flex::column()
                .with_main_axis_size(MainAxisSize::Min)
                .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
                .with_spacing(2.);
            for (option_index, option) in question.options.iter().enumerate() {
                let Some(mouse_state) = option_states.next() else {
                    break;
                };
                let is_selected = selected.contains(&option_index);
                let digit = (is_active && option_index < 9).then(|| option_index + 1);
                let row_look = look.clone();
                let label = option.label.clone();
                let description = option.description.clone();
                let is_multi = question.multi_select;
                let action = ComposerAction::ToggleOption {
                    question: question_index,
                    option: option_index,
                };
                options.add_child(
                    Hoverable::new(mouse_state.clone(), move |state| {
                        let look = &row_look;
                        let palette = &look.palette;
                        let sizes = &look.sizes;
                        let marker = if is_multi {
                            if is_selected {
                                Container::new(icon(WarpIcon::Check, palette.on_accent.0, 11.))
                                    .with_uniform_padding((sizes.icon_size - 11.) / 2.)
                                    .with_background_color(palette.accent.0)
                                    .with_corner_radius(look.radius(4.))
                                    .finish()
                            } else {
                                Container::new(widgets::spacer(
                                    sizes.icon_size - 2.,
                                    sizes.icon_size - 2.,
                                ))
                                .with_border(
                                    Border::all(1.5).with_border_color(palette.text_faint.0),
                                )
                                .with_corner_radius(look.radius(4.))
                                .finish()
                            }
                        } else {
                            let ring_color = if is_selected {
                                palette.accent.0
                            } else {
                                palette.text_faint.0
                            };
                            let mut radio = Stack::new().with_child(widgets::svg(
                                "bundled/svg/picolas-ring-track.svg",
                                ring_color,
                                sizes.icon_size,
                            ));
                            if is_selected {
                                radio.add_child(
                                    Container::new(widgets::block(6., 6., 3., palette.accent.0))
                                        .with_uniform_padding((sizes.icon_size - 6.) / 2.)
                                        .finish(),
                                );
                            }
                            radio.finish()
                        };
                        let mut text = Flex::column()
                            .with_main_axis_size(MainAxisSize::Min)
                            .with_cross_axis_alignment(CrossAxisAlignment::Start)
                            .with_spacing(2.)
                            .with_child(look.medium_text(
                                label.clone(),
                                sizes.small_font_size + 0.5,
                                if is_selected || state.is_hovered() {
                                    palette.text.0
                                } else {
                                    with_alpha(palette.text.0, 0xcc)
                                },
                            ));
                        if let Some(description) = &description {
                            text.add_child(look.paragraph(
                                description.clone(),
                                sizes.tiny_font_size + 0.5,
                                palette.text_muted.0,
                            ));
                        }
                        let mut row = Flex::row()
                            .with_main_axis_size(MainAxisSize::Max)
                            .with_cross_axis_alignment(CrossAxisAlignment::Center)
                            .with_spacing(10.)
                            .with_child(marker)
                            .with_child(Expanded::new(1., text.finish()).finish());
                        if let Some(digit) = digit {
                            row.add_child(widgets::key_cap(look, digit.to_string(), is_selected));
                        }
                        let mut container = Container::new(row.finish())
                            .with_horizontal_padding(10.)
                            .with_vertical_padding(7.)
                            .with_corner_radius(look.radius(sizes.chip_radius + 1.));
                        if is_selected {
                            container = container
                                .with_background_color(with_alpha(palette.accent.0, 0x1a))
                                .with_border(
                                    Border::all(1.)
                                        .with_border_color(with_alpha(palette.accent.0, 0x66)),
                                );
                        } else {
                            container = container
                                .with_border(Border::all(1.).with_border_color(palette.border.0));
                            if state.is_hovered() {
                                container =
                                    container.with_background_color(palette.surface_hover.0);
                            }
                        }
                        container.finish()
                    })
                    .with_cursor(Cursor::PointingHand)
                    .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
                    .finish(),
                );
            }
            column.add_child(options.finish());
        }
        column.add_child(look.text(
            "Ou escreva outra resposta na caixa abaixo",
            sizes.tiny_font_size,
            palette.text_faint.0,
        ));
        let buttons = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_main_axis_alignment(MainAxisAlignment::End)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(8.)
            .with_child(widgets::button(
                look,
                self.mouse.dismiss_question.clone(),
                "Dispensar",
                None,
                ButtonKind::Ghost,
                ComposerAction::DismissQuestion,
            ))
            .with_child(widgets::button(
                look,
                self.mouse.submit_answers.clone(),
                "Enviar",
                Some("↵"),
                ButtonKind::Primary,
                ComposerAction::SubmitAnswers,
            ))
            .finish();
        column.add_child(buttons);
        column.finish()
    }

    fn render_plan(&self, look: &Look, plan: &str) -> Box<dyn Element> {
        let palette = &look.palette;
        let sizes = &look.sizes;
        let body: Box<dyn Element> = match markdown_parser::parse_markdown(plan) {
            Ok(formatted) => FormattedTextElement::new(
                formatted,
                sizes.font_size - 0.5,
                look.ui_font,
                look.mono_font,
                palette.text.0,
                Default::default(),
            )
            .with_line_height_ratio(sizes.line_height_ratio)
            .with_heading_to_font_size_multipliers(HeadingFontSizeMultipliers {
                h1: 1.3,
                h2: 1.2,
                h3: 1.1,
                h4: 1.,
                h5: 1.,
                h6: 1.,
            })
            .with_inline_code_properties(
                Some(palette.text.0),
                Some(palette.inline_code_background.0),
            )
            .finish(),
            Err(_) => look.paragraph(plan, sizes.font_size - 0.5, palette.text.0),
        };
        let scroll = ConstrainedBox::new(
            ClippedScrollable::vertical(
                self.plan_scroll.clone(),
                Container::new(body).with_padding_right(10.).finish(),
                ScrollbarWidth::Custom(4.),
                palette.border_strong.0.into(),
                palette.text_faint.0.into(),
                ElementFill::None,
            )
            .finish(),
        )
        .with_max_height(PLAN_MAX_HEIGHT)
        .finish();
        let plan_box = Container::new(scroll)
            .with_horizontal_padding(14.)
            .with_vertical_padding(10.)
            .with_background_color(palette.page.0)
            .with_border(Border::all(1.).with_border_color(palette.border.0))
            .with_corner_radius(look.radius(sizes.card_radius))
            .finish();

        let buttons = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(8.)
            .with_child(widgets::button(
                look,
                self.mouse.keep_planning.clone(),
                "Continuar planejando",
                None,
                ButtonKind::Ghost,
                ComposerAction::Plan(PlanChoice::KeepPlanning),
            ))
            .with_child(Expanded::new(1., Empty::new().finish()).finish())
            .with_child(widgets::button(
                look,
                self.mouse.approve_accept_edits.clone(),
                "Aprovar e aceitar edições",
                None,
                ButtonKind::Secondary,
                ComposerAction::Plan(PlanChoice::ApproveAcceptEdits),
            ))
            .with_child(widgets::button(
                look,
                self.mouse.approve.clone(),
                "Aprovar",
                Some("↵"),
                ButtonKind::Primary,
                ComposerAction::Plan(PlanChoice::Approve),
            ))
            .finish();

        Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(10.)
            .with_child(Self::render_card_header(
                look,
                WarpIcon::BookOpen,
                palette.info.0,
                vec![("Plano pronto".to_owned(), true)],
            ))
            .with_child(plan_box)
            .with_child(buttons)
            .finish()
    }
}
