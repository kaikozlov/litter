//! Droid provider module.
//!
//! Implements the native Factory API transport for the Droid coding agent:
//!
//! **Native transport:**
//! `SSH → spawn "droid exec --input-format stream-jsonrpc --output-format stream-jsonrpc" → JSON-RPC 2.0`
//!
//! # Module Structure
//! - `transport` — `DroidNativeTransport` implementing `ProviderTransport`,
//!   manages SSH channel lifecycle, JSON-RPC 2.0 framing, and event mapping.
//! - `protocol` — Droid Factory API types (requests, responses, notifications),
//!   JSON-RPC 2.0 serialization/deserialization, and framing helpers.
//! - `mock` — Mock SSH channel for testing (in-memory JSON-RPC exchange).
//! - `detection` — Droid auto-detection over SSH (probe for `droid` binary).

pub mod detection;
pub mod mock;
pub mod protocol;
pub mod transport;
