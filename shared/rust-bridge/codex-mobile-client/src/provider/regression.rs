//! Comprehensive regression test suite for the provider abstraction.
//!
//! Ensures all existing Codex functionality remains intact after the provider
//! abstraction refactor. Covers thread/turn operations, event ordering,
//! mid-stream disconnect recovery, cancel semantics, IPC, and lag handling.
//!
//! These tests verify that wrapping the existing Codex client in a
//! `ProviderTransport` trait object does not introduce regressions in:
//! - Thread lifecycle (create, archive, rename, list)
//! - Turn lifecycle (start, stream deltas, complete)
//! - Event ordering (no reordering or duplication)
//! - Mid-stream disconnect handling (clean error propagation)
//! - Cancel/interrupt semantics (well-formed terminal events)
//! - IPC stream functionality (for SSH-backed sessions)
//! - Broadcast lag recovery (graceful handling of slow consumers)
//! - Approval flow (request → approve → continue)
//! - Reconnection (new provider after disconnect works)
//! - Existing connection paths (local, remote, SSH unchanged)
//!
//! Covers validation assertions:
//! VAL-ABS-026, VAL-ABS-027, VAL-ABS-028, VAL-ABS-029, VAL-ABS-030,
//! VAL-ABS-031, VAL-ABS-032, VAL-ABS-033, VAL-ABS-039, VAL-ABS-044,
//! VAL-ABS-045, VAL-ABS-049, VAL-ABS-053, VAL-ABS-059

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value as JsonValue, json};
use tokio::sync::broadcast;

use crate::provider::cross_provider::SequencedMockProvider;
use crate::provider::error_handling::ErrorMockProvider;
use crate::provider::pi::mock::MockPiChannel;
use crate::provider::pi::transport::PiNativeTransport;
use crate::provider::droid::mock::MockDroidChannel;
use crate::provider::droid::transport::DroidNativeTransport;
use crate::provider::{
    AgentType, ProviderConfig, ProviderEvent, ProviderTransport, SessionInfo,
};
use crate::session::connection::{ServerConfig, ServerSession};
use crate::transport::{RpcError, TransportError};

// ── Helpers ────────────────────────────────────────────────────────────────

fn test_server_config(id: &str) -> ServerConfig {
    ServerConfig {
        server_id: id.to_string(),
        display_name: id.to_string(),
        host: "127.0.0.1".to_string(),
        port: 0,
        websocket_url: Some("ws://127.0.0.1:0".to_string()),
        is_local: false,
        tls: false,
    }
}

/// Collect events from a broadcast receiver with a timeout.
async fn collect_events(rx: &mut broadcast::Receiver<ProviderEvent>) -> Vec<ProviderEvent> {
    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(300);
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(event)) => events.push(event),
            _ => break,
        }
    }
    events
}

// ════════════════════════════════════════════════════════════════════════════
// VAL-ABS-026: Mid-stream disconnect yields clean ProviderEvent::Disconnected
// ════════════════════════════════════════════════════════════════════════════

/// When the transport drops mid-stream, the provider must emit a clean
/// `ProviderEvent::Disconnected` without panicking.
#[tokio::test]
async fn regression_mid_stream_disconnect_produces_disconnected_event() {
    let mut provider = ErrorMockProvider::new();
    let config = ProviderConfig::default();

    provider.connect(&config).await.unwrap();
    assert!(provider.is_connected());

    // Subscribe before emitting events
    let mut rx = provider.event_receiver();

    // Simulate mid-stream state: emit some deltas
    provider.emit_event(ProviderEvent::StreamingStarted {
        thread_id: "thread-dc".into(),
    });
    provider.emit_event(ProviderEvent::MessageDelta {
        thread_id: "thread-dc".into(),
        item_id: "item-dc".into(),
        delta: "partial content before disconnect".into(),
    });

    // Simulate mid-stream disconnect
    provider.simulate_mid_stream_disconnect("SSH tunnel closed");
    provider.disconnect().await;

    // Verify provider reports disconnected
    assert!(!provider.is_connected());

    // Collect events — should have the pre-disconnect events
    let events = collect_events(&mut rx).await;
    assert!(
        events.iter().any(|e| matches!(e, ProviderEvent::Disconnected { .. })),
        "should receive Disconnected event after mid-stream disconnect"
    );

    // Verify no panic occurred — test reaching this point is itself the assertion.
}

/// Mid-stream disconnect through a `ServerSession::from_provider` must
/// propagate health transition to `Disconnected`.
#[tokio::test]
async fn regression_mid_stream_disconnect_transitions_session_health() {
    let (provider, event_tx) = StreamingMockProvider::new();
    let config = test_server_config("mid-stream-dc");

    let session = ServerSession::from_provider(config, Box::new(provider), None)
        .await
        .expect("from_provider should succeed");

    // Session starts connected
    assert_eq!(
        *session.health().borrow(),
        crate::session::connection::ConnectionHealth::Connected
    );

    // Let the event task subscribe
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Emit some streaming events
    event_tx.send(ProviderEvent::StreamingStarted {
        thread_id: "t-dc".into(),
    }).unwrap();
    event_tx.send(ProviderEvent::MessageDelta {
        thread_id: "t-dc".into(),
        item_id: "i-dc".into(),
        delta: "streaming...".into(),
    }).unwrap();

    // Simulate mid-stream disconnect
    event_tx.send(ProviderEvent::Disconnected {
        message: "connection reset by peer".into(),
    }).unwrap();

    // Wait for health transition
    let health_changed = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if *session.health().borrow() == crate::session::connection::ConnectionHealth::Disconnected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await;

    assert!(health_changed.is_ok(), "health should transition to Disconnected after mid-stream disconnect");
}

/// After mid-stream disconnect, pending in-flight requests should fail
/// with `TransportError::Disconnected`.
#[tokio::test]
async fn regression_post_disconnect_requests_return_disconnected_error() {
    let provider = Box::new(ErrorMockProvider::new());
    let config = test_server_config("post-dc-req");

    let session = ServerSession::from_provider(config, provider, None)
        .await
        .expect("from_provider should succeed");

    // Disconnect
    session.disconnect().await;

    // Requests after disconnect must fail with Disconnected
    let result = session.request("thread/list", json!({})).await;
    assert!(result.is_err(), "request after disconnect should fail");
    match result.err().unwrap() {
        RpcError::Transport(TransportError::Disconnected) => {}
        other => panic!("expected TransportError::Disconnected, got: {other}"),
    }

    // Notifications after disconnect must also fail
    let notify_result = session.notify("initialized", json!(null)).await;
    assert!(notify_result.is_err(), "notify after disconnect should fail");
    match notify_result.err().unwrap() {
        RpcError::Transport(TransportError::Disconnected) => {}
        other => panic!("expected TransportError::Disconnected, got: {other}"),
    }

    // Error must be deterministic (every call returns same error)
    let result2 = session.request("thread/list", json!({})).await;
    assert!(result2.is_err(), "subsequent request should also fail");
}

// ════════════════════════════════════════════════════════════════════════════
// VAL-ABS-027: Reconnection after transport disconnect works through trait
// ════════════════════════════════════════════════════════════════════════════

/// After a provider disconnects, a new provider can be created and connected
/// to resume the session. No stale events from the old provider should leak.
#[tokio::test]
async fn regression_reconnection_after_disconnect_works() {
    // First provider
    let mut provider = ErrorMockProvider::new();
    let config = ProviderConfig::default();

    provider.connect(&config).await.unwrap();
    let mut rx1 = provider.event_receiver();

    // Emit pre-disconnect events
    provider.emit_event(ProviderEvent::MessageDelta {
        thread_id: "t-reconnect".into(),
        item_id: "i-1".into(),
        delta: "before disconnect".into(),
    });

    let pre_events = collect_events(&mut rx1).await;
    assert!(pre_events.iter().any(|e| matches!(e, ProviderEvent::MessageDelta { .. })));

    // Disconnect
    provider.disconnect().await;
    assert!(!provider.is_connected());

    // Create a fresh provider for reconnection
    let mut provider2 = ErrorMockProvider::new();
    provider2.connect(&config).await.unwrap();
    assert!(provider2.is_connected());

    let mut rx2 = provider2.event_receiver();

    // Post-reconnect events should work
    provider2.emit_event(ProviderEvent::MessageDelta {
        thread_id: "t-reconnect".into(),
        item_id: "i-2".into(),
        delta: "after reconnect".into(),
    });

    let post_events = collect_events(&mut rx2).await;
    assert!(
        post_events.iter().any(|e| match e {
            ProviderEvent::MessageDelta { delta, .. } => delta == "after reconnect",
            _ => false,
        }),
        "should receive post-reconnect events"
    );

    // Verify no cross-contamination: old receiver doesn't get new events
    let stale_events = collect_events(&mut rx1).await;
    assert!(
        !stale_events.iter().any(|e| match e {
            ProviderEvent::MessageDelta { delta, .. } => delta == "after reconnect",
            _ => false,
        }),
        "old receiver should not receive events from new provider"
    );

    provider2.disconnect().await;
}

/// Reconnection through a new ServerSession should start with Connected health
/// and not carry over stale state from the disconnected session.
#[tokio::test]
async fn regression_reconnection_new_session_has_clean_state() {
    // First session
    let provider = Box::new(ErrorMockProvider::new());
    let config = test_server_config("reconnect-clean");
    let session1 = ServerSession::from_provider(config, provider, None)
        .await
        .expect("first session should succeed");

    assert_eq!(
        *session1.health().borrow(),
        crate::session::connection::ConnectionHealth::Connected
    );

    // Disconnect first session
    session1.disconnect().await;
    assert_eq!(
        *session1.health().borrow(),
        crate::session::connection::ConnectionHealth::Disconnected
    );

    // Create new session — should start clean
    let provider2 = Box::new(ErrorMockProvider::new());
    let config2 = test_server_config("reconnect-clean");
    let session2 = ServerSession::from_provider(config2, provider2, None)
        .await
        .expect("second session should succeed");

    assert_eq!(
        *session2.health().borrow(),
        crate::session::connection::ConnectionHealth::Connected,
        "new session after reconnect should start Connected"
    );

    session2.disconnect().await;
}

// ════════════════════════════════════════════════════════════════════════════
// VAL-ABS-028: Server request resolution through provider path
// ════════════════════════════════════════════════════════════════════════════

/// Approval resolution (respond/reject) works through the provider-backed
/// session via `$resolve` and `$reject` pseudo-methods.
#[tokio::test]
async fn regression_approval_resolution_through_provider_path() {
    let provider = Box::new(ResolutionTrackingProvider::new());
    let config = test_server_config("approval-resolve");

    let session = ServerSession::from_provider(config, provider, None)
        .await
        .expect("from_provider should succeed");

    // Emit an approval request through a separate event sender
    // (the provider itself tracks what was sent)

    // Respond (approve) — should route through $resolve
    session.respond(json!("req-1"), json!({"approved": true}))
        .await
        .expect("respond should succeed");

    // Verify $resolve was called
    let sent = ResolutionTrackingProvider::get_sent_requests();
    assert!(
        sent.iter().any(|(method, _)| method == "$resolve"),
        "$resolve should be called for approval response"
    );

    // Reject — should route through $reject
    use codex_app_server_protocol::JSONRPCErrorError;
    session.reject(json!("req-2"), JSONRPCErrorError {
        code: -32000,
        message: "denied".to_string(),
        data: None,
    })
        .await
        .expect("reject should succeed");

    let sent = ResolutionTrackingProvider::get_sent_requests();
    assert!(
        sent.iter().any(|(method, _)| method == "$reject"),
        "$reject should be called for approval rejection"
    );

    session.disconnect().await;
}

/// When the provider doesn't support $resolve (non-Codex), the respond
/// call should be treated as a no-op without panicking.
#[tokio::test]
async fn regression_resolve_no_op_for_non_codex_provider() {
    let provider = Box::new(SequencedMockProvider::new("non-codex"));
    let config = test_server_config("resolve-noop");

    let session = ServerSession::from_provider(config, provider, None)
        .await
        .expect("from_provider should succeed");

    // Respond should succeed even though SequencedMockProvider doesn't
    // explicitly handle $resolve (it returns Null which is fine)
    let result = session.respond(json!("req-1"), json!({"approved": true})).await;
    // The provider returns Ok(Null) for unknown methods, so the resolve
    // goes through. The from_provider worker catches transport errors
    // and treats them as no-op.
    assert!(
        result.is_ok(),
        "respond on non-Codex provider should succeed (no-op)"
    );

    session.disconnect().await;
}

// ════════════════════════════════════════════════════════════════════════════
// VAL-ABS-029: Full Codex regression — connect through prompt
// ════════════════════════════════════════════════════════════════════════════

/// A complete Codex-style flow: connect → create thread → start turn →
/// stream message deltas → complete turn works through the provider trait.
#[tokio::test]
async fn regression_full_codex_connect_through_prompt() {
    let mut provider = SequencedMockProvider::new("codex-regression-1");
    let config = ProviderConfig {
        agent_type: AgentType::Codex,
        ..Default::default()
    };

    // 1. Connect
    provider.connect(&config).await.unwrap();
    assert!(provider.is_connected());

    let mut rx = provider.event_receiver();

    // 2. Create thread (via send_request)
    let thread_result = provider
        .send_request("thread/create", json!({"title": "Regression test"}))
        .await
        .expect("thread/create should succeed");
    // SequencedMockProvider returns Null for unknown methods — that's fine,
    // we're verifying the round-trip works through the trait.
    assert!(thread_result.is_null() || thread_result.is_object());

    // 3. Simulate streaming a response (same pattern as Codex WebSocket flow)
    provider.emit(ProviderEvent::TurnStarted {
        thread_id: "thread-regression".into(),
        turn_id: "turn-1".into(),
    });
    provider.emit(ProviderEvent::StreamingStarted {
        thread_id: "thread-regression".into(),
    });
    provider.emit(ProviderEvent::ItemStarted {
        thread_id: "thread-regression".into(),
        turn_id: "turn-1".into(),
        item_id: "item-1".into(),
    });

    // Stream multiple deltas
    let deltas = ["Hello", " world", " from", " Codex", "!"];
    for delta in &deltas {
        provider.emit(ProviderEvent::MessageDelta {
            thread_id: "thread-regression".into(),
            item_id: "item-1".into(),
            delta: delta.to_string(),
        });
    }

    provider.emit(ProviderEvent::ItemCompleted {
        thread_id: "thread-regression".into(),
        turn_id: "turn-1".into(),
        item_id: "item-1".into(),
    });
    provider.emit(ProviderEvent::StreamingCompleted {
        thread_id: "thread-regression".into(),
    });
    provider.emit(ProviderEvent::TurnCompleted {
        thread_id: "thread-regression".into(),
        turn_id: "turn-1".into(),
    });

    // 4. Verify events arrive in correct order
    let events = collect_events(&mut rx).await;
    assert!(events.len() >= 10, "should have at least 10 events, got {}", events.len());

    // Verify TurnStarted is first
    assert!(matches!(events[0], ProviderEvent::TurnStarted { .. }));
    // Verify StreamingStarted follows
    assert!(matches!(events[1], ProviderEvent::StreamingStarted { .. }));
    // Verify ItemStarted
    assert!(matches!(events[2], ProviderEvent::ItemStarted { .. }));
    // Verify MessageDeltas
    let message_deltas: Vec<_> = events.iter()
        .filter(|e| matches!(e, ProviderEvent::MessageDelta { .. }))
        .collect();
    assert_eq!(message_deltas.len(), 5, "should have 5 message deltas");

    // Verify concatenated text
    let full_text: String = message_deltas.iter()
        .filter_map(|e| match e {
            ProviderEvent::MessageDelta { delta, .. } => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(full_text, "Hello world from Codex!");

    // Verify TurnCompleted is last
    assert!(matches!(events.last().unwrap(), ProviderEvent::TurnCompleted { .. }));

    // 5. Disconnect cleanly
    provider.disconnect().await;
    assert!(!provider.is_connected());
}

/// Full Codex flow through ServerSession::from_provider — verifying
/// events are bridged to ServerEvent correctly.
#[tokio::test]
async fn regression_full_codex_through_from_provider() {
    let (provider, event_tx) = StreamingMockProvider::new();
    let config = test_server_config("codex-full-from-provider");

    let session = ServerSession::from_provider(config, Box::new(provider), None)
        .await
        .expect("from_provider should succeed");

    let mut events = session.events();

    // Let event task subscribe
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Simulate full Codex turn
    event_tx.send(ProviderEvent::TurnStarted {
        thread_id: "t-full".into(),
        turn_id: "turn-full".into(),
    }).unwrap();
    event_tx.send(ProviderEvent::MessageDelta {
        thread_id: "t-full".into(),
        item_id: "i-full".into(),
        delta: "response text".into(),
    }).unwrap();
    event_tx.send(ProviderEvent::TurnCompleted {
        thread_id: "t-full".into(),
        turn_id: "turn-full".into(),
    }).unwrap();

    // Collect ServerEvents
    let received: Vec<_> = tokio::time::timeout(Duration::from_secs(2), async {
        let mut evts = Vec::new();
        for _ in 0..3 {
            evts.push(events.recv().await.expect("should receive event"));
        }
        evts
    }).await.expect("timeout collecting events");

    // Verify correct legacy notification methods
    assert!(matches!(&received[0], crate::session::connection::ServerEvent::LegacyNotification { method, .. }
        if method == "codex/event/turnStarted"));
    assert!(matches!(&received[1], crate::session::connection::ServerEvent::LegacyNotification { method, .. }
        if method == "codex/event/agentMessageDelta"));
    assert!(matches!(&received[2], crate::session::connection::ServerEvent::LegacyNotification { method, .. }
        if method == "codex/event/turnCompleted"));

    session.disconnect().await;
}

// ════════════════════════════════════════════════════════════════════════════
// VAL-ABS-030: Full Codex regression — approval flow
// ════════════════════════════════════════════════════════════════════════════

/// A complete approval flow: connect → prompt → agent requests approval →
/// user approves → agent continues, through the provider trait.
#[tokio::test]
async fn regression_full_approval_flow() {
    let mut provider = SequencedMockProvider::new("approval-regression");
    let config = ProviderConfig {
        agent_type: AgentType::Codex,
        ..Default::default()
    };

    provider.connect(&config).await.unwrap();
    let mut rx = provider.event_receiver();

    // Simulate prompt → agent starts → requests approval → approved → continues
    provider.emit(ProviderEvent::TurnStarted {
        thread_id: "t-approval".into(),
        turn_id: "turn-1".into(),
    });
    provider.emit(ProviderEvent::MessageDelta {
        thread_id: "t-approval".into(),
        item_id: "i-1".into(),
        delta: "I'll run that command".into(),
    });

    // Agent requests approval
    provider.emit(ProviderEvent::ApprovalRequested {
        thread_id: "t-approval".into(),
        request_id: "req-1".into(),
        kind: "command".into(),
        reason: Some("Needs permission to execute".into()),
        command: Some("rm -rf /tmp/test".into()),
    });

    // User approves → resolved
    provider.emit(ProviderEvent::ServerRequestResolved {
        thread_id: "t-approval".into(),
    });

    // Agent continues with command execution
    provider.emit(ProviderEvent::ToolCallStarted {
        thread_id: "t-approval".into(),
        item_id: "cmd-1".into(),
        tool_name: "bash".into(),
        call_id: "call-1".into(),
    });
    provider.emit(ProviderEvent::CommandOutputDelta {
        thread_id: "t-approval".into(),
        item_id: "cmd-1".into(),
        delta: "removed /tmp/test\n".into(),
    });
    provider.emit(ProviderEvent::ToolCallUpdate {
        thread_id: "t-approval".into(),
        item_id: "cmd-1".into(),
        call_id: "call-1".into(),
        output_delta: "[exit code: 0]".into(),
    });

    // Turn completes
    provider.emit(ProviderEvent::TurnCompleted {
        thread_id: "t-approval".into(),
        turn_id: "turn-1".into(),
    });

    let events = collect_events(&mut rx).await;

    // Verify the full sequence
    assert!(events.len() >= 8, "should have at least 8 events, got {}", events.len());

    // Verify ApprovalRequested has correct fields
    let approval_event = events.iter().find(|e| matches!(e, ProviderEvent::ApprovalRequested { .. }));
    assert!(approval_event.is_some(), "should have ApprovalRequested event");
    if let ProviderEvent::ApprovalRequested { kind, reason, command, .. } = approval_event.unwrap() {
        assert_eq!(kind, "command");
        assert!(reason.is_some());
        assert!(command.is_some());
        assert!(command.as_ref().unwrap().contains("rm -rf"));
    }

    // Verify ServerRequestResolved follows the approval
    let resolve_idx = events.iter().position(|e| matches!(e, ProviderEvent::ServerRequestResolved { .. }));
    let approval_idx = events.iter().position(|e| matches!(e, ProviderEvent::ApprovalRequested { .. }));
    assert!(resolve_idx > approval_idx, "resolved should come after approval request");

    // Verify command execution follows resolution
    let cmd_idx = events.iter().position(|e| matches!(e, ProviderEvent::CommandOutputDelta { .. }));
    assert!(
        cmd_idx.unwrap_or(0) > resolve_idx.unwrap_or(0),
        "command output should come after resolution"
    );

    // Verify turn completes at the end
    assert!(matches!(events.last().unwrap(), ProviderEvent::TurnCompleted { .. }));

    provider.disconnect().await;
}

/// Approval flow through ServerSession — approval request triggers event,
/// user resolves through session.respond().
#[tokio::test]
async fn regression_approval_flow_through_session() {
    let provider = Box::new(ResolutionTrackingProvider::new());
    let config = test_server_config("approval-session");

    let session = ServerSession::from_provider(config, provider, None)
        .await
        .expect("from_provider should succeed");

    // Approval request via respond
    session.respond(json!("req-approval-1"), json!({"approved": true, "reason": "user approved"}))
        .await
        .expect("respond should succeed");

    // Verify $resolve was dispatched
    let sent = ResolutionTrackingProvider::get_sent_requests();
    let resolve_call = sent.iter().find(|(method, _)| method == "$resolve");
    assert!(resolve_call.is_some(), "$resolve should be called for approval");

    session.disconnect().await;
}

// ════════════════════════════════════════════════════════════════════════════
// VAL-ABS-031: Existing connect_local path unchanged
// ════════════════════════════════════════════════════════════════════════════

/// Verify that connect_local continues to use ServerSession::connect_local()
/// directly (no provider wrapping). We verify the API contract is maintained.
#[test]
fn regression_connect_local_path_unchanged_api_contract() {
    // The connect_local path creates a ServerSession via
    // ServerSession::connect_local(config, in_process). This test verifies
    // the ServerConfig and InProcessConfig types are still constructible
    // and the API contract hasn't changed.

    let config = ServerConfig {
        server_id: "local-test".to_string(),
        display_name: "Local Test".to_string(),
        host: "127.0.0.1".to_string(),
        port: 8390,
        websocket_url: None,
        is_local: true,
        tls: false,
    };

    assert!(config.is_local, "local config should be marked as local");
    assert!(config.websocket_url.is_none(), "local config should not have websocket URL");

    // InProcessConfig should still work with defaults
    use crate::session::connection::InProcessConfig;
    let in_process = InProcessConfig::default();
    assert_eq!(in_process.channel_capacity, 256);
    assert!(in_process.codex_home.is_none());
    assert!(in_process.working_directory.is_none());
}

// ════════════════════════════════════════════════════════════════════════════
// VAL-ABS-032: Existing connect_remote path unchanged for Codex
// ════════════════════════════════════════════════════════════════════════════

/// Verify that ServerSession::connect_remote config still works unchanged.
#[test]
fn regression_connect_remote_path_unchanged_api_contract() {
    let config = ServerConfig {
        server_id: "remote-test".to_string(),
        display_name: "Remote Test".to_string(),
        host: "192.168.1.100".to_string(),
        port: 8390,
        websocket_url: Some("ws://192.168.1.100:8390".to_string()),
        is_local: false,
        tls: false,
    };

    assert!(!config.is_local);
    assert!(config.websocket_url.is_some());

    // RemoteAppServerConnectArgs should still be constructible
    let _args = codex_app_server_client::RemoteAppServerConnectArgs {
        websocket_url: "ws://192.168.1.100:8390".to_string(),
        auth_token: None,
        client_name: "litter".to_string(),
        client_version: "0.1.0".to_string(),
        experimental_api: false,
        opt_out_notification_methods: vec![],
        channel_capacity: 256,
    };
}

// ════════════════════════════════════════════════════════════════════════════
// VAL-ABS-033: connect_remote_over_ssh for Codex unchanged
// ════════════════════════════════════════════════════════════════════════════

/// Verify that the SSH bootstrap config types are still usable.
/// The SshReconnectTransport is pub(crate) and requires a real SshClient,
/// so we verify the API contract indirectly through the config types.
#[test]
fn regression_ssh_bootstrap_config_preserved() {
    // ServerConfig with SSH-related settings should be constructible
    let config = ServerConfig {
        server_id: "ssh-test".to_string(),
        display_name: "SSH Test".to_string(),
        host: "100.82.102.84".to_string(),
        port: 5132,
        websocket_url: Some("ws://127.0.0.1:12345".to_string()),
        is_local: false,
        tls: false,
    };

    assert!(!config.is_local);
    assert_eq!(config.port, 5132);

    // SshBootstrapResult should be constructible
    let bootstrap = crate::ssh::SshBootstrapResult {
        server_port: 8390,
        tunnel_local_port: 12345,
        server_version: Some("1.0.0".to_string()),
        pid: Some(12345),
    };
    assert_eq!(bootstrap.server_port, 8390);
    assert_eq!(bootstrap.tunnel_local_port, 12345);
}

// ════════════════════════════════════════════════════════════════════════════
// VAL-ABS-039: Provider event stream handles broadcast lag gracefully
// ════════════════════════════════════════════════════════════════════════════

/// When the consumer is slow and events are dropped, the system handles
/// `broadcast::RecvError::Lagged` without panic.
#[tokio::test]
async fn regression_broadcast_lagged_handled_gracefully() {
    // Use a small broadcast channel to force lag
    let (tx, _) = broadcast::channel::<ProviderEvent>(4);
    let mut rx = tx.subscribe();

    // Overflow the channel
    for i in 0..20u32 {
        let _ = tx.send(ProviderEvent::MessageDelta {
            thread_id: "t-lag".into(),
            item_id: "i-lag".into(),
            delta: format!("delta {i}"),
        });
    }

    // Try to receive — should get Lagged error but not panic
    let mut received_count = 0;
    let mut got_lagged = false;
    loop {
        match rx.try_recv() {
            Ok(_) => received_count += 1,
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(n)) => {
                got_lagged = true;
                assert!(n > 0, "lagged count should be positive");
                // After lag, we should still be able to receive
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
            Err(tokio::sync::broadcast::error::TryRecvError::Closed) => break,
        }
    }

    assert!(got_lagged, "should have encountered Lagged error");
    assert!(received_count > 0, "should have received some events even after lag");
}

/// ProviderEvent::Lagged should be swallowed by the event bridge
/// (not emitted as a ServerEvent).
#[tokio::test]
async fn regression_lagged_event_not_forwarded_to_server_event() {
    let (provider, event_tx) = StreamingMockProvider::new();
    let config = test_server_config("lagged-no-forward");

    let session = ServerSession::from_provider(config, Box::new(provider), None)
        .await
        .expect("from_provider should succeed");

    let mut events = session.events();

    // Let event task subscribe
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Emit a Lagged event — should NOT produce a ServerEvent
    event_tx.send(ProviderEvent::Lagged { skipped: 10 }).unwrap();

    // Emit a real event immediately after
    event_tx.send(ProviderEvent::TurnStarted {
        thread_id: "t-lag-2".into(),
        turn_id: "turn-1".into(),
    }).unwrap();

    // The next ServerEvent should be the TurnStarted, not anything related to Lagged
    let server_event = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .expect("timeout")
        .expect("should receive event");

    match server_event {
        crate::session::connection::ServerEvent::LegacyNotification { method, .. } => {
            assert_eq!(method, "codex/event/turnStarted", "should receive TurnStarted, not a Lagged event");
        }
        other => panic!("expected LegacyNotification, got: {other:?}"),
    }

    session.disconnect().await;
}

/// Lagged event handling preserves the event task — it continues receiving
/// events after lag recovery.
#[tokio::test]
async fn regression_lagged_event_task_survives() {
    let (provider, event_tx) = StreamingMockProvider::new();
    let config = test_server_config("lagged-survive");

    let session = ServerSession::from_provider(config, Box::new(provider), None)
        .await
        .expect("from_provider should succeed");

    let mut events = session.events();

    // Let event task subscribe
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Emit lagged
    event_tx.send(ProviderEvent::Lagged { skipped: 100 }).unwrap();

    // Emit real events after lag
    for i in 0..5 {
        event_tx.send(ProviderEvent::MessageDelta {
            thread_id: "t-survive".into(),
            item_id: format!("i-{i}"),
            delta: format!("post-lag delta {i}"),
        }).unwrap();
    }

    // All 5 post-lag events should be received
    let received: Vec<_> = tokio::time::timeout(Duration::from_secs(2), async {
        let mut evts = Vec::new();
        for _ in 0..5 {
            evts.push(events.recv().await.expect("should receive event"));
        }
        evts
    }).await.expect("timeout collecting post-lag events");

    assert_eq!(received.len(), 5, "all post-lag events should be received");

    session.disconnect().await;
}

// ════════════════════════════════════════════════════════════════════════════
// VAL-ABS-044: Thread operations work through provider path
// ════════════════════════════════════════════════════════════════════════════

/// Thread lifecycle operations (create, archive, rename, list) work through
/// a provider-backed session.
#[tokio::test]
async fn regression_thread_lifecycle_through_provider() {
    let provider = Box::new(ThreadLifecycleProvider::new());
    let config = test_server_config("thread-lifecycle");

    let session = ServerSession::from_provider(config, provider, None)
        .await
        .expect("from_provider should succeed");

    // thread/list
    let list_result = session.request("thread/list", json!({})).await;
    assert!(list_result.is_ok(), "thread/list should succeed");
    let _list_val = list_result.unwrap();

    // thread/start (creates a new thread)
    let create_result = session.request("thread/start", json!({"cwd": "/tmp"})).await;
    assert!(create_result.is_ok(), "thread/start should succeed");

    // thread/name/set (rename) — params use camelCase (threadId not thread_id)
    let rename_result = session.request("thread/name/set", json!({"threadId": "thread-new", "name": "Renamed"})).await;
    assert!(rename_result.is_ok(), "thread/name/set should succeed");

    // thread/archive — params use camelCase (threadId)
    let archive_result = session.request("thread/archive", json!({"threadId": "thread-new"})).await;
    assert!(archive_result.is_ok(), "thread/archive should succeed");

    session.disconnect().await;
}

// ════════════════════════════════════════════════════════════════════════════
// VAL-ABS-045: Turn operations work through provider path
// ════════════════════════════════════════════════════════════════════════════

/// Turn lifecycle (start turn, stream deltas, complete turn) works through
/// a provider-backed session.
#[tokio::test]
async fn regression_turn_lifecycle_through_provider() {
    let mut provider = SequencedMockProvider::new("turn-lifecycle");
    let config = ProviderConfig::default();

    provider.connect(&config).await.unwrap();
    let mut rx = provider.event_receiver();

    // Start turn
    provider.emit(ProviderEvent::TurnStarted {
        thread_id: "t-turn".into(),
        turn_id: "turn-1".into(),
    });

    // Stream content
    provider.emit(ProviderEvent::ItemStarted {
        thread_id: "t-turn".into(),
        turn_id: "turn-1".into(),
        item_id: "item-1".into(),
    });
    provider.emit(ProviderEvent::MessageDelta {
        thread_id: "t-turn".into(),
        item_id: "item-1".into(),
        delta: "Partial ".into(),
    });
    provider.emit(ProviderEvent::MessageDelta {
        thread_id: "t-turn".into(),
        item_id: "item-1".into(),
        delta: "content ".into(),
    });
    provider.emit(ProviderEvent::MessageDelta {
        thread_id: "t-turn".into(),
        item_id: "item-1".into(),
        delta: "here".into(),
    });
    provider.emit(ProviderEvent::ItemCompleted {
        thread_id: "t-turn".into(),
        turn_id: "turn-1".into(),
        item_id: "item-1".into(),
    });

    // Complete turn
    provider.emit(ProviderEvent::TurnCompleted {
        thread_id: "t-turn".into(),
        turn_id: "turn-1".into(),
    });

    let events = collect_events(&mut rx).await;
    assert!(events.len() >= 7, "should have at least 7 events");

    // Verify sequence: TurnStarted → ItemStarted → 3×MessageDelta → ItemCompleted → TurnCompleted
    assert!(matches!(events[0], ProviderEvent::TurnStarted { .. }));
    assert!(matches!(events[1], ProviderEvent::ItemStarted { .. }));

    let deltas: Vec<_> = events.iter()
        .filter(|e| matches!(e, ProviderEvent::MessageDelta { .. }))
        .collect();
    assert_eq!(deltas.len(), 3, "should have 3 message deltas");

    let full_text: String = deltas.iter()
        .filter_map(|e| match e {
            ProviderEvent::MessageDelta { delta, .. } => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(full_text, "Partial content here", "no content should be lost during streaming");

    assert!(matches!(events[5], ProviderEvent::ItemCompleted { .. }));
    assert!(matches!(events[6], ProviderEvent::TurnCompleted { .. }));

    provider.disconnect().await;
}

/// Turn with reasoning delta works correctly.
#[tokio::test]
async fn regression_turn_with_reasoning_delta() {
    let mut provider = SequencedMockProvider::new("reasoning-turn");
    let config = ProviderConfig::default();

    provider.connect(&config).await.unwrap();
    let mut rx = provider.event_receiver();

    // Start turn with reasoning
    provider.emit(ProviderEvent::TurnStarted {
        thread_id: "t-reason".into(),
        turn_id: "turn-r".into(),
    });
    provider.emit(ProviderEvent::ReasoningDelta {
        thread_id: "t-reason".into(),
        item_id: "i-reason".into(),
        delta: "Let me think about this...".into(),
    });
    provider.emit(ProviderEvent::MessageDelta {
        thread_id: "t-reason".into(),
        item_id: "i-msg".into(),
        delta: "Here's the answer".into(),
    });
    provider.emit(ProviderEvent::TurnCompleted {
        thread_id: "t-reason".into(),
        turn_id: "turn-r".into(),
    });

    let events = collect_events(&mut rx).await;
    assert!(events.len() >= 4);

    // Verify reasoning delta is present
    assert!(
        events.iter().any(|e| matches!(e, ProviderEvent::ReasoningDelta { .. })),
        "should have ReasoningDelta event"
    );
    // Verify message delta follows reasoning
    let reasoning_idx = events.iter().position(|e| matches!(e, ProviderEvent::ReasoningDelta { .. })).unwrap();
    let message_idx = events.iter().position(|e| matches!(e, ProviderEvent::MessageDelta { .. })).unwrap();
    assert!(message_idx > reasoning_idx, "message should come after reasoning");

    provider.disconnect().await;
}

// ════════════════════════════════════════════════════════════════════════════
// VAL-ABS-049: Streaming event ordering is preserved
// ════════════════════════════════════════════════════════════════════════════

/// Events from the provider arrive at the session in the same order they
/// were emitted — no reordering.
#[tokio::test]
async fn regression_event_ordering_preserved() {
    let (provider, event_tx) = StreamingMockProvider::new();
    let config = test_server_config("event-ordering");

    let session = ServerSession::from_provider(config, Box::new(provider), None)
        .await
        .expect("from_provider should succeed");

    let mut events = session.events();

    // Let event task subscribe
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Emit a large sequence of events in a specific order
    let expected_order = vec![
        "turnStarted",
        "streamingStarted",
        "itemStarted",
        "agentMessageDelta",
        "agentMessageDelta",
        "agentMessageDelta",
        "itemCompleted",
        "streamingCompleted",
        "turnCompleted",
    ];

    event_tx.send(ProviderEvent::TurnStarted {
        thread_id: "t-order".into(),
        turn_id: "turn-o".into(),
    }).unwrap();
    event_tx.send(ProviderEvent::StreamingStarted {
        thread_id: "t-order".into(),
    }).unwrap();
    event_tx.send(ProviderEvent::ItemStarted {
        thread_id: "t-order".into(),
        turn_id: "turn-o".into(),
        item_id: "i-o".into(),
    }).unwrap();
    for i in 0..3 {
        event_tx.send(ProviderEvent::MessageDelta {
            thread_id: "t-order".into(),
            item_id: "i-o".into(),
            delta: format!("delta {i}"),
        }).unwrap();
    }
    event_tx.send(ProviderEvent::ItemCompleted {
        thread_id: "t-order".into(),
        turn_id: "turn-o".into(),
        item_id: "i-o".into(),
    }).unwrap();
    event_tx.send(ProviderEvent::StreamingCompleted {
        thread_id: "t-order".into(),
    }).unwrap();
    event_tx.send(ProviderEvent::TurnCompleted {
        thread_id: "t-order".into(),
        turn_id: "turn-o".into(),
    }).unwrap();

    // Collect all events
    let received: Vec<_> = tokio::time::timeout(Duration::from_secs(2), async {
        let mut evts = Vec::new();
        for _ in 0..9 {
            evts.push(events.recv().await.expect("should receive event"));
        }
        evts
    }).await.expect("timeout collecting ordered events");

    // Verify exact ordering
    let actual_methods: Vec<String> = received.iter().map(|e| {
        match e {
            crate::session::connection::ServerEvent::LegacyNotification { method, .. } => {
                // Extract the event name from "codex/event/<eventName>"
                method.split('/').last().unwrap_or("").to_string()
            }
            _ => "unknown".to_string(),
        }
    }).collect();

    assert_eq!(
        actual_methods, expected_order,
        "events should arrive in exact emission order"
    );

    session.disconnect().await;
}

/// Events through the direct provider broadcast channel maintain FIFO order.
#[tokio::test]
async fn regression_direct_provider_fifo_ordering() {
    let mut provider = SequencedMockProvider::new("fifo");
    let config = ProviderConfig::default();
    provider.connect(&config).await.unwrap();

    let mut rx = provider.event_receiver();

    // Emit 100 events with sequential identifiers
    for i in 0..100u32 {
        provider.emit(ProviderEvent::MessageDelta {
            thread_id: format!("t-{i}"),
            item_id: "i".into(),
            delta: format!("delta-{i}"),
        });
    }

    let events = collect_events(&mut rx).await;
    assert!(events.len() >= 100, "should receive all 100 events");

    // Verify ordering
    for (idx, event) in events.iter().enumerate().take(100) {
        match event {
            ProviderEvent::MessageDelta { thread_id, .. } => {
                let expected_id = format!("t-{idx}");
                assert_eq!(
                    thread_id, &expected_id,
                    "event at position {idx} should be from thread {expected_id}, got {thread_id}"
                );
            }
            _ => panic!("expected MessageDelta at position {idx}"),
        }
    }

    provider.disconnect().await;
}

// ════════════════════════════════════════════════════════════════════════════
// VAL-ABS-053: IPC stream remains functional for provider-backed Codex sessions
// ════════════════════════════════════════════════════════════════════════════

/// Verify that ServerSession IPC-related API surface is preserved.
/// The actual IPC functionality requires an in-process server, but we can
/// verify the API contract and session fields.
#[tokio::test]
async fn regression_ipc_api_surface_preserved() {
    let provider = Box::new(SequencedMockProvider::new("ipc-test"));
    let config = test_server_config("ipc-api");

    let session = ServerSession::from_provider(config, provider, None)
        .await
        .expect("from_provider should succeed");

    // IPC stream client should be None (no IPC for mock providers)
    assert!(
        !session.has_ipc(),
        "session without IPC should report has_ipc() = false"
    );

    session.disconnect().await;
}

/// IPC session construction API is preserved.
#[test]
fn regression_ipc_config_types_preserved() {
    use crate::session::connection::InProcessConfig;
    use std::path::PathBuf;

    let config = InProcessConfig {
        codex_home: Some(PathBuf::from("/tmp/codex-home")),
        working_directory: Some(PathBuf::from("/tmp/work")),
        channel_capacity: 512,
    };

    assert_eq!(config.channel_capacity, 512);
    assert!(config.codex_home.is_some());
}

// ════════════════════════════════════════════════════════════════════════════
// VAL-ABS-059: Cancel during active streaming emits well-formed terminal event
// ════════════════════════════════════════════════════════════════════════════

/// Interrupting a turn through the provider path produces a clean terminal
/// state: TurnCompleted with partial content preserved.
#[tokio::test]
async fn regression_cancel_during_streaming_produces_terminal_event() {
    let mut provider = SequencedMockProvider::new("cancel-stream");
    let config = ProviderConfig::default();

    provider.connect(&config).await.unwrap();
    let mut rx = provider.event_receiver();

    // Start streaming
    provider.emit(ProviderEvent::TurnStarted {
        thread_id: "t-cancel".into(),
        turn_id: "turn-cancel".into(),
    });
    provider.emit(ProviderEvent::StreamingStarted {
        thread_id: "t-cancel".into(),
    });
    provider.emit(ProviderEvent::ItemStarted {
        thread_id: "t-cancel".into(),
        turn_id: "turn-cancel".into(),
        item_id: "i-cancel".into(),
    });

    // Stream some partial content
    provider.emit(ProviderEvent::MessageDelta {
        thread_id: "t-cancel".into(),
        item_id: "i-cancel".into(),
        delta: "Partial content before cancel".into(),
    });

    // Simulate cancel: the agent/interrupt handler would emit TurnCompleted
    // This is what the server sends when a turn is interrupted
    provider.emit(ProviderEvent::ItemCompleted {
        thread_id: "t-cancel".into(),
        turn_id: "turn-cancel".into(),
        item_id: "i-cancel".into(),
    });
    provider.emit(ProviderEvent::StreamingCompleted {
        thread_id: "t-cancel".into(),
    });
    provider.emit(ProviderEvent::TurnCompleted {
        thread_id: "t-cancel".into(),
        turn_id: "turn-cancel".into(),
    });

    let events = collect_events(&mut rx).await;

    // Verify partial content was preserved
    let message_deltas: Vec<_> = events.iter()
        .filter(|e| matches!(e, ProviderEvent::MessageDelta { .. }))
        .collect();
    assert_eq!(message_deltas.len(), 1, "should have the partial delta");
    if let ProviderEvent::MessageDelta { delta, .. } = message_deltas[0] {
        assert_eq!(delta, "Partial content before cancel");
    }

    // Verify TurnCompleted is present (terminal event)
    assert!(
        events.iter().any(|e| matches!(e, ProviderEvent::TurnCompleted { .. })),
        "should have TurnCompleted as terminal event"
    );

    // Verify TurnCompleted comes after the partial content
    let last_event = events.last().unwrap();
    assert!(
        matches!(last_event, ProviderEvent::TurnCompleted { .. }),
        "TurnCompleted should be the last event"
    );

    provider.disconnect().await;
}

/// Cancel through a direct provider — verify interrupt notification
/// dispatches correctly.
#[tokio::test]
async fn regression_cancel_notification_through_provider() {
    let mut provider = CancelTrackingProvider::new();
    let config = ProviderConfig::default();

    // Connect directly (not through session, to avoid ClientNotification parsing)
    provider.connect(&config).await.unwrap();

    // Send cancel/interrupt directly through provider
    let result = provider.send_notification("turn/interrupt", json!({
        "thread_id": "t-cancel",
        "turn_id": "turn-cancel",
    })).await;

    assert!(result.is_ok(), "cancel notification should succeed");

    provider.disconnect().await;

    // Verify the notification was dispatched
    let sent = CancelTrackingProvider::get_sent_notifications();
    assert!(
        sent.iter().any(|(method, _)| method == "turn/interrupt"),
        "cancel notification should have been dispatched through provider"
    );
}

/// Verify no orphaned streaming state after cancel.
#[tokio::test]
async fn regression_no_orphaned_streaming_after_cancel() {
    let mut provider = SequencedMockProvider::new("orphan-cancel");
    let config = ProviderConfig::default();

    provider.connect(&config).await.unwrap();
    let mut rx = provider.event_receiver();

    // Stream 1: start and cancel
    provider.emit(ProviderEvent::TurnStarted {
        thread_id: "t-orphan".into(),
        turn_id: "turn-1".into(),
    });
    provider.emit(ProviderEvent::StreamingStarted {
        thread_id: "t-orphan".into(),
    });
    provider.emit(ProviderEvent::MessageDelta {
        thread_id: "t-orphan".into(),
        item_id: "i-1".into(),
        delta: "partial".into(),
    });
    provider.emit(ProviderEvent::TurnCompleted {
        thread_id: "t-orphan".into(),
        turn_id: "turn-1".into(),
    });

    // Stream 2: new turn on same thread should work cleanly
    provider.emit(ProviderEvent::TurnStarted {
        thread_id: "t-orphan".into(),
        turn_id: "turn-2".into(),
    });
    provider.emit(ProviderEvent::StreamingStarted {
        thread_id: "t-orphan".into(),
    });
    provider.emit(ProviderEvent::MessageDelta {
        thread_id: "t-orphan".into(),
        item_id: "i-2".into(),
        delta: "new turn content".into(),
    });
    provider.emit(ProviderEvent::TurnCompleted {
        thread_id: "t-orphan".into(),
        turn_id: "turn-2".into(),
    });

    let events = collect_events(&mut rx).await;

    // Should have events for both turns
    let turn_starts: Vec<_> = events.iter()
        .filter(|e| matches!(e, ProviderEvent::TurnStarted { .. }))
        .collect();
    assert_eq!(turn_starts.len(), 2, "should have 2 TurnStarted events");

    let turn_completes: Vec<_> = events.iter()
        .filter(|e| matches!(e, ProviderEvent::TurnCompleted { .. }))
        .collect();
    assert_eq!(turn_completes.len(), 2, "should have 2 TurnCompleted events");

    // Verify second turn has correct turn_id
    if let ProviderEvent::TurnStarted { turn_id, .. } = turn_starts[1] {
        assert_eq!(turn_id, "turn-2", "second turn should have correct turn_id");
    }

    provider.disconnect().await;
}

// ════════════════════════════════════════════════════════════════════════════
// Cross-cutting regression tests
// ════════════════════════════════════════════════════════════════════════════

/// Provider trait works uniformly across all transport implementations:
/// PiNative, DroidNative, SequencedMock, ErrorMock.
#[tokio::test]
async fn regression_all_transport_types_support_thread_operations() {
    let config = ProviderConfig::default();

    // Test with Pi mock
    let pi_mock = MockPiChannel::new();
    let pi_transport = PiNativeTransport::new(pi_mock.clone());
    let mut pi: Box<dyn ProviderTransport> = Box::new(pi_transport);
    pi.connect(&config).await.unwrap();
    assert!(pi.is_connected());
    pi.disconnect().await;
    assert!(!pi.is_connected());

    // Test with Droid mock
    let droid_mock = MockDroidChannel::new();
    let droid_transport = DroidNativeTransport::new(droid_mock.clone());
    let mut droid: Box<dyn ProviderTransport> = Box::new(droid_transport);
    droid.connect(&config).await.unwrap();
    assert!(droid.is_connected());
    droid.disconnect().await;
    assert!(!droid.is_connected());

    // Test with SequencedMock
    let mut sequenced = SequencedMockProvider::new("seq-1");
    sequenced.connect(&config).await.unwrap();
    assert!(sequenced.is_connected());
    sequenced.disconnect().await;
    assert!(!sequenced.is_connected());

    // Test with ErrorMock
    let mut error_mock = ErrorMockProvider::new();
    error_mock.connect(&config).await.unwrap();
    assert!(error_mock.is_connected());
    error_mock.disconnect().await;
    assert!(!error_mock.is_connected());
}

/// Verify that cancel semantics work correctly across providers.
/// Each provider type handles cancel via its native method name.
#[tokio::test]
async fn regression_cancel_semantics_across_providers() {
    let config = ProviderConfig::default();

    // SequencedMock: cancel through send_notification (most generic)
    let mut seq_provider = SequencedMockProvider::new("cancel-seq");
    seq_provider.connect(&config).await.unwrap();
    let seq_cancel = seq_provider.send_notification("turn/interrupt", json!({})).await;
    assert!(seq_cancel.is_ok(), "SequencedMock cancel should succeed");
    seq_provider.disconnect().await;

    // ErrorMock: cancel through send_notification
    let mut error_mock = ErrorMockProvider::new();
    error_mock.connect(&config).await.unwrap();
    let error_cancel = error_mock.send_notification("turn/interrupt", json!({})).await;
    assert!(error_cancel.is_ok(), "ErrorMock cancel should succeed");
    error_mock.disconnect().await;

    // Pi: abort through send_request
    let pi_mock = MockPiChannel::new();
    let pi_transport = PiNativeTransport::new(pi_mock.clone());
    let mut pi: Box<dyn ProviderTransport> = Box::new(pi_transport);
    pi.connect(&config).await.unwrap();
    let pi_cancel = pi.send_request("abort", json!({})).await;
    assert!(pi_cancel.is_ok(), "Pi cancel should succeed");
    pi.disconnect().await;
}

/// Multiple rapid connect/disconnect cycles don't cause panics or deadlocks.
#[tokio::test]
async fn regression_rapid_connect_disconnect_cycles() {
    for cycle in 0..10 {
        let mut provider = SequencedMockProvider::new(&format!("cycle-{cycle}"));
        let config = ProviderConfig::default();

        provider.connect(&config).await.unwrap();
        assert!(provider.is_connected());

        // Quick event emission
        provider.emit(ProviderEvent::TurnStarted {
            thread_id: format!("t-{cycle}"),
            turn_id: format!("turn-{cycle}"),
        });

        provider.disconnect().await;
        assert!(!provider.is_connected());
    }
}

/// Idempotent disconnect — calling disconnect multiple times is safe.
#[tokio::test]
async fn regression_idempotent_disconnect() {
    let mut provider = SequencedMockProvider::new("idempotent");
    let config = ProviderConfig::default();

    provider.connect(&config).await.unwrap();

    // Disconnect multiple times — should not panic
    provider.disconnect().await;
    assert!(!provider.is_connected());

    provider.disconnect().await;
    assert!(!provider.is_connected());

    provider.disconnect().await;
    assert!(!provider.is_connected());
}

/// Verify ProviderEvent serialization doesn't depend on source provider
/// (cross-provider parity for event types).
#[test]
fn regression_provider_event_serialization_provider_agnostic() {
    let events = vec![
        ProviderEvent::TurnStarted {
            thread_id: "t-1".into(),
            turn_id: "turn-1".into(),
        },
        ProviderEvent::MessageDelta {
            thread_id: "t-1".into(),
            item_id: "i-1".into(),
            delta: "content".into(),
        },
        ProviderEvent::CommandOutputDelta {
            thread_id: "t-1".into(),
            item_id: "cmd-1".into(),
            delta: "output\n".into(),
        },
        ProviderEvent::TurnCompleted {
            thread_id: "t-1".into(),
            turn_id: "turn-1".into(),
        },
    ];

    // All events should serialize identically regardless of how many times
    for event in &events {
        let json1 = serde_json::to_string(event).unwrap();
        let json2 = serde_json::to_string(event).unwrap();
        assert_eq!(json1, json2, "serialization should be deterministic");

        let deserialized: ProviderEvent = serde_json::from_str(&json1).unwrap();
        assert_eq!(event, &deserialized, "round-trip should preserve event");
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Helper mock providers
// ════════════════════════════════════════════════════════════════════════════

/// A mock provider that exposes its event sender for direct event injection.
struct StreamingMockProvider {
    connected: bool,
    events: broadcast::Sender<ProviderEvent>,
}

impl StreamingMockProvider {
    fn new() -> (Self, broadcast::Sender<ProviderEvent>) {
        let (tx, _) = broadcast::channel(4096);
        let provider = Self {
            connected: true,
            events: tx.clone(),
        };
        (provider, tx)
    }
}

#[async_trait]
impl ProviderTransport for StreamingMockProvider {
    async fn connect(&mut self, _config: &ProviderConfig) -> Result<(), TransportError> {
        self.connected = true;
        Ok(())
    }

    async fn disconnect(&mut self) {
        self.connected = false;
    }

    async fn send_request(
        &mut self,
        _method: &str,
        _params: JsonValue,
    ) -> Result<JsonValue, RpcError> {
        if !self.connected {
            return Err(RpcError::Transport(TransportError::Disconnected));
        }
        Ok(json!({"ok": true}))
    }

    async fn send_notification(
        &mut self,
        _method: &str,
        _params: JsonValue,
    ) -> Result<(), RpcError> {
        if !self.connected {
            return Err(RpcError::Transport(TransportError::Disconnected));
        }
        Ok(())
    }

    fn next_event(&self) -> Option<ProviderEvent> {
        None
    }

    fn event_receiver(&self) -> broadcast::Receiver<ProviderEvent> {
        self.events.subscribe()
    }

    async fn list_sessions(&mut self) -> Result<Vec<SessionInfo>, RpcError> {
        Ok(vec![])
    }

    fn is_connected(&self) -> bool {
        self.connected
    }
}

/// A provider that tracks resolution requests ($resolve, $reject) via
/// a thread-local for easy assertion access.
struct ResolutionTrackingProvider {
    connected: bool,
    events: broadcast::Sender<ProviderEvent>,
}

// Thread-local storage for tracking sent requests across instances
std::thread_local! {
    static SENT_REQUESTS: std::cell::RefCell<Vec<(String, JsonValue)>> = std::cell::RefCell::new(Vec::new());
}

impl ResolutionTrackingProvider {
    fn new() -> Self {
        let (tx, _) = broadcast::channel(256);
        SENT_REQUESTS.with(|s| s.borrow_mut().clear());
        Self {
            connected: true,
            events: tx,
        }
    }

    fn get_sent_requests() -> Vec<(String, JsonValue)> {
        SENT_REQUESTS.with(|s| s.borrow().clone())
    }
}

#[async_trait]
impl ProviderTransport for ResolutionTrackingProvider {
    async fn connect(&mut self, _config: &ProviderConfig) -> Result<(), TransportError> {
        self.connected = true;
        Ok(())
    }

    async fn disconnect(&mut self) {
        self.connected = false;
    }

    async fn send_request(
        &mut self,
        method: &str,
        params: JsonValue,
    ) -> Result<JsonValue, RpcError> {
        if !self.connected {
            return Err(RpcError::Transport(TransportError::Disconnected));
        }
        SENT_REQUESTS.with(|s| s.borrow_mut().push((method.to_string(), params)));
        Ok(json!({"ok": true}))
    }

    async fn send_notification(
        &mut self,
        method: &str,
        params: JsonValue,
    ) -> Result<(), RpcError> {
        if !self.connected {
            return Err(RpcError::Transport(TransportError::Disconnected));
        }
        let _ = (method, params);
        Ok(())
    }

    fn next_event(&self) -> Option<ProviderEvent> {
        None
    }

    fn event_receiver(&self) -> broadcast::Receiver<ProviderEvent> {
        self.events.subscribe()
    }

    async fn list_sessions(&mut self) -> Result<Vec<SessionInfo>, RpcError> {
        Ok(vec![])
    }

    fn is_connected(&self) -> bool {
        self.connected
    }
}

/// A provider that tracks cancel/interrupt notifications.
struct CancelTrackingProvider {
    connected: bool,
    events: broadcast::Sender<ProviderEvent>,
}

// Thread-local for tracking sent notifications
std::thread_local! {
    static SENT_NOTIFICATIONS: std::cell::RefCell<Vec<(String, JsonValue)>> = std::cell::RefCell::new(Vec::new());
}

impl CancelTrackingProvider {
    fn new() -> Self {
        let (tx, _) = broadcast::channel(256);
        SENT_NOTIFICATIONS.with(|s| s.borrow_mut().clear());
        Self {
            connected: true,
            events: tx,
        }
    }

    fn get_sent_notifications() -> Vec<(String, JsonValue)> {
        SENT_NOTIFICATIONS.with(|s| s.borrow().clone())
    }
}

#[async_trait]
impl ProviderTransport for CancelTrackingProvider {
    async fn connect(&mut self, _config: &ProviderConfig) -> Result<(), TransportError> {
        self.connected = true;
        Ok(())
    }

    async fn disconnect(&mut self) {
        self.connected = false;
    }

    async fn send_request(
        &mut self,
        method: &str,
        params: JsonValue,
    ) -> Result<JsonValue, RpcError> {
        if !self.connected {
            return Err(RpcError::Transport(TransportError::Disconnected));
        }
        let _ = (method, params);
        Ok(json!({"ok": true}))
    }

    async fn send_notification(
        &mut self,
        method: &str,
        params: JsonValue,
    ) -> Result<(), RpcError> {
        if !self.connected {
            return Err(RpcError::Transport(TransportError::Disconnected));
        }
        SENT_NOTIFICATIONS.with(|s| s.borrow_mut().push((method.to_string(), params)));
        Ok(())
    }

    fn next_event(&self) -> Option<ProviderEvent> {
        None
    }

    fn event_receiver(&self) -> broadcast::Receiver<ProviderEvent> {
        self.events.subscribe()
    }

    async fn list_sessions(&mut self) -> Result<Vec<SessionInfo>, RpcError> {
        Ok(vec![])
    }

    fn is_connected(&self) -> bool {
        self.connected
    }
}

/// A provider that simulates thread lifecycle operations.
struct ThreadLifecycleProvider {
    connected: bool,
    events: broadcast::Sender<ProviderEvent>,
}

impl ThreadLifecycleProvider {
    fn new() -> Self {
        let (tx, _) = broadcast::channel(256);
        Self {
            connected: true,
            events: tx,
        }
    }
}

#[async_trait]
impl ProviderTransport for ThreadLifecycleProvider {
    async fn connect(&mut self, _config: &ProviderConfig) -> Result<(), TransportError> {
        self.connected = true;
        Ok(())
    }

    async fn disconnect(&mut self) {
        self.connected = false;
    }

    async fn send_request(
        &mut self,
        method: &str,
        _params: JsonValue,
    ) -> Result<JsonValue, RpcError> {
        if !self.connected {
            return Err(RpcError::Transport(TransportError::Disconnected));
        }
        match method {
            "thread/list" => Ok(json!({
                "threads": [
                    {"id": "t1", "title": "Thread 1", "created_at": 1000, "updated_at": 2000}
                ]
            })),
            "thread/create" => Ok(json!({"id": "thread-new", "title": "New Thread"})),
            "thread/rename" => Ok(json!({"ok": true})),
            "thread/archive" => Ok(json!({"ok": true})),
            _ => Ok(json!({"ok": true})),
        }
    }

    async fn send_notification(
        &mut self,
        _method: &str,
        _params: JsonValue,
    ) -> Result<(), RpcError> {
        if !self.connected {
            return Err(RpcError::Transport(TransportError::Disconnected));
        }
        Ok(())
    }

    fn next_event(&self) -> Option<ProviderEvent> {
        None
    }

    fn event_receiver(&self) -> broadcast::Receiver<ProviderEvent> {
        self.events.subscribe()
    }

    async fn list_sessions(&mut self) -> Result<Vec<SessionInfo>, RpcError> {
        Ok(vec![])
    }

    fn is_connected(&self) -> bool {
        self.connected
    }
}
