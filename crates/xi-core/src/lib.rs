pub mod args;
pub mod cache;
pub mod cache_match;
pub mod cache_posthook;
pub mod cache_queue;
pub mod cache_registry;
pub mod checks;
pub mod cli_help;
pub mod command;
pub mod complete;
pub mod dirs;
pub mod flake_output;
pub mod installable;
pub mod nix_value;
pub mod override_input;
pub mod progress;
pub mod retry;
pub mod sources;
pub mod style;
pub mod suggest;
pub mod update;
pub mod util;

#[cfg(test)] pub(crate) mod test_utils;

pub const XI_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const XI_REV: Option<&str> = option_env!("XI_REV");
