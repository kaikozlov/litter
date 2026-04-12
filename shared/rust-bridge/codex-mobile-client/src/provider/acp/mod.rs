//! ACP (Agent Client Protocol) transport module.
//!
//! Implements NDJSON framing over bidirectional streams, an ACP client
//! lifecycle driver, and a mock transport for testing.
//!
//! # Module Structure
//! - `framing` — NDJSON codec: serialize to `\n`-delimited lines, deserialize
//!   incoming byte streams to typed ACP messages, with buffering for partial
//!   reads and graceful handling of malformed data.
//! - `client` — ACP protocol lifecycle: `initialize` → `authenticate` →
//!   `session/new` → `session/prompt` (streaming) → `session/cancel`.
//!   Handles protocol version negotiation, authentication failure retry,
//!   and cancel-during-streaming partial content flush.
//! - `mock` — In-memory bidirectional channel with message capture and response
//!   queuing, used by all ACP unit tests.

pub mod client;
pub mod framing;
pub mod mock;
