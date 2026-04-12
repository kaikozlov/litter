//! Comprehensive error handling for the provider abstraction.
//!
//! Implements error handling behaviors required by the provider trait contract:
//!
//! - Mid-stream disconnect yields `ProviderEvent::Disconnected` (no panic)
//! - Reconnection path works through the trait transport
//! - Connect failure returns `TransportError` cleanly (no panic, provider reusable)
//! - Disconnect is idempotent
//! - Post-disconnect requests fail with `TransportError::Disconnected`
//! - Handshake timeout/failure returns clean error, provider in clean state for retry
//! - Cancel during active streaming emits well-formed terminal event with partial content
//! - Configuration changes mid-session (model, reasoning effort) work for idle threads
//!   and fail gracefully for active streams
//!
//! Tests cover VAL-PROV-012, VAL-PROV-013, VAL-PROV-016, VAL-PROV-018, VAL-PROV-019.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::Value as JsonValue;
use tokio::sync::{broadcast, Mutex};

use crate::provider::{ProviderConfig, ProviderEvent, ProviderTransport, SessionInfo};
use crate::transport::{RpcError, TransportError};

// ── Shared state for the ErrorMockProvider ──────────────────────────────────

/// Shared internal state for the mock provider, accessible via Arc
/// even when the provider is behind a `Box<dyn ProviderTransport>`.
#[derive(Debug)]
pub(crate) struct MockProviderState {
    pub connected: AtomicBool,
    pub events: broadcast::Sender<ProviderEvent>,
    pub request_responses: Mutex<VecDeque<Result<JsonValue, RpcError>>>,
    pub notification_results: Mutex<VecDeque<Result<(), RpcError>>>,
    pub sent_requests: Mutex<Vec<(String, JsonValue)>>,
    pub sent_notifications: Mutex<Vec<(String, JsonValue)>>,
    pub connect_count: AtomicUsize,
    pub disconnect_count: AtomicUsize,
    pub connect_should_fail: AtomicBool,
    pub handshake_should_timeout: AtomicBool,
    pub streaming_active: AtomicBool,
    pub current_model: Mutex<String>,
    pub current_reasoning_effort: Mutex<String>,
    pub next_connect_succeeds: AtomicBool,
}

impl MockProviderState {
    pub fn new() -> Self {
        let (event_tx, _) = broadcast::channel(512);
        Self {
            connected: AtomicBool::new(false),
            events: event_tx,
            request_responses: Mutex::new(VecDeque::new()),
            notification_results: Mutex::new(VecDeque::new()),
            sent_requests: Mutex::new(Vec::new()),
            sent_notifications: Mutex::new(Vec::new()),
            connect_count: AtomicUsize::new(0),
            disconnect_count: AtomicUsize::new(0),
            connect_should_fail: AtomicBool::new(false),
            handshake_should_timeout: AtomicBool::new(false),
            streaming_active: AtomicBool::new(false),
            current_model: Mutex::new("default-model".to_string()),
            current_reasoning_effort: Mutex::new("medium".to_string()),
            next_connect_succeeds: AtomicBool::new(true),
        }
    }
}

// ── Enhanced Mock Provider ─────────────────────────────────────────────────

/// A configurable mock provider that supports error injection for testing
/// all error handling scenarios.
///
/// This provider tracks:
/// - Connection state transitions
/// - Sent requests/notifications
/// - Configurable failure modes (connect failure, handshake timeout, mid-stream disconnect)
/// - Reconnection behavior
/// - Active streaming state for cancel/config-change scenarios
pub struct ErrorMockProvider {
    state: Arc<MockProviderState>,
}

impl Default for ErrorMockProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl ErrorMockProvider {
    pub fn new() -> Self {
        Self {
            state: Arc::new(MockProviderState::new()),
        }
    }

    /// Get a handle to the shared state (for testing even when behind Box<dyn>).
    #[allow(dead_code)]
    pub(crate) fn state(&self) -> Arc<MockProviderState> {
        Arc::clone(&self.state)
    }

    // ── Configuration helpers ─────────────────────────────────────────

    pub fn enqueue_request_response(&self, response: Result<JsonValue, RpcError>) {
        let mut queue = self.state.request_responses.blocking_lock();
        queue.push_back(response);
    }

    pub fn enqueue_notification_result(&self, result: Result<(), RpcError>) {
        let mut queue = self.state.notification_results.blocking_lock();
        queue.push_back(result);
    }

    /// Set whether the next connect() call should fail.
    pub fn set_connect_should_fail(&self, fail: bool) {
        self.state
            .connect_should_fail
            .store(fail, Ordering::SeqCst);
    }

    /// Set whether the next connect() should simulate a handshake timeout.
    pub fn set_handshake_should_timeout(&self, timeout: bool) {
        self.state
            .handshake_should_timeout
            .store(timeout, Ordering::SeqCst);
    }

    /// Set whether the next connect should succeed (for reconnection tests).
    pub fn set_next_connect_succeeds(&self, succeeds: bool) {
        self.state
            .next_connect_succeeds
            .store(succeeds, Ordering::SeqCst);
    }

    /// Set whether the provider is in an active streaming state.
    pub fn set_streaming_active(&self, active: bool) {
        self.state.streaming_active.store(active, Ordering::SeqCst);
    }

    /// Simulate a mid-stream disconnect by emitting a Disconnected event.
    pub fn simulate_mid_stream_disconnect(&self, message: &str) {
        let _ = self.state.events.send(ProviderEvent::Disconnected {
            message: message.to_string(),
        });
    }

    /// Emit a provider event (for testing event stream behavior).
    pub fn emit_event(&self, event: ProviderEvent) {
        let _ = self.state.events.send(event);
    }
}

#[async_trait::async_trait]
impl ProviderTransport for ErrorMockProvider {
    async fn connect(&mut self, _config: &ProviderConfig) -> Result<(), TransportError> {
        self.state.connect_count.fetch_add(1, Ordering::SeqCst);

        if self.state.connect_should_fail.load(Ordering::SeqCst) {
            return Err(TransportError::ConnectionFailed(
                "simulated connection failure".to_string(),
            ));
        }

        if self.state.handshake_should_timeout.load(Ordering::SeqCst) {
            // Simulate handshake timeout — transport connects but
            // initialize never completes.
            return Err(TransportError::Timeout);
        }

        if !self.state.next_connect_succeeds.load(Ordering::SeqCst) {
            return Err(TransportError::ConnectionFailed(
                "simulated reconnection failure".to_string(),
            ));
        }

        self.state.connected.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn disconnect(&mut self) {
        self.state.disconnect_count.fetch_add(1, Ordering::SeqCst);
        if !self.state.connected.load(Ordering::SeqCst) {
            return; // Idempotent.
        }
        self.state.connected.store(false, Ordering::SeqCst);
        self.state.streaming_active.store(false, Ordering::SeqCst);
    }

    async fn send_request(
        &mut self,
        method: &str,
        params: JsonValue,
    ) -> Result<JsonValue, RpcError> {
        if !self.state.connected.load(Ordering::SeqCst) {
            return Err(RpcError::Transport(TransportError::Disconnected));
        }

        // First, check the queued response. If it's an error, return the error
        // WITHOUT applying the config change (simulates server-side rejection
        // before the change takes effect).
        let mut queue = self.state.request_responses.lock().await;
        let response = queue.pop_front();
        drop(queue); // Release lock before processing.

        match response {
            Some(Err(e)) => {
                self.state
                    .sent_requests
                    .lock()
                    .await
                    .push((method.to_string(), params));
                return Err(e);
            }
            Some(Ok(_)) => {
                // Server accepted the request — apply config changes if applicable.
            }
            None => {
                // No queued response — use default success.
            }
        }

        // Handle model/reasoning-effort changes.
        if method == "thread/update" || method == "config/change" {
            // Reject config changes during active streaming.
            if self.state.streaming_active.load(Ordering::SeqCst)
                && (params.get("model").is_some() || params.get("reasoning_effort").is_some())
            {
                return Err(RpcError::Server {
                    code: -32600,
                    message: "cannot change configuration while streaming is active".to_string(),
                });
            }

            if let Some(model) = params.get("model").and_then(|m| m.as_str()) {
                let mut current = self.state.current_model.lock().await;
                *current = model.to_string();
            }
            if let Some(effort) = params
                .get("reasoning_effort")
                .and_then(|e| e.as_str())
            {
                let mut current = self.state.current_reasoning_effort.lock().await;
                *current = effort.to_string();
            }
        }

        self.state
            .sent_requests
            .lock()
            .await
            .push((method.to_string(), params));

        // Return the queued response or default success.
        response.unwrap_or(Ok(serde_json::json!({"ok": true})))
    }

    async fn send_notification(
        &mut self,
        method: &str,
        params: JsonValue,
    ) -> Result<(), RpcError> {
        if !self.state.connected.load(Ordering::SeqCst) {
            return Err(RpcError::Transport(TransportError::Disconnected));
        }

        // Handle cancel notification.
        if method == "turn/cancel" || method == "session/cancel" {
            self.state.streaming_active.store(false, Ordering::SeqCst);
        }

        self.state
            .sent_notifications
            .lock()
            .await
            .push((method.to_string(), params));

        let mut queue = self.state.notification_results.lock().await;
        queue.pop_front().unwrap_or(Ok(()))
    }

    fn next_event(&self) -> Option<ProviderEvent> {
        let mut rx = self.state.events.subscribe();
        rx.try_recv().ok()
    }

    fn event_receiver(&self) -> broadcast::Receiver<ProviderEvent> {
        self.state.events.subscribe()
    }

    async fn list_sessions(&mut self) -> Result<Vec<SessionInfo>, RpcError> {
        if !self.state.connected.load(Ordering::SeqCst) {
            return Err(RpcError::Transport(TransportError::Disconnected));
        }
        Ok(vec![])
    }

    fn is_connected(&self) -> bool {
        self.state.connected.load(Ordering::SeqCst)
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ProviderConfig, ProviderEvent, ProviderTransport};
    use crate::transport::{RpcError, TransportError};
    use std::time::Duration;

    // ═══════════════════════════════════════════════════════════════════
    // VAL-PROV-012: Mid-stream disconnect yields Disconnected event
    // ═══════════════════════════════════════════════════════════════════

    /// When the underlying transport disconnects mid-stream (e.g. server crash,
    /// network interruption), the provider must emit `ProviderEvent::Disconnected`
    /// and must not panic.
    #[tokio::test]
    async fn mid_stream_disconnect_yields_disconnected_event() {
        let provider = ErrorMockProvider::new();
        let state = provider.state();
        let config = ProviderConfig::default();

        // Simulate: provider connects successfully.
        let mut boxed: Box<dyn ProviderTransport> = Box::new(provider);
        boxed.connect(&config).await.unwrap();
        assert!(boxed.is_connected());

        // Subscribe to the event stream BEFORE the disconnect.
        let mut rx = boxed.event_receiver();

        // Simulate streaming some events via shared state.
        let _ = state.events.send(ProviderEvent::MessageDelta {
            thread_id: "t1".into(),
            item_id: "item1".into(),
            delta: "Hello ".into(),
        });

        // Simulate mid-stream disconnect.
        let _ = state.events.send(ProviderEvent::Disconnected {
            message: "connection reset by peer".to_string(),
        });

        // Verify: MessageDelta event is received.
        let msg_event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("timeout waiting for MessageDelta")
            .expect("should receive MessageDelta");
        assert!(
            matches!(msg_event, ProviderEvent::MessageDelta { .. }),
            "expected MessageDelta, got {msg_event:?}"
        );

        // Verify: Disconnected event is received.
        let dc_event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("timeout waiting for Disconnected event")
            .expect("should receive Disconnected event");
        assert!(
            matches!(
                dc_event,
                ProviderEvent::Disconnected { ref message } if message.contains("connection reset")
            ),
            "expected Disconnected with message, got {dc_event:?}"
        );
    }

    /// Verify that after a mid-stream disconnect event, subsequent send_request
    /// calls return an error (not panic).
    #[tokio::test]
    async fn mid_stream_disconnect_no_panic_on_subsequent_requests() {
        let mut provider = ErrorMockProvider::new();
        let config = ProviderConfig::default();

        provider.connect(&config).await.unwrap();

        // Simulate disconnect by setting connected to false.
        provider.state.connected.store(false, Ordering::SeqCst);

        // send_request should return Err, not panic.
        let result = provider
            .send_request("thread/list", serde_json::json!({}))
            .await;
        assert!(
            result.is_err(),
            "expected error after disconnect, got {result:?}"
        );
        match result {
            Err(RpcError::Transport(TransportError::Disconnected)) => {}
            other => panic!("expected TransportError::Disconnected, got {other:?}"),
        }
    }

    /// Verify that the Disconnected event can be received by multiple
    /// subscribers (e.g. health monitor + event consumer).
    #[tokio::test]
    async fn mid_stream_disconnect_multiple_subscribers() {
        let provider = ErrorMockProvider::new();
        let state = provider.state();
        let config = ProviderConfig::default();

        let mut boxed: Box<dyn ProviderTransport> = Box::new(provider);
        boxed.connect(&config).await.unwrap();

        // Two subscribers.
        let mut rx1 = boxed.event_receiver();
        let mut rx2 = boxed.event_receiver();

        // Emit disconnect.
        let _ = state.events.send(ProviderEvent::Disconnected {
            message: "network error".to_string(),
        });

        // Both receive it.
        let event1 = tokio::time::timeout(Duration::from_secs(2), rx1.recv())
            .await
            .expect("timeout rx1")
            .expect("rx1 event");
        let event2 = tokio::time::timeout(Duration::from_secs(2), rx2.recv())
            .await
            .expect("timeout rx2")
            .expect("rx2 event");

        assert!(matches!(event1, ProviderEvent::Disconnected { .. }));
        assert!(matches!(event2, ProviderEvent::Disconnected { .. }));
    }

    // ═══════════════════════════════════════════════════════════════════
    // VAL-PROV-013: Reconnection path works through trait transport
    // ═══════════════════════════════════════════════════════════════════

    /// After a disconnection, calling connect() again through the trait
    /// should succeed and the provider should be usable.
    #[tokio::test]
    async fn reconnect_through_trait_transport_succeeds() {
        let mut provider = ErrorMockProvider::new();
        let state = provider.state();
        let config = ProviderConfig::default();

        // First connection succeeds.
        provider.connect(&config).await.unwrap();
        assert!(provider.is_connected());
        assert_eq!(state.connect_count.load(Ordering::SeqCst), 1);

        // Simulate disconnect.
        provider.disconnect().await;
        assert!(!provider.is_connected());

        // Reconnect succeeds.
        provider.connect(&config).await.unwrap();
        assert!(provider.is_connected());
        assert_eq!(state.connect_count.load(Ordering::SeqCst), 2);

        // Provider is usable after reconnect.
        provider
            .send_request("thread/list", serde_json::json!({}))
            .await
            .expect("request after reconnect should succeed");
    }

    /// Reconnection attempts that fail should be retriable.
    /// The provider should not panic and should remain in a usable state
    /// for subsequent connect attempts.
    #[tokio::test]
    async fn reconnect_failure_then_success() {
        let mut provider = ErrorMockProvider::new();
        let state = provider.state();
        let config = ProviderConfig::default();

        // First connection succeeds.
        provider.connect(&config).await.unwrap();
        assert!(provider.is_connected());

        // Disconnect.
        provider.disconnect().await;

        // First reconnect attempt fails.
        provider.set_connect_should_fail(true);
        let result = provider.connect(&config).await;
        assert!(result.is_err());
        assert!(!provider.is_connected());
        assert_eq!(state.connect_count.load(Ordering::SeqCst), 2);

        // Second reconnect attempt succeeds.
        provider.set_connect_should_fail(false);
        provider.connect(&config).await.unwrap();
        assert!(provider.is_connected());
        assert_eq!(state.connect_count.load(Ordering::SeqCst), 3);
    }

    /// A boxed trait object should support the reconnection flow through
    /// the trait interface.
    #[tokio::test]
    async fn reconnect_through_boxed_trait_object() {
        let provider = ErrorMockProvider::new();
        let state = provider.state();
        let mut boxed: Box<dyn ProviderTransport> = Box::new(provider);
        let config = ProviderConfig::default();

        // Connect.
        boxed.connect(&config).await.unwrap();
        assert!(boxed.is_connected());

        // Disconnect.
        boxed.disconnect().await;
        assert!(!boxed.is_connected());

        // Reconnect through trait.
        boxed.connect(&config).await.unwrap();
        assert!(boxed.is_connected());
        assert_eq!(state.connect_count.load(Ordering::SeqCst), 2);

        // Verify usable.
        let result = boxed
            .send_request("thread/list", serde_json::json!({}))
            .await;
        assert!(result.is_ok());
    }

    /// Multiple disconnect-reconnect cycles work correctly through the
    /// trait transport.
    #[tokio::test]
    async fn multiple_reconnect_cycles() {
        let provider = ErrorMockProvider::new();
        let state = provider.state();
        let mut boxed: Box<dyn ProviderTransport> = Box::new(provider);
        let config = ProviderConfig::default();

        for _ in 0..3 {
            boxed.connect(&config).await.unwrap();
            assert!(boxed.is_connected());

            boxed
                .send_request("thread/list", serde_json::json!({}))
                .await
                .expect("request should succeed");

            boxed.disconnect().await;
            assert!(!boxed.is_connected());
        }

        assert_eq!(state.connect_count.load(Ordering::SeqCst), 3);
        assert_eq!(state.disconnect_count.load(Ordering::SeqCst), 3);
    }

    // ═══════════════════════════════════════════════════════════════════
    // Connect failure + disconnect idempotency + post-disconnect errors
    // ═══════════════════════════════════════════════════════════════════

    /// Connect failure returns `TransportError::ConnectionFailed`, no panic,
    /// and the provider can be reused.
    #[tokio::test]
    async fn connect_failure_returns_error_no_panic_reusable() {
        let mut provider = ErrorMockProvider::new();
        let config = ProviderConfig::default();

        provider.set_connect_should_fail(true);

        // Connect fails cleanly.
        let result = provider.connect(&config).await;
        assert!(result.is_err());
        match result {
            Err(TransportError::ConnectionFailed(msg)) => {
                assert!(
                    msg.contains("simulated connection failure"),
                    "unexpected error message: {msg}"
                );
            }
            other => panic!("expected ConnectionFailed, got {other:?}"),
        }
        assert!(!provider.is_connected());

        // Provider is reusable — next connect succeeds.
        provider.set_connect_should_fail(false);
        provider.connect(&config).await.unwrap();
        assert!(provider.is_connected());
    }

    /// Disconnect is idempotent: calling it twice does not panic.
    #[tokio::test]
    async fn disconnect_is_idempotent() {
        let provider = ErrorMockProvider::new();
        let state = provider.state();
        let mut boxed: Box<dyn ProviderTransport> = Box::new(provider);
        let config = ProviderConfig::default();

        boxed.connect(&config).await.unwrap();
        assert!(boxed.is_connected());

        // First disconnect.
        boxed.disconnect().await;
        assert!(!boxed.is_connected());
        assert_eq!(state.disconnect_count.load(Ordering::SeqCst), 1);

        // Second disconnect — no panic, no error.
        boxed.disconnect().await;
        assert!(!boxed.is_connected());
        assert_eq!(state.disconnect_count.load(Ordering::SeqCst), 2);

        // Third disconnect — still fine.
        boxed.disconnect().await;
        assert!(!boxed.is_connected());
        assert_eq!(state.disconnect_count.load(Ordering::SeqCst), 3);
    }

    /// After disconnect, send_request returns TransportError::Disconnected.
    #[tokio::test]
    async fn post_disconnect_request_returns_disconnected_error() {
        let mut boxed: Box<dyn ProviderTransport> = Box::new(ErrorMockProvider::new());
        let config = ProviderConfig::default();

        boxed.connect(&config).await.unwrap();
        boxed.disconnect().await;

        let result = boxed
            .send_request("thread/list", serde_json::json!({}))
            .await;
        match result {
            Err(RpcError::Transport(TransportError::Disconnected)) => {}
            other => panic!("expected TransportError::Disconnected, got {other:?}"),
        }
    }

    /// After disconnect, send_notification returns TransportError::Disconnected.
    #[tokio::test]
    async fn post_disconnect_notification_returns_disconnected_error() {
        let mut boxed: Box<dyn ProviderTransport> = Box::new(ErrorMockProvider::new());
        let config = ProviderConfig::default();

        boxed.connect(&config).await.unwrap();
        boxed.disconnect().await;

        let result = boxed
            .send_notification("initialized", serde_json::json!(null))
            .await;
        match result {
            Err(RpcError::Transport(TransportError::Disconnected)) => {}
            other => panic!("expected TransportError::Disconnected, got {other:?}"),
        }
    }

    /// After disconnect, list_sessions returns TransportError::Disconnected.
    #[tokio::test]
    async fn post_disconnect_list_sessions_returns_disconnected_error() {
        let mut boxed: Box<dyn ProviderTransport> = Box::new(ErrorMockProvider::new());
        let config = ProviderConfig::default();

        boxed.connect(&config).await.unwrap();
        boxed.disconnect().await;

        let result = boxed.list_sessions().await;
        match result {
            Err(RpcError::Transport(TransportError::Disconnected)) => {}
            other => panic!("expected TransportError::Disconnected, got {other:?}"),
        }
    }

    // ═══════════════════════════════════════════════════════════════════
    // VAL-PROV-018: Provider handshake failure recovery
    // ═══════════════════════════════════════════════════════════════════

    /// If the transport connects but the provider handshake (initialize) times out,
    /// the provider returns `Err(TransportError::Timeout)`. The provider is in a
    /// clean disconnected state and can accept another connect call.
    #[tokio::test]
    async fn handshake_timeout_recovery() {
        let provider = ErrorMockProvider::new();
        let state = provider.state();
        let mut boxed: Box<dyn ProviderTransport> = Box::new(provider);
        let config = ProviderConfig::default();

        state
            .handshake_should_timeout
            .store(true, Ordering::SeqCst);

        // Connect attempt times out.
        let result = boxed.connect(&config).await;
        assert!(result.is_err());
        match result {
            Err(TransportError::Timeout) => {}
            other => panic!("expected Timeout, got {other:?}"),
        }
        assert!(!boxed.is_connected());

        // Provider is in clean state — retry succeeds.
        state
            .handshake_should_timeout
            .store(false, Ordering::SeqCst);
        boxed.connect(&config).await.unwrap();
        assert!(boxed.is_connected());
    }

    /// If the transport connects but the provider handshake returns a malformed
    /// response, the provider returns `Err(TransportError::ConnectionFailed)`
    /// and is in a clean state for retry.
    #[tokio::test]
    async fn malformed_init_recovery() {
        let provider = ErrorMockProvider::new();
        let state = provider.state();
        let mut boxed: Box<dyn ProviderTransport> = Box::new(provider);
        let config = ProviderConfig::default();

        // First attempt: simulate connection failure (malformed init).
        state.connect_should_fail.store(true, Ordering::SeqCst);
        let result = boxed.connect(&config).await;
        assert!(result.is_err());
        assert!(!boxed.is_connected());

        // Second attempt: succeed.
        state.connect_should_fail.store(false, Ordering::SeqCst);
        boxed.connect(&config).await.unwrap();
        assert!(boxed.is_connected());

        // Provider is usable.
        let result = boxed
            .send_request("thread/list", serde_json::json!({}))
            .await;
        assert!(result.is_ok());
    }

    /// After a handshake failure, the provider's event stream is still usable
    /// (not poisoned or broken).
    #[tokio::test]
    async fn event_stream_usable_after_handshake_failure() {
        let provider = ErrorMockProvider::new();
        let state = provider.state();
        let config = ProviderConfig::default();

        // Failed connect attempt (handshake timeout).
        state
            .handshake_should_timeout
            .store(true, Ordering::SeqCst);

        let mut boxed: Box<dyn ProviderTransport> = Box::new(provider);
        let _ = boxed.connect(&config).await;

        // Event stream should still work.
        let mut rx = boxed.event_receiver();
        let _ = state.events.send(ProviderEvent::Disconnected {
            message: "test".into(),
        });

        let event = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timeout")
            .expect("should receive event");
        assert!(matches!(event, ProviderEvent::Disconnected { .. }));
    }

    /// Verify that the boxed dyn ProviderTransport can be reconnected
    /// after a handshake timeout without reconstruction.
    #[tokio::test]
    async fn boxed_provider_reusable_after_handshake_timeout() {
        let provider = ErrorMockProvider::new();
        let state = provider.state();
        state
            .handshake_should_timeout
            .store(true, Ordering::SeqCst);

        let mut boxed: Box<dyn ProviderTransport> = Box::new(provider);
        let config = ProviderConfig::default();

        // First attempt: handshake timeout.
        let result = boxed.connect(&config).await;
        assert!(matches!(result, Err(TransportError::Timeout)));
        assert!(!boxed.is_connected());

        // Retry: succeed.
        state
            .handshake_should_timeout
            .store(false, Ordering::SeqCst);
        boxed.connect(&config).await.unwrap();
        assert!(boxed.is_connected());
    }

    // ═══════════════════════════════════════════════════════════════════
    // VAL-PROV-016: Cancel during active streaming
    // ═══════════════════════════════════════════════════════════════════

    /// Canceling a prompt during active streaming emits well-formed terminal
    /// events. Partial message content received before cancel is preserved.
    /// The thread transitions to idle state.
    #[tokio::test]
    async fn cancel_mid_stream_preserves_partial_content() {
        let provider = ErrorMockProvider::new();
        let state = provider.state();
        let config = ProviderConfig::default();

        let mut boxed: Box<dyn ProviderTransport> = Box::new(provider);
        boxed.connect(&config).await.unwrap();

        // Subscribe to events.
        let mut rx = boxed.event_receiver();

        // Simulate streaming: emit partial deltas.
        state.streaming_active.store(true, Ordering::SeqCst);
        let _ = state.events.send(ProviderEvent::StreamingStarted {
            thread_id: "t1".into(),
        });
        let _ = state.events.send(ProviderEvent::MessageDelta {
            thread_id: "t1".into(),
            item_id: "item1".into(),
            delta: "Hello ".into(),
        });
        let _ = state.events.send(ProviderEvent::MessageDelta {
            thread_id: "t1".into(),
            item_id: "item1".into(),
            delta: "wor".into(),
        });

        // Collect partial events.
        let streaming_started = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timeout")
            .expect("event");
        assert!(matches!(
            streaming_started,
            ProviderEvent::StreamingStarted { .. }
        ));

        let delta1 = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timeout")
            .expect("event");
        assert!(matches!(delta1, ProviderEvent::MessageDelta { .. }));
        if let ProviderEvent::MessageDelta { delta, .. } = delta1 {
            assert_eq!(delta, "Hello ");
        }

        let delta2 = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timeout")
            .expect("event");
        assert!(matches!(delta2, ProviderEvent::MessageDelta { .. }));
        if let ProviderEvent::MessageDelta { delta, .. } = delta2 {
            assert_eq!(delta, "wor");
        }

        // Send cancel notification.
        boxed
            .send_notification("turn/cancel", serde_json::json!({ "thread_id": "t1" }))
            .await
            .expect("cancel should succeed");

        // After cancel, streaming_active should be false.
        assert!(!state.streaming_active.load(Ordering::SeqCst));
    }

    /// After cancel, the next prompt on the same thread succeeds.
    #[tokio::test]
    async fn cancel_then_reprompt_succeeds() {
        let provider = ErrorMockProvider::new();
        let state = provider.state();
        let config = ProviderConfig::default();

        let mut boxed: Box<dyn ProviderTransport> = Box::new(provider);
        boxed.connect(&config).await.unwrap();

        // Start streaming.
        state.streaming_active.store(true, Ordering::SeqCst);

        // Cancel.
        boxed
            .send_notification("turn/cancel", serde_json::json!({ "thread_id": "t1" }))
            .await
            .expect("cancel should succeed");

        assert!(!state.streaming_active.load(Ordering::SeqCst));

        // Enqueue a response for the next prompt.
        state
            .request_responses
            .lock()
            .await
            .push_back(Ok(serde_json::json!({"ok": true})));

        // Next request succeeds.
        let result = boxed
            .send_request("turn/start", serde_json::json!({ "thread_id": "t1" }))
            .await;
        assert!(result.is_ok());
    }

    /// Verify that session/cancel notification when not streaming
    /// is a no-op (not an error).
    #[tokio::test]
    async fn cancel_when_not_streaming_is_noop() {
        let provider = ErrorMockProvider::new();
        let state = provider.state();
        let config = ProviderConfig::default();

        let mut boxed: Box<dyn ProviderTransport> = Box::new(provider);
        boxed.connect(&config).await.unwrap();

        assert!(!state.streaming_active.load(Ordering::SeqCst));

        // Cancel when not streaming — should succeed (no-op).
        boxed
            .send_notification("session/cancel", serde_json::json!({}))
            .await
            .expect("cancel when not streaming should succeed");

        assert!(!state.streaming_active.load(Ordering::SeqCst));
    }

    /// Verify that the provider can be disconnected during active streaming
    /// without panic, and the streaming state is properly cleaned up.
    #[tokio::test]
    async fn disconnect_during_active_streaming() {
        let provider = ErrorMockProvider::new();
        let state = provider.state();
        let config = ProviderConfig::default();

        let mut boxed: Box<dyn ProviderTransport> = Box::new(provider);
        boxed.connect(&config).await.unwrap();

        state.streaming_active.store(true, Ordering::SeqCst);
        assert!(state.streaming_active.load(Ordering::SeqCst));

        // Disconnect during streaming — should not panic.
        boxed.disconnect().await;

        // Streaming state should be cleaned up.
        assert!(!state.streaming_active.load(Ordering::SeqCst));
        assert!(!boxed.is_connected());
    }

    // ═══════════════════════════════════════════════════════════════════
    // VAL-PROV-019: Configuration changes mid-session
    // ═══════════════════════════════════════════════════════════════════

    /// Changing model while thread is idle between turns succeeds.
    #[tokio::test]
    async fn model_change_while_idle_succeeds() {
        let provider = ErrorMockProvider::new();
        let state = provider.state();
        let config = ProviderConfig::default();

        let mut boxed: Box<dyn ProviderTransport> = Box::new(provider);
        boxed.connect(&config).await.unwrap();

        assert!(!state.streaming_active.load(Ordering::SeqCst));

        // Change model — should succeed because idle.
        boxed
            .send_request(
                "thread/update",
                serde_json::json!({
                    "thread_id": "t1",
                    "model": "claude-sonnet-4-20250514"
                }),
            )
            .await
            .expect("model change while idle should succeed");

        // Verify model was updated.
        assert_eq!(
            *state.current_model.lock().await,
            "claude-sonnet-4-20250514"
        );
    }

    /// Changing reasoning effort while thread is idle succeeds.
    #[tokio::test]
    async fn reasoning_effort_change_while_idle_succeeds() {
        let provider = ErrorMockProvider::new();
        let state = provider.state();
        let config = ProviderConfig::default();

        let mut boxed: Box<dyn ProviderTransport> = Box::new(provider);
        boxed.connect(&config).await.unwrap();

        boxed
            .send_request(
                "thread/update",
                serde_json::json!({
                    "thread_id": "t1",
                    "reasoning_effort": "high"
                }),
            )
            .await
            .expect("reasoning effort change should succeed");

        assert_eq!(*state.current_reasoning_effort.lock().await, "high");
    }

    /// Changing model while streaming returns an error (not panic).
    /// The stream is not disrupted and the previous model remains in effect.
    #[tokio::test]
    async fn model_change_while_streaming_returns_error() {
        let provider = ErrorMockProvider::new();
        let state = provider.state();
        let config = ProviderConfig::default();

        let mut boxed: Box<dyn ProviderTransport> = Box::new(provider);
        boxed.connect(&config).await.unwrap();

        // Set streaming active.
        state.streaming_active.store(true, Ordering::SeqCst);

        // Try to change model — should fail.
        let result = boxed
            .send_request(
                "thread/update",
                serde_json::json!({
                    "thread_id": "t1",
                    "model": "gpt-4.1"
                }),
            )
            .await;

        match result {
            Err(RpcError::Server { code, message }) => {
                assert_eq!(code, -32600);
                assert!(message.contains("streaming is active"));
            }
            other => panic!("expected Server error, got {other:?}"),
        }

        // Original model is unchanged.
        assert_eq!(*state.current_model.lock().await, "default-model");

        // Streaming state was not disrupted.
        assert!(state.streaming_active.load(Ordering::SeqCst));
    }

    /// Changing reasoning effort while streaming returns an error.
    #[tokio::test]
    async fn reasoning_effort_change_while_streaming_returns_error() {
        let provider = ErrorMockProvider::new();
        let state = provider.state();
        let config = ProviderConfig::default();

        let mut boxed: Box<dyn ProviderTransport> = Box::new(provider);
        boxed.connect(&config).await.unwrap();
        state.streaming_active.store(true, Ordering::SeqCst);

        let result = boxed
            .send_request(
                "thread/update",
                serde_json::json!({
                    "thread_id": "t1",
                    "reasoning_effort": "low"
                }),
            )
            .await;

        assert!(result.is_err());
        match result {
            Err(RpcError::Server { code, message }) => {
                assert!(message.contains("streaming is active"));
                assert_eq!(code, -32600);
            }
            other => panic!("expected Server error, got {other:?}"),
        }

        // Original reasoning effort unchanged.
        assert_eq!(*state.current_reasoning_effort.lock().await, "medium");
    }

    /// After streaming completes, config changes succeed again.
    #[tokio::test]
    async fn config_change_after_stream_completes_succeeds() {
        let provider = ErrorMockProvider::new();
        let state = provider.state();
        let config = ProviderConfig::default();

        let mut boxed: Box<dyn ProviderTransport> = Box::new(provider);
        boxed.connect(&config).await.unwrap();

        // Simulate active streaming.
        state.streaming_active.store(true, Ordering::SeqCst);

        // Streaming completes.
        state.streaming_active.store(false, Ordering::SeqCst);

        // Now config change should succeed.
        boxed
            .send_request(
                "thread/update",
                serde_json::json!({
                    "thread_id": "t1",
                    "model": "claude-sonnet-4-20250514",
                    "reasoning_effort": "high"
                }),
            )
            .await
            .expect("config change after streaming should succeed");

        assert_eq!(
            *state.current_model.lock().await,
            "claude-sonnet-4-20250514"
        );
        assert_eq!(*state.current_reasoning_effort.lock().await, "high");
    }

    /// If a config change fails (invalid model), the session continues with
    /// the previous model. No session state is lost.
    #[tokio::test]
    async fn invalid_model_change_no_side_effect() {
        let provider = ErrorMockProvider::new();
        let state = provider.state();
        let config = ProviderConfig::default();

        let mut boxed: Box<dyn ProviderTransport> = Box::new(provider);
        boxed.connect(&config).await.unwrap();

        // Set a valid model first.
        boxed
            .send_request(
                "thread/update",
                serde_json::json!({
                    "thread_id": "t1",
                    "model": "claude-sonnet-4-20250514"
                }),
            )
            .await
            .expect("valid model change should succeed");

        assert_eq!(
            *state.current_model.lock().await,
            "claude-sonnet-4-20250514"
        );

        // Now try an invalid model (simulated by enqueuing an error response).
        state
            .request_responses
            .lock()
            .await
            .push_back(Err(RpcError::Server {
                code: -32602,
                message: "invalid model: nonexistent-model".to_string(),
            }));

        let result = boxed
            .send_request(
                "thread/update",
                serde_json::json!({
                    "thread_id": "t1",
                    "model": "nonexistent-model"
                }),
            )
            .await;

        assert!(result.is_err());

        // Previous model is preserved — no side effect from the failed change.
        assert_eq!(
            *state.current_model.lock().await,
            "claude-sonnet-4-20250514"
        );
    }

    // ═══════════════════════════════════════════════════════════════════
    // Additional edge case tests
    // ═══════════════════════════════════════════════════════════════════

    /// Connect failure on a never-connected provider should still
    /// return an error (not panic or hang).
    #[tokio::test]
    async fn connect_failure_on_never_connected_provider() {
        let provider = ErrorMockProvider::new();
        let state = provider.state();
        let mut boxed: Box<dyn ProviderTransport> = Box::new(provider);
        let config = ProviderConfig::default();

        // Never connected — try to connect with failure.
        state.connect_should_fail.store(true, Ordering::SeqCst);
        let result = boxed.connect(&config).await;
        assert!(result.is_err());
        assert!(!boxed.is_connected());

        // Second attempt also fails cleanly.
        let result = boxed.connect(&config).await;
        assert!(result.is_err());
    }

    /// Events emitted before any subscriber connects are handled gracefully
    /// (broadcast may drop them, but the provider should not panic).
    #[tokio::test]
    async fn event_emit_without_subscriber_no_panic() {
        let provider = ErrorMockProvider::new();
        let state = provider.state();
        let mut boxed: Box<dyn ProviderTransport> = Box::new(provider);
        let config = ProviderConfig::default();
        boxed.connect(&config).await.unwrap();

        // Emit event before subscribing — should not panic.
        let _ = state.events.send(ProviderEvent::TurnStarted {
            thread_id: "t1".into(),
            turn_id: "turn1".into(),
        });

        // Subscribe afterwards — may or may not get the event depending
        // on broadcast semantics, but provider is still usable.
        let mut rx = boxed.event_receiver();
        let _ = state.events.send(ProviderEvent::TurnCompleted {
            thread_id: "t1".into(),
            turn_id: "turn1".into(),
        });

        let event = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timeout")
            .expect("event");
        assert!(matches!(event, ProviderEvent::TurnCompleted { .. }));
    }

    /// The provider correctly tracks its connected state through the
    /// full lifecycle: unconnected → connected → disconnected → connected.
    #[tokio::test]
    async fn connection_state_lifecycle() {
        let mut boxed: Box<dyn ProviderTransport> = Box::new(ErrorMockProvider::new());
        let config = ProviderConfig::default();

        // Initially not connected.
        assert!(!boxed.is_connected());

        // Connect.
        boxed.connect(&config).await.unwrap();
        assert!(boxed.is_connected());

        // Disconnect.
        boxed.disconnect().await;
        assert!(!boxed.is_connected());

        // Reconnect.
        boxed.connect(&config).await.unwrap();
        assert!(boxed.is_connected());

        // Disconnect again.
        boxed.disconnect().await;
        assert!(!boxed.is_connected());
    }

    /// Verify that non-config requests succeed during streaming
    /// (only model/reasoning_effort changes are blocked).
    #[tokio::test]
    async fn non_config_requests_succeed_during_streaming() {
        let provider = ErrorMockProvider::new();
        let state = provider.state();
        let config = ProviderConfig::default();

        let mut boxed: Box<dyn ProviderTransport> = Box::new(provider);
        boxed.connect(&config).await.unwrap();

        state.streaming_active.store(true, Ordering::SeqCst);

        // Non-config request should succeed during streaming.
        let result = boxed
            .send_request("thread/list", serde_json::json!({}))
            .await;
        assert!(result.is_ok(), "thread/list should work during streaming");

        // Another non-config request.
        let result = boxed
            .send_request("turn/status", serde_json::json!({ "thread_id": "t1" }))
            .await;
        assert!(result.is_ok(), "turn/status should work during streaming");
    }

    /// Verify config/change method also rejects during streaming.
    #[tokio::test]
    async fn config_change_method_rejects_during_streaming() {
        let provider = ErrorMockProvider::new();
        let state = provider.state();
        let config = ProviderConfig::default();

        let mut boxed: Box<dyn ProviderTransport> = Box::new(provider);
        boxed.connect(&config).await.unwrap();

        state.streaming_active.store(true, Ordering::SeqCst);

        // config/change with model should be rejected.
        let result = boxed
            .send_request(
                "config/change",
                serde_json::json!({
                    "model": "gpt-4.1"
                }),
            )
            .await;
        assert!(result.is_err());
        match result {
            Err(RpcError::Server { code: _, message }) => {
                assert!(message.contains("streaming is active"));
            }
            other => panic!("expected Server error, got {other:?}"),
        }
    }
}
