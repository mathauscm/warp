//! "Uso do plano": the subscription's 5-hour and weekly limits, and this session's cost.

use std::time::SystemTime;

use claude_chat::reducer::RateLimitWindow;
use pathfinder_color::ColorU;
use warpui::AppContext;
use warpui::elements::{
    Container, CrossAxisAlignment, Element, Flex, MainAxisAlignment, MainAxisSize, ParentElement,
};

use super::format::{format_cost, format_percent, format_reset};
use super::widgets::{self, Look, ROW_PADDING_X};
use super::{SidePanelAction, SidePanelSection, SidePanelView};

const BAR_HEIGHT: f32 = 6.;
const WINDOW_SPACING: f32 = 14.;
const LIMIT_WARNING_PERCENT: f64 = 75.;

impl SidePanelView {
    pub(super) fn render_usage(&self, look: Look<'_>, app: &AppContext) -> Box<dyn Element> {
        let palette = look.palette;
        let section = SidePanelSection::Usage;
        let is_expanded = self.is_expanded(section);
        let conversation = self.session.as_ref(app).conversation();
        let limits = conversation.rate_limits();
        let windows = [
            ("Sessão atual", limits.five_hour.as_ref()),
            ("Semana", limits.seven_day.as_ref()),
        ];

        let summary = limits
            .five_hour
            .as_ref()
            .filter(|_| !is_expanded)
            .map(|window| {
                let percent = window.utilization_percent;
                Flex::row()
                    .with_main_axis_size(MainAxisSize::Min)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(8.)
                    .with_child(look.meta(format_percent(percent), palette.text_muted.0))
                    .with_child(widgets::mini_bar(
                        percent / 100.,
                        limit_color(percent, look),
                        palette.surface_strong.0,
                    ))
                    .finish()
            });
        let mut children = vec![widgets::card_header(
            "Uso do plano",
            is_expanded,
            summary,
            self.mouse("header:usage"),
            SidePanelAction::ToggleSection(section),
            look,
        )];
        if !is_expanded {
            return widgets::card(children);
        }

        let now = SystemTime::now();
        let mut body = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(WINDOW_SPACING);
        let mut has_any = false;
        for (title, window) in windows {
            if let Some(window) = window {
                has_any = true;
                body.add_child(render_window(title, window, now, look));
            }
        }
        let cost = conversation.total_cost_usd();
        if cost > 0. {
            has_any = true;
            body.add_child(spread(
                look.meta("Custo nesta sessão", palette.text_muted.0),
                look.meta(format_cost(cost), palette.text.0),
            ));
        }
        if !has_any {
            body.add_child(look.meta("Sem dados de uso para esta conta.", palette.text_faint.0));
        }
        children.push(
            Container::new(body.finish())
                .with_horizontal_padding(ROW_PADDING_X)
                .with_padding_top(2.)
                .with_padding_bottom(8.)
                .finish(),
        );
        widgets::card(children)
    }
}

fn render_window(
    title: &'static str,
    window: &RateLimitWindow,
    now: SystemTime,
    look: Look<'_>,
) -> Box<dyn Element> {
    let palette = look.palette;
    let percent = window.utilization_percent;
    let mut column = Flex::column()
        .with_main_axis_size(MainAxisSize::Min)
        .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
        .with_spacing(6.)
        .with_child(spread(
            look.label(title, palette.text.0),
            look.meta(
                format!("{} usado", format_percent(percent)),
                palette.text_muted.0,
            ),
        ))
        .with_child(widgets::progress_bar(
            percent / 100.,
            limit_color(percent, look),
            palette.surface_strong.0,
            BAR_HEIGHT,
        ));
    if let Some(resets_at) = window.resets_at {
        let remaining = resets_at.duration_since(now).unwrap_or_default();
        column.add_child(look.meta(format_reset(remaining), palette.text_faint.0));
    }
    column.finish()
}

fn limit_color(percent: f64, look: Look<'_>) -> ColorU {
    widgets::meter_color(
        percent,
        look.palette.info.0,
        LIMIT_WARNING_PERCENT,
        look.palette,
    )
}

/// `left` at the start of the row and `right` at its end.
fn spread(left: Box<dyn Element>, right: Box<dyn Element>) -> Box<dyn Element> {
    Flex::row()
        .with_main_axis_size(MainAxisSize::Max)
        .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_child(left)
        .with_child(right)
        .finish()
}
