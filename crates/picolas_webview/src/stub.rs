use std::path::Path;

use anyhow::{Result, anyhow};

/// Placeholder for the native parent view type; it cannot be constructed on this platform.
pub enum ParentView {}

/// No-op web view for platforms without a native implementation.
pub struct WebView {
    _not_send: std::marker::PhantomData<*const ()>,
}

impl WebView {
    pub fn new(parent: &ParentView) -> Result<Self> {
        match *parent {}
    }

    pub fn set_frame(&self, _x: f32, _y: f32, _width: f32, _height: f32, _parent_height: f32) {}

    pub fn set_visible(&self, _visible: bool) {}

    pub fn set_background(&self, _r: f32, _g: f32, _b: f32, _a: f32) {}

    pub fn set_dark_appearance(&self, _dark: bool) {}

    pub fn load_file(&self, _html_path: &Path, _read_access_dir: &Path) {}

    pub fn load_html(&self, _html: &str, _base_url: Option<&Path>) {}

    pub fn reload(&self) {}

    pub fn evaluate_js(&self, _script: &str) {}

    pub fn snapshot_png(&self, _out_path: &Path, done: impl FnOnce(Result<()>) + 'static) {
        done(Err(anyhow!(
            "web view snapshots are not supported on this platform"
        )));
    }

    pub fn remove(&self) {}
}
