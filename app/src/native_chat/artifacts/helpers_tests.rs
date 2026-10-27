use std::path::Path;
use std::time::{Duration, SystemTime};

use chrono::{DateTime, FixedOffset, TimeZone as _, Utc};
use claude_chat::artifacts::{ArtifactKind, ArtifactMeta};

use super::*;

fn brt() -> FixedOffset {
    FixedOffset::west_opt(3 * 3600).unwrap()
}

fn utc(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(year, month, day, hour, minute, 0)
        .unwrap()
}

fn meta(id: &str, kind: ArtifactKind, version: u32) -> ArtifactMeta {
    ArtifactMeta {
        id: id.to_owned(),
        title: id.to_owned(),
        kind,
        description: None,
        created_at: utc(2026, 10, 20, 12, 0),
        updated_at: utc(2026, 10, 26, 14, 0),
        session_id: None,
        cwd: None,
        version,
    }
}

#[test]
fn relative_dates_in_portuguese() {
    // 15:00 in São Paulo.
    let now = utc(2026, 10, 26, 18, 0);
    let cases = [
        (utc(2026, 10, 26, 18, 0), "agora"),
        (utc(2026, 10, 26, 17, 59), "há 1 min"),
        (utc(2026, 10, 26, 17, 55), "há 5 min"),
        (utc(2026, 10, 26, 15, 0), "há 3 h"),
        // 23:30 the day before, local time, though fewer than 24 hours ago.
        (utc(2026, 10, 26, 2, 30), "ontem"),
        (utc(2026, 10, 23, 12, 0), "há 3 dias"),
        (utc(2026, 10, 2, 12, 0), "em 2 de out."),
        (utc(2025, 5, 9, 12, 0), "em 9 de mai. de 2025"),
    ];
    for (at, expected) in cases {
        assert_eq!(relative_date_pt(at, now, brt()), expected, "{at}");
    }
}

#[test]
fn local_midnight_decides_between_hours_and_yesterday() {
    // 00:30 local; 23:00 the previous day is only 90 minutes earlier but already "ontem".
    let now = utc(2026, 10, 26, 3, 30);
    assert_eq!(
        relative_date_pt(utc(2026, 10, 26, 3, 0), now, brt()),
        "há 30 min"
    );
    assert_eq!(
        relative_date_pt(utc(2026, 10, 26, 2, 0), now, brt()),
        "ontem"
    );
}

#[test]
fn caption_says_created_or_updated() {
    let now = utc(2026, 10, 26, 14, 10);
    assert_eq!(
        card_caption_pt(&meta("a", ArtifactKind::Document, 1), now, brt()),
        "Criado há 10 min"
    );
    assert_eq!(
        card_caption_pt(&meta("a", ArtifactKind::Document, 3), now, brt()),
        "Atualizado há 10 min"
    );
}

#[test]
fn grid_fits_as_many_columns_as_the_minimum_width_allows() {
    assert_eq!(grid_columns(1000., 300., 20., None), 3);
    assert_eq!(grid_columns(940., 300., 20., None), 3);
    assert_eq!(grid_columns(939., 300., 20., None), 2);
    assert_eq!(grid_columns(100., 300., 20., None), 1);
    assert_eq!(grid_columns(5000., 100., 10., Some(5)), 5);
    assert_eq!(grid_columns(f32::INFINITY, 100., 10., None), 1);
}

#[test]
fn grid_items_share_the_width_minus_gaps() {
    assert_eq!(grid_item_width(940., 3, 20.), 300.);
    assert_eq!(grid_item_width(500., 1, 20.), 500.);
    assert_eq!(grid_item_width(10., 3, 20.), 0.);
}

#[test]
fn filters_list_only_kinds_present_in_order() {
    let artifacts = [
        meta("a", ArtifactKind::Dashboard, 1),
        meta("b", ArtifactKind::Document, 1),
        meta("c", ArtifactKind::Dashboard, 1),
    ];
    assert_eq!(
        available_filters(&artifacts),
        vec![ArtifactKind::Document, ArtifactKind::Dashboard]
    );
    assert!(matches_filter(&artifacts[0], None));
    assert!(matches_filter(&artifacts[0], Some(ArtifactKind::Dashboard)));
    assert!(!matches_filter(
        &artifacts[1],
        Some(ArtifactKind::Dashboard)
    ));
}

#[test]
fn thumbnail_is_needed_when_missing_or_stale() {
    let earlier = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
    let later = earlier + Duration::from_secs(5);
    assert!(needs_thumbnail(None, Some(later)));
    assert!(needs_thumbnail(Some(earlier), Some(later)));
    assert!(!needs_thumbnail(Some(later), Some(earlier)));
    assert!(!needs_thumbnail(Some(later), None));
}

#[test]
fn file_urls_escape_spaces_and_accents() {
    assert_eq!(
        file_url(Path::new("/Users/me/Área de trabalho/a.html")),
        "file:///Users/me/%C3%81rea%20de%20trabalho/a.html"
    );
}
