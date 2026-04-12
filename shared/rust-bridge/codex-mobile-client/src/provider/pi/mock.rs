//! Mock SSH channel for Pi native transport testing.
//!
//! Provides an in-memory bidirectional channel that simulates the
//! stdin/stdout of a `pi --mode rpc` process. Uses `tokio::sync::Notify`
//! for proper async read signaling.
//!
//! Supports:
//! - Queuing JSONL event responses for sequential reads
//! - Capturing all commands sent to the "Pi process"
//! - Simulating process crash / disconnect
//! - Simulating `pi` binary not found

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
    /// Whether to simulate "pi binary not found".
    pi_not_found: bool,
}

/// Mock bidirectional channel for testing Pi native transport.
///
/// Implements `AsyncRead + AsyncWrite` so it can be used as a drop-in
/// replacement for a real SSH channel. Uses `Notify` to wake readers
/// when new data is queued.
#[derive(Debug, Clone)]
pub struct MockPiChannel {
    inner: Arc<Mutex<MockChannelInner>>,
    /// Notifier to wake readers when new data is available.
    read_notify: Arc<Notify>,
}

impl Default for MockPiChannel {
    fn default() -> Self {
        Self::new()
    }
}

impl MockPiChannel {
    /// Create a new mock channel with empty state.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(MockChannelInner {
                read_buffer: Vec::new(),
                written: Vec::new(),
                disconnected: false,
                pi_not_found: false,
            })),
            read_notify: Arc::new(Notify::new()),
        }
    }

    /// Create a mock channel that simulates `pi` binary not found.
    pub fn new_pi_not_found() -> Self {
        let channel = Self::new();
        // Mark as pi_not_found and notify any waiting readers.
        {
            let inner = Arc::clone(&channel.inner);
            let notify = Arc::clone(&channel.read_notify);
            tokio::spawn(async move {
                let mut guard = inner.lock().await;
                guard.pi_not_found = true;
                drop(guard);
                notify.notify_one();
            });
        }
        channel
    }

    /// Queue a JSONL event line for reading.
    ///
    /// Lines are appended to the internal read buffer and any waiting
    /// reader is woken.
    pub async fn queue_event(&self, line: &str) {
        let mut inner = self.inner.lock().await;
        inner.read_buffer.extend_from_slice(line.as_bytes());
        drop(inner);
        self.read_notify.notify_one();
    }

    /// Queue multiple event lines at once.
    ///
    /// Each line will have a `\n` appended automatically for proper JSONL framing.
    pub async fn queue_events(&self, lines: &[&str]) {
        let mut inner = self.inner.lock().await;
        for line in lines {
            inner.read_buffer.extend_from_slice(line.as_bytes());
            inner.read_buffer.push(b'\n');
        }
        drop(inner);
        self.read_notify.notify_one();
    }

    /// Queue a complete Pi lifecycle for a simple prompt/response.
    pub async fn queue_simple_response(&self, text_chunks: &[&str]) {
        let events: Vec<String> = vec![
            "{\"type\":\"agent_start\"}\n".to_string(),
            "{\"type\":\"message_start\",\"message_id\":\"msg-1\",\"role\":\"assistant\"}\n"
                .to_string(),
        ]
        .into_iter()
        .chain(text_chunks.iter().map(|chunk| {
            format!(
                "{{\"type\":\"message_update\",\"message_id\":\"msg-1\",\"text_delta\":{}}}\n",
                serde_json::to_string(chunk).unwrap()
            )
        }))
        .chain(std::iter::once(
            "{\"type\":\"message_end\",\"message_id\":\"msg-1\"}\n".to_string(),
        ))
        .chain(std::iter::once(
            "{\"type\":\"agent_end\",\"reason\":\"complete\"}\n".to_string(),
        ))
        .collect();

        let all = events.join("");
        let mut inner = self.inner.lock().await;
        inner.read_buffer.extend_from_slice(all.as_bytes());
        drop(inner);
        self.read_notify.notify_one();
    }

    /// Queue a complete Pi lifecycle with thinking.
    pub async fn queue_response_with_thinking(&self, thinking: &str, text: &str) {
        let mut events = Vec::new();
        events.push("{\"type\":\"agent_start\"}\n".to_string());
        events.push(
            "{\"type\":\"message_start\",\"message_id\":\"msg-1\",\"role\":\"assistant\"}\n"
                .to_string(),
        );
        if !thinking.is_empty() {
            events.push(format!(
                "{{\"type\":\"message_update\",\"message_id\":\"msg-1\",\"thinking_delta\":{}}}\n",
                serde_json::to_string(thinking).unwrap()
            ));
        }
        if !text.is_empty() {
            events.push(format!(
                "{{\"type\":\"message_update\",\"message_id\":\"msg-1\",\"text_delta\":{}}}\n",
                serde_json::to_string(text).unwrap()
            ));
        }
        events.push("{\"type\":\"message_end\",\"message_id\":\"msg-1\"}\n".to_string());
        events.push("{\"type\":\"agent_end\",\"reason\":\"complete\"}\n".to_string());

        let all = events.join("");
        let mut inner = self.inner.lock().await;
        inner.read_buffer.extend_from_slice(all.as_bytes());
        drop(inner);
        self.read_notify.notify_one();
    }

    /// Queue a complete Pi lifecycle with tool execution.
    pub async fn queue_response_with_tool_execution(
        &self,
        command: &str,
        stdout: &str,
        text: &str,
    ) {
        let mut events = Vec::new();
        events.push("{\"type\":\"agent_start\"}\n".to_string());
        events.push(
            "{\"type\":\"message_start\",\"message_id\":\"msg-1\",\"role\":\"assistant\"}\n"
                .to_string(),
        );
        if !text.is_empty() {
            events.push(format!(
                "{{\"type\":\"message_update\",\"message_id\":\"msg-1\",\"text_delta\":{}}}\n",
                serde_json::to_string(text).unwrap()
            ));
        }
        events.push("{\"type\":\"message_end\",\"message_id\":\"msg-1\"}\n".to_string());
        events.push(format!(
            "{{\"type\":\"tool_execution_start\",\"execution_id\":\"exec-1\",\"command\":{}}}\n",
            serde_json::to_string(command).unwrap()
        ));
        if !stdout.is_empty() {
            events.push(format!(
                "{{\"type\":\"tool_execution_update\",\"execution_id\":\"exec-1\",\"stdout\":{}}}\n",
                serde_json::to_string(stdout).unwrap()
            ));
        }
        events.push(
            "{\"type\":\"tool_execution_end\",\"execution_id\":\"exec-1\",\"exit_code\":0}\n"
                .to_string(),
        );
        events.push("{\"type\":\"agent_end\",\"reason\":\"complete\"}\n".to_string());

        let all = events.join("");
        let mut inner = self.inner.lock().await;
        inner.read_buffer.extend_from_slice(all.as_bytes());
        drop(inner);
        self.read_notify.notify_one();
    }

    /// Queue a command response.
    pub async fn queue_command_response(&self, command: &str, data: serde_json::Value) {
        let event = serde_json::json!({
            "type": "response",
            "command": command,
            "data": data,
        });
        let mut line = serde_json::to_string(&event).unwrap();
        line.push('\n');
        self.queue_event(&line).await;
    }

    /// Queue an error response.
    pub async fn queue_error(&self, message: &str, code: i64) {
        let event = serde_json::json!({
            "type": "error",
            "message": message,
            "code": code,
        });
        let mut line = serde_json::to_string(&event).unwrap();
        line.push('\n');
        self.queue_event(&line).await;
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

    /// Get captured output as individual lines.
    pub async fn captured_commands(&self) -> Vec<String> {
        let output = self.captured_output_string().await;
        output
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.to_string())
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
        inner.pi_not_found = false;
    }
}

/// _Future: Wait for data to be available in the read buffer or for disconnect.
#[allow(dead_code)]
struct ReadReady {
    inner: Arc<Mutex<MockChannelInner>>,
    notify: Arc<Notify>,
}

impl std::future::Future for ReadReady {
    type Output = ();

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        // Check if data is available.
        if let Ok(guard) = self.inner.try_lock()
            && (!guard.read_buffer.is_empty() || guard.disconnected || guard.pi_not_found)
        {
            return std::task::Poll::Ready(());
        }
        // Wait for notification.
        let notified = self.notify.notified();
        std::pin::pin!(notified).poll(cx)
    }
}

impl AsyncRead for MockPiChannel {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let inner = self.inner.clone();
        let notify = self.read_notify.clone();

        match inner.try_lock() {
            Ok(mut guard) => {
                if guard.pi_not_found {
                    return std::task::Poll::Ready(Ok(())); // EOF
                }
                if guard.disconnected && guard.read_buffer.is_empty() {
                    return std::task::Poll::Ready(Ok(())); // EOF
                }
                if guard.read_buffer.is_empty() {
                    // No data yet — register for notification.
                    drop(guard);
                    // Set up the waker to be called when data arrives.
                    let waker = cx.waker().clone();
                    let notify_clone = notify.clone();
                    tokio::spawn(async move {
                        notify_clone.notified().await;
                        waker.wake();
                    });
                    return std::task::Poll::Pending;
                }

                // Copy available data.
                let to_read = std::cmp::min(guard.read_buffer.len(), buf.remaining());
                buf.put_slice(&guard.read_buffer[..to_read]);
                guard.read_buffer.drain(..to_read);
                std::task::Poll::Ready(Ok(()))
            }
            Err(_) => {
                // Lock contention — register for retry.
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

impl AsyncWrite for MockPiChannel {
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
        let channel = MockPiChannel::new();
        channel.queue_event("line1\n").await;
        channel.queue_event("line2\n").await;
        assert!(!channel.is_disconnected().await);
    }

    #[tokio::test]
    async fn mock_channel_capture_write() {
        let channel = MockPiChannel::new();
        use tokio::io::AsyncWriteExt;
        let mut writable = channel.clone();
        writable.write_all(b"test command\n").await.unwrap();
        let output = channel.captured_output_string().await;
        assert_eq!(output, "test command\n");
    }

    #[tokio::test]
    async fn mock_channel_disconnect() {
        let channel = MockPiChannel::new();
        channel.queue_event("data\n").await;
        channel.simulate_disconnect().await;
        assert!(channel.is_disconnected().await);
    }

    #[tokio::test]
    async fn mock_channel_pi_not_found() {
        let channel = MockPiChannel::new_pi_not_found();
        // Allow the spawned task to run.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let inner = channel.inner.lock().await;
        assert!(inner.pi_not_found);
    }

    #[tokio::test]
    async fn mock_channel_reset() {
        let channel = MockPiChannel::new();
        channel.queue_event("data\n").await;
        channel.reset().await;
        let inner = channel.inner.lock().await;
        assert!(inner.read_buffer.is_empty());
        assert!(inner.written.is_empty());
        assert!(!inner.disconnected);
    }
}
