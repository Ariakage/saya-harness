//! Terminal frontend for Saya's shared application services.
pub mod commands;
pub mod input;
pub mod terminal;

mod controller;
pub mod editor;
mod rich_text;
mod theme;
pub mod view;
pub use controller::{Options, run};
