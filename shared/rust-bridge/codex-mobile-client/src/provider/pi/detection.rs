//! Pi auto-detection over SSH.
//!
//! Probes an SSH host for Pi agent availability by checking:
//! 1. Whether the `pi` binary exists on PATH (`which pi` / `command -v pi`)
//! 2. Whether `pi --version` returns a valid version string
//! 3. Whether `npx pi-acp --version` is available (for ACP transport)
//!
//! The probe results populate `DetectedPiAgent` with the available transport
//! types, used by the transport preference logic to select the best transport.

use crate::ssh::SshClient;

/// Common paths to check for the `pi` binary.
const PI_COMMON_PATHS: &[&str] = &[
    "$HOME/.bun/bin/pi",
    "$HOME/.local/bin/pi",
    "/usr/local/bin/pi",
    "$HOME/.cargo/bin/pi",
    "$HOME/.volta/bin/pi",
];

/// Result of probing an SSH host for Pi agent availability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedPiAgent {
    /// Path to the `pi` binary, if found.
    pub pi_binary_path: Option<String>,
    /// Version string reported by `pi --version`, if available.
    pub pi_version: Option<String>,
    /// Whether `npx pi-acp` is available.
    pub pi_acp_available: bool,
    /// Detected transport types.
    pub detected_transports: Vec<PiTransportKind>,
}

/// Kind of Pi transport detected on a remote host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PiTransportKind {
    /// Native JSONL RPC transport (`pi --mode rpc`).
    Native,
    /// ACP transport via `npx pi-acp` adapter.
    Acp,
}

impl DetectedPiAgent {
    /// Returns true if any Pi transport is available.
    pub fn is_available(&self) -> bool {
        !self.detected_transports.is_empty()
    }

    /// Returns the preferred transport kind.
    ///
    /// Native is preferred over ACP if both are available.
    /// Returns `None` if no transport is available.
    pub fn preferred_transport(&self) -> Option<PiTransportKind> {
        if self.detected_transports.contains(&PiTransportKind::Native) {
            Some(PiTransportKind::Native)
        } else if self.detected_transports.contains(&PiTransportKind::Acp) {
            Some(PiTransportKind::Acp)
        } else {
            None
        }
    }

    /// Returns the fallback transport kind (opposite of preferred).
    ///
    /// Returns `None` if only one or no transport is available.
    pub fn fallback_transport(&self) -> Option<PiTransportKind> {
        match self.preferred_transport() {
            Some(PiTransportKind::Native) if self.detected_transports.contains(&PiTransportKind::Acp) => {
                Some(PiTransportKind::Acp)
            }
            Some(PiTransportKind::Acp) if self.detected_transports.contains(&PiTransportKind::Native) => {
                Some(PiTransportKind::Native)
            }
            _ => None,
        }
    }
}

/// Probe an SSH host for Pi agent availability.
///
/// Runs three probes:
/// 1. `which pi` / `command -v pi` — checks if Pi binary is on PATH
/// 2. `pi --version` — verifies Pi binary works and gets version
/// 3. `npx pi-acp --version` — checks if ACP adapter is available
///
/// Each probe has a timeout of `PROBE_TIMEOUT_SECS` seconds.
/// Failures are non-fatal — the function returns partial results.
pub async fn detect_pi_agent(ssh_client: &SshClient) -> DetectedPiAgent {
    let mut detected = DetectedPiAgent {
        pi_binary_path: None,
        pi_version: None,
        pi_acp_available: false,
        detected_transports: Vec::new(),
    };

    // Probe 1: Check if pi binary exists on PATH.
    let pi_path = probe_pi_binary(ssh_client).await;
    let pi_binary_found = pi_path.is_some();
    detected.pi_binary_path = pi_path;

    // Probe 2: Check pi version (only if binary found).
    if pi_binary_found {
        detected.pi_version = probe_pi_version(ssh_client).await;
        if detected.pi_version.is_some() {
            detected.detected_transports.push(PiTransportKind::Native);
        }
    }

    // Probe 3: Check npx pi-acp availability.
    detected.pi_acp_available = probe_pi_acp(ssh_client).await;
    if detected.pi_acp_available {
        detected.detected_transports.push(PiTransportKind::Acp);
    }

    tracing::info!(
        "Pi detection complete: binary_path={:?}, version={:?}, acp_available={}, transports={:?}",
        detected.pi_binary_path,
        detected.pi_version,
        detected.pi_acp_available,
        detected.detected_transports,
    );

    detected
}

/// Probe for the `pi` binary on PATH and in common install locations.
///
/// Uses `command -v pi` (with shell profiles sourced) to locate the binary.
/// Falls back to checking common install locations if not on PATH.
async fn probe_pi_binary(ssh_client: &SshClient) -> Option<String> {
    // Try PATH with shell profiles sourced.
    match ssh_client.exec_with_profile("command -v pi 2>/dev/null").await {
        Ok(result) if result.exit_code == 0 => {
            let path = result.stdout.trim().to_string();
            if !path.is_empty() {
                tracing::debug!("Pi binary found at: {path}");
                return Some(path);
            }
        }
        _ => {}
    }

    // Try common install locations.
    for path in PI_COMMON_PATHS {
        let cmd = format!("test -x {path} && echo {path} 2>/dev/null");
        if let Ok(result) = ssh_client.exec_with_profile(&cmd).await
            && result.exit_code == 0
        {
            let found = result.stdout.trim().to_string();
            if !found.is_empty() {
                tracing::debug!("Pi binary found at common location: {found}");
                return Some(found);
            }
        }
    }

    tracing::debug!("Pi binary not found");
    None
}

/// Probe `pi --version` to verify the binary works and get version.
///
/// Note: `pi --version` writes the version string to **stderr** (as of
/// pi 0.67.x). We must NOT discard stderr — instead we check both
/// stdout and stderr for the version string.
async fn probe_pi_version(ssh_client: &SshClient) -> Option<String> {
    match ssh_client.exec_with_profile("pi --version").await {
        Ok(result) if result.exit_code == 0 => {
            // pi --version may write to stderr or stdout depending on
            // version, so check both.
            let version = if !result.stdout.trim().is_empty() {
                result.stdout.trim().to_string()
            } else if !result.stderr.trim().is_empty() {
                result.stderr.trim().to_string()
            } else {
                return None;
            };
            tracing::debug!("Pi version: {version}");
            Some(version)
        }
        _ => {
            tracing::debug!("pi --version failed");
            None
        }
    }
}

/// Probe `npx pi-acp --version` to check if ACP adapter is available.
async fn probe_pi_acp(ssh_client: &SshClient) -> bool {
    match ssh_client
        .exec_with_profile("npx pi-acp --version 2>/dev/null")
        .await
    {
        Ok(result) if result.exit_code == 0 => {
            let output = result.stdout.trim();
            if output.is_empty() {
                tracing::debug!("npx pi-acp available but no version output");
                // Still consider it available if exit code was 0.
                true
            } else {
                tracing::debug!("npx pi-acp version: {output}");
                true
            }
        }
        _ => {
            tracing::debug!("npx pi-acp not available");
            false
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── DetectedPiAgent logic tests ────────────────────────────────────

    #[test]
    fn detected_pi_agent_nothing_available() {
        let detected = DetectedPiAgent {
            pi_binary_path: None,
            pi_version: None,
            pi_acp_available: false,
            detected_transports: Vec::new(),
        };
        assert!(!detected.is_available());
        assert_eq!(detected.preferred_transport(), None);
        assert_eq!(detected.fallback_transport(), None);
    }

    #[test]
    fn detected_pi_agent_native_only() {
        let detected = DetectedPiAgent {
            pi_binary_path: Some("/usr/local/bin/pi".to_string()),
            pi_version: Some("pi 0.66.1".to_string()),
            pi_acp_available: false,
            detected_transports: vec![PiTransportKind::Native],
        };
        assert!(detected.is_available());
        assert_eq!(detected.preferred_transport(), Some(PiTransportKind::Native));
        assert_eq!(detected.fallback_transport(), None);
    }

    #[test]
    fn detected_pi_agent_acp_only() {
        let detected = DetectedPiAgent {
            pi_binary_path: None,
            pi_version: None,
            pi_acp_available: true,
            detected_transports: vec![PiTransportKind::Acp],
        };
        assert!(detected.is_available());
        assert_eq!(detected.preferred_transport(), Some(PiTransportKind::Acp));
        assert_eq!(detected.fallback_transport(), None);
    }

    #[test]
    fn detected_pi_agent_both_available() {
        let detected = DetectedPiAgent {
            pi_binary_path: Some("/home/ubuntu/.bun/bin/pi".to_string()),
            pi_version: Some("pi 0.66.1".to_string()),
            pi_acp_available: true,
            detected_transports: vec![PiTransportKind::Native, PiTransportKind::Acp],
        };
        assert!(detected.is_available());
        // Native is preferred.
        assert_eq!(detected.preferred_transport(), Some(PiTransportKind::Native));
        // ACP is fallback.
        assert_eq!(detected.fallback_transport(), Some(PiTransportKind::Acp));
    }

    // ── VAL-PI-008: Auto-detection probe results ───────────────────────

    #[test]
    fn pi_detection_scenario_a_native_only() {
        // Scenario A: `which pi` succeeds, `npx pi-acp` fails
        let detected = DetectedPiAgent {
            pi_binary_path: Some("/usr/local/bin/pi".to_string()),
            pi_version: Some("pi 0.66.1".to_string()),
            pi_acp_available: false,
            detected_transports: vec![PiTransportKind::Native],
        };
        assert!(detected.detected_transports.contains(&PiTransportKind::Native));
        assert!(!detected.detected_transports.contains(&PiTransportKind::Acp));
    }

    #[test]
    fn pi_detection_scenario_b_both() {
        // Scenario B: both succeed
        let detected = DetectedPiAgent {
            pi_binary_path: Some("/home/ubuntu/.bun/bin/pi".to_string()),
            pi_version: Some("pi 0.66.1".to_string()),
            pi_acp_available: true,
            detected_transports: vec![PiTransportKind::Native, PiTransportKind::Acp],
        };
        assert!(detected.detected_transports.contains(&PiTransportKind::Native));
        assert!(detected.detected_transports.contains(&PiTransportKind::Acp));
    }

    #[test]
    fn pi_detection_scenario_c_neither() {
        // Scenario C: both fail
        let detected = DetectedPiAgent {
            pi_binary_path: None,
            pi_version: None,
            pi_acp_available: false,
            detected_transports: Vec::new(),
        };
        assert!(!detected.is_available());
        assert!(detected.detected_transports.is_empty());
    }

    #[test]
    fn pi_detection_partial_native_binary_no_version() {
        // Binary found but version check fails — should not add Native transport.
        let detected = DetectedPiAgent {
            pi_binary_path: Some("/usr/local/bin/pi".to_string()),
            pi_version: None, // Version check failed.
            pi_acp_available: false,
            detected_transports: Vec::new(), // Native NOT added because version check failed.
        };
        assert!(!detected.is_available());
    }

    // ── Transport preference tests (VAL-PI-015) ────────────────────────

    #[test]
    fn transport_preference_native_over_acp() {
        let detected = DetectedPiAgent {
            pi_binary_path: Some("/usr/local/bin/pi".to_string()),
            pi_version: Some("pi 0.66.1".to_string()),
            pi_acp_available: true,
            detected_transports: vec![PiTransportKind::Native, PiTransportKind::Acp],
        };
        // Native should be preferred.
        assert_eq!(detected.preferred_transport(), Some(PiTransportKind::Native));
    }

    #[test]
    fn transport_fallback_acp_when_native_unavailable() {
        // Only ACP available — should prefer ACP.
        let detected = DetectedPiAgent {
            pi_binary_path: None,
            pi_version: None,
            pi_acp_available: true,
            detected_transports: vec![PiTransportKind::Acp],
        };
        assert_eq!(detected.preferred_transport(), Some(PiTransportKind::Acp));
    }

    #[test]
    fn transport_fallback_native_preferred_acp_secondary() {
        let detected = DetectedPiAgent {
            pi_binary_path: Some("/usr/local/bin/pi".to_string()),
            pi_version: Some("pi 0.66.1".to_string()),
            pi_acp_available: true,
            detected_transports: vec![PiTransportKind::Native, PiTransportKind::Acp],
        };
        // Preferred = Native, Fallback = ACP.
        assert_eq!(detected.preferred_transport(), Some(PiTransportKind::Native));
        assert_eq!(detected.fallback_transport(), Some(PiTransportKind::Acp));
    }
}
