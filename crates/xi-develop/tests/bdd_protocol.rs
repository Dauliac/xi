//! BDD: tests/features/20_daemon_protocol.feature
//!
//! Tests for the daemon socket protocol.
//! Pure unit tests — serialization roundtrips, wire format, message limits.

use xi_develop::daemon::protocol::*;

/// BDD: 20_daemon_protocol.feature#Wire format
#[test]
fn wire_format_length_prefixed_json() {
  let req = DaemonRequest::Status;
  let mut buf = Vec::new();
  write_message(&mut buf, &req).unwrap();

  // First 4 bytes are LE length
  let len = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
  // Remaining bytes are JSON
  assert_eq!(buf.len(), 4 + len);

  // Verify JSON is valid
  let json_bytes = &buf[4..];
  let _: DaemonRequest = serde_json::from_slice(json_bytes).unwrap();
}

/// BDD: 20_daemon_protocol.feature#Wire format (roundtrip)
#[test]
fn write_read_roundtrip_all_variants() {
  let requests = vec![
    DaemonRequest::Prompt(PromptRequest {
      consumer_pid: 12345,
      target: "default".into(),
      cwd: "/tmp".into(),
      is_subshell: false,
      parent_pid: None,
    }),
    DaemonRequest::Deregister(DeregisterRequest { consumer_pid: 99 }),
    DaemonRequest::Eval(EvalRequest {
      target: "python".into(),
    }),
    DaemonRequest::Status,
    DaemonRequest::Shutdown,
    DaemonRequest::CachePush(CachePushRequest {
      store_path: "/nix/store/abc-test".into(),
      cache_url: "s3://my-cache".into(),
      push_command: vec!["cachix".into(), "push".into(), "mycache".into()],
      sign_key: Some("/path/to/key".into()),
    }),
  ];

  for req in requests {
    let mut buf = Vec::new();
    write_message(&mut buf, &req).unwrap();
    let mut cursor = std::io::Cursor::new(buf);
    let decoded: DaemonRequest = read_message(&mut cursor).unwrap();

    // Verify variant matches (can't derive PartialEq on all, so check tag)
    let orig_json = serde_json::to_string(&req).unwrap();
    let decoded_json = serde_json::to_string(&decoded).unwrap();
    assert_eq!(orig_json, decoded_json);
  }
}

/// BDD: 20_daemon_protocol.feature#Request/Response types (tagged union)
#[test]
fn request_tagged_union_format() {
  let req = DaemonRequest::Prompt(PromptRequest {
    consumer_pid: 1,
    target: "default".into(),
    cwd: "/tmp".into(),
    is_subshell: false,
    parent_pid: None,
  });
  let json = serde_json::to_string(&req).unwrap();
  assert!(json.contains("\"type\":\"Prompt\""));

  let req = DaemonRequest::Status;
  let json = serde_json::to_string(&req).unwrap();
  assert!(json.contains("\"type\":\"Status\""));

  let req = DaemonRequest::Shutdown;
  let json = serde_json::to_string(&req).unwrap();
  assert!(json.contains("\"type\":\"Shutdown\""));
}

/// BDD: 20_daemon_protocol.feature#Wire format (max message size)
#[test]
fn reject_oversized_message() {
  // Craft a message claiming to be 17MB
  let len_bytes = (17u32 * 1024 * 1024).to_le_bytes();
  let mut buf = Vec::new();
  buf.extend_from_slice(&len_bytes);
  buf.extend_from_slice(b"{}"); // dummy payload (won't be read)

  let mut cursor = std::io::Cursor::new(buf);
  let result: std::io::Result<DaemonRequest> = read_message(&mut cursor);
  assert!(result.is_err());
  let err = result.unwrap_err();
  assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
  assert!(err.to_string().contains("too large"));
}

/// BDD: 20_daemon_protocol.feature#Protocol versioning (DaemonState serde)
#[test]
fn daemon_state_simple_variants_serialize() {
  let states = vec![
    DaemonState::Starting,
    DaemonState::Ready,
    DaemonState::Evaluating,
    DaemonState::WatcherDegraded,
    DaemonState::ShuttingDown,
  ];

  for state in states {
    let json = serde_json::to_string(&state).unwrap();
    let decoded: DaemonState = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded, state);
  }
}

/// BDD: 20_daemon_protocol.feature#StatusResponse includes version
#[test]
fn status_response_has_version() {
  let resp = StatusResponse {
    state: DaemonState::Ready,
    uptime_secs: 120,
    consumer_count: 2,
    current_target: "default".into(),
    package_count: 15,
    version: "4.5.0".into(),
    active_cache_pushes: vec![],
  };

  let json = serde_json::to_string(&resp).unwrap();
  assert!(json.contains("\"version\":\"4.5.0\""));

  let decoded: StatusResponse = serde_json::from_str(&json).unwrap();
  assert_eq!(decoded.version, "4.5.0");
}

/// BDD: 20_daemon_protocol.feature#DaemonState all variants serde roundtrip
#[test]
fn daemon_state_v2_all_variants_serde() {
  let states = vec![
    DaemonState::Starting,
    DaemonState::Evaluating,
    DaemonState::Ready,
    DaemonState::BuildFailed {
      error: "eval failed: attribute 'foo' missing".into(),
      retry_count: 3,
    },
    DaemonState::WatcherDegraded,
    DaemonState::ConfigError {
      error: "invalid .xi.toml: unknown key".into(),
    },
    DaemonState::ShuttingDown,
  ];

  for state in states {
    let json = serde_json::to_string(&state).unwrap();
    let decoded: DaemonState = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded, state);
  }
}

/// BDD: 20_daemon_protocol.feature#PromptRequest serialization roundtrip
#[test]
fn prompt_request_roundtrip() {
  let req = PromptRequest {
    consumer_pid: 42,
    target: "devShells.x86_64-linux.default".into(),
    cwd: "/home/user/project".into(),
    is_subshell: true,
    parent_pid: Some(1000),
  };

  let json = serde_json::to_string(&req).unwrap();
  let decoded: PromptRequest = serde_json::from_str(&json).unwrap();

  assert_eq!(decoded.consumer_pid, 42);
  assert_eq!(decoded.target, "devShells.x86_64-linux.default");
  assert_eq!(decoded.cwd, "/home/user/project");
  assert!(decoded.is_subshell);
  assert_eq!(decoded.parent_pid, Some(1000));

  // Also test with parent_pid = None
  let req_no_parent = PromptRequest {
    consumer_pid: 99,
    target: "default".into(),
    cwd: "/tmp".into(),
    is_subshell: false,
    parent_pid: None,
  };

  let json2 = serde_json::to_string(&req_no_parent).unwrap();
  let decoded2: PromptRequest = serde_json::from_str(&json2).unwrap();

  assert_eq!(decoded2.consumer_pid, 99);
  assert!(!decoded2.is_subshell);
  assert_eq!(decoded2.parent_pid, None);
}

/// BDD: 20_daemon_protocol.feature#PromptResponse serialization roundtrip
#[test]
fn prompt_response_roundtrip() {
  let resp = PromptResponse {
    should_source_env: true,
    env_file_path: Some("/tmp/xi/env-default.sh".into()),
    should_source_hook: true,
    hook_file_path: Some("/tmp/xi/hook-default.sh".into()),
    should_exit: false,
    should_spawn_subshell: true,
    spawn_flake_root: Some("/home/user/project".into()),
    notifications: vec![
      Notification::success("devshell ready"),
      Notification::info("2 packages added"),
      Notification::warn("input 'nixpkgs' is 45 days old"),
    ],
    daemon_state: DaemonState::Ready,
    is_trusted: true,
  };

  let json = serde_json::to_string(&resp).unwrap();
  let decoded: PromptResponse = serde_json::from_str(&json).unwrap();

  assert!(decoded.should_source_env);
  assert_eq!(
    decoded.env_file_path.as_deref(),
    Some("/tmp/xi/env-default.sh")
  );
  assert!(decoded.should_source_hook);
  assert_eq!(
    decoded.hook_file_path.as_deref(),
    Some("/tmp/xi/hook-default.sh")
  );
  assert!(!decoded.should_exit);
  assert!(decoded.should_spawn_subshell);
  assert_eq!(
    decoded.spawn_flake_root.as_deref(),
    Some("/home/user/project")
  );
  assert_eq!(decoded.notifications.len(), 3);
  assert_eq!(decoded.notifications[0].kind, NotifKind::Success);
  assert_eq!(decoded.notifications[1].kind, NotifKind::Info);
  assert_eq!(decoded.notifications[2].kind, NotifKind::Warn);
  assert_eq!(decoded.daemon_state, DaemonState::Ready);
  assert!(decoded.is_trusted);
}

/// BDD: 20_daemon_protocol.feature#DaemonRequest::Prompt tagged union format
#[test]
fn daemon_request_prompt_tagged() {
  let req = DaemonRequest::Prompt(PromptRequest {
    consumer_pid: 7,
    target: "default".into(),
    cwd: "/home/user".into(),
    is_subshell: false,
    parent_pid: None,
  });

  let json = serde_json::to_string(&req).unwrap();
  assert!(json.contains("\"type\":\"Prompt\""));
  assert!(json.contains("\"consumer_pid\":7"));
  assert!(json.contains("\"target\":\"default\""));

  let decoded: DaemonRequest = serde_json::from_str(&json).unwrap();
  assert!(matches!(decoded, DaemonRequest::Prompt(_)));
}

/// BDD: 20_daemon_protocol.feature#DaemonRequest::Deregister tagged union format
#[test]
fn daemon_request_deregister_tagged() {
  let req = DaemonRequest::Deregister(DeregisterRequest { consumer_pid: 55 });

  let json = serde_json::to_string(&req).unwrap();
  assert!(json.contains("\"type\":\"Deregister\""));
  assert!(json.contains("\"consumer_pid\":55"));

  let decoded: DaemonRequest = serde_json::from_str(&json).unwrap();
  assert!(matches!(decoded, DaemonRequest::Deregister(_)));
}

/// BDD: 20_daemon_protocol.feature#DeregisterRequest/Response roundtrip
#[test]
fn deregister_roundtrip() {
  // Request
  let req = DeregisterRequest { consumer_pid: 123 };
  let json = serde_json::to_string(&req).unwrap();
  let decoded_req: DeregisterRequest = serde_json::from_str(&json).unwrap();
  assert_eq!(decoded_req.consumer_pid, 123);

  // Response
  let resp = DeregisterResponse {
    was_registered: true,
    remaining_consumers: 4,
  };
  let json = serde_json::to_string(&resp).unwrap();
  let decoded_resp: DeregisterResponse = serde_json::from_str(&json).unwrap();
  assert!(decoded_resp.was_registered);
  assert_eq!(decoded_resp.remaining_consumers, 4);

  // Response with was_registered = false
  let resp2 = DeregisterResponse {
    was_registered: false,
    remaining_consumers: 0,
  };
  let json2 = serde_json::to_string(&resp2).unwrap();
  let decoded_resp2: DeregisterResponse = serde_json::from_str(&json2).unwrap();
  assert!(!decoded_resp2.was_registered);
  assert_eq!(decoded_resp2.remaining_consumers, 0);
}

/// BDD: 20_daemon_protocol.feature#PromptRequest/Response wire format roundtrip
#[test]
fn prompt_wire_format_roundtrip() {
  // Write a DaemonRequest::Prompt through the wire format
  let req = DaemonRequest::Prompt(PromptRequest {
    consumer_pid: 42,
    target: "devShells.x86_64-linux.default".into(),
    cwd: "/home/user/project".into(),
    is_subshell: true,
    parent_pid: Some(1000),
  });

  let mut buf = Vec::new();
  write_message(&mut buf, &req).unwrap();
  let mut cursor = std::io::Cursor::new(&buf);
  let decoded: DaemonRequest = read_message(&mut cursor).unwrap();

  // Verify JSON equality (since DaemonRequest doesn't impl PartialEq)
  let orig_json = serde_json::to_string(&req).unwrap();
  let decoded_json = serde_json::to_string(&decoded).unwrap();
  assert_eq!(orig_json, decoded_json);

  // Write a DaemonResponse::Prompt through the wire format
  let resp = DaemonResponse::Prompt(PromptResponse {
    should_source_env: true,
    env_file_path: Some("/tmp/xi/env.sh".into()),
    should_source_hook: false,
    hook_file_path: None,
    should_exit: false,
    should_spawn_subshell: false,
    spawn_flake_root: None,
    notifications: vec![Notification::loading("evaluating devshell...")],
    daemon_state: DaemonState::Evaluating,
    is_trusted: true,
  });

  let mut buf2 = Vec::new();
  write_message(&mut buf2, &resp).unwrap();
  let mut cursor2 = std::io::Cursor::new(&buf2);
  let decoded_resp: DaemonResponse = read_message(&mut cursor2).unwrap();

  let orig_resp_json = serde_json::to_string(&resp).unwrap();
  let decoded_resp_json = serde_json::to_string(&decoded_resp).unwrap();
  assert_eq!(orig_resp_json, decoded_resp_json);

  // Write a DaemonResponse::Deregister through the wire format
  let dereg_resp = DaemonResponse::Deregister(DeregisterResponse {
    was_registered: true,
    remaining_consumers: 2,
  });

  let mut buf3 = Vec::new();
  write_message(&mut buf3, &dereg_resp).unwrap();
  let mut cursor3 = std::io::Cursor::new(&buf3);
  let decoded_dereg: DaemonResponse = read_message(&mut cursor3).unwrap();

  let orig_dereg_json = serde_json::to_string(&dereg_resp).unwrap();
  let decoded_dereg_json = serde_json::to_string(&decoded_dereg).unwrap();
  assert_eq!(orig_dereg_json, decoded_dereg_json);
}
