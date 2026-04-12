//! Droid provider module.
//!
//! Implements the native Factory API transport and ACP stream-json transport
//! for the Droid coding agent:
//!
//! **Native transport:**
//! `SSH → spawn "droid exec --input-format stream-jsonrpc --output-format stream-jsonrpc" → JSON-RPC 2.0`
//!
//! **ACP transport:**
//! `SSH → spawn "droid exec --output-format stream-json --input-format stream-json" → Streaming JSON`
//!
//! # Module Structure
//! - `transport` — `DroidNativeTransport` implementing `ProviderTransport`,
//!   manages SSH channel lifecycle, JSON-RPC 2.0 framing, and event mapping.
//! - `acp_transport` — `DroidAcpTransport` implementing `ProviderTransport`,
//!   manages SSH channel lifecycle, stream-json framing, event mapping,
//!   MCP config management, and permission auto-handling.
//! - `stream_json` — Droid `--output-format stream-json` protocol types
//!   (system init, messages, tool calls, tool results, completion).
//! - `protocol` — Droid Factory API types (requests, responses, notifications),
//!   JSON-RPC 2.0 serialization/deserialization, and framing helpers.
//! - `mock` — Mock SSH channel for testing (in-memory JSON-RPC exchange).
//! - `detection` — Droid auto-detection over SSH (probe for `droid` binary).

pub mod acp_transport;
pub mod detection;
pub mod mock;
pub mod protocol;
pub mod stream_json;
pub mod transport;
