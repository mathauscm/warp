//! Drives the Claude Code CLI headlessly (stream-json over stdio) and models its conversations
//! for a native chat UI. Free of UI code so it builds and tests quickly.

pub mod artifacts;
pub mod history;
pub mod presentation;
pub mod process;
pub mod protocol;
pub mod reducer;
pub mod timeline;
