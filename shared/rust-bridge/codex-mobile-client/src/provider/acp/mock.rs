//! Mock transport for ACP testing.
//!
//! Provides an in-memory bidirectional channel that supports:
//! - Queuing responses for sequential reads
//! - Capturing all sent messages
//! - Simulating disconnect
//!
//! This mock implements the same `AsyncRead + AsyncWrite` interface as real
//! transports (SSH PTY, TCP), so the NDJSON framing codec can be tested
//! without any I/O.

use std::io;
use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::Mutex;

/// State shared between the read and write halves of the mock transport.
#[derive(Debug)]
struct MockTransportInner {
    /// Queue of response lines that will be returned on read.
    read_queue: Vec<String>,
    /// Buffer of data written to the transport (captured for inspection).
    written: Vec<u8>,
    /// Whether the transport is disconnected.
    disconnected: bool,
}

/// Mock bidirectional transport for testing NDJSON framing.
///
/// Usage:
/// ```ignore
/// let transport = MockTransport::new();
/// transport.queue_response("{\"jsonrpc\":\"2.0\",...}\n");
/// // ... read from transport using NdjsonReader ...
/// let sent = transport.captured_output();
/// assert!(sent.contains("initialize"));
/// ```
#[derive(Debug, Clone)]
pub struct MockTransport {
    inner: Arc<Mutex<MockTransportInner>>,
}

impl MockTransport {
    /// Create a new mock transport with empty state.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(MockTransportInner {
                read_queue: Vec::new(),
                written: Vec::new(),
                disconnected: false,
            })),
        }
    }

    /// Queue a response line that will be returned on the next read.
    ///
    /// Lines are returned in FIFO order. Each call appends to the queue.
    /// The line should include a trailing `\n` if you want it to be treated
    /// as a complete line by the NDJSON reader.
    pub async fn queue_response(&self, line: &str) {
        let mut inner = self.inner.lock().await;
        inner.read_queue.push(line.to_string());
    }

    /// Queue multiple response lines at once.
    pub async fn queue_responses(&self, lines: &[&str]) {
        let mut inner = self.inner.lock().await;
        for line in lines {
            inner.read_queue.push(line.to_string());
        }
    }

    /// Get all data that has been written to the transport.
    pub async fn captured_output(&self) -> Vec<u8> {
        let inner = self.inner.lock().await;
        inner.written.clone()
    }

    /// Get the captured output as a UTF-8 string.
    pub async fn captured_output_string(&self) -> String {
        String::from_utf8_lossy(&self.captured_output().await).to_string()
    }

    /// Simulate a transport disconnect. Subsequent reads will return EOF
    /// and writes will return an error.
    pub async fn simulate_disconnect(&self) {
        let mut inner = self.inner.lock().await;
        inner.disconnected = true;
    }

    /// Check if the transport is disconnected.
    pub async fn is_disconnected(&self) -> bool {
        let inner = self.inner.lock().await;
        inner.disconnected
    }

    /// Clear all captured output and queued responses.
    pub async fn reset(&self) {
        let mut inner = self.inner.lock().await;
        inner.written.clear();
        inner.read_queue.clear();
        inner.disconnected = false;
    }
}

impl Default for MockTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl AsyncRead for MockTransport {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        let inner = self.inner.clone();
        // We need to do a synchronous poll. Since we're using Arc<Mutex>,
        // we need to try_lock. If we can't, we wake up and retry.
        let mut guard = match inner.try_lock() {
            Ok(g) => g,
            Err(_) => {
                cx.waker().wake_by_ref();
                return std::task::Poll::Pending;
            }
        };

        // If there's data in the queue, return it regardless of disconnect state.
        // This allows queued messages to be read even after disconnect was set.
        if !guard.read_queue.is_empty() {
            let response = guard.read_queue.remove(0);
            let response_bytes = response.as_bytes();
            let to_copy = std::cmp::min(response_bytes.len(), buf.remaining());
            buf.put_slice(&response_bytes[..to_copy]);

            // If there's leftover data, prepend it back.
            if to_copy < response_bytes.len() {
                guard
                    .read_queue
                    .insert(0, response[to_copy..].to_string());
            }

            return std::task::Poll::Ready(Ok(()));
        }

        if guard.disconnected {
            return std::task::Poll::Ready(Ok(())); // EOF
        }

        // No data available and not disconnected — signal pending.
        std::task::Poll::Pending
    }
}

impl AsyncWrite for MockTransport {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        let inner = self.inner.clone();
        let mut guard = match inner.try_lock() {
            Ok(g) => g,
            Err(_) => {
                // Can't lock — return 0 written, will retry.
                return std::task::Poll::Ready(Ok(0));
            }
        };

        if guard.disconnected {
            return std::task::Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "transport disconnected",
            )));
        }

        guard.written.extend_from_slice(buf);
        std::task::Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::acp::framing::{
        serialize_client_notification, serialize_client_request, NdjsonReader, FramingError,
    };
    use agent_client_protocol_schema::{
        CancelNotification, ClientNotification, ClientRequest, ProtocolVersion, RequestId,
        SessionId,
    };
    use tokio::io::AsyncWriteExt;

    /// Helper to create a valid session/update notification line.
    fn make_session_update_line(sid: &str) -> String {
        serde_json::to_string(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": sid,
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": { "type": "text", "text": "hello" }
                }
            }
        })).unwrap()
    }

    #[tokio::test]
    async fn mock_transport_queue_responses() {
        let transport = MockTransport::new();
        transport
            .queue_response(&format!("{}\n", make_session_update_line("s1")))
            .await;

        let mut reader = NdjsonReader::new(transport.clone());
        let msg = reader.next_message().await;
        assert!(msg.is_ok(), "should read queued response: {msg:?}");
    }

    #[tokio::test]
    async fn mock_transport_captures_sent_messages() {
        let transport = MockTransport::new();
        let mut writer = transport.clone();

        let cancel = ClientNotification::CancelNotification(
            CancelNotification::new(SessionId::new("test-session")),
        );
        let line = serialize_client_notification(&cancel).unwrap();
        crate::provider::acp::framing::write_line(&mut writer, &line)
            .await
            .unwrap();

        let captured = transport.captured_output_string().await;
        assert!(
            captured.contains("cancel"),
            "captured output should contain sent message"
        );
        assert!(
            captured.ends_with('\n'),
            "captured output should end with newline"
        );
    }

    #[tokio::test]
    async fn mock_transport_simulate_disconnect() {
        let transport = MockTransport::new();
        assert!(!transport.is_disconnected().await);

        transport.simulate_disconnect().await;
        assert!(transport.is_disconnected().await);

        // Reading from disconnected transport should give EOF.
        let mut reader = NdjsonReader::new(transport.clone());
        let result = reader.next_message().await;
        match result {
            Err(FramingError::StreamClosed) => {}
            other => panic!("expected StreamClosed after disconnect, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mock_transport_write_after_disconnect_errors() {
        let transport = MockTransport::new();
        let mut writer = transport.clone();

        transport.simulate_disconnect().await;

        let result: Result<(), _> = writer.write_all(b"test\n").await;
        assert!(result.is_err(), "write after disconnect should fail");
    }

    #[tokio::test]
    async fn mock_transport_multiple_queued_responses() {
        let transport = MockTransport::new();

        let line1 = format!("{}\n", make_session_update_line("s1"));
        let line2 = format!("{}\n", make_session_update_line("s2"));

        transport.queue_responses(&[&line1, &line2]).await;

        let mut reader = NdjsonReader::new(transport.clone());

        let msg1 = reader.next_message().await;
        assert!(msg1.is_ok(), "first message should succeed: {msg1:?}");

        let msg2 = reader.next_message().await;
        assert!(msg2.is_ok(), "second message should succeed: {msg2:?}");
    }

    #[tokio::test]
    async fn mock_transport_full_exchange() {
        let transport = MockTransport::new();

        // Queue a response from the "agent" — a simple JSON-RPC response.
        let response = serde_json::to_string(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": { "initialize": { "protocolVersion": "2025-03-26", "capabilities": {} } }
        })).unwrap();
        transport.queue_response(&format!("{response}\n")).await;

        // Client sends initialize request.
        let init = ClientRequest::InitializeRequest(
            agent_client_protocol_schema::InitializeRequest::new(
                ProtocolVersion::V1,
            ),
        );
        let line = serialize_client_request(&RequestId::Number(1), &init).unwrap();

        let mut writer = transport.clone();
        crate::provider::acp::framing::write_line(&mut writer, &line)
            .await
            .unwrap();

        // Verify sent data was captured.
        let captured = transport.captured_output_string().await;
        assert!(
            captured.contains("initialize"),
            "captured output should contain initialize"
        );
    }

    #[tokio::test]
    async fn mock_transport_reset_clears_state() {
        let transport = MockTransport::new();
        transport.queue_response("test\n").await;
        transport.simulate_disconnect().await;

        transport.reset().await;

        assert!(!transport.is_disconnected().await);
        assert!(transport.captured_output().await.is_empty());
    }

    #[tokio::test]
    async fn mock_transport_disconnect_mid_stream() {
        let transport = MockTransport::new();

        // Queue one valid message, then disconnect.
        transport
            .queue_response(&format!("{}\n", make_session_update_line("s1")))
            .await;
        transport.simulate_disconnect().await;

        let mut reader = NdjsonReader::new(transport.clone());

        // First message should succeed.
        let msg = reader.next_message().await;
        assert!(msg.is_ok(), "first message should succeed: {msg:?}");

        // Second read should get StreamClosed (no more data).
        let result = reader.next_message().await;
        match result {
            Err(FramingError::StreamClosed) => {}
            other => panic!(
                "expected StreamClosed after queue exhausted + disconnect, got {other:?}"
            ),
        }
    }
}
