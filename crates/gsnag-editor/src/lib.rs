//! Non-destructive GTK4/libadwaita annotation editor and portable .gsnag projects.

mod canvas;
pub mod model;
pub mod render;
pub mod storage;
mod ui;

pub use ui::open;
