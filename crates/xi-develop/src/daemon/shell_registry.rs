//! Per-consumer shell instance tracking.
//!
//! The daemon tracks each connected shell (identified by PID) to know:
//! - Which env/hook generation each shell has sourced
//! - Whether a shell needs to re-source after a daemon eval
//! - Parent-child relationships for nested devshells
//! - Consumer count for daemon lifecycle (auto-shutdown when 0)

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

fn now_secs() -> u64 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or_default()
    .as_secs()
}

/// Per-shell-instance state tracked by the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellInstance {
  pub pid: u32,
  pub parent_pid: Option<u32>,
  pub flake_id: String,
  pub target: String,
  pub last_env_gen: u64,
  pub last_hook_gen: u64,
  pub registered_at: u64, // unix timestamp
}

/// Registry of active shell consumers.
pub struct ShellRegistry {
  instances: HashMap<u32, ShellInstance>,
}

impl ShellRegistry {
  #[must_use]
  pub fn new() -> Self {
    Self {
      instances: HashMap::new(),
    }
  }

  pub fn register(
    &mut self,
    pid: u32,
    parent_pid: Option<u32>,
    flake_id: &str,
    target: &str,
  ) {
    self.instances.entry(pid).or_insert_with(|| ShellInstance {
      pid,
      parent_pid,
      flake_id: flake_id.to_string(),
      target: target.to_string(),
      last_env_gen: 0,
      last_hook_gen: 0,
      registered_at: now_secs(),
    });
  }

  pub fn deregister(&mut self, pid: u32) -> bool {
    self.instances.remove(&pid).is_some()
  }

  #[must_use]
  pub fn get(&self, pid: u32) -> Option<&ShellInstance> {
    self.instances.get(&pid)
  }

  #[must_use]
  pub fn consumer_count(&self) -> usize {
    self.instances.len()
  }

  #[must_use]
  pub fn should_source_env(&self, pid: u32, current_gen: u64) -> bool {
    self
      .instances
      .get(&pid)
      .is_some_and(|i| i.last_env_gen < current_gen)
  }

  #[must_use]
  pub fn should_source_hook(&self, pid: u32, current_hook_gen: u64) -> bool {
    self
      .instances
      .get(&pid)
      .is_some_and(|i| i.last_hook_gen < current_hook_gen)
  }

  pub fn mark_sourced_env(&mut self, pid: u32, generation: u64) {
    if let Some(inst) = self.instances.get_mut(&pid) {
      inst.last_env_gen = generation;
    }
  }

  pub fn mark_sourced_hook(&mut self, pid: u32, generation: u64) {
    if let Some(inst) = self.instances.get_mut(&pid) {
      inst.last_hook_gen = generation;
    }
  }
}

impl Default for ShellRegistry {
  fn default() -> Self {
    Self::new()
  }
}
