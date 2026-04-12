//! Mock SSH channel for Droid native transport testing.
//!
//! Provides an in-memory bidirectional channel that simulates the
//! stdin/stdout of a `droid exec --input-format stream-jsonrpc --output-format stream-jsonrpc`
//! process. Uses `tokio::sync::Notify` for proper async read signaling.
//!
//! Supports:
//! - Queuing JSON-RPC responses and notifications for sequential reads
//! - Capturing all requests sent to the "Droid process"
//! - Simulating process crash / disconnect
//! - Simulating `droid` binary not found

use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{Mutex, Notify};

/// Inner state shared between read and write halves.
#[derive(Debug)]
struct MockChannelInner {
    /// Buffer of queued data for reading.
    read_buffer: Vec<u8>,
    /// Buffer of data written to the channel (captured for inspection).
    written: Vec<u8>,
    /// Whether the channel is disconnected.
    disconnected: bool,
    /// Whether to simulate "droid binary not found".
    droid_not_found: bool,
}

/// Mock bidirectional channel for testing Droid native transport.
///
/// Implements `AsyncRead + AsyncWrite` so it can be used as a drop-in
/// replacement for a real SSH channel. Uses `Notify` to wake readers
/// when new data is queued.
#[derive(Debug, Clone)]
pub struct MockDroidChannel {
    inner: Arc<Mutex<MockChannelInner>>,
    /// Notifier to wake readers when new data is available.
    read_notify: Arc<Notify>,
}

impl Default for MockDroidChannel {
    fn default() -> Self {
        Self::new()
    }
}

impl MockDroidChannel {
    /// Create a new mock channel with empty state.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(MockChannelInner {
                read_buffer: Vec::new(),
                written: Vec::new(),
                disconnected: false,
                droid_not_found: false,
            })),
            read_notify: Arc::new(Notify::new()),
        }
    }

    /// Create a mock channel that simulates `droid` binary not found.
    pub fn new_droid_not_found() -> Self {
        let channel = Self::new();
        {
            let inner = Arc::clone(&channel.inner);
            let notify = Arc::clone(&channel.read_notify);
            tokio::spawn(async move {
                let mut guard = inner.lock().await;
                guard.droid_not_found = true;
                drop(guard);
                notify.notify_one();
            });
        }
        channel
    }

    /// Queue a raw line for reading (should be valid JSON-RPC).
    pub async fn queue_line(&self, line: &str) {
        let mut inner = self.inner.lock().await;
        inner.read_buffer.extend_from_slice(line.as_bytes());
        if !line.ends_with('\n') {
            inner.read_buffer.push(b'\n');
        }
        drop(inner);
        self.read_notify.notify_one();
    }

    /// Queue multiple lines at once.
    pub async fn queue_lines(&self, lines: &[&str]) {
        let mut inner = self.inner.lock().await;
        for line in lines {
            inner.read_buffer.extend_from_slice(line.as_bytes());
            if !line.ends_with('\n') {
                inner.read_buffer.push(b'\n');
            }
        }
        drop(inner);
        self.read_notify.notify_one();
    }

    /// Queue a JSON-RPC success response for a request ID.
    pub async fn queue_response(&self, id: i64, result: serde_json::Value) {
        let json = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        });
        let mut line = serde_json::to_string(&json).unwrap();
        line.push('\n');
        self.queue_line(&line).await;
    }

    /// Queue a JSON-RPC error response for a request ID.
    pub async fn queue_error_response(&self, id: i64, code: i64, message: &str) {
        let json = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": code, "message": message}
        });
        let mut line = serde_json::to_string(&json).unwrap();
        line.push('\n');
        self.queue_line(&line).await;
    }

    /// Queue a JSON-RPC notification.
    pub async fn queue_notification(&self, method: &str, params: serde_json::Value) {
        let json = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let mut line = serde_json::to_string(&json).unwrap();
        line.push('\n');
        self.queue_line(&line).await;
    }

    /// Queue a complete Droid lifecycle for a simple initialize + prompt/response flow.
    ///
    /// This simulates:
    /// 1. Response to `droid.initialize_session` (id=1)
    /// 2. Response to `droid.add_user_message` (id=2)
    /// 3. Working state → streaming notification
    /// 4. Create message notifications with text chunks
    /// 5. Working state → idle notification
    /// 6. Complete notification
    pub async fn queue_simple_session(&self, text_chunks: &[&str]) {
        // Initialize response
        self.queue_response(
            1,
            serde_json::json!({
                "sessionId": "droid-sess-1",
                "model": {"id": "claude-sonnet-4-20250514"},
            }),
        )
        .await;

        // Add user message response (ack)
        self.queue_response(2, serde_json::json!({"ok": true})).await;

        // Working state: streaming
        self.queue_notification(
            "droid_working_state_changed",
            serde_json::json!({"state": "streaming"}),
        )
        .await;

        // Create message notifications
        for (i, chunk) in text_chunks.iter().enumerate() {
            let is_last = i == text_chunks.len() - 1;
            self.queue_notification(
                "create_message",
                serde_json::json!({
                    "message_id": "droid-msg-1",
                    "text_delta": chunk,
                    "complete": is_last,
                }),
            )
            .await;
        }

        // Working state: idle
        self.queue_notification(
            "droid_working_state_changed",
            serde_json::json!({"state": "idle"}),
        )
        .await;

        // Complete
        self.queue_notification("complete", serde_json::json!({"reason": "done"}))
            .await;
    }

    /// Queue a complete Droid session with tool use.
    pub async fn queue_session_with_tool_use(
        &self,
        tool_name: &str,
        tool_output: &str,
        text: &str,
    ) {
        // Initialize response
        self.queue_response(
            1,
            serde_json::json!({
                "sessionId": "droid-sess-tool",
                "model": {"id": "claude-sonnet-4-20250514"},
            }),
        )
        .await;

        // Add user message response
        self.queue_response(2, serde_json::json!({"ok": true})).await;

        // Working state: streaming
        self.queue_notification(
            "droid_working_state_changed",
            serde_json::json!({"state": "streaming"}),
        )
        .await;

        // Tool result notification
        self.queue_notification(
            "tool_result",
            serde_json::json!({
                "tool_call_id": "tc-1",
                "tool_name": tool_name,
                "input": format!("running {tool_name}"),
                "output": tool_output,
                "status": "completed",
            }),
        )
        .await;

        // Assistant text
        if !text.is_empty() {
            self.queue_notification(
                "create_message",
                serde_json::json!({
                    "message_id": "droid-msg-1",
                    "text_delta": text,
                    "complete": true,
                }),
            )
            .await;
        }

        // Working state: idle
        self.queue_notification(
            "droid_working_state_changed",
            serde_json::json!({"state": "idle"}),
        )
        .await;

        // Complete
        self.queue_notification("complete", serde_json::json!({"reason": "done"}))
            .await;
    }

    /// Get all data written to the channel.
    pub async fn captured_output(&self) -> Vec<u8> {
        let inner = self.inner.lock().await;
        inner.written.clone()
    }

    /// Get captured output as a string.
    pub async fn captured_output_string(&self) -> String {
        String::from_utf8_lossy(&self.captured_output().await).to_string()
    }

    /// Get captured output as individual parsed JSON-RPC requests.
    pub async fn captured_requests(&self) -> Vec<serde_json::Value> {
        let output = self.captured_output_string().await;
        output
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    /// Simulate process crash / disconnect.
    pub async fn simulate_disconnect(&self) {
        let mut inner = self.inner.lock().await;
        inner.disconnected = true;
        drop(inner);
        self.read_notify.notify_waiters();
    }

    /// Check if disconnected.
    pub async fn is_disconnected(&self) -> bool {
        let inner = self.inner.lock().await;
        inner.disconnected
    }

    /// Clear state.
    pub async fn reset(&self) {
        let mut inner = self.inner.lock().await;
        inner.read_buffer.clear();
        inner.written.clear();
        inner.disconnected = false;
        inner.droid_not_found = false;
    }
}

impl AsyncRead for MockDroidChannel {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let inner = self.inner.clone();
        let notify = self.read_notify.clone();

        match inner.try_lock() {
            Ok(mut guard) => {
                if guard.droid_not_found {
                    return std::task::Poll::Ready(Ok(())); // EOF
                }
                if guard.disconnected && guard.read_buffer.is_empty() {
                    return std::task::Poll::Ready(Ok(())); // EOF
                }
                if guard.read_buffer.is_empty() {
                    drop(guard);
                    let waker = cx.waker().clone();
                    let notify_clone = notify.clone();
                    tokio::spawn(async move {
                        notify_clone.notified().await;
                        waker.wake();
                    });
                    return std::task::Poll::Pending;
                }

                let to_read = std::cmp::min(guard.read_buffer.len(), buf.remaining());
                buf.put_slice(&guard.read_buffer[..to_read]);
                guard.read_buffer.drain(..to_read);
                std::task::Poll::Ready(Ok(()))
            }
            Err(_) => {
                let waker = cx.waker().clone();
                let notify_clone = notify;
                tokio::spawn(async move {
                    notify_clone.notified().await;
                    waker.wake();
                });
                std::task::Poll::Pending
            }
        }
    }
}

impl AsyncWrite for MockDroidChannel {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let inner = self.inner.clone();
        match inner.try_lock() {
            Ok(mut guard) => {
                if guard.disconnected {
                    return std::task::Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "channel disconnected",
                    )));
                }
                guard.written.extend_from_slice(buf);
                std::task::Poll::Ready(Ok(buf.len()))
            }
            Err(_) => {
                _cx.waker().wake_by_ref();
                std::task::Poll::Pending
            }
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let inner = self.inner.clone();
        match inner.try_lock() {
            Ok(mut guard) => {
                guard.disconnected = true;
                std::task::Poll::Ready(Ok(()))
            }
            Err(_) => {
                _cx.waker().wake_by_ref();
                std::task::Poll::Pending
            }
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_channel_queue_and_read() {
        let channel = MockDroidChannel::new();
        channel.queue_line("test\n").await;
        assert!(!channel.is_disconnected().await);
    }

    #[tokio::test]
    async fn mock_channel_capture_write() {
        let channel = MockDroidChannel::new();
        use tokio::io::AsyncWriteExt;
        let mut writable = channel.clone();
        writable.write_all(b"test command\n").await.unwrap();
        let output = channel.captured_output_string().await;
        assert_eq!(output, "test command\n");
    }

    #[tokio::test]
    async fn mock_channel_disconnect() {
        let channel = MockDroidChannel::new();
        channel.queue_line("data\n").await;
        channel.simulate_disconnect().await;
        assert!(channel.is_disconnected().await);
    }

    #[tokio::test]
    async fn mock_channel_droid_not_found() {
        let channel = MockDroidChannel::new_droid_not_found();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let inner = channel.inner.lock().await;
        assert!(inner.droid_not_found);
    }

    #[tokio::test]
    async fn mock_channel_reset() {
        let channel = MockDroidChannel::new();
        channel.queue_line("data\n").await;
        channel.reset().await;
        let inner = channel.inner.lock().await;
        assert!(inner.read_buffer.is_empty());
        assert!(inner.written.is_empty());
        assert!(!inner.disconnected);
    }

    #[tokio::test]
    async fn mock_channel_queue_response() {
        let channel = MockDroidChannel::new();
        channel.queue_response(1, serde_json::json!({"ok": true})).await;
        let inner = channel.inner.lock().await;
        let data = String::from_utf8_lossy(&inner.read_buffer);
        assert!(data.contains("\"id\":1"));
        assert!(data.contains("\"ok\":true"));
    }

    #[tokio::test]
    async fn mock_channel_queue_notification() {
        let channel = MockDroidChannel::new();
        channel
            .queue_notification("test_method", serde_json::json!({"key": "value"}))
            .await;
        let inner = channel.inner.lock().await;
        let data = String::from_utf8_lossy(&inner.read_buffer);
        assert!(data.contains("\"method\":\"test_method\""));
    }

    #[tokio::test]
    async fn mock_channel_captured_requests() {
        let channel = MockDroidChannel::new();
        use tokio::io::AsyncWriteExt;
        let mut writable = channel.clone();
        writable
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"test\"}\n")
            .await
            .unwrap();
        let requests = channel.captured_requests().await;
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["method"], "test");
    }
}
