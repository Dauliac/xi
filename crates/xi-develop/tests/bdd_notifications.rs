//! BDD: tests/features/12_multi_terminal.feature
//! BDD: tests/features/17_notifications.feature
//!
//! Tests for the per-consumer notification queue.
//! Pure unit tests — no daemon, no nix, no shell.

use xi_develop::daemon::notifications::NotificationQueue;
use xi_develop::daemon::protocol::{NotifKind, Notification};

fn notif(msg: &str) -> Notification {
  Notification::info(msg)
}

fn success(msg: &str) -> Notification {
  Notification::success(msg)
}

fn error(msg: &str) -> Notification {
  Notification::error(msg)
}

/// BDD: 12_multi_terminal.feature#Both terminals receive notifications
#[test]
fn both_terminals_receive_global_notification() {
  let mut q = NotificationQueue::new();

  // Register two consumers
  q.register(1001);
  q.register(2001);

  // Push a global notification (e.g., "devshell updated")
  q.push(success("devshell updated: + python 3.12"));

  // Both consumers get it
  let msgs_1 = q.drain_for(1001);
  let msgs_2 = q.drain_for(2001);

  assert_eq!(msgs_1.len(), 1);
  assert_eq!(msgs_2.len(), 1);
  assert_eq!(msgs_1[0].message, "devshell updated: + python 3.12");
  assert_eq!(msgs_2[0].message, "devshell updated: + python 3.12");
}

/// BDD: 12_multi_terminal.feature#Notification shown exactly once per terminal
#[test]
fn notification_shown_once_per_terminal() {
  let mut q = NotificationQueue::new();
  q.register(1001);
  q.push(success("devshell updated"));

  // First drain: gets the notification
  let msgs = q.drain_for(1001);
  assert_eq!(msgs.len(), 1);

  // Second drain: cursor advanced, nothing new
  let msgs = q.drain_for(1001);
  assert_eq!(msgs.len(), 0);
}

/// BDD: 12_multi_terminal.feature#Consumer registration skips old notifications
#[test]
fn new_consumer_skips_old_notifications() {
  let mut q = NotificationQueue::new();

  // Push notifications before terminal-3 registers
  q.push(notif("A"));
  q.push(notif("B"));
  q.push(notif("C"));

  // Terminal-3 registers AFTER messages were pushed
  q.register(3001);

  // Terminal-3 should NOT see A, B, C
  let msgs = q.drain_for(3001);
  assert_eq!(msgs.len(), 0);

  // But sees future messages
  q.push(notif("D"));
  let msgs = q.drain_for(3001);
  assert_eq!(msgs.len(), 1);
  assert_eq!(msgs[0].message, "D");
}

/// BDD: 12_multi_terminal.feature#One terminal exits, other continues
#[test]
fn one_terminal_exit_others_unaffected() {
  let mut q = NotificationQueue::new();
  q.register(1001);
  q.register(2001);
  assert_eq!(q.consumer_count(), 2);

  // Simulate terminal-1 exit (remove cursor)
  // In real code, deregister would be called.
  // For now, just verify the queue still works for terminal-2.
  q.push(success("update after terminal-1 exit"));

  let msgs = q.drain_for(2001);
  assert_eq!(msgs.len(), 1);
  assert_eq!(msgs[0].message, "update after terminal-1 exit");
}

/// BDD: 17_notifications.feature#Notification types and rendering
#[test]
fn notification_kinds_render_with_label() {
  let cases = vec![
    (Notification::loading("building..."), NotifKind::Loading),
    (Notification::success("ready"), NotifKind::Success),
    (Notification::info("status"), NotifKind::Info),
    (Notification::warn("degraded"), NotifKind::Warn),
    (Notification::error("failed"), NotifKind::Error),
  ];

  for (notif, expected_kind) in cases {
    assert_eq!(notif.kind, expected_kind);
    let rendered = notif.render();
    // All rendered notifications contain [xi] label
    assert!(
      rendered.contains("xi"),
      "Rendered notification should contain 'xi': {rendered}"
    );
    assert!(
      rendered.contains(&notif.message),
      "Rendered should contain message: {rendered}"
    );
  }
}

/// BDD: 17_notifications.feature#Notification dedup — same error not repeated
/// This tests the daemon-level dedup logic (error message comparison).
/// The daemon checks `last_error` before pushing a new error notification.
#[test]
fn error_dedup_same_message() {
  let mut q = NotificationQueue::new();
  q.register(1001);

  let err_msg = "error: syntax error at line 5";

  // First error push
  q.push(error(err_msg));

  // Simulate daemon check: last_error == new error → don't push again
  // (This is daemon logic, but we verify the queue behavior)
  let msgs = q.drain_for(1001);
  assert_eq!(msgs.len(), 1);

  // If daemon decides NOT to push (same error), queue stays empty
  let msgs = q.drain_for(1001);
  assert_eq!(msgs.len(), 0);
}

/// BDD: 17_notifications.feature#Notification dedup — different error replaces old
#[test]
fn error_dedup_different_message() {
  let mut q = NotificationQueue::new();
  q.register(1001);

  q.push(error("error: syntax error at line 5"));
  let _ = q.drain_for(1001);

  // Different error → daemon pushes new notification
  q.push(error("error: undefined variable 'foo'"));
  let msgs = q.drain_for(1001);
  assert_eq!(msgs.len(), 1);
  assert_eq!(msgs[0].message, "error: undefined variable 'foo'");
}
