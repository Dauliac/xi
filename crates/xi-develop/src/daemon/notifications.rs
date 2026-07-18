//! Per-consumer notification queue.
//!
//! Each shell terminal (consumer) has an independent cursor.
//! Multiple terminals viewing the same devshell each get their own copy
//! of every notification.

use std::collections::HashMap;

use super::protocol::Notification;

const DEFAULT_CAPACITY: usize = 256;

/// Notification queue with per-consumer cursors (cimera pattern).
pub struct NotificationQueue {
  messages: Vec<Notification>,
  cursors: HashMap<u32, usize>,
  capacity: usize,
}

impl NotificationQueue {
  #[must_use]
  pub fn new() -> Self {
    Self {
      messages: Vec::new(),
      cursors: HashMap::new(),
      capacity: DEFAULT_CAPACITY,
    }
  }

  /// Push a notification. All consumers will see it on next drain.
  pub fn push(&mut self, notification: Notification) {
    if self.messages.len() >= self.capacity {
      let drain_count = self.capacity / 2;
      self.messages.drain(..drain_count);
      for cursor in self.cursors.values_mut() {
        *cursor = cursor.saturating_sub(drain_count);
      }
    }
    self.messages.push(notification);
  }

  /// Drain pending notifications for a consumer.
  /// Returns all messages since this consumer's last drain.
  pub fn drain_for(&mut self, consumer_pid: u32) -> Vec<Notification> {
    let cursor = self.cursors.entry(consumer_pid).or_insert(0);
    let start = (*cursor).min(self.messages.len());
    let pending = self.messages[start..].to_vec();
    *cursor = self.messages.len();
    pending
  }

  /// Register a consumer (idempotent).
  pub fn register(&mut self, consumer_pid: u32) {
    self
      .cursors
      .entry(consumer_pid)
      .or_insert(self.messages.len());
  }

  /// Remove cursors for dead processes.
  pub fn reap_dead_consumers(&mut self) {
    self.cursors.retain(|pid, _| is_process_alive(*pid));
  }

  /// Number of active consumers.
  #[must_use]
  pub fn consumer_count(&self) -> usize {
    self.cursors.len()
  }
}

impl Default for NotificationQueue {
  fn default() -> Self {
    Self::new()
  }
}

#[cfg(target_os = "linux")]
fn is_process_alive(pid: u32) -> bool {
  std::path::Path::new(&format!("/proc/{pid}/stat")).exists()
}

#[cfg(all(unix, not(target_os = "linux")))]
fn is_process_alive(pid: u32) -> bool {
  std::process::Command::new("kill")
    .args(["-0", &pid.to_string()])
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .status()
    .is_ok_and(|s| s.success())
}

#[cfg(not(unix))]
fn is_process_alive(_pid: u32) -> bool {
  true
}

#[cfg(test)]
mod tests {
  use super::*;

  fn notif(msg: &str) -> Notification {
    Notification::info(msg)
  }

  #[test]
  fn empty_queue_drains_nothing() {
    let mut q = NotificationQueue::new();
    assert!(q.drain_for(1).is_empty());
  }

  #[test]
  fn push_and_drain() {
    let mut q = NotificationQueue::new();
    q.push(notif("hello"));
    q.push(notif("world"));

    let msgs = q.drain_for(1);
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0].message, "hello");
    assert_eq!(msgs[1].message, "world");
  }

  #[test]
  fn drain_advances_cursor() {
    let mut q = NotificationQueue::new();
    q.push(notif("first"));
    let _ = q.drain_for(1);
    q.push(notif("second"));

    let msgs = q.drain_for(1);
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].message, "second");
  }

  #[test]
  fn per_consumer_isolation() {
    let mut q = NotificationQueue::new();
    q.push(notif("shared"));

    let msgs_a = q.drain_for(1);
    let msgs_b = q.drain_for(2);

    // Both consumers see the message
    assert_eq!(msgs_a.len(), 1);
    assert_eq!(msgs_b.len(), 1);
  }

  #[test]
  fn register_skips_old_messages() {
    let mut q = NotificationQueue::new();
    q.push(notif("old"));
    q.register(1); // registers AFTER the push
    q.push(notif("new"));

    let msgs = q.drain_for(1);
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].message, "new");
  }

  #[test]
  fn capacity_overflow_drains_half() {
    let mut q = NotificationQueue {
      messages: Vec::new(),
      cursors: HashMap::new(),
      capacity: 4,
    };

    q.push(notif("1"));
    q.push(notif("2"));
    q.push(notif("3"));
    q.push(notif("4"));
    // This push triggers drain of bottom 2
    q.push(notif("5"));

    assert_eq!(q.messages.len(), 3); // 3,4,5 remain
    assert_eq!(q.messages[0].message, "3");
  }
}
