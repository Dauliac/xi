pub mod args;
pub mod cache;
pub mod cache_queue;
pub mod checks;
pub mod command;
pub mod complete;
pub mod installable;
pub mod progress;
pub mod style;
pub mod suggest;
pub mod update;
pub mod util;

pub const XI_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const XI_REV: Option<&str> = option_env!("XI_REV");
