//! A native web view (WKWebView on macOS) hosted as a child view of a window's content view, used
//! to show interactive local HTML artifacts and web pages on top of the GPU-rendered UI.
//!
//! On platforms without an implementation, [`WebView::new`] returns an error and every other
//! method is a no-op.

#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
pub use mac::{ParentView, WebView};

#[cfg(not(target_os = "macos"))]
mod stub;
#[cfg(not(target_os = "macos"))]
pub use stub::{ParentView, WebView};

/// What a browsing web view (see `WebView::new_browser`) reports. Delivered synchronously from
/// AppKit callbacks, possibly in the middle of a call into the web view, so handlers should only
/// queue the event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebViewEvent {
    /// The page's URL, title, history or loading state changed; read it with
    /// `WebView::navigation_state`.
    StateChanged,
    /// A page failed to load.
    LoadFailed {
        url: Option<String>,
        message: String,
    },
}

/// Where a browsing web view stands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NavigationState {
    pub url: Option<String>,
    pub title: Option<String>,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub loading: bool,
}
