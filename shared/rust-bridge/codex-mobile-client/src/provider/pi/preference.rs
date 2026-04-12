//! Pi transport preference and selection.
//!
//! Manages the selection between Pi native and ACP transports based on
//! detected availability and user configuration. The default preference
//! is native over ACP when both are available, with automatic fallback
//! when the preferred transport fails.
//!
//! # Transport Selection Logic
//!
//! 1. Detect available transports on the remote host.
//! 2. Apply user-configured preference (default: native preferred).
//! 3. If the preferred transport is unavailable, fall back to the other.
//! 4. If no transport is available, return an error.

use crate::provider::pi::detection::{DetectedPiAgent, PiTransportKind};
use crate::provider::AgentType;
use crate::transport::TransportError;

/// User-configured transport preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PiTransportPreference {
    /// Prefer native transport, fall back to ACP if unavailable.
    #[default]
    PreferNative,
    /// Prefer ACP transport, fall back to native if unavailable.
    PreferAcp,
    /// Force native transport (error if unavailable).
    ForceNative,
    /// Force ACP transport (error if unavailable).
    ForceAcp,
}

impl std::fmt::Display for PiTransportPreference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PreferNative => write!(f, "PreferNative"),
            Self::PreferAcp => write!(f, "PreferAcp"),
            Self::ForceNative => write!(f, "ForceNative"),
            Self::ForceAcp => write!(f, "ForceAcp"),
        }
    }
}

/// Result of transport selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PiTransportSelection {
    /// The selected transport kind.
    pub transport: PiTransportKind,
    /// Whether this was the user's preferred choice or a fallback.
    pub is_fallback: bool,
    /// The corresponding AgentType for UniFFI.
    pub agent_type: AgentType,
}

/// Select the best Pi transport based on detection results and user preference.
///
/// Returns the selected transport kind and whether it's a fallback from the
/// user's preference. Returns an error if no suitable transport is available.
pub fn select_pi_transport(
    detection: &DetectedPiAgent,
    preference: PiTransportPreference,
) -> Result<PiTransportSelection, TransportError> {
    match preference {
        PiTransportPreference::PreferNative => {
            // Try native first, fall back to ACP.
            if detection.detected_transports.contains(&PiTransportKind::Native) {
                Ok(PiTransportSelection {
                    transport: PiTransportKind::Native,
                    is_fallback: false,
                    agent_type: AgentType::PiNative,
                })
            } else if detection.detected_transports.contains(&PiTransportKind::Acp) {
                Ok(PiTransportSelection {
                    transport: PiTransportKind::Acp,
                    is_fallback: true,
                    agent_type: AgentType::PiAcp,
                })
            } else {
                Err(TransportError::ConnectionFailed(
                    "No Pi transport available on this host".to_string(),
                ))
            }
        }
        PiTransportPreference::PreferAcp => {
            // Try ACP first, fall back to native.
            if detection.detected_transports.contains(&PiTransportKind::Acp) {
                Ok(PiTransportSelection {
                    transport: PiTransportKind::Acp,
                    is_fallback: false,
                    agent_type: AgentType::PiAcp,
                })
            } else if detection.detected_transports.contains(&PiTransportKind::Native) {
                Ok(PiTransportSelection {
                    transport: PiTransportKind::Native,
                    is_fallback: true,
                    agent_type: AgentType::PiNative,
                })
            } else {
                Err(TransportError::ConnectionFailed(
                    "No Pi transport available on this host".to_string(),
                ))
            }
        }
        PiTransportPreference::ForceNative => {
            if detection.detected_transports.contains(&PiTransportKind::Native) {
                Ok(PiTransportSelection {
                    transport: PiTransportKind::Native,
                    is_fallback: false,
                    agent_type: AgentType::PiNative,
                })
            } else {
                Err(TransportError::ConnectionFailed(
                    "Native Pi transport not available (pi binary not found or --version failed)".to_string(),
                ))
            }
        }
        PiTransportPreference::ForceAcp => {
            if detection.detected_transports.contains(&PiTransportKind::Acp) {
                Ok(PiTransportSelection {
                    transport: PiTransportKind::Acp,
                    is_fallback: false,
                    agent_type: AgentType::PiAcp,
                })
            } else {
                Err(TransportError::ConnectionFailed(
                    "ACP Pi transport not available (npx pi-acp not found)".to_string(),
                ))
            }
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::pi::detection::PiTransportKind;

    fn detection_both() -> DetectedPiAgent {
        DetectedPiAgent {
            pi_binary_path: Some("/usr/local/bin/pi".to_string()),
            pi_version: Some("pi 0.66.1".to_string()),
            pi_acp_available: true,
            detected_transports: vec![PiTransportKind::Native, PiTransportKind::Acp],
        }
    }

    fn detection_native_only() -> DetectedPiAgent {
        DetectedPiAgent {
            pi_binary_path: Some("/usr/local/bin/pi".to_string()),
            pi_version: Some("pi 0.66.1".to_string()),
            pi_acp_available: false,
            detected_transports: vec![PiTransportKind::Native],
        }
    }

    fn detection_acp_only() -> DetectedPiAgent {
        DetectedPiAgent {
            pi_binary_path: None,
            pi_version: None,
            pi_acp_available: true,
            detected_transports: vec![PiTransportKind::Acp],
        }
    }

    fn detection_none() -> DetectedPiAgent {
        DetectedPiAgent {
            pi_binary_path: None,
            pi_version: None,
            pi_acp_available: false,
            detected_transports: Vec::new(),
        }
    }

    // ── VAL-PI-015: Transport preference selection ─────────────────────

    #[test]
    fn select_prefer_native_both_available() {
        let selection = select_pi_transport(&detection_both(), PiTransportPreference::PreferNative).unwrap();
        assert_eq!(selection.transport, PiTransportKind::Native);
        assert!(!selection.is_fallback);
        assert_eq!(selection.agent_type, AgentType::PiNative);
    }

    #[test]
    fn select_prefer_native_fallback_to_acp() {
        let selection = select_pi_transport(&detection_acp_only(), PiTransportPreference::PreferNative).unwrap();
        assert_eq!(selection.transport, PiTransportKind::Acp);
        assert!(selection.is_fallback);
        assert_eq!(selection.agent_type, AgentType::PiAcp);
    }

    #[test]
    fn select_prefer_acp_both_available() {
        let selection = select_pi_transport(&detection_both(), PiTransportPreference::PreferAcp).unwrap();
        assert_eq!(selection.transport, PiTransportKind::Acp);
        assert!(!selection.is_fallback);
        assert_eq!(selection.agent_type, AgentType::PiAcp);
    }

    #[test]
    fn select_prefer_acp_fallback_to_native() {
        let selection = select_pi_transport(&detection_native_only(), PiTransportPreference::PreferAcp).unwrap();
        assert_eq!(selection.transport, PiTransportKind::Native);
        assert!(selection.is_fallback);
        assert_eq!(selection.agent_type, AgentType::PiNative);
    }

    #[test]
    fn select_force_native_available() {
        let selection = select_pi_transport(&detection_native_only(), PiTransportPreference::ForceNative).unwrap();
        assert_eq!(selection.transport, PiTransportKind::Native);
        assert!(!selection.is_fallback);
    }

    #[test]
    fn select_force_native_unavailable() {
        let result = select_pi_transport(&detection_acp_only(), PiTransportPreference::ForceNative);
        assert!(result.is_err());
        match result.unwrap_err() {
            TransportError::ConnectionFailed(msg) => {
                assert!(msg.contains("Native Pi transport not available"));
            }
            other => panic!("expected ConnectionFailed, got: {other:?}"),
        }
    }

    #[test]
    fn select_force_acp_available() {
        let selection = select_pi_transport(&detection_acp_only(), PiTransportPreference::ForceAcp).unwrap();
        assert_eq!(selection.transport, PiTransportKind::Acp);
        assert!(!selection.is_fallback);
    }

    #[test]
    fn select_force_acp_unavailable() {
        let result = select_pi_transport(&detection_native_only(), PiTransportPreference::ForceAcp);
        assert!(result.is_err());
        match result.unwrap_err() {
            TransportError::ConnectionFailed(msg) => {
                assert!(msg.contains("ACP Pi transport not available"));
            }
            other => panic!("expected ConnectionFailed, got: {other:?}"),
        }
    }

    #[test]
    fn select_no_transport_available() {
        let result = select_pi_transport(&detection_none(), PiTransportPreference::PreferNative);
        assert!(result.is_err());
    }

    #[test]
    fn select_default_preference_is_prefer_native() {
        let pref = PiTransportPreference::default();
        assert_eq!(pref, PiTransportPreference::PreferNative);
    }

    #[test]
    fn transport_preference_display() {
        assert_eq!(format!("{}", PiTransportPreference::PreferNative), "PreferNative");
        assert_eq!(format!("{}", PiTransportPreference::PreferAcp), "PreferAcp");
        assert_eq!(format!("{}", PiTransportPreference::ForceNative), "ForceNative");
        assert_eq!(format!("{}", PiTransportPreference::ForceAcp), "ForceAcp");
    }

    // ── Mid-session transport switching ─────────────────────────────────

    #[test]
    fn transport_selection_recomputes_on_availability_change() {
        // Start with ACP only.
        let detection_v1 = detection_acp_only();
        let selection_v1 = select_pi_transport(&detection_v1, PiTransportPreference::PreferNative).unwrap();
        assert_eq!(selection_v1.transport, PiTransportKind::Acp);
        assert!(selection_v1.is_fallback);

        // Later, native becomes available (e.g. after install).
        let detection_v2 = detection_both();
        let selection_v2 = select_pi_transport(&detection_v2, PiTransportPreference::PreferNative).unwrap();
        assert_eq!(selection_v2.transport, PiTransportKind::Native);
        assert!(!selection_v2.is_fallback);
    }
}
