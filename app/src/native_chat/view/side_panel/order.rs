//! The order of the side panel's cards, which the user picks by dragging them and which every
//! chat shares.

use std::path::PathBuf;

use serde_json::json;

use super::SidePanelSection;

const FILE_NAME: &str = "picolas_chat_panel.json";

pub(super) const DEFAULT_ORDER: [SidePanelSection; 4] = [
    SidePanelSection::Environment,
    SidePanelSection::Context,
    SidePanelSection::Usage,
    SidePanelSection::Connectors,
];

fn key(section: SidePanelSection) -> &'static str {
    match section {
        SidePanelSection::Environment => "environment",
        SidePanelSection::Context => "context",
        SidePanelSection::Usage => "usage",
        SidePanelSection::Connectors => "connectors",
    }
}

fn from_key(key: &str) -> Option<SidePanelSection> {
    DEFAULT_ORDER
        .into_iter()
        .find(|section| self::key(*section) == key)
}

/// The order saved in `text`: known cards once each, then any card it leaves out in the default
/// order.
pub(super) fn parse_order(text: &str) -> Vec<SidePanelSection> {
    let mut order: Vec<SidePanelSection> = serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|value| {
            value.get("order")?.as_array().map(|keys| {
                keys.iter()
                    .filter_map(|key| from_key(key.as_str()?))
                    .collect()
            })
        })
        .unwrap_or_default();
    let mut seen = Vec::new();
    order.retain(|section| {
        let is_new = !seen.contains(section);
        seen.push(*section);
        is_new
    });
    for section in DEFAULT_ORDER {
        if !order.contains(&section) {
            order.push(section);
        }
    }
    order
}

pub(super) fn serialize_order(order: &[SidePanelSection]) -> String {
    let keys: Vec<&str> = order.iter().map(|section| key(*section)).collect();
    json!({ "order": keys }).to_string()
}

fn path() -> PathBuf {
    warp_core::paths::data_dir().join(FILE_NAME)
}

pub(super) fn load() -> Vec<SidePanelSection> {
    std::fs::read_to_string(path())
        .map(|text| parse_order(&text))
        .unwrap_or_else(|_| DEFAULT_ORDER.to_vec())
}

pub(super) fn save(order: &[SidePanelSection]) {
    let text = serialize_order(order);
    if let Err(err) = std::fs::write(path(), text) {
        log::warn!("Couldn't save the chat panel's card order: {err}");
    }
}
