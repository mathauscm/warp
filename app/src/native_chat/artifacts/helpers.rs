//! Pure helpers of the artifacts gallery: dates in pt-BR, grid math and kind filters.

use std::time::SystemTime;

use chrono::{DateTime, Datelike as _, FixedOffset, Utc};
use claude_chat::artifacts::{ArtifactKind, ArtifactMeta};

const MONTHS_PT: [&str; 12] = [
    "jan.", "fev.", "mar.", "abr.", "mai.", "jun.", "jul.", "ago.", "set.", "out.", "nov.", "dez.",
];

/// When `at` happened, seen from `now` in the `offset` time zone: "agora", "há 5 min", "há 3 h",
/// "ontem", "há 4 dias", "em 23 de out." or, in another year, "em 23 de out. de 2025".
pub fn relative_date_pt(at: DateTime<Utc>, now: DateTime<Utc>, offset: FixedOffset) -> String {
    let elapsed = now.signed_duration_since(at);
    let minutes = elapsed.num_minutes();
    if minutes < 1 {
        return "agora".to_owned();
    }
    if minutes < 60 {
        return format!("há {minutes} min");
    }
    let local_at = at.with_timezone(&offset);
    let local_now = now.with_timezone(&offset);
    let days = local_now
        .date_naive()
        .signed_duration_since(local_at.date_naive())
        .num_days();
    match days {
        0 => format!("há {} h", elapsed.num_hours()),
        1 => "ontem".to_owned(),
        2..=6 => format!("há {days} dias"),
        _ => {
            let month = MONTHS_PT[local_at.month0() as usize];
            let day = local_at.day();
            if local_at.year() == local_now.year() {
                format!("em {day} de {month}")
            } else {
                format!("em {day} de {month} de {}", local_at.year())
            }
        }
    }
}

/// The line under a card's title, like "Atualizado há 5 min".
pub fn card_caption_pt(meta: &ArtifactMeta, now: DateTime<Utc>, offset: FixedOffset) -> String {
    let verb = if meta.version > 1 {
        "Atualizado"
    } else {
        "Criado"
    };
    format!("{verb} {}", relative_date_pt(meta.updated_at, now, offset))
}

/// Gallery tab label for a kind.
pub fn kind_plural_pt(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Document => "Documentos",
        ArtifactKind::Presentation => "Apresentações",
        ArtifactKind::Design => "Design",
        ArtifactKind::Dashboard => "Painéis",
        ArtifactKind::Motion => "Motion",
        ArtifactKind::Other => "Outros",
    }
}

/// The kinds offered in the "Crie algo novo" row.
pub const CREATABLE_KINDS: [ArtifactKind; 5] = [
    ArtifactKind::Document,
    ArtifactKind::Presentation,
    ArtifactKind::Design,
    ArtifactKind::Dashboard,
    ArtifactKind::Motion,
];

/// How many columns fit in `available` width with items at least `min_item` wide, `gap` apart,
/// capped at `max_columns` when given.
pub fn grid_columns(available: f32, min_item: f32, gap: f32, max_columns: Option<usize>) -> usize {
    let fitting = if min_item + gap <= 0. || !available.is_finite() {
        1
    } else {
        ((available + gap) / (min_item + gap)).floor().max(1.) as usize
    };
    max_columns.map_or(fitting, |max| fitting.min(max.max(1)))
}

/// The width of each of `columns` items sharing `available` width, `gap` apart.
pub fn grid_item_width(available: f32, columns: usize, gap: f32) -> f32 {
    let columns = columns.max(1) as f32;
    ((available - gap * (columns - 1.)) / columns).max(0.)
}

/// The tabs to show above the cards: every kind with at least one artifact, in a fixed order.
/// `None` stands for "Todos".
pub fn available_filters(artifacts: &[ArtifactMeta]) -> Vec<ArtifactKind> {
    ArtifactKind::ALL
        .into_iter()
        .filter(|kind| artifacts.iter().any(|meta| meta.kind == *kind))
        .collect()
}

pub fn matches_filter(meta: &ArtifactMeta, filter: Option<ArtifactKind>) -> bool {
    filter.is_none_or(|kind| meta.kind == kind)
}

/// Whether the thumbnail is missing or older than the page it shows.
pub fn needs_thumbnail(thumbnail: Option<SystemTime>, html: Option<SystemTime>) -> bool {
    match (thumbnail, html) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(thumbnail), Some(html)) => thumbnail < html,
    }
}

/// `file://` URL of a local path, with the characters URLs can't hold percent-encoded.
pub fn file_url(path: &std::path::Path) -> String {
    let mut url = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                url.push(byte as char)
            }
            _ => url.push_str(&format!("%{byte:02X}")),
        }
    }
    url
}

#[cfg(test)]
#[path = "helpers_tests.rs"]
mod tests;
