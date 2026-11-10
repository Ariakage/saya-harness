//! Terminal frontend for Saya's shared application services.
pub mod commands;
pub mod input;
#[cfg(debug_assertions)]
pub mod preview;
pub mod terminal;

mod controller;
pub mod editor;
pub(crate) mod i18n;
mod rich_text;
mod theme;
pub(crate) mod tool_render;
pub(crate) mod tool_summary;
pub mod view;
pub use controller::{Options, run};
