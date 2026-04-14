//! ACP (Agent Client Protocol) transport module.
//!
//! Implements NDJSON framing over bidirectional streams, an ACP client
//! lifecycle driver, client-side request handlers, a generic ACP transport
//! for any ACP-compatible agent, and a mock transport for testing.
//!
//! # Module Structure
//! - `framing` — NDJSON codec: serialize to `\n`-delimited lines, deserialize
//!   incoming byte streams to typed ACP messages, with buffering for partial
//!   reads and graceful handling of malformed data.
//! - `client` — ACP protocol lifecycle: `initialize` → `authenticate` →
//!   `session/new` → `session/prompt` (streaming) → `session/cancel`.
//!   Handles protocol version negotiation, authentication failure retry,
//!   and cancel-during-streaming partial content flush. Also maps
//!   `SessionUpdate` → `ProviderEvent` for all streaming event types.
//! - `handlers` — Client-side request handlers: `fs/read_text_file`,
//!   `fs/write_text_file`, `request_permission` (with configurable policy),
//!   `terminal/*` (returns MethodNotFound).
//! - `generic_transport` — Configurable ACP transport for any agent binary.
//!   Accepts a configurable remote command (from `ProviderConfig::remote_command`)
//!   to launch the agent, then speaks standard ACP over the resulting stream.
//!   Implements the full `ProviderTransport` trait with handshake timeout,
//!   idempotent disconnect, and Codex wire method adapters.
//! - `mock` — In-memory bidirectional channel with message capture and response
//!   queuing, used by all ACP unit tests.

pub mod client;
pub mod framing;
pub mod generic_transport;
pub mod handlers;
pub mod mock;
