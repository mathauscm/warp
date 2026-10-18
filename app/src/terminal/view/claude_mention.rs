//! Text selected in a pane running Claude Code can be mentioned in the conversation, like quoting
//! a message in a chat app: a "Mencionar" pill floats under the end of the selection, and ⌘L does
//! the same. The selection lands in Claude's prompt as a Markdown quote, ready for the question
//! about it.
//!
//! In fullscreen mode Claude takes the mouse and selects text itself, copying it with `pbcopy`
//! when the drag ends; Warp only forwards the mouse events. So a forwarded drag that changes the
//! clipboard is treated as a selection too.

use std::time::Duration;

use pathfinder_geometry::vector::vec2f;
use warp_core::ui::Icon as WarpIcon;
use warp_core::ui::theme::color::internal_colors;
use warpui::r#async::Timer;
use warpui::elements::{
    Border, ChildAnchor, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, DropShadow,
    Element, Flex, Hoverable, MainAxisSize, OffsetPositioning, ParentElement,
    PositionedElementAnchor, PositionedElementOffsetBounds, Radius, Text,
};
use warpui::fonts::{Properties, Weight};
use warpui::platform::Cursor;
use warpui::{AppContext, SingletonEntity, ViewContext};

use super::{TerminalAction, TerminalView};
use crate::appearance::Appearance;
use crate::terminal::cli_agent_sessions::CLIAgentSessionsModel;
use crate::terminal::model::index::Point;
use crate::terminal::model::mouse::{MouseAction, MouseButton, MouseState};

const BUTTON_LABEL: &str = "Mencionar";
const SHORTCUT_LABEL: &str = "⌘L";
const FONT_SIZE: f32 = 12.;
const ICON_SIZE: f32 = 13.;
const BUTTON_HEIGHT: f32 = 24.;
const BUTTON_HORIZONTAL_PADDING: f32 = 9.;
const BUTTON_GAP: f32 = 5.;
/// Space between the end of the selection and the pill.
const BUTTON_OFFSET: f32 = 4.;
/// When to look for the text Claude copies after a drag; `pbcopy` takes a moment.
const CLIPBOARD_CHECK_DELAYS: [Duration; 3] = [
    Duration::from_millis(120),
    Duration::from_millis(300),
    Duration::from_millis(700),
];

/// What the "Mencionar" pill sends to Claude.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ClaudeMention {
    /// Warp's own text selection.
    WarpSelection,
    /// Text Claude selected and copied itself; the pill goes under `anchor`, where the drag ended.
    ClaudeSelection { text: String, anchor: Point },
}

/// A left-button drag forwarded to Claude.
#[derive(Debug, Clone)]
pub(super) struct ClaudeDrag {
    start: Option<Point>,
    moved: bool,
    clipboard_before: String,
}

/// The selection as a Markdown quote, followed by an empty line for the question about it.
pub(super) fn quote(text: &str) -> String {
    let mut quoted = text
        .trim_matches('\n')
        .lines()
        .map(|line| {
            let line = line.trim_end();
            if line.is_empty() {
                ">".to_owned()
            } else {
                format!("> {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    quoted.push_str("\n\n");
    quoted
}

impl TerminalView {
    /// Save-position id of the last rect of this pane's text selection.
    pub(super) fn selection_end_position_id(&self) -> String {
        format!("terminal_view:selection_end_{}", self.view_id)
    }

    /// Offers (or stops offering) to mention Warp's own text selection.
    pub(super) fn update_claude_mention(
        &mut self,
        has_selection: bool,
        ctx: &mut ViewContext<Self>,
    ) {
        let mention = if has_selection && self.runs_claude(ctx) {
            Some(ClaudeMention::WarpSelection)
        } else if matches!(self.claude_mention, Some(ClaudeMention::WarpSelection)) {
            None
        } else {
            return;
        };
        self.set_claude_mention(mention, ctx);
    }

    /// Hides the pill once the user moves on, e.g. by typing.
    pub(super) fn dismiss_claude_mention(&mut self, ctx: &mut ViewContext<Self>) {
        self.set_claude_mention(None, ctx);
    }

    fn set_claude_mention(&mut self, mention: Option<ClaudeMention>, ctx: &mut ViewContext<Self>) {
        if self.claude_mention != mention {
            self.claude_mention = mention;
            ctx.notify();
        }
    }

    /// Follows a mouse event forwarded to Claude, to notice the selections Claude makes itself.
    pub(super) fn track_claude_mouse(
        &mut self,
        mouse_state: &MouseState,
        ctx: &mut ViewContext<Self>,
    ) {
        if !self.runs_claude(ctx) {
            return;
        }
        let point = mouse_state.maybe_point();
        match (mouse_state.button(), mouse_state.action()) {
            (MouseButton::Left, MouseAction::Pressed) => {
                self.set_claude_mention(None, ctx);
                self.claude_drag = Some(ClaudeDrag {
                    start: point,
                    moved: false,
                    clipboard_before: ctx.clipboard().read().plain_text,
                });
            }
            (MouseButton::LeftDrag, _) => {
                if let Some(drag) = &mut self.claude_drag {
                    drag.moved |= point != drag.start;
                }
            }
            (MouseButton::Left, MouseAction::Released) => {
                if let Some(drag) = self.claude_drag.take()
                    && drag.moved
                    && let Some(released_at) = point
                {
                    // A drag can go backwards; the pill belongs under the end of the selection.
                    let anchor = drag
                        .start
                        .map_or(released_at, |start| start.max(released_at));
                    self.watch_for_claude_copy(drag.clipboard_before, anchor, 0, ctx);
                }
            }
            (MouseButton::Left, MouseAction::Scrolled { .. })
            | (MouseButton::Right | MouseButton::Wheel | MouseButton::Move, _) => {}
        }
    }

    /// Offers the text Claude copied at the end of a drag, once the clipboard changes.
    fn watch_for_claude_copy(
        &mut self,
        clipboard_before: String,
        anchor: Point,
        attempt: usize,
        ctx: &mut ViewContext<Self>,
    ) {
        let Some(delay) = CLIPBOARD_CHECK_DELAYS.get(attempt).copied() else {
            return;
        };
        ctx.spawn(Timer::after(delay), move |me, _, ctx| {
            // A new click started another drag, or dismissed this one.
            if me.claude_drag.is_some() {
                return;
            }
            let text = ctx.clipboard().read().plain_text;
            if text.trim().is_empty() || text == clipboard_before {
                me.watch_for_claude_copy(clipboard_before, anchor, attempt + 1, ctx);
                return;
            }
            me.set_claude_mention(Some(ClaudeMention::ClaudeSelection { text, anchor }), ctx);
        });
    }

    /// The cell under which the pill goes when Claude made the selection.
    pub(super) fn claude_mention_anchor(&self) -> Option<Point> {
        match &self.claude_mention {
            Some(ClaudeMention::ClaudeSelection { anchor, .. }) => Some(*anchor),
            Some(ClaudeMention::WarpSelection) | None => None,
        }
    }

    /// Mentions the selected text: as a quote in the rich input when it's open, or else as a chip in
    /// the prompt card that's sent along with the next message.
    pub(super) fn mention_selection_in_claude(&mut self, ctx: &mut ViewContext<Self>) {
        if !self.runs_claude(ctx) {
            return;
        }
        let text = match self.claude_mention.take() {
            Some(ClaudeMention::ClaudeSelection { text, .. }) => Some(text),
            Some(ClaudeMention::WarpSelection) | None => self.selected_text(ctx),
        };
        ctx.notify();
        let Some(text) = text.filter(|text| !text.trim().is_empty()) else {
            return;
        };
        if CLIAgentSessionsModel::as_ref(ctx).is_input_open(self.view_id) {
            let quoted = quote(&text);
            self.input.update(ctx, |input, ctx| {
                input.insert_into_cli_agent_rich_input(&quoted, ctx);
            });
            self.clear_selected_text(ctx);
            self.focus_input_box(ctx);
        } else {
            self.add_claude_mention(text, ctx);
            self.clear_selected_text(ctx);
            self.focus_terminal(ctx);
        }
    }

    /// The "Mencionar" pill and where it goes: under the end of the selection, right-aligned to
    /// it.
    pub(super) fn render_claude_mention_button(
        &self,
        app: &AppContext,
    ) -> Option<(Box<dyn Element>, OffsetPositioning)> {
        if self.claude_mention.is_none() || self.is_selecting {
            return None;
        }
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let font_family = appearance.ui_font_family();
        let background = theme.surface_2();
        let main_text = theme.main_text_color(background);
        let sub_text = theme.sub_text_color(background);
        let hover_background = internal_colors::fg_overlay_2(theme);
        let border = internal_colors::fg_overlay_3(theme);

        let button = Hoverable::new(
            self.claude_mention_button_mouse_state.clone(),
            move |state| {
                let content = Flex::row()
                    .with_main_axis_size(MainAxisSize::Min)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(BUTTON_GAP)
                    .with_child(
                        ConstrainedBox::new(
                            WarpIcon::MessagePlusSquare
                                .to_warpui_icon(sub_text)
                                .finish(),
                        )
                        .with_width(ICON_SIZE)
                        .with_height(ICON_SIZE)
                        .finish(),
                    )
                    .with_child(
                        Text::new_inline(BUTTON_LABEL, font_family, FONT_SIZE)
                            .with_style(Properties::default().weight(Weight::Medium))
                            .with_color(main_text.into())
                            .finish(),
                    )
                    .with_child(
                        Text::new_inline(SHORTCUT_LABEL, font_family, FONT_SIZE)
                            .with_color(sub_text.into())
                            .finish(),
                    )
                    .finish();
                let mut pill = Container::new(
                    ConstrainedBox::new(content)
                        .with_height(BUTTON_HEIGHT)
                        .finish(),
                )
                .with_horizontal_padding(BUTTON_HORIZONTAL_PADDING)
                .with_background(background)
                .with_border(Border::all(1.).with_border_fill(border))
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(BUTTON_HEIGHT / 2.)))
                .with_drop_shadow(DropShadow::default());
                if state.is_hovered() {
                    pill = pill.with_foreground_overlay(hover_background);
                }
                pill.finish()
            },
        )
        .with_cursor(Cursor::PointingHand)
        .on_click(|ctx, _, _| ctx.dispatch_typed_action(TerminalAction::MentionSelectionInClaude))
        .finish();

        let positioning = OffsetPositioning::offset_from_save_position_element(
            self.selection_end_position_id(),
            vec2f(0., BUTTON_OFFSET),
            PositionedElementOffsetBounds::WindowByPosition,
            PositionedElementAnchor::BottomRight,
            ChildAnchor::TopRight,
        );
        Some((button, positioning))
    }
}

#[cfg(test)]
#[path = "claude_mention_tests.rs"]
mod tests;
