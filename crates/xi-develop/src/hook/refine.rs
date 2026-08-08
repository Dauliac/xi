//! Between-tick queue refinement.
//!
//! Thin wrapper around [`Queue::load`], [`Queue::refine`], and
//! [`Queue::atomic_rewrite`]: load the persisted queue, collapse duplicates
//! and drop stale entries, atomically write the refined list back, return
//! the list to the caller so it can dispatch entries in FIFO order under
//! the tick's remaining budget.

use color_eyre::Result;

use super::queue::{Queue, QueueEntry};

/// Load, refine, rewrite, return.
///
/// The returned vec is FIFO-ordered by `created_at` ascending — the caller
/// dispatches in that order and stops when the tick's budget is exceeded.
///
/// # Errors
///
/// Returns an error if loading the queue file or the atomic rewrite fails.
/// Queue-file corruption is *not* an error here: it is quarantined by
/// [`Queue::load`] and yields an empty list.
pub fn refine_and_flush(queue: &Queue) -> Result<Vec<QueueEntry>> {
  let loaded = queue.load()?;
  let refined = Queue::refine(loaded.clone());
  // Skip the rewrite when refinement is a no-op: the `.tmp` create + rename
  // costs a couple ms even without fsync, and the common case on a healthy
  // daemon is a queue that was already clean. Compare by id; entry order is
  // preserved by `refine` under a no-op.
  if !same_entries(&loaded, &refined) {
    queue.atomic_rewrite(&refined)?;
  }
  Ok(refined)
}

fn same_entries(a: &[QueueEntry], b: &[QueueEntry]) -> bool {
  a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.id == y.id)
}
