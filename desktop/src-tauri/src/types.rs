//! DTO types shared with the `soyo helper` daemon — re-exported from `soyo-ipc`
//! so the desktop crate keeps its `crate::types::*` paths stable.

pub use soyo_ipc::types::*;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct HelperStatusDto {
    pub connected: bool,
    pub version: Option<String>,
    pub pid: Option<u32>,
    pub active_runs: u32,
    pub active_clients: u32,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, specta::Type)]
pub struct UpdateDesktopPrefsParams {
    pub theme: Option<String>,
    pub font_size: Option<u16>,
    pub conversation_font: Option<String>,
    pub reduce_motion: Option<bool>,
    pub inspector_open: Option<bool>,
}
