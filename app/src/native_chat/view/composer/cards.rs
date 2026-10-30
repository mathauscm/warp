//! The cards stacked above the composer: a crashed process, queued messages, Claude's task list
//! and the request waiting for an answer (a permission, a question or the plan).

use claude_chat::timeline::{
    PendingRequest, PendingRequestKind, Question, QuestionOption, TodoItem, TodoStatus,
};
use serde_json::Value;
use warp_core::ui::Icon as WarpIcon;
use warpui::AppContext;
use warpui::elements::{
    Border, ClippedScrollable, ConstrainedBox, Container, CrossAxisAlignment,
    DEFAULT_UI_LINE_HEIGHT_RATIO, DropShadow, Element, Empty, Expanded, Fill as ElementFill, Flex,
    FormattedTextElement, HeadingFontSizeMultipliers, Hoverable, MainAxisAlignment, MainAxisSize,
    MouseStateHandle, ParentElement, ScrollbarWidth, Shrinkable, SizeConstraintCondition,
    SizeConstraintSwitch, Stack, Text, Wrap,
};
use warpui::fonts::{Properties, Weight};
use warpui::geometry::vector::vec2f;
use warpui::platform::Cursor;

use super::logic::{self, PermissionPreview};
use super::widgets::{self, ButtonKind, Look, icon, with_alpha};
use super::{
    ComposerAction, ComposerView, PermissionChoice, PlanChoice, QueuedMessage, Withdrawal,
    exit_error,
};

const PREVIEW_MAX_HEIGHT: f32 = 132.;
const PLAN_MAX_HEIGHT: f32 = 300.;
const TODO_SEGMENTS_MAX: usize = 12;
/// Below this width the task list's header drops its progress bar.
const TODO_BAR_MIN_WIDTH: f32 = 360.;
const ACTION_SPACING: f32 = 8.;
/// A rough average glyph width as a share of the font size, to tell when buttons fit in a line.
const GLYPH_WIDTH_RATIO: f32 = 0.6;
/// More queued messages than this scroll.
const QUEUE_VISIBLE_ROWS: usize = 3;
const QUEUE_ROW_SPACING: f32 = 2.;
const QUEUE_INDEX_WIDTH: f32 = 16.;
/// Space between the question card's edge and its option rows, whose radius follows the card's.
const QUESTION_CARD_PADDING: f32 = 8.;
const OPTION_PADDING_X: f32 = 8.;
const OPTION_PADDING_Y: f32 = 5.;
const OPTION_GAP: f32 = 9.;
const OPTION_MARKER_SIZE: f32 = 13.;

impl ComposerView {
    pub(super) fn render_cards(&self, look: &Look, app: &AppContext) -> Vec<Box<dyn Element>> {
        let session = self.session.as_ref(app);
        let agent = session.provider().label();
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
            cards.push(self.render_request(
                look,
                request,
                &self.question.selected,
                pending.len(),
                agent,
            ));
        } else if let Some((request, selected)) = dev_request() {
            cards.push(self.render_request(look, &request, &selected, 2, agent));
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

    /// The messages waiting for Claude, oldest first, each one sendable now, editable or
    /// deletable.
    fn render_queued(&self, look: &Look) -> Box<dyn Element> {
        let palette = &look.palette;
        let sizes = &look.sizes;
        let header = ConstrainedBox::new(
            Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(8.)
                .with_child(icon(
                    WarpIcon::Queued,
                    palette.text_muted.0,
                    sizes.small_icon_size,
                ))
                .with_child(look.medium_text("Na fila", sizes.small_font_size, palette.text.0))
                .with_child(look.text(
                    self.queued.len().to_string(),
                    sizes.small_font_size,
                    palette.text_muted.0,
                ))
                .with_child(
                    Shrinkable::new(
                        1.,
                        look.text(
                            "o Claude lê no próximo passo",
                            sizes.tiny_font_size,
                            palette.text_faint.0,
                        ),
                    )
                    .finish(),
                )
                .finish(),
        )
        .with_height(sizes.chip_height)
        .finish();

        let mut rows = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(QUEUE_ROW_SPACING);
        for (position, message) in self.queued.iter().enumerate() {
            rows.add_child(queued_row(look, position, message));
        }
        let visible_rows = self.queued.len().min(QUEUE_VISIBLE_ROWS) as f32;
        let list = ConstrainedBox::new(
            ClippedScrollable::vertical(
                self.queue_scroll.clone(),
                rows.finish(),
                ScrollbarWidth::Custom(4.),
                palette.border_strong.0.into(),
                palette.text_faint.0.into(),
                ElementFill::None,
            )
            .finish(),
        )
        .with_max_height(
            visible_rows * queue_row_height(look) + (visible_rows - 1.) * QUEUE_ROW_SPACING,
        )
        .finish();

        let column = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(header)
            .with_child(Container::new(list).with_padding_top(2.).finish())
            .finish();
        Container::new(column)
            .with_padding_left(sizes.composer_padding)
            .with_padding_right(6.)
            .with_vertical_padding(6.)
            .with_background_color(palette.surface.0)
            .with_border(Border::all(1.).with_border_color(palette.border.0))
            .with_corner_radius(look.radius(sizes.card_radius))
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
            let header_row = |show_bar: bool| {
                let segments = if show_bar {
                    statuses.len().min(TODO_SEGMENTS_MAX)
                } else {
                    0
                };
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
                    ));
                if show_bar {
                    row.add_child(bar.finish());
                }
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
            };
            SizeConstraintSwitch::new(
                header_row(true),
                [(
                    SizeConstraintCondition::WidthLessThan(TODO_BAR_MIN_WIDTH),
                    header_row(false),
                )],
            )
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

    /// `selected` holds the options picked in each question, when the request is a question.
    fn render_request(
        &self,
        look: &Look,
        request: &PendingRequest,
        selected: &[Vec<usize>],
        count: usize,
        agent: &str,
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
                agent,
                tool_name,
                display_name.as_deref(),
                input,
                description.as_deref(),
                blocked_path.as_deref(),
                suggestions.first(),
            ),
            PendingRequestKind::Question { questions, .. } => {
                self.render_questions(look, questions, selected)
            }
            PendingRequestKind::PlanApproval { plan, .. } => self.render_plan(look, plan),
        };
        let palette = &look.palette;
        let sizes = &look.sizes;
        let is_question = matches!(request.kind, PendingRequestKind::Question { .. });
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
                .with_padding_left(if is_question { OPTION_PADDING_X } else { 0. })
                .finish(),
            );
        }
        if is_question {
            return Container::new(column.finish())
                .with_uniform_padding(QUESTION_CARD_PADDING)
                .with_padding_top(QUESTION_CARD_PADDING + 4.)
                .with_background_color(palette.surface.0)
                .with_border(Border::all(1.).with_border_color(palette.border.0))
                .with_corner_radius(look.radius(sizes.composer_radius))
                .with_drop_shadow(DropShadow {
                    color: palette.shadow.0,
                    offset: vec2f(0., 4.),
                    blur_radius: sizes.shadow_blur,
                    spread_radius: 0.,
                })
                .finish();
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
            let part = if is_strong {
                look.medium_text(part, sizes.font_size - 0.5, palette.text.0)
            } else {
                look.text(part, sizes.font_size - 0.5, palette.text_muted.0)
            };
            text.add_child(Shrinkable::new(1., part).finish());
        }
        row.add_child(Shrinkable::new(1., text.finish()).finish());
        row.finish()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_permission(
        &self,
        look: &Look,
        agent: &str,
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
                vec![(format!("{agent} quer usar"), false), (tool, true)],
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
                            Text::new_inline(
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
            column.add_child(look.paragraph(
                format!("Fora da pasta do projeto: {path}"),
                sizes.tiny_font_size,
                palette.text_faint.0,
            ));
        }

        let allow_always = suggestion.map(|suggestion| ActionButton {
            label: logic::suggestion_label(suggestion),
            hint: None,
            kind: ButtonKind::Ghost,
            mouse: self.mouse.allow_always.clone(),
            action: ComposerAction::Permission(PermissionChoice::AllowAlways),
        });
        let buttons = action_row(
            look,
            allow_always.into_iter().collect(),
            vec![
                ActionButton {
                    label: "Negar".to_owned(),
                    hint: Some("esc"),
                    kind: ButtonKind::Secondary,
                    mouse: self.mouse.deny.clone(),
                    action: ComposerAction::Permission(PermissionChoice::Deny),
                },
                ActionButton {
                    label: "Permitir".to_owned(),
                    hint: Some("↵"),
                    kind: ButtonKind::Primary,
                    mouse: self.mouse.allow.clone(),
                    action: ComposerAction::Permission(PermissionChoice::Allow),
                },
            ],
        );
        column.add_child(Container::new(buttons).with_padding_top(2.).finish());
        column.finish()
    }

    /// A preview in a code box that scrolls once it's taller than [`PREVIEW_MAX_HEIGHT`].
    fn render_code_box(&self, look: &Look, content: Box<dyn Element>) -> Box<dyn Element> {
        let palette = &look.palette;
        let scrollable = ClippedScrollable::vertical(
            self.preview_scroll.clone(),
            Container::new(content).with_padding_right(6.).finish(),
            ScrollbarWidth::Custom(4.),
            palette.border_strong.0.into(),
            palette.text_faint.0.into(),
            ElementFill::None,
        )
        .finish();
        ConstrainedBox::new(
            Container::new(scrollable)
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

    /// `selected` holds the options picked in each question.
    fn render_questions(
        &self,
        look: &Look,
        questions: &[Question],
        selected: &[Vec<usize>],
    ) -> Box<dyn Element> {
        let sizes = &look.sizes;
        let option_radius = sizes.composer_radius - QUESTION_CARD_PADDING;
        let mut column = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(14.);
        let mut option_states = self.mouse.options.iter();
        for (question_index, question) in questions.iter().enumerate() {
            let picked = selected
                .get(question_index)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let is_active = question_index == self.question.active;
            let mut block = Flex::column()
                .with_main_axis_size(MainAxisSize::Min)
                .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
                .with_spacing(1.)
                .with_child(
                    Container::new(question_heading(
                        look,
                        question,
                        question_index,
                        questions.len(),
                    ))
                    .with_horizontal_padding(OPTION_PADDING_X)
                    .with_padding_bottom(6.)
                    .finish(),
                );
            for (option_index, option) in question.options.iter().enumerate() {
                let Some(mouse_state) = option_states.next() else {
                    break;
                };
                block.add_child(option_row(
                    look,
                    mouse_state.clone(),
                    option,
                    OptionState {
                        is_multi: question.multi_select,
                        is_selected: picked.contains(&option_index),
                        digit: (is_active && option_index < 9).then_some(option_index + 1),
                        radius: option_radius,
                    },
                    ComposerAction::ToggleOption {
                        question: question_index,
                        option: option_index,
                    },
                ));
            }
            column.add_child(block.finish());
        }
        column.add_child(
            Container::new(note_and_actions(
                look,
                "Ou escreva outra resposta na caixa abaixo",
                vec![
                    ActionButton {
                        label: "Dispensar".to_owned(),
                        hint: None,
                        kind: ButtonKind::Ghost,
                        mouse: self.mouse.dismiss_question.clone(),
                        action: ComposerAction::DismissQuestion,
                    },
                    ActionButton {
                        label: "Enviar".to_owned(),
                        hint: Some("↵"),
                        kind: ButtonKind::Primary,
                        mouse: self.mouse.submit_answers.clone(),
                        action: ComposerAction::SubmitAnswers,
                    },
                ],
            ))
            .with_padding_left(OPTION_PADDING_X)
            .finish(),
        );
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

        let buttons = action_row(
            look,
            vec![ActionButton {
                label: "Continuar planejando".to_owned(),
                hint: None,
                kind: ButtonKind::Ghost,
                mouse: self.mouse.keep_planning.clone(),
                action: ComposerAction::Plan(PlanChoice::KeepPlanning),
            }],
            vec![
                ActionButton {
                    label: "Aprovar e aceitar edições".to_owned(),
                    hint: None,
                    kind: ButtonKind::Secondary,
                    mouse: self.mouse.approve_accept_edits.clone(),
                    action: ComposerAction::Plan(PlanChoice::ApproveAcceptEdits),
                },
                ActionButton {
                    label: "Aprovar".to_owned(),
                    hint: Some("↵"),
                    kind: ButtonKind::Primary,
                    mouse: self.mouse.approve.clone(),
                    action: ComposerAction::Plan(PlanChoice::Approve),
                },
            ],
        );

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

/// `PICOLAS_FAKE_REQUEST=permission|question|plan` shows that request with made-up content when
/// Claude isn't waiting for one, to check the cards' layout, with some options picked. Debug builds
/// only.
fn dev_request() -> Option<(PendingRequest, Vec<Vec<usize>>)> {
    if !cfg!(debug_assertions) {
        return None;
    }
    let kind = match std::env::var("PICOLAS_FAKE_REQUEST").ok()?.as_str() {
        "permission" => PendingRequestKind::Permission {
            tool_name: "Bash".to_owned(),
            display_name: None,
            input: serde_json::json!({
                "command": (1..=24)
                    .map(|step| format!(
                        "echo \"passo {step}: verificando /Users/exemplo/projetos/um-caminho-bem-longo/sem-espacos/arquivo-{step}.txt\""
                    ))
                    .collect::<Vec<_>>()
                    .join(" && \\\n  "),
            }),
            description: Some("Executa uma sequência longa de comandos de verificação".to_owned()),
            blocked_path: None,
            suggestions: vec![serde_json::json!({
                "type": "addRules",
                "rules": [{ "toolName": "Bash", "ruleContent": "echo:*" }],
                "destination": "localSettings",
            })],
        },
        "question" => PendingRequestKind::Question {
            input: Value::Null,
            questions: vec![
                Question {
                    question: "Qual abordagem você prefere para o layout responsivo?".to_owned(),
                    header: Some("Layout".to_owned()),
                    multi_select: false,
                    options: vec![
                        QuestionOption {
                            label: "Empilhar os botões".to_owned(),
                            description: Some("Um botão por linha em painéis estreitos".to_owned()),
                        },
                        QuestionOption {
                            label: "Quebrar em linhas".to_owned(),
                            description: None,
                        },
                    ],
                },
                Question {
                    question: "Quais recursos entram na primeira versão?".to_owned(),
                    header: Some("Recursos".to_owned()),
                    multi_select: true,
                    options: vec![
                        QuestionOption {
                            label: "Autenticação".to_owned(),
                            description: Some("Login com e-mail e senha".to_owned()),
                        },
                        QuestionOption {
                            label: "Notificações".to_owned(),
                            description: Some("Avisos por e-mail quando algo muda".to_owned()),
                        },
                        QuestionOption {
                            label: "Relatórios".to_owned(),
                            description: None,
                        },
                    ],
                },
            ],
        },
        "plan" => PendingRequestKind::PlanApproval {
            input: Value::Null,
            plan: (1..=30)
                .map(|step| format!("{step}. Um passo do plano com bastante texto para quebrar"))
                .collect::<Vec<_>>()
                .join("\n"),
            plan_file_path: None,
        },
        _ => return None,
    };
    Some((
        PendingRequest {
            request_id: String::new(),
            tool_use_id: None,
            kind,
        },
        vec![vec![0], vec![0, 2]],
    ))
}

/// A button of a card's action row.
struct ActionButton {
    label: String,
    hint: Option<&'static str>,
    kind: ButtonKind,
    mouse: MouseStateHandle,
    action: ComposerAction,
}

impl ActionButton {
    fn render(&self, look: &Look) -> Box<dyn Element> {
        widgets::button(
            look,
            self.mouse.clone(),
            self.label.clone(),
            self.hint,
            self.kind,
            self.action.clone(),
        )
    }

    /// About how wide the button draws, padding included.
    fn estimated_width(&self, look: &Look) -> f32 {
        let glyph = |text: &str, size: f32| text.chars().count() as f32 * size * GLYPH_WIDTH_RATIO;
        let hint = self
            .hint
            .map_or(0., |hint| 6. + glyph(hint, look.sizes.tiny_font_size));
        glyph(&self.label, look.sizes.small_font_size) + hint + 24.
    }
}

/// A card's buttons in one line, `leading` at the left and `trailing` at the right; when they
/// don't fit, they wrap onto more lines, aligned right, so none goes out of the card.
fn action_row(
    look: &Look,
    leading: Vec<ActionButton>,
    trailing: Vec<ActionButton>,
) -> Box<dyn Element> {
    let buttons = || leading.iter().chain(&trailing);
    let inline_width = buttons()
        .map(|button| button.estimated_width(look) + ACTION_SPACING)
        .sum::<f32>()
        + ACTION_SPACING;

    let mut inline = Flex::row()
        .with_main_axis_size(MainAxisSize::Max)
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(ACTION_SPACING);
    for button in &leading {
        inline.add_child(button.render(look));
    }
    inline.add_child(Expanded::new(1., Empty::new().finish()).finish());
    for button in &trailing {
        inline.add_child(button.render(look));
    }

    let mut wrapped = Wrap::row()
        .with_main_axis_alignment(MainAxisAlignment::End)
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(ACTION_SPACING)
        .with_run_spacing(ACTION_SPACING);
    wrapped.extend(buttons().map(|button| button.render(look)));

    SizeConstraintSwitch::new(
        inline.finish(),
        [(
            SizeConstraintCondition::WidthLessThan(inline_width),
            wrapped.finish(),
        )],
    )
    .finish()
}

/// A muted note at the left of a card's buttons; when both don't fit in one line, the note goes
/// above the buttons, which then wrap like [`action_row`]'s.
fn note_and_actions(look: &Look, note: &str, trailing: Vec<ActionButton>) -> Box<dyn Element> {
    let note_size = look.sizes.tiny_font_size;
    let note_color = look.palette.text_faint.0;
    let inline_width = note.chars().count() as f32 * note_size * GLYPH_WIDTH_RATIO
        + trailing
            .iter()
            .map(|button| button.estimated_width(look) + ACTION_SPACING)
            .sum::<f32>()
        + ACTION_SPACING;

    let mut inline = Flex::row()
        .with_main_axis_size(MainAxisSize::Max)
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(ACTION_SPACING)
        .with_child(Expanded::new(1., look.text(note, note_size, note_color)).finish());
    for button in &trailing {
        inline.add_child(button.render(look));
    }

    let stacked = Flex::column()
        .with_main_axis_size(MainAxisSize::Min)
        .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
        .with_spacing(8.)
        .with_child(look.paragraph(note, note_size, note_color))
        .with_child(action_row(look, Vec::new(), trailing));

    SizeConstraintSwitch::new(
        inline.finish(),
        [(
            SizeConstraintCondition::WidthLessThan(inline_width),
            stacked.finish(),
        )],
    )
    .finish()
}

/// A question's subtle header chip, its place among several questions, and the question itself.
fn question_heading(
    look: &Look,
    question: &Question,
    index: usize,
    count: usize,
) -> Box<dyn Element> {
    let palette = &look.palette;
    let sizes = &look.sizes;
    let mut tags = Flex::row()
        .with_main_axis_size(MainAxisSize::Min)
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(6.);
    if let Some(header) = question.header.as_ref().filter(|header| !header.is_empty()) {
        tags.add_child(
            Shrinkable::new(
                1.,
                Container::new(look.medium_text(
                    header.clone(),
                    sizes.tiny_font_size - 0.5,
                    palette.text_muted.0,
                ))
                .with_horizontal_padding(6.)
                .with_vertical_padding(1.5)
                .with_background_color(palette.surface_strong.0)
                .with_corner_radius(look.radius(6.))
                .finish(),
            )
            .finish(),
        );
    }
    if count > 1 {
        tags.add_child(look.text(
            format!("{} de {count}", index + 1),
            sizes.tiny_font_size - 0.5,
            palette.text_faint.0,
        ));
    }
    let title = Text::new(
        question.question.clone(),
        look.ui_font,
        sizes.small_font_size + 1.,
    )
    .soft_wrap(true)
    .with_style(Properties::default().weight(Weight::Medium))
    .with_color(palette.text.0)
    .with_line_height_ratio(1.35)
    .finish();
    let mut column = Flex::column()
        .with_main_axis_size(MainAxisSize::Min)
        .with_cross_axis_alignment(CrossAxisAlignment::Start)
        .with_spacing(6.);
    if question
        .header
        .as_ref()
        .is_some_and(|header| !header.is_empty())
        || count > 1
    {
        column.add_child(tags.finish());
    }
    column.add_child(title);
    column.finish()
}

/// How an option row of a question draws.
#[derive(Clone, Copy)]
struct OptionState {
    is_multi: bool,
    is_selected: bool,
    /// The key that picks it, shown while its question takes the digits.
    digit: Option<usize>,
    radius: f32,
}

/// An option of a question: a light row, tinted with the accent color once picked.
fn option_row(
    look: &Look,
    mouse_state: MouseStateHandle,
    option: &QuestionOption,
    state: OptionState,
    action: ComposerAction,
) -> Box<dyn Element> {
    let look = look.clone();
    let label = option.label.clone();
    let description = option
        .description
        .clone()
        .filter(|description| !description.trim().is_empty());
    Hoverable::new(mouse_state, move |mouse| {
        let palette = &look.palette;
        let sizes = &look.sizes;
        let is_hovered = mouse.is_hovered();
        let label_size = sizes.small_font_size + 0.5;
        let label_color = if state.is_selected || is_hovered {
            palette.text.0
        } else {
            with_alpha(palette.text.0, 0xe0)
        };
        let mut text = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_spacing(1.)
            .with_child(look.text(label.clone(), label_size, label_color));
        if let Some(description) = &description {
            text.add_child(look.paragraph(
                description.clone(),
                sizes.tiny_font_size + 0.5,
                palette.text_muted.0,
            ));
        }
        // Lines the marker and the key up with the label's first line when a description follows.
        let line_height = label_size * DEFAULT_UI_LINE_HEIGHT_RATIO;
        let mut row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_spacing(OPTION_GAP)
            .with_child(
                Container::new(option_marker(&look, state.is_multi, state.is_selected))
                    .with_padding_top(((line_height - OPTION_MARKER_SIZE) / 2.).max(0.))
                    .finish(),
            )
            .with_child(Expanded::new(1., text.finish()).finish());
        if let Some(digit) = state.digit {
            row.add_child(
                Container::new(widgets::key_cap(
                    &look,
                    digit.to_string(),
                    state.is_selected,
                ))
                .with_padding_top(((line_height - widgets::KEY_CAP_SIZE) / 2.).max(0.))
                .finish(),
            );
        }
        let background = match (state.is_selected, is_hovered) {
            (true, false) => Some(with_alpha(palette.accent.0, 0x29)),
            (true, true) => Some(with_alpha(palette.accent.0, 0x36)),
            (false, true) => Some(palette.surface_hover.0),
            (false, false) => None,
        };
        let mut container = Container::new(row.finish())
            .with_horizontal_padding(OPTION_PADDING_X)
            .with_vertical_padding(OPTION_PADDING_Y)
            .with_corner_radius(look.radius(state.radius));
        if let Some(background) = background {
            container = container.with_background_color(background);
        }
        container.finish()
    })
    .with_cursor(Cursor::PointingHand)
    .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
    .finish()
}

/// A small checkbox for a multiple-choice question, a radio button otherwise.
fn option_marker(look: &Look, is_multi: bool, is_selected: bool) -> Box<dyn Element> {
    let palette = &look.palette;
    let size = OPTION_MARKER_SIZE;
    match (is_multi, is_selected) {
        (true, true) => Container::new(icon(WarpIcon::Check, palette.on_accent.0, size - 4.))
            .with_uniform_padding(2.)
            .with_background_color(palette.accent.0)
            .with_corner_radius(look.radius(4.))
            .finish(),
        (true, false) => Container::new(widgets::spacer(size - 3., size - 3.))
            .with_border(Border::all(1.5).with_border_color(palette.text_faint.0))
            .with_corner_radius(look.radius(4.))
            .finish(),
        (false, is_selected) => {
            let ring_color = if is_selected {
                palette.accent.0
            } else {
                palette.text_faint.0
            };
            let mut radio = Stack::new().with_child(widgets::svg(
                "bundled/svg/picolas-ring-track.svg",
                ring_color,
                size,
            ));
            if is_selected {
                let dot = 5.;
                radio.add_child(
                    Container::new(widgets::block(dot, dot, dot / 2., palette.accent.0))
                        .with_uniform_padding((size - dot) / 2.)
                        .finish(),
                );
            }
            radio.finish()
        }
    }
}

fn queue_row_height(look: &Look) -> f32 {
    look.sizes.chip_height - 2.
}

/// One queued message: its place in line, a one-line preview, and its actions or what is
/// happening to it.
fn queued_row(look: &Look, position: usize, message: &QueuedMessage) -> Box<dyn Element> {
    let state = if message.sending_now {
        Some("enviando agora…")
    } else {
        match message.withdrawing {
            Some(Withdrawal::Edit) => Some("voltando ao editor…"),
            Some(Withdrawal::Delete) => Some("apagando…"),
            None => None,
        }
    };
    let look = look.clone();
    let uuid = message.uuid.clone();
    let preview = message.preview.clone();
    let buttons = [
        (
            message.send_now.clone(),
            WarpIcon::ArrowUp,
            ComposerAction::SendQueuedNow(uuid.clone()),
        ),
        (
            message.edit.clone(),
            WarpIcon::Pencil,
            ComposerAction::EditQueued(uuid.clone()),
        ),
        (
            message.delete.clone(),
            WarpIcon::Trash,
            ComposerAction::DeleteQueued(uuid),
        ),
    ];
    Hoverable::new(message.row.clone(), move |row_state| {
        let palette = &look.palette;
        let sizes = &look.sizes;
        let hovered = row_state.is_hovered() && state.is_none();
        let mut row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(
                ConstrainedBox::new(look.text(
                    (position + 1).to_string(),
                    sizes.tiny_font_size,
                    palette.text_faint.0,
                ))
                .with_width(QUEUE_INDEX_WIDTH)
                .finish(),
            )
            .with_child(
                Expanded::new(
                    1.,
                    look.text(
                        preview.clone(),
                        sizes.small_font_size,
                        match (state, hovered) {
                            (Some(_), _) => palette.text_faint.0,
                            (None, true) => palette.text.0,
                            (None, false) => palette.text_muted.0,
                        },
                    ),
                )
                .finish(),
            );
        match state {
            Some(state) => {
                row.add_child(
                    Container::new(look.text(state, sizes.tiny_font_size, palette.text_faint.0))
                        .with_padding_left(8.)
                        .with_padding_right(6.)
                        .finish(),
                );
            }
            None => {
                for (mouse, warp_icon, action) in &buttons {
                    row.add_child(widgets::subtle_icon_button(
                        &look,
                        mouse.clone(),
                        *warp_icon,
                        hovered,
                        action.clone(),
                    ));
                }
            }
        }
        let mut container = Container::new(
            ConstrainedBox::new(row.finish())
                .with_height(queue_row_height(&look))
                .finish(),
        )
        .with_padding_left(6.)
        .with_padding_right(2.)
        .with_corner_radius(look.radius(look.sizes.chip_radius));
        if hovered {
            container = container.with_background_color(palette.surface_hover.0);
        }
        container.finish()
    })
    .finish()
}
