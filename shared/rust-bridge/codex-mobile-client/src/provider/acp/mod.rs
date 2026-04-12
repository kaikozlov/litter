//! ACP (Agent Client Protocol) transport module.
//!
//! Implements NDJSON framing over bidirectional streams and a mock transport
//! for testing. This module provides the transport layer that the ACP client
//! lifecycle (initialize → authenticate → session/*) will use in a follow-up
//! feature.
//!
//! # Module Structure
//! - `framing` — NDJSON codec: serialize to `\n`-delimited lines, deserialize
//!   incoming byte streams to typed ACP messages, with buffering for partial
//!   reads and graceful handling of malformed data.
//! - `mock` — In-memory bidirectional channel with message capture and response
//!   queuing, used by all ACP unit tests.

pub mod framing;
pub mod mock;
