Feature: Daemon socket protocol
  As the xi develop system
  I want a well-defined protocol between the CLI and daemon
  So that communication is reliable and extensible

  Scenario: Wire format
    Then the daemon uses Unix domain sockets
    And the wire format is: 4-byte little-endian length prefix + JSON payload
    And the maximum message size is 16MB
    And messages exceeding 16MB are rejected with an error

  Scenario: Request/Response types
    Then the protocol supports these request types:
      | Request    | Purpose                                          |
      | Prompt     | Shell prompt hook: get state, notifications       |
      | Eval       | Trigger synchronous evaluation                   |
      | CachePush  | Request background cache push                    |
      | Status     | Get daemon state and stats                       |
      | Deregister | Shell exiting: remove consumer, decrement count  |
      | Shutdown   | Request graceful daemon shutdown                 |

  Scenario: PromptRequest contains shell context
    Then a PromptRequest includes:
      | Field        | Type   | Description                        |
      | consumer_pid | u32    | Shell PID for notification routing  |
      | target       | String | devShell attribute name             |
      | cwd          | String | Current working directory           |
      | is_subshell  | bool   | Whether this is a subshell prompt   |
      | parent_pid   | Option | Parent shell PID (for nesting tree) |

  Scenario: PromptResponse contains structured actions
    Then a PromptResponse includes:
      | Field              | Type          | Description                           |
      | should_source_env  | bool          | Whether env file changed for this PID |
      | env_file_path      | Option<String>| Path to source                        |
      | should_source_hook | bool          | Whether hook file changed             |
      | hook_file_path     | Option<String>| Path to source                        |
      | should_exit        | bool          | Shell should exit (left flake/untrust)|
      | should_spawn       | Option<SpawnInfo> | Spawn a nested subshell           |
      | notifications      | Vec<Notification> | Pending notifications for this PID|
      | daemon_state       | DaemonState   | Current daemon state                  |
      | is_trusted         | bool          | Trust status for the flake            |
    And the Rust binary converts this structured response into shell code
    And the daemon NEVER generates shell code

  Scenario: Connection timeouts
    Then the client uses these timeouts:
      | Timeout   | Duration | Purpose                            |
      | connect   | 500ms    | Detect unresponsive daemon         |
      | read      | 5s       | Wait for response                  |
      | write     | 5s       | Send request                       |
    And timeout triggers daemon restart logic (not a hard failure)

  Scenario: Protocol versioning
    Then the DaemonRequest and DaemonResponse use serde tagged unions
    And the "type" field identifies the request/response variant
    And unknown variants are deserialized as errors (not panics)
    And the StatusResponse includes a "version" field for compatibility checks
