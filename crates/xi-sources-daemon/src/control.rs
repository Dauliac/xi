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
//! `tick_in_flight` itself into `Inner`, set by
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

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tracing::{debug, info, warn};
use xi_core::sources::{
  SourcesId,
  config::SyncMode,
  pause::{PauseController, PauseReply, PauseRequest, PauseState, TickGate},
};

use crate::{
  exec::Exec,
  health::{DegradedReason, TickOutcome},
  singleton::Singleton,
  tick::{self, TickInputs},
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

/// Write one raw, already-encoded frame: a little-endian `u32` length
/// prefix followed by `payload`, matching [`read_frame`].
///
/// The byte-level half of the framing; [`write_message`] layers JSON
/// encoding on top for typed callers, and [`relay_stdio`] calls this
/// directly so it can pass a frame through without decoding it.
fn write_frame<W: Write>(writer: &mut W, payload: &[u8]) -> io::Result<()> {
  #[allow(clippy::cast_possible_truncation, reason = "frames are tiny")]
  let len = (payload.len() as u32).to_le_bytes();
  writer.write_all(&len)?;
  writer.write_all(payload)?;
  writer.flush()
}

/// Read one raw frame: a little-endian `u32` length prefix followed by
/// that many bytes, matching [`write_frame`].
fn read_frame<R: Read>(reader: &mut R) -> io::Result<Vec<u8>> {
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
  Ok(buf)
}

/// Encode `msg` as JSON and write it as one length-prefixed frame.
///
/// `pub`, not `pub(crate)`: `xi push`'s `target-pull` transport
/// ([[xi-push-target-pull]] task 3.1) builds this exact frame shape
/// itself, writing it to an `xi-sources-daemon ctl` subprocess's stdin
/// over SSH rather than a local [`UnixStream`] - it has no socket of its
/// own to go through [`call`]/[`advance`], only a child process's piped
/// stdio, so it needs the framing primitive directly.
///
/// # Errors
/// Returns an error if `msg` cannot be serialised, or if writing to
/// `writer` fails.
pub fn write_message<W: Write, T: Serialize>(
  writer: &mut W,
  msg: &T,
) -> io::Result<()> {
  let json = serde_json::to_vec(msg)
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
  write_frame(writer, &json)
}

/// Read one length-prefixed frame and decode it as JSON. See
/// [`write_message`] for why this is `pub`.
///
/// # Errors
/// Returns an error if reading from `reader` fails, or the frame's bytes
/// do not decode as the expected JSON shape.
pub fn read_message<R: Read, T: DeserializeOwned>(
  reader: &mut R,
) -> io::Result<T> {
  let buf = read_frame(reader)?;
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

/// Call `advance_now` on the daemon listening at `socket_path`
/// ([[xi-push-target-pull]]).
///
/// # Errors
/// Returns an error if the socket cannot be reached, the daemon does not
/// answer within its read/write timeouts, or the reply cannot be decoded -
/// including the daemon's tick degrading for a reason [`AdvanceReply`] has
/// no variant for (see [`handle_advance`]), which surfaces as a
/// connection-level error rather than a fabricated reply.
pub fn advance(
  socket_path: &Path,
  request: &AdvanceNow,
) -> io::Result<AdvanceReply> {
  let stream = UnixStream::connect(socket_path)?;
  stream.set_write_timeout(Some(REQUEST_READ_TIMEOUT))?;
  stream.set_read_timeout(Some(DEFERRED_REPLY_TIMEOUT))?;

  let mut writer = BufWriter::new(&stream);
  write_message(&mut writer, request)?;

  let mut reader = BufReader::new(&stream);
  read_message(&mut reader)
}

/// Relay exactly one request frame read from `stdin` to the daemon
/// listening at `socket_path`, then exactly one reply frame from the
/// daemon back to `stdout`, then return.
///
/// The transport [[xi-push-target-pull]]'s `ctl` subcommand wraps: `xi
/// push`'s `target-pull` mode cannot reach this `AF_UNIX` socket directly
/// over SSH, so it execs `xi-sources-daemon ctl` on the target and talks to
/// *its* stdin/stdout instead, exactly like every other remote command `xi
/// push` already runs through `Transport::run`. This has no opinion on
/// whether the frame it relays is a [`PauseRequest`] or an [`AdvanceNow`] -
/// it moves raw, already-framed bytes, the same [`write_frame`]/
/// [`read_frame`] a same-host [`call`] or [`advance`] caller would produce
/// and expect, without decoding either side.
///
/// # Errors
/// Returns an error if the socket cannot be reached, `stdin`'s frame cannot
/// be read in full, or the daemon does not answer within its own
/// read/write timeouts.
pub fn relay_stdio<R: Read, W: Write>(
  socket_path: &Path,
  stdin: &mut R,
  stdout: &mut W,
) -> io::Result<()> {
  let stream = UnixStream::connect(socket_path)?;
  stream.set_write_timeout(Some(REQUEST_READ_TIMEOUT))?;
  stream.set_read_timeout(Some(DEFERRED_REPLY_TIMEOUT))?;

  let request = read_frame(stdin)?;
  let mut writer = BufWriter::new(&stream);
  write_frame(&mut writer, &request)?;

  let mut reader = BufReader::new(&stream);
  let reply = read_frame(&mut reader)?;
  write_frame(stdout, &reply)
}

// ---------------------------------------------------------------------------
// `AdvanceNow` - target-pull's sibling request/reply pair
// ([[xi-push-target-pull]])
// ---------------------------------------------------------------------------

/// Ask the daemon to fetch and fast-forward right now, instead of waiting
/// for its poll interval.
///
/// Sent by `xi push`'s `target-pull` transport ([[xi-push-target-pull]]).
/// `to_sha` names the commit the caller expects the sources dir to land on;
/// `requester_pid` identifies the caller the same way [`PauseRequest`]'s
/// field does.
///
/// Has no `method` tag of its own on the wire - [`ControlRequest`]
/// distinguishes it from a [`PauseRequest`] by shape, since a `PauseRequest`
/// always carries a `method` field and this does not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdvanceNow {
  /// The commit the caller expects the sources dir to fast-forward to.
  pub to_sha:        String,
  /// Pid of the process asking (the `xi push` client).
  pub requester_pid: u32,
}

/// The daemon's answer to an [`AdvanceNow`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum AdvanceReply {
  /// The fetch-and-resolve step ran and the sources dir is now at
  /// `new_head`.
  Advanced {
    /// The sha the sources dir fast-forwarded to.
    new_head: String,
  },
  /// The fetched tip was not a descendant of the sources dir's current
  /// `HEAD` - the same refusal [`crate::tick::run`] already makes on its own
  /// poll cycle, surfaced here rather than duplicated.
  NotFastForward,
  /// A tick is already in flight. The caller should retry; `AdvanceNow`
  /// never queues or blocks behind a tick it has no visibility into.
  Busy,
}

/// Either request shape the control socket accepts, decoded from the same
/// length-prefixed JSON frame [`handle_connection`] reads.
///
/// `untagged`: tried in order, so a frame carrying a `method` field matches
/// [`PauseRequest`] first; one without matches [`AdvanceNow`].
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum ControlRequest {
  Pause(PauseRequest),
  Advance(AdvanceNow),
}

/// The fixed, per-process config [`handle_advance`] needs to build a
/// [`TickInputs`] and run [`crate::tick::run`] on demand.
///
/// Owned, not borrowed: built once in `main` from the same resolved config
/// and `remotes[0]` its own loop uses for `sources_id` (multi-remote fan-in
/// has no way to say which remote an `AdvanceNow` means, so this follows
/// `main`'s existing precedent of keying identity on the first configured
/// remote), and handed to [`ControlServer::spawn`] - which outlives the
/// `main` stack frame that built it, so `TickInputs`' borrowed fields have
/// to come from somewhere that does too.
#[derive(Debug)]
pub struct AdvanceContext {
  /// Identity of the source this daemon watches.
  pub sources_id:      SourcesId,
  /// The managed clone `AdvanceNow` fast-forwards.
  pub sources_dir:     PathBuf,
  /// The flat metadata dir `sources-head` is recorded in.
  pub state_dir:       PathBuf,
  /// Git URL to fetch from - `remotes[0]`, matching `sources_id`.
  pub remote_url:      String,
  /// Branch to track.
  pub branch:          String,
  /// Whether the tip must carry a valid signature to be accepted.
  pub require_signed:  bool,
  /// SSH trust root, when configured.
  pub allowed_signers: Option<PathBuf>,
  /// GPG trust root, when configured.
  pub gpg_public_keys: Option<PathBuf>,
  /// Shell command to run after a successful advance, if any.
  pub on_advance:      Option<String>,
  /// The working-dir reconciliation policy `TickOutcome::Advanced` reports.
  pub sync_mode:       SyncMode,
}

impl AdvanceContext {
  fn tick_inputs(&self) -> TickInputs<'_> {
    TickInputs {
      sources_id:      &self.sources_id,
      sources_dir:     &self.sources_dir,
      state_dir:       &self.state_dir,
      remote_url:      &self.remote_url,
      branch:          &self.branch,
      require_signed:  self.require_signed,
      allowed_signers: self.allowed_signers.as_deref(),
      gpg_public_keys: self.gpg_public_keys.as_deref(),
      on_advance:      self.on_advance.as_deref(),
      sync_mode:       self.sync_mode,
    }
  }
}

/// Everything [`handle_advance`] needs beyond `Inner`.
///
/// `gate` is the *same* [`Singleton`] `main`'s poll loop enters around every
/// scheduled tick (shared via the `Arc` [`ControlServer::spawn`]'s caller
/// passes in) - not a second lock, because two locks would let an
/// `AdvanceNow`-triggered tick and a poll-triggered tick run concurrently
/// against the same sources dir, exactly what [`Singleton`] exists to rule
/// out. `exec` and `context` are the other two things [`crate::tick::run`]
/// needs that `Inner` has no reason to also hold.
struct AdvanceEnv {
  gate:    Arc<Singleton>,
  exec:    Box<dyn Exec>,
  context: AdvanceContext,
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
  /// Bind `socket_path` and start accepting `pause`/`resume`/`advance_now`
  /// connections.
  ///
  /// `timeout` is the auto-resume default passed to
  /// [`PauseController::new`]. A stale socket file at `socket_path` (left by
  /// a previous, uncleanly-stopped daemon) is removed first.
  ///
  /// `gate` must be the *same* [`Singleton`] the caller's own poll loop
  /// enters around its scheduled ticks ([[xi-push-target-pull]]) - this is
  /// what keeps an `AdvanceNow`-triggered tick and a poll-triggered tick
  /// from ever running concurrently against `context.sources_dir`. `exec`
  /// and `context` are what an `AdvanceNow`'s on-demand
  /// [`crate::tick::run`] call needs.
  ///
  /// # Errors
  /// Returns an error if the socket cannot be bound.
  pub fn spawn(
    socket_path: PathBuf,
    timeout: Duration,
    gate: Arc<Singleton>,
    exec: Box<dyn Exec>,
    context: AdvanceContext,
  ) -> io::Result<Self> {
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
    let advance = Arc::new(AdvanceEnv {
      gate,
      exec,
      context,
    });
    let stop = Arc::new(AtomicBool::new(false));

    let accept_thread = {
      let inner = Arc::clone(&inner);
      let advance = Arc::clone(&advance);
      let stop = Arc::clone(&stop);
      thread::Builder::new()
        .name("xi-sources-control".to_owned())
        .spawn(move || accept_loop(&listener, &inner, &advance, &stop))?
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
    begin_tick_locked(&self.inner);
  }

  /// Mark the tick as finished and deliver the deferred `Paused` reply, if
  /// one is owed.
  ///
  /// MUST be called after the tick's `Singleton` permit has been dropped -
  /// a `pause` caller receiving this reply takes it as permission to
  /// acquire, and that is only true once nothing is still mid-write.
  pub fn end_tick(&self) {
    end_tick_locked(&self.inner);
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

/// The body of [`ControlServer::begin_tick`], also called by
/// [`handle_advance`] around its own on-demand tick so a concurrent `pause`
/// sees `tick_in_flight` correctly either way.
fn begin_tick_locked(inner: &Arc<Mutex<Inner>>) {
  lock(inner).tick_in_flight = true;
}

/// The body of [`ControlServer::end_tick`], also called by
/// [`handle_advance`] - see [`begin_tick_locked`].
fn end_tick_locked(inner: &Arc<Mutex<Inner>>) {
  let mut guard = lock(inner);
  guard.tick_in_flight = false;
  let reply = guard.controller.on_tick_complete();
  let sender = reply.and_then(|_| guard.deferred.take());
  drop(guard);
  if let (Some(reply), Some(sender)) = (reply, sender) {
    // The pausing client may already have given up and disconnected;
    // that is its call to make, not an error on the daemon's side.
    let _ignore_gone_client = sender.send(reply);
  }
}

fn accept_loop(
  listener: &UnixListener,
  inner: &Arc<Mutex<Inner>>,
  advance: &Arc<AdvanceEnv>,
  stop: &Arc<AtomicBool>,
) {
  while !stop.load(Ordering::SeqCst) {
    match listener.accept() {
      Ok((stream, _addr)) => {
        let inner = Arc::clone(inner);
        let advance = Arc::clone(advance);
        thread::spawn(move || {
          if let Err(error) = handle_connection(&stream, &inner, &advance) {
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
  advance: &Arc<AdvanceEnv>,
) -> io::Result<()> {
  stream.set_read_timeout(Some(REQUEST_READ_TIMEOUT))?;

  let mut reader = BufReader::new(stream);
  let request: ControlRequest = read_message(&mut reader)?;

  match request {
    ControlRequest::Pause(request) => handle_pause(stream, inner, &request),
    ControlRequest::Advance(request) => {
      handle_advance(stream, inner, advance, &request)
    },
  }
}

fn handle_pause(
  stream: &UnixStream,
  inner: &Arc<Mutex<Inner>>,
  request: &PauseRequest,
) -> io::Result<()> {
  let now = Instant::now();

  let mut guard = lock(inner);
  let tick_in_flight = guard.tick_in_flight;
  let reply = guard.controller.on_request(request, tick_in_flight, now);
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

/// Handle an [`AdvanceNow`] by running [`crate::tick::run`] on demand
/// ([[xi-push-target-pull]] Open Question 1).
///
/// `tick::run`'s own pipeline never reaches build/switch/activation - see
/// its module doc: that is `main`'s own job, run as a separate step outside
/// this same `Singleton` permit specifically to avoid deadlocking against
/// the `xi <verb> switch` subprocess's own fresh `SourcesLock` acquisition.
/// So "reuse the whole pipeline" and "reuse only the fetch-and-resolve
/// prefix" are the same instruction once you read what the pipeline
/// actually contains: invoking `tick::run` as-is already *is* the
/// fetch-and-resolve step, with nothing left to carve out. Build/switch, if
/// a target also runs them, land on that target's own next scheduled tick -
/// this handler does not persist `pending-activation` itself, since doing
/// so would not even wake `main`'s loop (it only reloads that file at
/// startup), only leave a correct-but-unread record for a restart.
///
/// Acquires `env.gate` - the exact [`Singleton`] `main`'s poll loop enters
/// around every scheduled tick - non-blockingly: if it is already held (by
/// a poll-driven tick, or a previous `AdvanceNow`), this replies `Busy`
/// immediately per SC-TP-003, the same way a `tick_in_flight` read already
/// did before this landed. `begin_tick_locked`/`end_tick_locked` run around
/// the actual [`tick::run`] call in the same order `main`'s loop uses -
/// permit held during the tick, dropped, *then* `tick_in_flight` cleared -
/// so a concurrent `pause` still only learns it is safe to acquire once the
/// write has actually finished.
///
/// Note: this does not consult [`PauseController`]'s own gate - an
/// `AdvanceNow` arriving while the daemon is `pause`d today still runs,
/// since `Singleton` only tracks "a tick is in flight", not "ticks are
/// held". Whether `AdvanceNow` should instead respect an active pause is
/// unresolved; flagged as a discovery on `xi-lutp.1.2` rather than guessed
/// at here, since no task or success criterion asks for it.
///
/// # Errors
/// Returns a connection-level `io::Error` - no reply is written, so the
/// caller observes the connection close - when the tick degrades for a
/// reason [`AdvanceReply`] has no variant for (anything other than a clean
/// advance, "no change", or the force-push refusal `NotFastForward`
/// reports).
fn handle_advance(
  stream: &UnixStream,
  inner: &Arc<Mutex<Inner>>,
  env: &AdvanceEnv,
  request: &AdvanceNow,
) -> io::Result<()> {
  let Some(permit) = env.gate.try_enter() else {
    let reply = AdvanceReply::Busy;
    debug!(?request, ?reply, "control socket request handled");
    let mut writer = BufWriter::new(stream);
    return write_message(&mut writer, &reply);
  };

  begin_tick_locked(inner);
  let inputs = env.context.tick_inputs();
  let outcome = tick::run(env.exec.as_ref(), &inputs);
  drop(permit);
  end_tick_locked(inner);

  let reply = match &outcome {
    TickOutcome::Advanced { to, .. } => {
      AdvanceReply::Advanced {
        new_head: to.clone(),
      }
    },
    TickOutcome::NoChange { sha } => {
      AdvanceReply::Advanced {
        new_head: sha.clone(),
      }
    },
    TickOutcome::Degraded(DegradedReason::RemoteForcePush { .. }) => {
      AdvanceReply::NotFastForward
    },
    other => {
      warn!(
        ?request,
        outcome = ?other,
        "AdvanceNow's tick degraded for a reason AdvanceReply has no \
         variant for"
      );
      return Err(io::Error::other(format!(
        "AdvanceNow's tick did not complete cleanly: {other:?}"
      )));
    },
  };

  debug!(?request, ?reply, "control socket request handled");
  let mut writer = BufWriter::new(stream);
  write_message(&mut writer, &reply)
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "Fine in tests")]
mod tests {
  use std::{fs, process::Command, sync::mpsc as std_mpsc};

  use tempfile::TempDir;
  use xi_core::sources::Scope;

  use super::*;
  use crate::exec::recorder::RecordingExec;

  /// An [`AdvanceContext`] for tests that never actually invoke
  /// [`handle_advance`]'s real tick (pause/resume/busy tests): the paths
  /// are never touched, so they do not need to exist.
  fn dummy_context() -> AdvanceContext {
    AdvanceContext {
      sources_id:      SourcesId::new(
        Scope::System,
        "https://example.invalid/unused.git",
      ),
      sources_dir:     PathBuf::from("/nonexistent-xi-lutp-dummy/sources"),
      state_dir:       PathBuf::from("/nonexistent-xi-lutp-dummy/state"),
      remote_url:      "https://example.invalid/unused.git".to_owned(),
      branch:          "main".to_owned(),
      require_signed:  false,
      allowed_signers: None,
      gpg_public_keys: None,
      on_advance:      None,
      sync_mode:       SyncMode::Prompt,
    }
  }

  fn start(
    timeout: Duration,
    context: AdvanceContext,
  ) -> (ControlServer, PathBuf, Arc<Singleton>, TempDir) {
    let tmp = TempDir::new().expect("tempdir");
    let path = socket_path(tmp.path());
    let gate = Arc::new(Singleton::new());
    let server = ControlServer::spawn(
      path.clone(),
      timeout,
      Arc::clone(&gate),
      Box::new(RecordingExec::always_ok()),
      context,
    )
    .expect("spawn");
    (server, path, gate, tmp)
  }

  fn git(at: &Path, args: &[&str]) {
    let status = Command::new("git")
      .env("GIT_TERMINAL_PROMPT", "0")
      .arg("-C")
      .arg(at)
      .args(args)
      .status()
      .expect("git should run");
    assert!(status.success(), "git {args:?} failed");
  }

  fn commit(repo: &Path, name: &str, body: &str) {
    fs::write(repo.join(name), body).expect("write");
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", body]);
  }

  /// A real git fixture (remote + sources dir) for tests that exercise
  /// `handle_advance`'s actual `tick::run` call, mirroring `tick.rs`'s own
  /// test `World`.
  struct AdvanceWorld {
    _tmp:        TempDir,
    remote:      PathBuf,
    sources_dir: PathBuf,
    state_dir:   PathBuf,
  }

  impl AdvanceWorld {
    fn new() -> Self {
      let tmp = TempDir::new().expect("tempdir");
      let remote = tmp.path().join("remote");
      let sources_dir = tmp.path().join("sources");
      let state_dir = tmp.path().join("state");
      fs::create_dir_all(&remote).expect("mkdir");

      git(&remote, &["init", "-q", "-b", "main"]);
      git(&remote, &["config", "user.email", "test@example.com"]);
      git(&remote, &["config", "user.name", "Test"]);
      commit(&remote, "flake.nix", "base");

      fs::create_dir_all(&sources_dir).expect("mkdir");
      git(&sources_dir, &["init", "-q", "-b", "main"]);
      fs::create_dir_all(&state_dir).expect("mkdir");

      Self {
        _tmp: tmp,
        remote,
        sources_dir,
        state_dir,
      }
    }

    fn sources_id(&self) -> SourcesId {
      SourcesId::new(Scope::System, self.remote.to_str().expect("utf8"))
    }

    fn context(&self) -> AdvanceContext {
      AdvanceContext {
        sources_id:      self.sources_id(),
        sources_dir:     self.sources_dir.clone(),
        state_dir:       self.state_dir.clone(),
        remote_url:      self.remote.to_str().expect("utf8").to_owned(),
        branch:          "main".to_owned(),
        require_signed:  false,
        allowed_signers: None,
        gpg_public_keys: None,
        on_advance:      None,
        sync_mode:       SyncMode::Prompt,
      }
    }

    fn write_sentinel(&self) {
      crate::sentinel::Sentinel::new(self.sources_id())
        .write(&self.sources_dir)
        .expect("write sentinel");
    }
  }

  #[test]
  fn pause_while_idle_replies_immediately() {
    let (server, path, _gate, _tmp) =
      start(Duration::from_secs(30), dummy_context());

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
    let (server, path, _gate, _tmp) =
      start(Duration::from_secs(30), dummy_context());

    let reply = call(&path, &PauseRequest::Resume {
      requester_pid: std::process::id(),
    })
    .expect("call");

    assert_eq!(reply, PauseReply::NotPaused);
    server.shutdown();
  }

  #[test]
  fn pause_then_resume_round_trips() {
    let (server, path, _gate, _tmp) =
      start(Duration::from_secs(30), dummy_context());
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
    let (server, path, _gate, _tmp) =
      start(Duration::from_secs(30), dummy_context());
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
    let (server, path, _gate, _tmp) =
      start(Duration::from_secs(30), dummy_context());

    assert_eq!(server.gate(Instant::now()), TickGate::Run);

    call(&path, &PauseRequest::Pause {
      requester_pid: std::process::id(),
      timeout_secs:  None,
    })
    .expect("pause");

    assert_eq!(server.gate(Instant::now()), TickGate::Hold);

    server.shutdown();
  }

  /// The connection handler must tell an `AdvanceNow` apart from a
  /// `PauseRequest` sharing the same socket and framing - not a second
  /// listener, a second message type ([[xi-push-target-pull]]) - and
  /// `AdvanceNow`'s own handler must actually run the fetch-and-resolve
  /// step (`xi-lutp.1.2`): a first `AdvanceNow` against a freshly-cloned,
  /// sentinel-written sources dir adopts the remote tip, matching what
  /// `tick.rs`'s own `first_tick_on_an_empty_clone_adopts_the_remote_tip`
  /// asserts for the poll-driven path.
  #[test]
  fn advance_now_while_idle_runs_the_tick_and_advances() {
    let world = AdvanceWorld::new();
    world.write_sentinel();
    let (server, path, _gate, _tmp) =
      start(Duration::from_secs(30), world.context());

    let expected = {
      let output = Command::new("git")
        .arg("-C")
        .arg(&world.remote)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("rev-parse");
      String::from_utf8_lossy(&output.stdout).trim().to_owned()
    };

    let reply = advance(&path, &AdvanceNow {
      to_sha:        expected.clone(),
      requester_pid: std::process::id(),
    })
    .expect("advance");
    assert_eq!(reply, AdvanceReply::Advanced { new_head: expected });

    // The socket is still healthy for an unrelated `PauseRequest` after a
    // successful advance - confirms the two message types are dispatched
    // independently, not through shared, corruptible state.
    let reply = call(&path, &PauseRequest::Resume {
      requester_pid: std::process::id(),
    })
    .expect("call");
    assert_eq!(reply, PauseReply::NotPaused);

    server.shutdown();
  }

  /// SC-TP-002: `AdvanceNow` to a sha that is not a descendant of the
  /// sources dir's current `HEAD` replies `NotFastForward` - the same
  /// refusal `tick::run` already makes on its own poll cycle (see
  /// `tick.rs`'s `a_force_pushed_remote_is_rejected...` test), surfaced
  /// here rather than a second, divergent check.
  #[test]
  fn advance_now_to_a_non_descendant_replies_not_fast_forward() {
    let world = AdvanceWorld::new();
    world.write_sentinel();
    let (server, path, _gate, _tmp) =
      start(Duration::from_secs(30), world.context());

    // Adopt the base commit first, as a real daemon would on its first
    // tick, then force-push: rewrite history instead of extending it.
    // `to_sha` is not validated by `handle_advance` (it is informational,
    // carried through to logs), so an arbitrary value is fine here.
    advance(&path, &AdvanceNow {
      to_sha:        "unused".to_owned(),
      requester_pid: std::process::id(),
    })
    .expect("first advance");
    git(&world.remote, &["checkout", "-q", "--orphan", "rewritten"]);
    commit(&world.remote, "flake.nix", "rewritten-history");
    git(&world.remote, &["branch", "-M", "rewritten", "main"]);

    let reply = advance(&path, &AdvanceNow {
      to_sha:        "deadbeef".to_owned(),
      requester_pid: std::process::id(),
    })
    .expect("advance");
    assert_eq!(reply, AdvanceReply::NotFastForward);

    server.shutdown();
  }

  /// SC-TP-003: `AdvanceNow` arriving while the shared `Singleton` (the
  /// exact thing `main`'s poll loop holds around a scheduled tick) is held
  /// replies `Busy` immediately, without deferring the way a mid-tick
  /// `pause` does.
  #[test]
  fn advance_now_mid_tick_replies_busy_immediately() {
    let (server, path, gate, _tmp) =
      start(Duration::from_secs(30), dummy_context());

    let permit = gate.enter();
    server.begin_tick();

    let reply = advance(&path, &AdvanceNow {
      to_sha:        "deadbeef".to_owned(),
      requester_pid: std::process::id(),
    })
    .expect("advance");
    assert_eq!(reply, AdvanceReply::Busy);

    drop(permit);
    server.end_tick();
    server.shutdown();
  }
}
