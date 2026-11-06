//! BDD: the Prometheus endpoint is reachable and non-empty.
//!
//! Mirrors `openspec/specs/xi-sources-observability/spec.md`'s "Metrics
//! endpoint reachable" scenario:
//!
//! - GIVEN `xi.sources.observability.prometheus.port = 9321`
//! - WHEN the operator runs `curl http://127.0.0.1:9321/metrics`
//! - THEN the response includes every metric name listed in the spec
//!
//! `server::tests` (task 5.2) already proves the endpoint answers at all;
//! this file is the BDD-level proof, driven through the crate's public
//! API end to end against a real bound socket, that it answers with
//! *every* spec-listed metric name - `metrics::names::ALL`, not a
//! hand-picked subset - rather than just "some text".
//!
//! A fixed port (`9321`, the spec's own example) would make this test
//! flaky against anything else on the machine using it, or against a
//! second run of this same suite; `Server::bind` is still exercised with
//! a real `PrometheusConfig`, just constructed with an ephemeral port
//! (`0`) the way a test harness always should, with the OS-assigned port
//! read back via `local_addr` standing in for the operator's configured
//! `9321`.

#![expect(clippy::expect_used, reason = "Fine in tests")]

use std::{
  io::{Read as _, Write as _},
  net::TcpStream,
};

use xi_sources_observability::{
  config::PrometheusConfig,
  metrics::{Metrics, names},
  server::Server,
};

/// GET `path` from a server already listening at `addr`, returning the
/// status code and the response body.
fn get(addr: std::net::SocketAddr, path: &str) -> (u16, String) {
  let mut stream = TcpStream::connect(addr).expect("connect to the exporter");
  write!(stream, "GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n")
    .expect("write the request");
  let mut response = String::new();
  stream
    .read_to_string(&mut response)
    .expect("read the response");

  let status_line = response.lines().next().expect("a status line");
  let code: u16 = status_line
    .split_whitespace()
    .nth(1)
    .expect("a status code")
    .parse()
    .expect("the status code is a number");
  let body = response.split("\r\n\r\n").nth(1).unwrap_or("").to_owned();
  (code, body)
}

#[test]
fn metrics_endpoint_reachable_lists_every_spec_metric_name() {
  let server =
    Server::bind(&PrometheusConfig::loopback(0)).expect("bind the exporter");
  let addr = server.local_addr().expect("the bound address");
  let metrics = Metrics::new().expect("metrics must register");

  let handle = std::thread::spawn(move || server.serve_one(&metrics));
  let (status, body) = get(addr, "/metrics");
  handle
    .join()
    .expect("the server thread")
    .expect("serve_one");

  assert_eq!(status, 200, "GET /metrics must succeed: {body}");
  assert!(!body.is_empty(), "the metrics response must not be empty");
  for name in names::ALL {
    assert!(
      body.contains(name),
      "'{name}' missing from the metrics response:\n{body}"
    );
  }
}

/// A fresh registry (no ticks have run yet) still answers - a scraper
/// that connects before the first tick must see every metric name at
/// zero, not an empty or missing response (spec's Prometheus exporter
/// requirement; see
/// `metrics::tests::a_fresh_registry_still_exposes_every_metric_name_with_zero_values`
/// for the registry-level half of this proof).
#[test]
fn an_untouched_registry_is_still_reachable_and_non_empty() {
  let server =
    Server::bind(&PrometheusConfig::loopback(0)).expect("bind the exporter");
  let addr = server.local_addr().expect("the bound address");
  let metrics = Metrics::new().expect("metrics must register");

  let handle = std::thread::spawn(move || server.serve_one(&metrics));
  let (status, body) = get(addr, "/metrics");
  handle
    .join()
    .expect("the server thread")
    .expect("serve_one");

  assert_eq!(status, 200);
  assert!(!body.is_empty());
}
