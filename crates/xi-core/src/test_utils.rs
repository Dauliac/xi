/// Test utilities shared across xi-core test modules.
///
/// Only compiled under `#[cfg(test)]`.

/// RAII guard that sets an environment variable for the duration of a test
/// and restores the original value on drop.
pub struct EnvGuard {
  key: String,
  original: Option<String>,
}

impl EnvGuard {
  pub fn new(key: &str, value: &str) -> Self {
    let original = std::env::var(key).ok();
    unsafe {
      std::env::set_var(key, value);
    }
    Self {
      key: key.to_string(),
      original,
    }
  }
}

impl Drop for EnvGuard {
  fn drop(&mut self) {
    unsafe {
      match &self.original {
        Some(val) => std::env::set_var(&self.key, val),
        None => std::env::remove_var(&self.key),
      }
    }
  }
}
