//! The window top bar's plan usage readout: how much of the Claude plan's 5-hour and weekly limits
//! is used and when they start over. Clicking it opens a chat's "Uso do plano" card.

use std::time::{Duration, SystemTime};

use claude_chat::reducer::{ConversationChange, RateLimitWindow, RateLimits};
use pathfinder_color::ColorU;
use pathfinder_geometry::vector::{Vector2F, vec2f};
use warp_core::ui::theme::color::internal_colors;
use warpui::r#async::{SpawnedFutureHandle, Timer};
use warpui::elements::{
    Border, ChildAnchor, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, Element,
    Empty, Flex, Hoverable, MainAxisSize, MouseStateHandle, OffsetPositioning, ParentAnchor,
    ParentElement, ParentOffsetBounds, Point, Radius, Rect, Stack, Text,
};
use warpui::event::DispatchedEvent;
use warpui::fonts::{FamilyId, Properties, Weight};
use warpui::platform::Cursor;
use warpui::ui_components::components::UiComponent;
use warpui::{
    AfterLayoutContext, AppContext, Entity, EventContext, LayoutContext, ModelHandle, PaintContext,
    SingletonEntity, SizeConstraint, TypedActionView, View, ViewContext, ViewHandle,
    WeakModelHandle, WeakViewHandle,
};

use super::side_panel::format::{format_cost, format_percent, format_reset};
use super::side_panel::widgets::{meter_color, ring};
use super::{ClaudeChatView, SidePanelSection};
use crate::appearance::Appearance;
use crate::native_chat::design::{ChatDesign, ChatDesignEvent};
use crate::native_chat::session::{ClaudeChatSession, ClaudeChatSessionEvent};
use crate::settings::ThemeSettings;

const HEIGHT: f32 = 24.;
/// The gap between the capsule's edge and a segment's hover background.
const INSET: f32 = 2.;
const SEGMENT_PADDING_X: f32 = 8.;
const SEGMENT_SPACING: f32 = 6.;
const RING_SIZE: f32 = 13.;
const VALUE_FONT_SIZE: f32 = 12.;
const RESET_FONT_SIZE: f32 = 11.;
const DIVIDER_HEIGHT: f32 = 12.;
const MARGIN_RIGHT: f32 = 8.;
const TOOLTIP_OFFSET: f32 = 6.;
/// The gap between a value and its suffix, tighter than between the ring and the value.
const SUFFIX_SPACING: f32 = 3.;
/// Below these window widths the weekly segment, then the reset countdown, give up their room to
/// the rest of the title bar.
const WEEKLY_MIN_WINDOW_WIDTH: f32 = 1000.;
const RESET_MIN_WINDOW_WIDTH: f32 = 860.;
/// The plan card's threshold for turning amber.
const PLAN_WARNING_PERCENT: f64 = 75.;
/// Lets a countdown tick land just after the minute it waits for has turned.
const TICK_SLACK: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatUsageBarAction {
    OpenPlanCard,
}

/// A chat the bar reads plan usage from or opens the plan card of.
struct ChatRef {
    view: WeakViewHandle<ClaudeChatView>,
    session: WeakModelHandle<ClaudeChatSession>,
}

pub struct ChatUsageBar {
    /// The chat in the focused pane, watched for the plan usage it reports.
    focused: Option<ChatRef>,
    /// The last chat seen reporting plan usage, which the bar shows. The plan is the account's,
    /// so the readout stays while the focus moves to a terminal or a chat that hasn't reported.
    reporter: Option<ChatRef>,
    plan_mouse: MouseStateHandle,
    weekly_mouse: MouseStateHandle,
    /// Re-renders the reset countdown when its minute turns, while a plan window is shown.
    countdown: Option<SpawnedFutureHandle>,
}

impl ChatUsageBar {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        // Carries the theme settings, including whether the readout shows at all.
        ctx.subscribe_to_model(&ChatDesign::handle(ctx), |_, _, event, ctx| match event {
            ChatDesignEvent::Changed => ctx.notify(),
        });
        Self {
            focused: None,
            reporter: None,
            plan_mouse: Default::default(),
            weekly_mouse: Default::default(),
            countdown: None,
        }
    }

    /// Watches `chat`, the chat in the focused pane, if any, and shows its plan usage once it
    /// reports some.
    pub fn set_focused_chat(
        &mut self,
        chat: Option<&ViewHandle<ClaudeChatView>>,
        ctx: &mut ViewContext<Self>,
    ) {
        let current = self
            .focused
            .as_ref()
            .and_then(|chat| chat.view.upgrade(ctx));
        if current.as_ref().map(|chat| chat.id()) == chat.map(|chat| chat.id()) {
            return;
        }
        if let Some(session) = self
            .focused
            .take()
            .and_then(|chat| chat.session.upgrade(ctx))
        {
            ctx.unsubscribe_to_model(&session);
        }
        let Some(chat) = chat else {
            return;
        };
        let session = chat.as_ref(ctx).session().clone();
        ctx.subscribe_to_model(&session, |me, _, event, ctx| {
            if affects_bar(event) {
                me.adopt_focused_if_reporting(ctx);
                me.schedule_countdown(false, ctx);
                ctx.notify();
            }
        });
        self.focused = Some(ChatRef {
            view: chat.downgrade(),
            session: session.downgrade(),
        });
        if self.adopt_focused_if_reporting(ctx) {
            self.schedule_countdown(true, ctx);
            ctx.notify();
        }
    }

    /// Makes the focused chat the one shown when it has plan usage; returns whether it did.
    fn adopt_focused_if_reporting(&mut self, ctx: &mut ViewContext<Self>) -> bool {
        let Some(focused) = &self.focused else {
            return false;
        };
        let reports = focused.session.upgrade(ctx).is_some_and(|session| {
            headline_window(session.as_ref(ctx).conversation().rate_limits()).is_some()
        });
        if reports {
            self.reporter = Some(ChatRef {
                view: focused.view.clone(),
                session: focused.session.clone(),
            });
        }
        reports
    }

    fn reporter_session(&self, app: &AppContext) -> Option<ModelHandle<ClaudeChatSession>> {
        self.reporter.as_ref()?.session.upgrade(app)
    }

    /// Waits for the shown countdown's minute to turn; with `restart`, drops the wait already
    /// running, which may be for another chat's window.
    fn schedule_countdown(&mut self, restart: bool, ctx: &mut ViewContext<Self>) {
        if restart && let Some(countdown) = self.countdown.take() {
            countdown.abort();
        }
        if self.countdown.is_some() {
            return;
        }
        let Some(session) = self.reporter_session(ctx) else {
            return;
        };
        let Some(resets_at) = headline_window(session.as_ref(ctx).conversation().rate_limits())
            .and_then(|window| window.resets_at)
        else {
            return;
        };
        let remaining = resets_at
            .duration_since(SystemTime::now())
            .unwrap_or_default();
        if remaining.is_zero() {
            return;
        }
        let delay = until_minute_turns(remaining) + TICK_SLACK;
        self.countdown = Some(ctx.spawn(Timer::after(delay), |me, _, ctx| {
            me.countdown = None;
            me.schedule_countdown(false, ctx);
            ctx.notify();
        }));
    }

    /// Opens the plan card in the focused chat, else in the chat the numbers came from.
    fn open_plan_card(&self, ctx: &mut ViewContext<Self>) {
        let chat = [&self.focused, &self.reporter]
            .into_iter()
            .flatten()
            .find_map(|chat| chat.view.upgrade(ctx));
        if let Some(chat) = chat {
            chat.update(ctx, |chat, ctx| {
                chat.reveal_side_panel(SidePanelSection::Usage, ctx)
            });
        }
    }

    fn render_segment(
        &self,
        gauge: &Gauge,
        show_reset: bool,
        look: BarLook,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let gauge = gauge.clone();
        let ui_builder = Appearance::as_ref(app).ui_builder().clone();
        Hoverable::new(gauge.mouse.clone(), move |state| {
            let is_hovered = state.is_hovered();
            let value_color = if gauge.is_warning {
                gauge.color
            } else if is_hovered {
                look.strong
            } else {
                look.value
            };
            let mut row = Flex::row()
                .with_main_axis_size(MainAxisSize::Min)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(SEGMENT_SPACING)
                .with_child(ring(gauge.fraction, gauge.color, look.track, RING_SIZE))
                .with_child(
                    Flex::row()
                        .with_main_axis_size(MainAxisSize::Min)
                        .with_cross_axis_alignment(CrossAxisAlignment::Center)
                        .with_spacing(SUFFIX_SPACING)
                        .with_child(look.text(
                            gauge.value.clone(),
                            VALUE_FONT_SIZE,
                            value_color,
                            Weight::Medium,
                        ))
                        .with_children(gauge.suffix.map(|suffix| {
                            look.text(suffix, RESET_FONT_SIZE, look.faint, Weight::Normal)
                        }))
                        .finish(),
                );
            if show_reset && let Some(reset) = &gauge.reset {
                row.add_child(look.text(
                    reset.clone(),
                    RESET_FONT_SIZE,
                    look.faint,
                    Weight::Normal,
                ));
            }
            let height = segment_height();
            let mut segment = Container::new(
                ConstrainedBox::new(row.finish())
                    .with_height(height)
                    .finish(),
            )
            .with_horizontal_padding(SEGMENT_PADDING_X)
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(height / 2.)));
            if is_hovered {
                segment = segment.with_background_color(look.hover);
            }
            let segment = segment.finish();
            if !is_hovered {
                return segment;
            }
            let mut stack = Stack::new().with_child(segment);
            stack.add_positioned_overlay_child(
                ui_builder.tool_tip(gauge.tooltip.clone()).build().finish(),
                OffsetPositioning::offset_from_parent(
                    vec2f(0., TOOLTIP_OFFSET + INSET),
                    ParentOffsetBounds::WindowByPosition,
                    ParentAnchor::BottomRight,
                    ChildAnchor::TopRight,
                ),
            );
            stack.finish()
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(|ctx, _, _| {
            ctx.dispatch_typed_action(ChatUsageBarAction::OpenPlanCard);
        })
        .finish()
    }

    /// One rounded capsule holding `gauges`, split by thin dividers.
    fn render_capsule(
        &self,
        gauges: &[&Gauge],
        show_reset: bool,
        look: BarLook,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let mut row = Flex::row()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Center);
        for (index, gauge) in gauges.iter().enumerate() {
            if index > 0 {
                row.add_child(look.divider());
            }
            row.add_child(self.render_segment(gauge, show_reset, look, app));
        }
        Container::new(
            Container::new(row.finish())
                .with_uniform_padding(INSET)
                .with_background_color(look.capsule)
                .with_border(Border::all(1.).with_border_color(look.border))
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(HEIGHT / 2.)))
                .finish(),
        )
        .with_margin_right(MARGIN_RIGHT)
        .finish()
    }
}

impl Entity for ChatUsageBar {
    type Event = ();
}

impl View for ChatUsageBar {
    fn ui_name() -> &'static str {
        "ChatUsageBar"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        if !*ThemeSettings::as_ref(app).show_chat_usage_in_title_bar {
            return Empty::new().finish();
        }
        let Some(session) = self.reporter_session(app) else {
            return Empty::new().finish();
        };
        let conversation = session.as_ref(app).conversation();
        let limits = conversation.rate_limits();
        if headline_window(limits).is_none() {
            return Empty::new().finish();
        }

        let look = BarLook::new(app);
        let palette = ChatDesign::as_ref(app).palette(app);
        let now = SystemTime::now();
        let window_gauge = |window: &RateLimitWindow, mouse: &MouseStateHandle| {
            let percent = window.utilization_percent;
            Gauge {
                mouse: mouse.clone(),
                fraction: percent / 100.,
                color: meter_color(percent, palette.info.0, PLAN_WARNING_PERCENT, palette),
                is_warning: percent >= PLAN_WARNING_PERCENT,
                value: format_percent(percent),
                suffix: None,
                reset: None,
                tooltip: String::new(),
            }
        };
        let plan = limits.five_hour.as_ref().map(|window| Gauge {
            reset: window
                .resets_at
                .map(|resets_at| compact_reset(remaining(resets_at, now))),
            tooltip: window_tooltip(
                "Sessão de 5 horas",
                window,
                conversation.total_cost_usd(),
                now,
            ),
            ..window_gauge(window, &self.plan_mouse)
        });
        let weekly = limits.seven_day.as_ref().map(|window| Gauge {
            suffix: Some("sem"),
            tooltip: window_tooltip("Semana", window, 0., now),
            ..window_gauge(window, &self.weekly_mouse)
        });

        let all: Vec<&Gauge> = plan.iter().chain(weekly.iter()).collect();
        let first: Vec<&Gauge> = all.iter().copied().take(1).collect();
        WindowWidthSwitch::new(
            vec![
                (
                    WEEKLY_MIN_WINDOW_WIDTH,
                    self.render_capsule(&all, true, look, app),
                ),
                (
                    RESET_MIN_WINDOW_WIDTH,
                    self.render_capsule(&first, true, look, app),
                ),
            ],
            self.render_capsule(&first, false, look, app),
        )
        .finish()
    }
}

impl TypedActionView for ChatUsageBar {
    type Action = ChatUsageBarAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            ChatUsageBarAction::OpenPlanCard => self.open_plan_card(ctx),
        }
    }
}

/// One plan window's part of the readout: a ring filled to `fraction`, the value beside it and,
/// optionally, when the window starts over.
#[derive(Clone)]
struct Gauge {
    mouse: MouseStateHandle,
    fraction: f64,
    color: ColorU,
    /// Near the limit, the value takes the ring's warning color.
    is_warning: bool,
    value: String,
    /// Tells the value apart from a neighbor's, like `sem` on the weekly percent.
    suffix: Option<&'static str>,
    reset: Option<String>,
    tooltip: String,
}

#[derive(Clone, Copy)]
struct BarLook {
    font: FamilyId,
    capsule: ColorU,
    border: ColorU,
    hover: ColorU,
    track: ColorU,
    strong: ColorU,
    value: ColorU,
    faint: ColorU,
}

impl BarLook {
    fn new(app: &AppContext) -> Self {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let background = theme.background();
        Self {
            font: appearance.ui_font_family(),
            capsule: internal_colors::fg_overlay_1(theme).into(),
            border: internal_colors::fg_overlay_2(theme).into(),
            hover: internal_colors::fg_overlay_2(theme).into(),
            track: internal_colors::fg_overlay_3(theme).into(),
            strong: theme.main_text_color(background).into(),
            value: theme.sub_text_color(background).into(),
            faint: theme.hint_text_color(background).into(),
        }
    }

    fn text(
        &self,
        text: impl Into<String>,
        size: f32,
        color: ColorU,
        weight: Weight,
    ) -> Box<dyn Element> {
        Text::new_inline(text.into(), self.font, size)
            .with_style(Properties::default().weight(weight))
            .with_color(color)
            .finish()
    }

    fn divider(&self) -> Box<dyn Element> {
        Container::new(
            ConstrainedBox::new(Rect::new().with_background_color(self.border).finish())
                .with_width(1.)
                .with_height(DIVIDER_HEIGHT)
                .finish(),
        )
        .with_horizontal_margin(2.)
        .finish()
    }
}

/// The height of a segment inside the capsule's inset and border.
fn segment_height() -> f32 {
    HEIGHT - 2. * INSET - 2.
}

fn affects_bar(event: &ClaudeChatSessionEvent) -> bool {
    match event {
        ClaudeChatSessionEvent::Changed(changes) => changes.iter().any(|change| match change {
            ConversationChange::RateLimits
            | ConversationChange::Cost
            | ConversationChange::Session => true,
            ConversationChange::ContextUsage
            | ConversationChange::ItemInserted { .. }
            | ConversationChange::ItemUpdated { .. }
            | ConversationChange::ItemRemoved { .. }
            | ConversationChange::PendingRequests
            | ConversationChange::Status
            | ConversationChange::Commands
            | ConversationChange::Models
            | ConversationChange::Account
            | ConversationChange::McpServers
            | ConversationChange::RemoteControl
            | ConversationChange::Todos
            | ConversationChange::Tasks => false,
        }),
        ClaudeChatSessionEvent::HistoryLoaded => true,
        ClaudeChatSessionEvent::ProcessStateChanged
        | ClaudeChatSessionEvent::TitleChanged
        | ClaudeChatSessionEvent::SessionIdAssigned
        | ClaudeChatSessionEvent::FileSuggestions { .. }
        | ClaudeChatSessionEvent::QueuedMessageCancelled { .. }
        | ClaudeChatSessionEvent::Error(_) => false,
    }
}

/// The plan window the bar shows: the 5-hour one, or the weekly one when that's all there is.
fn headline_window(limits: &RateLimits) -> Option<&RateLimitWindow> {
    limits.five_hour.as_ref().or(limits.seven_day.as_ref())
}

fn remaining(resets_at: SystemTime, now: SystemTime) -> Duration {
    resets_at.duration_since(now).unwrap_or_default()
}

/// How long until a countdown shown in whole minutes (rounded up) changes.
fn until_minute_turns(remaining: Duration) -> Duration {
    let into_minute = Duration::from_nanos((remaining.as_nanos() % 60_000_000_000) as u64);
    if into_minute.is_zero() {
        Duration::from_secs(60)
    } else {
        into_minute
    }
}

/// A reset countdown short enough for the title bar: `2h38`, `45m`, `3d4h`, `agora`.
fn compact_reset(remaining: Duration) -> String {
    let minutes = remaining.as_secs().div_ceil(60);
    let days = minutes / (24 * 60);
    let hours = minutes % (24 * 60) / 60;
    let minutes = minutes % 60;
    match (days, hours, minutes) {
        (0, 0, 0) => "agora".to_owned(),
        (0, 0, minutes) => format!("{minutes}m"),
        (0, hours, 0) => format!("{hours}h"),
        (0, hours, minutes) => format!("{hours}h{minutes:02}"),
        (days, 0, _) => format!("{days}d"),
        (days, hours, _) => format!("{days}d{hours}h"),
    }
}

/// A plan window's use, what's left of it and when it starts over, then the conversation's cost
/// when there is one.
fn window_tooltip(title: &str, window: &RateLimitWindow, cost_usd: f64, now: SystemTime) -> String {
    let used = window.utilization_percent;
    let mut line = format!(
        "{title}: {} usado · {} restante",
        format_percent(used),
        format_percent((100. - used.round()).max(0.))
    );
    if let Some(resets_at) = window.resets_at {
        line.push_str(&format!(" · {}", format_reset(remaining(resets_at, now))));
    }
    let mut lines = vec![line];
    if cost_usd > 0. {
        lines.push(format!("Custo nesta conversa: {}", format_cost(cost_usd)));
    }
    lines.push("Clique para ver o uso do plano".to_owned());
    lines.join("\n")
}

/// Shows the first child whose minimum window width the window meets, else `fallback`. It decides
/// at layout, so a window resize, which lays the bar out again without rendering it, still picks.
struct WindowWidthSwitch {
    children: Vec<(f32, Box<dyn Element>)>,
    fallback: Box<dyn Element>,
    active: Option<usize>,
}

impl WindowWidthSwitch {
    fn new(children: Vec<(f32, Box<dyn Element>)>, fallback: Box<dyn Element>) -> Self {
        Self {
            children,
            fallback,
            active: None,
        }
    }

    fn active(&self) -> &dyn Element {
        match self.active.and_then(|index| self.children.get(index)) {
            Some((_, child)) => child.as_ref(),
            None => self.fallback.as_ref(),
        }
    }

    fn active_mut(&mut self) -> &mut dyn Element {
        match self.active.and_then(|index| self.children.get_mut(index)) {
            Some((_, child)) => child.as_mut(),
            None => self.fallback.as_mut(),
        }
    }
}

impl Element for WindowWidthSwitch {
    fn layout(
        &mut self,
        constraint: SizeConstraint,
        ctx: &mut LayoutContext,
        app: &AppContext,
    ) -> Vector2F {
        let window_width = ctx.window_size.x();
        self.active = self
            .children
            .iter()
            .position(|(min_width, _)| window_width >= *min_width);
        self.active_mut().layout(constraint, ctx, app)
    }

    fn after_layout(&mut self, ctx: &mut AfterLayoutContext, app: &AppContext) {
        self.active_mut().after_layout(ctx, app);
    }

    fn paint(&mut self, origin: Vector2F, ctx: &mut PaintContext, app: &AppContext) {
        self.active_mut().paint(origin, ctx, app);
    }

    fn dispatch_event(
        &mut self,
        event: &DispatchedEvent,
        ctx: &mut EventContext,
        app: &AppContext,
    ) -> bool {
        self.active_mut().dispatch_event(event, ctx, app)
    }

    fn size(&self) -> Option<Vector2F> {
        self.active().size()
    }

    fn origin(&self) -> Option<Point> {
        self.active().origin()
    }
}

#[cfg(test)]
#[path = "usage_bar_tests.rs"]
mod tests;
