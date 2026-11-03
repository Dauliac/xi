//! The loopback-only HTTP server `/metrics` is served from.
//!
//! A metrics endpoint carries the shape of a machine's deployment
//! history, and a liveness probe sitting next to it is a free oracle for
//! anyone who can reach it (see [`crate::config::validate_bind`]'s doc
//! comment, which this module enforces by calling it before ever opening
//! a socket). Neither belongs on a routable interface, so [`Server::bind`]
//! refuses anything but a loopback address rather than warning about it.
//!
//! This is task 5.2: the socket exists, is loopback-only, and serves
//! `/metrics`. `/healthz` and the "nothing else" closed-route guarantee
//! are task 5.3, owned elsewhere - this module deliberately answers every
//! other path with a plain 404 so that task can add the route without
//! fighting this one's shape.
//!
//! # Why a hand-rolled HTTP/1.1 reader instead of a server crate
//!
//! Nothing else in this crate (outside the `otlp` feature, which already
//! pays for `reqwest` as an HTTP *client*) depends on an HTTP stack, and
//! this socket only ever has to answer a `GET` with no body: a dependency
//! that would pull in an async runtime for that is not proportionate.
//! [`handle_one`] reads a request line and discards headers until the
//! blank line that ends them, which is the whole protocol surface this
//! server needs.

use std::{
  io::{self, BufRead, BufReader, Write as _},
  net::{SocketAddr, TcpListener, TcpStream},
};

use crate::{
  config::{self, PrometheusConfig},
  metrics::Metrics,
};

/// Everything that can go wrong opening the exporter's socket.
#[derive(Debug, thiserror::Error)]
pub enum BindError {
  /// `config.bind` is not a loopback address.
  ///
  /// Carries [`config::validate_bind`]'s own message, which already
  /// names the rejected address and explains why.
  #[error("{0}")]
  NonLoopback(String),
  /// The address validated as loopback but the socket could not be
  /// opened (port already in use, no permission, ...).
  #[error("failed to bind the sources exporter to {addr}: {source}")]
  Io {
    /// The address that could not be bound.
    addr:   SocketAddr,
    /// The underlying I/O error.
    #[source]
    source: io::Error,
  },
}

/// The bound loopback listener a daemon serves `/metrics` from.
///
/// Binding and serving are separate on purpose: a caller (the daemon's
/// startup path) needs to know binding succeeded - and at exactly which
/// port, when `port` was `0` for a test - before deciding anything else
/// about startup, well before it is ready to block serving requests.
#[derive(Debug)]
pub struct Server {
  listener: TcpListener,
}

impl Server {
  /// Bind the exporter per `config`, refusing anything but a loopback
  /// address.
  ///
  /// Matches spec.md's "Non-loopback port refused" scenario: validation
  /// happens, and fails with a clear error, before any socket is opened -
  /// a daemon started with a misconfigured bind never listens on
  /// anything, rather than listening on the wrong thing.
  ///
  /// # Errors
  ///
  /// Returns [`BindError::NonLoopback`] if `config.bind` is not a
  /// loopback address, or [`BindError::Io`] if the address is loopback
  /// but the socket cannot be opened.
  pub fn bind(config: &PrometheusConfig) -> Result<Self, BindError> {
    let ip = config::validate_bind(&config.bind).map_err(BindError::NonLoopback)?;
    let addr = SocketAddr::new(ip, config.port);
    let listener = TcpListener::bind(addr)
      .map_err(|source| BindError::Io { addr, source })?;
    Ok(Self { listener })
  }

  /// The address actually bound.
  ///
  /// Differs from the configured port when `config.port` was `0`
  /// (ephemeral - what tests bind to avoid colliding on a fixed port).
  ///
  /// # Errors
  ///
  /// Propagates the underlying socket's `local_addr` error, which the
  /// standard library documents as only happening on a socket that is no
  /// longer valid.
  pub fn local_addr(&self) -> io::Result<SocketAddr> {
    self.listener.local_addr()
  }

  /// Accept and answer exactly one request, then return.
  ///
  /// The unit a test drives directly, and the body of the loop a real
  /// daemon's exporter thread calls forever. One connection at a time by
  /// design: a scrape is infrequent and brief, and serialising requests
  /// means [`Metrics::encode`] (which reads every registered collector)
  /// never races a second scrape's read of the same registry.
  ///
  /// # Errors
  ///
  /// Propagates the I/O error from accepting or answering the
  /// connection. A single malformed or dropped connection does not
  /// poison the listener - a caller looping on this method should log
  /// the error and accept the next connection rather than stop serving.
  pub fn serve_one(&self, metrics: &Metrics) -> io::Result<()> {
    let (stream, _peer) = self.listener.accept()?;
    handle_one(stream, metrics)
  }
}

/// Read one request off `stream` and answer it.
fn handle_one(mut stream: TcpStream, metrics: &Metrics) -> io::Result<()> {
  let mut reader = BufReader::new(stream.try_clone()?);

  let mut request_line = String::new();
  reader.read_line(&mut request_line)?;

  // Discard headers up to the blank line that ends them; this server
  // reads no header value, so there is nothing to collect.
  loop {
    let mut line = String::new();
    let read = reader.read_line(&mut line)?;
    if read == 0 || line == "\r\n" || line == "\n" {
      break;
    }
  }

  let path = request_line.split_whitespace().nth(1).unwrap_or("/");
  match path {
    "/metrics" => {
      match metrics.encode() {
        Ok(body) => {
          respond(&mut stream, 200, "OK", "text/plain; version=0.0.4", &body)
        },
        Err(err) => {
          respond(
            &mut stream,
            500,
            "Internal Server Error",
            "text/plain",
            &err.to_string(),
          )
        },
      }
    },
    // `/healthz` lands here too until task 5.3 adds its own route; a
    // plain 404 is correct in the meantime, not a stand-in liveness
    // answer this module has no business making up.
    _ => respond(&mut stream, 404, "Not Found", "text/plain", "not found"),
  }
}

/// Write a minimal HTTP/1.1 response and close the connection.
///
/// `Connection: close` rather than keep-alive: a scraper issues one
/// request per connection, and not pretending to support persistent
/// connections keeps [`handle_one`] from needing a request-parsing loop.
fn respond(
  stream: &mut TcpStream,
  code: u16,
  reason: &str,
  content_type: &str,
  body: &str,
) -> io::Result<()> {
  write!(
    stream,
    "HTTP/1.1 {code} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: \
     {}\r\nConnection: close\r\n\r\n{body}",
    body.len()
  )?;
  stream.flush()
}

#[cfg(test)]
#[expect(
  clippy::expect_used,
  clippy::panic,
  clippy::unwrap_used,
  reason = "Fine in tests"
)]
mod tests {
  use std::io::Read as _;

  use super::*;

  /// An ephemeral-port config (`port = 0`) so tests never collide on a
  /// fixed one.
  fn loopback_ephemeral() -> PrometheusConfig {
    PrometheusConfig::loopback(0)
  }

  fn get(addr: SocketAddr, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).expect("connect");
    write!(stream, "GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n")
      .expect("write request");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");

    let status_line = response.lines().next().expect("status line");
    let code: u16 = status_line
      .split_whitespace()
      .nth(1)
      .expect("status code")
      .parse()
      .expect("status code is a number");
    let body = response.split("\r\n\r\n").nth(1).unwrap_or("").to_owned();
    (code, body)
  }

  // -- Scenario: Non-loopback port refused ---------------------------------

  #[test]
  fn binding_a_loopback_config_succeeds() {
    let server = Server::bind(&loopback_ephemeral());
    assert!(server.is_ok(), "{:?}", server.err());
  }

  #[test]
  fn binding_a_non_loopback_config_is_refused_and_names_the_address() {
    let config = PrometheusConfig {
      port: 9321,
      bind: "0.0.0.0".to_owned(),
    };
    let err = Server::bind(&config).expect_err("must refuse a non-loopback bind");
    let BindError::NonLoopback(message) = err else {
      panic!("expected NonLoopback, got {err:?}")
    };
    assert!(message.contains("0.0.0.0"), "{message}");
    assert!(message.contains("non-loopback"), "{message}");
  }

  #[test]
  fn a_garbage_bind_address_is_refused_before_any_socket_opens() {
    let config = PrometheusConfig {
      port: 9321,
      bind: "not-an-address".to_owned(),
    };
    assert!(matches!(Server::bind(&config), Err(BindError::NonLoopback(_))));
  }

  #[test]
  fn binding_the_same_port_twice_is_an_io_error_naming_the_address() {
    let first = Server::bind(&loopback_ephemeral()).expect("first bind");
    let addr = first.local_addr().expect("local_addr");
    let config = PrometheusConfig {
      port: addr.port(),
      bind: "127.0.0.1".to_owned(),
    };

    let err = Server::bind(&config).expect_err("must refuse an in-use port");
    let BindError::Io { addr: got, .. } = err else {
      panic!("expected Io, got {err:?}")
    };
    assert_eq!(got, addr);
  }

  // -- Scenario: Metrics endpoint reachable ---------------------------------

  #[test]
  fn metrics_endpoint_is_reachable_and_non_empty() {
    let server = Server::bind(&loopback_ephemeral()).expect("bind");
    let addr = server.local_addr().expect("local_addr");
    let metrics = Metrics::new().expect("metrics must register");

    let handle = std::thread::spawn(move || server.serve_one(&metrics));
    let (status, body) = get(addr, "/metrics");
    handle.join().expect("server thread").expect("serve_one");

    assert_eq!(status, 200);
    assert!(!body.is_empty(), "body must not be empty");
    assert!(body.contains("xi_sources_fetch_total"), "{body}");
  }

  #[test]
  fn an_unknown_path_is_a_plain_404() {
    let server = Server::bind(&loopback_ephemeral()).expect("bind");
    let addr = server.local_addr().expect("local_addr");
    let metrics = Metrics::new().expect("metrics must register");

    let handle = std::thread::spawn(move || server.serve_one(&metrics));
    let (status, _body) = get(addr, "/does-not-exist");
    handle.join().expect("server thread").expect("serve_one");

    assert_eq!(status, 404);
  }
}
