//! Project tree viewer — the left panel showing workspace files and MCU selection.

pub mod clipboard;
pub mod extract_crate;
pub mod gui;
pub mod logic;
pub mod move_module;
pub mod templates;

pub use gui::show_project_tree;
pub use logic::ProjectTreeState;
