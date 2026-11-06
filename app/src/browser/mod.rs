//! Picolas Code's in-app browser ("Navegador"): web pages in browser tabs, and the project's dev
//! servers from `.claude/launch.json`, started with their output in a log panel.

pub mod launch;
pub mod server;
mod tabs;
pub mod url;
mod view;

use std::path::PathBuf;

pub use view::{BrowserView, BrowserViewEvent};

/// The folders whose dev servers the browser's start page lists, the first one being where the
/// browser was opened from (like the focused chat's folder).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BrowserContext {
    pub dirs: Vec<PathBuf>,
}

/// `PICOLAS_OPEN_BROWSER=<url>` (or `=start` for the start page) opens the browser when the
/// first window opens, beside the chat of `PICOLAS_OPEN_CHAT` when there is one;
/// `PICOLAS_BROWSER_DIR=<folder>` lists that folder's dev servers first and
/// `PICOLAS_BROWSER_START=<server name>` starts one of them. Debug builds only.
pub fn take_dev_open_browser_action() -> Option<crate::workspace::WorkspaceAction> {
    use std::sync::atomic::{AtomicBool, Ordering};

    static TAKEN: AtomicBool = AtomicBool::new(false);
    if !cfg!(debug_assertions) || TAKEN.swap(true, Ordering::Relaxed) {
        return None;
    }
    let spec = std::env::var("PICOLAS_OPEN_BROWSER").ok()?;
    Some(crate::workspace::WorkspaceAction::OpenBrowser {
        url: (spec != "start").then_some(spec),
    })
}

/// The folder `PICOLAS_BROWSER_DIR` names. Debug builds only.
pub fn dev_browser_dir() -> Option<PathBuf> {
    if !cfg!(debug_assertions) {
        return None;
    }
    std::env::var_os("PICOLAS_BROWSER_DIR").map(PathBuf::from)
}
