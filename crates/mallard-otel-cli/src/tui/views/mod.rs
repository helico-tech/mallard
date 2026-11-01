//! The five views. Each adds `draw_*`, `key_*` and `refresh_*` methods to
//! [`super::Embedded`] and keeps its data in a `State`.

pub mod logs;
pub mod metrics;
pub mod overview;
pub mod status;
pub mod traces;
