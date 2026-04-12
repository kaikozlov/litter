//! Pi native RPC transport module.
//!
//! Implements Pi's JSONL RPC protocol over SSH PTY:
//! `SSH → spawn "pi --mode rpc" → stdin/stdout JSONL`.
//!
//! # Module Structure
//! - `transport` — `PiNativeTransport` implementing `ProviderTransport`,
//!   manages SSH channel lifecycle, JSONL framing, and event mapping.
//! - `protocol` — Pi RPC protocol types (commands, events, responses),
//!   JSON serialization/deserialization, and JSONL framing helpers.
//! - `mock` — Mock SSH channel for testing (in-memory JSONL exchange).

pub mod mock;
pub mod protocol;
pub mod transport;
