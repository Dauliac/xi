//! The daemon's control socket: `pause`/`resume` RPC transport
//! (`xi-sources-race` task 4.1).
//!
//! [`xi_core::sources::pause`] owns the state machine - what a `pause` or
//! `resume` call means, when a reply is owed immediately versus deferred
//! until the in-flight tick drains, and both ways a pause auto-resumes. This
//! module is the other half: a real `AF_UNIX` socket a client (today:
//! `xi push`, see `xi-sources-push`) can connect to, and the four things the
//! daemon's own tick loop does around it (see [`ControlServer`]'s methods).
//!
//! # Avoiding a second copy of "is a tick in flight"
//!
//! [`PauseController::on_request`] needs to know whether a tick is running
//! right now. The daemon's tick loop already has an authoritative answer to
//! that in `crate::singleton::Singleton::try_enter`, but this module does
//! not read it directly - see "why one mutex, not two" below for why.
//! [`ControlServer::begin_tick`] and [`ControlServer::end_tick`] are the
//! tick loop's two hooks into this module: call the former right after
//! entering the `Singleton` permit, the latter right after dropping it.
//!
//! # Why one mutex, not two
//!
//! An earlier draft kept `tick_in_flight` out of this module entirely,
//! computing it from `Singleton::try_enter` *inside* a connection handler.
//! That has a race: a tick could finish (and the main loop could call
//! [`ControlServer::end_tick`], finding no deferred sender yet, because none
//! had been registered) in the gap between a connection thread reading
//! "tick in flight" and it calling `on_request`. The result would be a
//! `pause` caller told `Pausing` with nobody left to ever send it the
//! deferred `Paused` - wedged until the next tick happens to run. Folding
//! `tick_in_flight` itself into [`Inner`], set by
//! [`ControlServer::begin_tick`]/[`ControlServer::end_tick`] under the same
//! lock a connection reads it through, closes that window: every transition
//! a connection can observe and every transition the tick loop makes happen
//! inside one critical section.

use std::{
  io::{self, BufReader, BufWriter, Read, Write},
  os::unix::net::{UnixListener, UnixStream},
  path::{Path, PathBuf},
  sync::{
    Arc,
    Mutex,
    MutexGuard,
    PoisonError,
    atomic::{AtomicBool, Ordering},
    mpsc,
  },
  thread::{self, JoinHandle},
  time::{Duration, Instant},
};

use serde::{Serialize, de::DeserializeOwned};
use tracing::{debug, info, warn};
use xi_core::sources::pause::{
  PauseController,
  PauseReply,
  PauseRequest,
  PauseState,
  TickGate,
};

/// Pause/resume payloads are a handful of bytes; this only bounds a
/// misbehaving or garbled client.
const MAX_FRAME_BYTES: usize = 64 * 1024;

/// Ceiling on how long a connection waits for a deferred `Paused` reply.
///
/// Not a real expected path: a tick always finishes, and
/// [`ControlServer::end_tick`] always delivers the reply when it does. This
/// exists so a transport bug fails a `pause` call loudly instead of hanging
/// a client forever.
const DEFERRED_REPLY_TIMEOUT: Duration = Duration::from_mins(10);

/// How often the accept loop checks its stop flag between connections.
const ACCEPT_POLL: Duration = Duration::from_millis(50);

/// Per-connection read timeout for the request frame.
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// The control socket's conventional path inside a scope's state dir.
#[must_use]
pub fn socket_path(state_dir: &Path) -> PathBuf {
  state_dir.join("control.sock")
}

// ---------------------------------------------------------------------------
// Framing - length-prefixed JSON, matching `xi-develop`'s daemon protocol
// ---------------------------------------------------------------------------

fn write_message<W: Write, T: Serialize>(
  writer: &mut W,
  msg: &T,
) -> io::Result<()> {
  let json = serde_json::to_vec(msg)
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
  #[allow(clippy::cast_possible_truncation, reason = "frames are tiny")]
  let len = (json.len() as u32).to_le_bytes();
  writer.write_all(&len)?;
  writer.write_all(&json)?;
  writer.flush()
}

fn read_message<R: Read, T: DeserializeOwned>(reader: &mut R) -> io::Result<T> {
  let mut len_buf = [0_u8; 4];
  reader.read_exact(&mut len_buf)?;
  let len = u32::from_le_bytes(len_buf) as usize;
  if len > MAX_FRAME_BYTES {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      format!("control socket frame too large: {len} bytes"),
    ));
  }
  let mut buf = vec![0_u8; len];
  reader.read_exact(&mut buf)?;
  serde_json::from_slice(&buf)
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Call `pause` or `resume` on the daemon listening at `socket_path`.
///
/// The client side of this transport; used by `xi push` ([[xi-sources-push]])
/// and by this module's own tests. Blocks until the daemon answers, which for
/// a `pause` sent mid-tick means until the tick drains - see
/// [`ControlServer::end_tick`].
///
/// # Errors
/// Returns an error if the socket cannot be reached, the daemon does not
/// answer within its read/write timeouts, or the reply cannot be decoded.
pub fn call(
  socket_path: &Path,
  request: &PauseRequest,
) -> io::Result<PauseReply> {
  let stream = UnixStream::connect(socket_path)?;
  stream.set_write_timeout(Some(REQUEST_READ_TIMEOUT))?;
  stream.set_read_timeout(Some(DEFERRED_REPLY_TIMEOUT))?;

  let mut writer = BufWriter::new(&stream);
  write_message(&mut writer, request)?;

  let mut reader = BufReader::new(&stream);
  read_message(&mut reader)
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// State a connection handler and the daemon's tick loop both touch, behind
/// one lock so the two can never observe an inconsistent half-transition.
struct Inner {
  controller:     PauseController,
  /// Mirrors whether the tick loop currently holds its `Singleton` permit.
  /// Set by [`ControlServer::begin_tick`]/[`ControlServer::end_tick`], read
  /// by a connection handler before calling
  /// [`PauseController::on_request`] - see the module doc for why this is
  /// not instead computed from `Singleton::try_enter` at read time.
  tick_in_flight: bool,
  /// The reply owed to whichever `pause` call is currently deferred, if any.
  deferred:       Option<mpsc::Sender<PauseReply>>,
}

/// A running control socket: the accept thread, plus the four hooks the
/// daemon's tick loop uses to drive it (see the module doc).
pub struct ControlServer {
  inner:         Arc<Mutex<Inner>>,
  stop:          Arc<AtomicBool>,
  accept_thread: Option<JoinHandle<()>>,
  socket_path:   PathBuf,
}

impl ControlServer {
  /// Bind `socket_path` and start accepting `pause`/`resume` connections.
  ///
  /// `timeout` is the auto-resume default passed to
  /// [`PauseController::new`]. A stale socket file at `socket_path` (left by
  /// a previous, uncleanly-stopped daemon) is removed first.
  ///
  /// # Errors
  /// Returns an error if the socket cannot be bound.
  pub fn spawn(socket_path: PathBuf, timeout: Duration) -> io::Result<Self> {
    let _ignore_missing = std::fs::remove_file(&socket_path);
    if let Some(parent) = socket_path.parent() {
      std::fs::create_dir_all(parent)?;
    }

    let listener = UnixListener::bind(&socket_path)?;
    listener.set_nonblocking(true)?;

    let inner = Arc::new(Mutex::new(Inner {
      controller:     PauseController::new(timeout),
      tick_in_flight: false,
      deferred:       None,
    }));
    let stop = Arc::new(AtomicBool::new(false));

    let accept_thread = {
      let inner = Arc::clone(&inner);
      let stop = Arc::clone(&stop);
      thread::Builder::new()
        .name("xi-sources-control".to_owned())
        .spawn(move || accept_loop(&listener, &inner, &stop))?
    };

    info!(socket = %socket_path.display(), "control socket listening");

    Ok(Self {
      inner,
      stop,
      accept_thread: Some(accept_thread),
      socket_path,
    })
  }

  /// May a tick start now? Also polls auto-resume - see
  /// [`PauseController::gate`]. Call this before entering the tick loop's
  /// `Singleton` permit for each remote.
  #[must_use]
  pub fn gate(&self, now: Instant) -> TickGate {
    lock(&self.inner).controller.gate(now)
  }

  /// Current pause state, for a future `xi sources status` surface.
  #[must_use]
  pub fn state(&self) -> PauseState {
    lock(&self.inner).controller.state()
  }

  /// Mark a tick as started. Call after entering the tick loop's
  /// `Singleton` permit, before running the tick.
  pub fn begin_tick(&self) {
    lock(&self.inner).tick_in_flight = true;
  }

  /// Mark the tick as finished and deliver the deferred `Paused` reply, if
  /// one is owed.
  ///
  /// MUST be called after the tick's `Singleton` permit has been dropped -
  /// a `pause` caller receiving this reply takes it as permission to
  /// acquire, and that is only true once nothing is still mid-write.
  pub fn end_tick(&self) {
    let mut inner = lock(&self.inner);
    inner.tick_in_flight = false;
    let reply = inner.controller.on_tick_complete();
    let sender = reply.and_then(|_| inner.deferred.take());
    drop(inner);
    if let (Some(reply), Some(sender)) = (reply, sender) {
      // The pausing client may already have given up and disconnected;
      // that is its call to make, not an error on the daemon's side.
      let _ignore_gone_client = sender.send(reply);
    }
  }

  /// Stop accepting connections, join the accept thread, and remove the
  /// socket file.
  pub fn shutdown(mut self) {
    self.stop.store(true, Ordering::SeqCst);
    if let Some(handle) = self.accept_thread.take() {
      let _ignore_panicked_thread = handle.join();
    }
    let _ignore_missing = std::fs::remove_file(&self.socket_path);
  }
}

fn lock(inner: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
  inner.lock().unwrap_or_else(PoisonError::into_inner)
}

fn accept_loop(
  listener: &UnixListener,
  inner: &Arc<Mutex<Inner>>,
  stop: &Arc<AtomicBool>,
) {
  while !stop.load(Ordering::SeqCst) {
    match listener.accept() {
      Ok((stream, _addr)) => {
        let inner = Arc::clone(inner);
        thread::spawn(move || {
          if let Err(error) = handle_connection(&stream, &inner) {
            debug!(%error, "control socket connection ended with an error");
          }
        });
      },
      Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
        thread::sleep(ACCEPT_POLL);
      },
      Err(error) => {
        warn!(%error, "control socket accept error");
        thread::sleep(ACCEPT_POLL);
      },
    }
  }
}

fn handle_connection(
  stream: &UnixStream,
  inner: &Arc<Mutex<Inner>>,
) -> io::Result<()> {
  stream.set_read_timeout(Some(REQUEST_READ_TIMEOUT))?;

  let mut reader = BufReader::new(stream);
  let request: PauseRequest = read_message(&mut reader)?;
  let now = Instant::now();

  let mut guard = lock(inner);
  let tick_in_flight = guard.tick_in_flight;
  let reply = guard.controller.on_request(&request, tick_in_flight, now);
  let pending = if matches!(reply, PauseReply::Pausing) {
    let (tx, rx) = mpsc::channel();
    guard.deferred = Some(tx);
    Err(rx)
  } else {
    Ok(reply)
  };
  drop(guard);

  let reply = match pending {
    Ok(reply) => reply,
    Err(rx) => {
      rx.recv_timeout(DEFERRED_REPLY_TIMEOUT).map_err(|_error| {
        io::Error::new(
          io::ErrorKind::TimedOut,
          "tick never completed; deferred pause reply was never delivered",
        )
      })?
    },
  };

  debug!(?request, ?reply, "control socket request handled");
  let mut writer = BufWriter::new(stream);
  write_message(&mut writer, &reply)
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "Fine in tests")]
mod tests {
  use std::sync::mpsc as std_mpsc;

  use tempfile::TempDir;

  use super::*;

  fn start(timeout: Duration) -> (ControlServer, PathBuf, TempDir) {
    let tmp = TempDir::new().expect("tempdir");
    let path = socket_path(tmp.path());
    let server = ControlServer::spawn(path.clone(), timeout).expect("spawn");
    (server, path, tmp)
  }

  #[test]
  fn pause_while_idle_replies_immediately() {
    let (server, path, _tmp) = start(Duration::from_secs(30));

    let reply = call(&path, &PauseRequest::Pause {
      requester_pid: std::process::id(),
      timeout_secs:  None,
    })
    .expect("call");

    assert_eq!(reply, PauseReply::Paused);
    assert_eq!(server.state(), PauseState::Paused {
      requester_pid: std::process::id(),
    });

    server.shutdown();
  }

  #[test]
  fn resume_without_a_pause_replies_not_paused() {
    let (server, path, _tmp) = start(Duration::from_secs(30));

    let reply = call(&path, &PauseRequest::Resume {
      requester_pid: std::process::id(),
    })
    .expect("call");

    assert_eq!(reply, PauseReply::NotPaused);
    server.shutdown();
  }

  #[test]
  fn pause_then_resume_round_trips() {
    let (server, path, _tmp) = start(Duration::from_secs(30));
    let pid = std::process::id();

    let paused = call(&path, &PauseRequest::Pause {
      requester_pid: pid,
      timeout_secs:  None,
    })
    .expect("pause");
    assert_eq!(paused, PauseReply::Paused);

    let resumed = call(&path, &PauseRequest::Resume { requester_pid: pid })
      .expect("resume");
    assert_eq!(resumed, PauseReply::Resumed);
    assert_eq!(server.state(), PauseState::Running);

    server.shutdown();
  }

  /// The transport half of SC-E-002: a `pause` arriving while a tick is
  /// in flight must not get its reply until [`ControlServer::end_tick`]
  /// delivers the deferred `Paused` - over a real socket, not just inside
  /// [`PauseController`] directly (that half is `xi-core`'s
  /// `bdd_sources_race` suite).
  #[test]
  fn pause_mid_tick_defers_until_end_tick() {
    let (server, path, _tmp) = start(Duration::from_secs(30));
    let pid = std::process::id();

    server.begin_tick();

    let (tx, rx) = std_mpsc::channel();
    let client = thread::spawn(move || {
      let reply = call(&path, &PauseRequest::Pause {
        requester_pid: pid,
        timeout_secs:  None,
      })
      .expect("call");
      tx.send(reply).expect("send back to test thread");
    });

    // The client must still be blocked: nobody has ended the tick yet.
    assert_eq!(
      rx.recv_timeout(Duration::from_millis(200)),
      Err(std_mpsc::RecvTimeoutError::Timeout),
      "pause replied before the tick finished"
    );
    assert_eq!(server.state(), PauseState::Pausing { requester_pid: pid });

    server.end_tick();

    let reply = rx
      .recv_timeout(Duration::from_secs(5))
      .expect("deferred reply delivered after end_tick");
    assert_eq!(reply, PauseReply::Paused);
    assert_eq!(server.state(), PauseState::Paused { requester_pid: pid });

    client.join().expect("client thread should not panic");
    server.shutdown();
  }

  #[test]
  fn gate_holds_new_ticks_once_paused() {
    let (server, path, _tmp) = start(Duration::from_secs(30));

    assert_eq!(server.gate(Instant::now()), TickGate::Run);

    call(&path, &PauseRequest::Pause {
      requester_pid: std::process::id(),
      timeout_secs:  None,
    })
    .expect("pause");

    assert_eq!(server.gate(Instant::now()), TickGate::Hold);

    server.shutdown();
  }
}
