//! A native web view (WKWebView on macOS) hosted as a child view of a window's content view, used
//! to show interactive local HTML artifacts on top of the GPU-rendered UI.
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
