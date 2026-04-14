//! Droid provider module.
//!
//! Implements the native Factory API transport and ACP transports
//! for the Droid coding agent:
//!
//! **Native transport:**
//! `SSH → spawn "droid exec --input-format stream-jsonrpc --output-format stream-jsonrpc" → JSON-RPC 2.0`
//!
//! **ACP transport (standard ACP — preferred):**
//! `SSH → spawn "droid exec --output-format acp" → ACP JSON-RPC over NDJSON`
//! Uses the shared `PiAcpTransport` / `AcpClient` pipeline for consistent
//! behavior with Pi ACP and GenericAcp agents.
//!
//! **ACP transport (legacy stream-json — deprecated):**
//! `SSH → spawn "droid exec --output-format stream-json --input-format stream-json" → Streaming JSON`
//! The `DroidAcpTransport` is deprecated. Use the standard ACP path instead.
//!
//! # Module Structure
//! - `transport` — `DroidNativeTransport` implementing `ProviderTransport`,
//!   manages SSH channel lifecycle, JSON-RPC 2.0 framing, and event mapping.
//! - `acp_transport` — `DroidAcpTransport` (DEPRECATED) implementing `ProviderTransport`,
//!   manages SSH channel lifecycle, stream-json framing, event mapping,
//!   MCP config management, and permission auto-handling.
//!   Retained for backward compatibility but NOT used by the factory.
//! - `stream_json` — Droid `--output-format stream-json` protocol types
//!   (system init, messages, tool calls, tool results, completion).
//!   Retained for backward compatibility.
//! - `protocol` — Droid Factory API types (requests, responses, notifications),
//!   JSON-RPC 2.0 serialization/deserialization, and framing helpers.
//! - `mock` — Mock SSH channel for testing (in-memory JSON-RPC exchange).
//! - `detection` — Droid auto-detection over SSH (probe for `droid` binary).
//! - `e2e` — E2E integration tests (VAL-CROSS-004/005/009/011/014).

#[allow(deprecated)]
pub mod acp_transport;
pub mod detection;
#[cfg(test)]
mod e2e;
pub mod mock;
pub mod protocol;
#[allow(deprecated)]
pub mod stream_json;
pub mod transport;
