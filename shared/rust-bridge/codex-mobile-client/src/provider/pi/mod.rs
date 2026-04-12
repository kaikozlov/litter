//! Pi provider module.
//!
//! Implements both native and ACP transports for the Pi coding agent:
//!
//! **Native transport:**
//! `SSH → spawn "pi --mode rpc" → stdin/stdout JSONL`
//!
//! **ACP transport:**
//! `SSH → spawn "npx pi-acp" → ACP JSON-RPC (NDJSON)`
//!
//! # Module Structure
//! - `transport` — `PiNativeTransport` implementing `ProviderTransport`,
//!   manages SSH channel lifecycle, JSONL framing, and event mapping.
//! - `acp_transport` — `PiAcpTransport` implementing `ProviderTransport`,
//!   wraps the universal ACP client for Pi via the `pi-acp` adapter.
//! - `protocol` — Pi RPC protocol types (commands, events, responses),
//!   JSON serialization/deserialization, and JSONL framing helpers.
//! - `mock` — Mock SSH channel for testing (in-memory JSONL exchange).
//! - `detection` — Pi auto-detection over SSH (probe for `pi` binary and
//!   `npx pi-acp` availability).
//! - `sessions` — Pi session persistence (list/load from `~/.pi/agent/sessions/`).
//! - `preference` — Transport preference selection (native preferred, ACP fallback).

pub mod acp_transport;
pub mod detection;
pub mod e2e;
pub mod mock;
pub mod preference;
pub mod protocol;
pub mod sessions;
pub mod transport;
