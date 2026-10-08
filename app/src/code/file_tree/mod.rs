//! File picker component for rendering expandable folder structures.

#[cfg_attr(not(feature = "local_fs"), allow(dead_code))]
mod git_status;
pub mod snapshot;

#[cfg_attr(not(feature = "local_fs"), allow(dead_code, unused_imports))]
mod view;

pub use view::*;
