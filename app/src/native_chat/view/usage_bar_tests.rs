use super::*;

fn window(percent: f64, resets_in: Option<Duration>, now: SystemTime) -> RateLimitWindow {
    RateLimitWindow {
        utilization_percent: percent,
        resets_at: resets_in.map(|duration| now + duration),
    }
}

#[test]
fn compact_reset_fits_the_title_bar() {
    assert_eq!(
        compact_reset(Duration::from_secs(2 * 3600 + 37 * 60 + 30)),
        "2h38"
    );
    assert_eq!(
        compact_reset(Duration::from_secs(3 * 3600 + 5 * 60)),
        "3h05"
    );
    assert_eq!(compact_reset(Duration::from_secs(2 * 3600)), "2h");
    assert_eq!(compact_reset(Duration::from_secs(45 * 60)), "45m");
    assert_eq!(compact_reset(Duration::from_secs(20)), "1m");
    assert_eq!(
        compact_reset(Duration::from_secs(4 * 86_400 + 2 * 3600 + 30)),
        "4d2h"
    );
    assert_eq!(compact_reset(Duration::from_secs(86_400)), "1d");
    assert_eq!(compact_reset(Duration::ZERO), "agora");
}

#[test]
fn headline_prefers_the_five_hour_window() {
    let now = SystemTime::now();
    let limits = RateLimits {
        five_hour: Some(window(32., None, now)),
        seven_day: Some(window(12., None, now)),
        status: None,
    };
    assert_eq!(headline_window(&limits).unwrap().utilization_percent, 32.);

    let weekly_only = RateLimits {
        five_hour: None,
        ..limits
    };
    assert_eq!(
        headline_window(&weekly_only).unwrap().utilization_percent,
        12.
    );

    assert!(headline_window(&RateLimits::default()).is_none());
}

#[test]
fn window_tooltip_shows_use_what_is_left_reset_and_cost() {
    let now = SystemTime::now();
    let five_hour = window(32., Some(Duration::from_secs(3 * 3600 + 10 * 60)), now);
    assert_eq!(
        window_tooltip("Sessão de 5 horas", &five_hour, 0.034, now),
        "Sessão de 5 horas: 32% usado · 68% restante · reinicia em 3h 10m\n\
         Custo nesta conversa: US$ 0,03\n\
         Clique para ver o uso do plano"
    );
    let weekly = window(23.4, Some(Duration::from_secs(3 * 86_400 + 4 * 3600)), now);
    assert_eq!(
        window_tooltip("Semana", &weekly, 0., now),
        "Semana: 23% usado · 77% restante · reinicia em 3d 4h\n\
         Clique para ver o uso do plano"
    );
}

#[test]
fn window_tooltip_skips_a_missing_reset_and_never_goes_below_zero_left() {
    let now = SystemTime::now();
    assert_eq!(
        window_tooltip("Semana", &window(0.3, None, now), 0., now),
        "Semana: <1% usado · 100% restante\nClique para ver o uso do plano"
    );
    assert_eq!(
        window_tooltip("Semana", &window(104., None, now), 0., now),
        "Semana: 100% usado · 0% restante\nClique para ver o uso do plano"
    );
}

#[test]
fn remaining_is_zero_once_the_reset_has_passed() {
    let now = SystemTime::now();
    assert_eq!(remaining(now - Duration::from_secs(5), now), Duration::ZERO);
    assert_eq!(
        remaining(now + Duration::from_secs(90), now),
        Duration::from_secs(90)
    );
    assert_eq!(
        format_reset(remaining(
            now + Duration::from_secs(3 * 3600 + 10 * 60),
            now
        )),
        "reinicia em 3h 10m"
    );
    assert_eq!(
        format_reset(remaining(now - Duration::from_secs(5), now)),
        "reinicia em instantes"
    );
}

#[test]
fn countdown_ticks_when_the_rounded_minute_changes() {
    // 3m 20s reads as 4m until 3m flat, 20s later.
    assert_eq!(
        until_minute_turns(Duration::from_secs(200)),
        Duration::from_secs(20)
    );
    assert_eq!(
        until_minute_turns(Duration::from_millis(60_500)),
        Duration::from_millis(500)
    );
    assert_eq!(
        until_minute_turns(Duration::from_secs(120)),
        Duration::from_secs(60)
    );
}

#[test]
fn ring_arc_rounds_to_five_percent_steps() {
    use crate::native_chat::view::side_panel::widgets::ring_arc;

    assert_eq!(ring_arc(0.), None);
    assert_eq!(ring_arc(-0.2), None);
    assert_eq!(ring_arc(f64::NAN), None);
    assert_eq!(ring_arc(0.001), Some("bundled/svg/picolas-ring-005.svg"));
    assert_eq!(ring_arc(0.45), Some("bundled/svg/picolas-ring-045.svg"));
    assert_eq!(ring_arc(0.46), Some("bundled/svg/picolas-ring-045.svg"));
    assert_eq!(ring_arc(1.), Some("bundled/svg/picolas-ring-100.svg"));
    assert_eq!(ring_arc(3.), Some("bundled/svg/picolas-ring-100.svg"));
}
