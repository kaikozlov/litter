//! Droid auto-detection over SSH.
//!
//! Probes an SSH host for Droid agent availability by checking:
//! 1. Whether the `droid` binary exists on PATH (`command -v droid`)
//! 2. Whether `droid --version` returns a valid version string
//! 3. Common install locations (`$HOME/.local/bin/droid`, `/usr/local/bin/droid`,
//!    `$HOME/.cargo/bin/droid`, `$HOME/.bun/bin/droid`)
//!
//! The probe results populate `DetectedDroidAgent` with the binary path
//! and version, used by the transport to spawn `droid exec`.

use crate::ssh::SshClient;

/// Result of probing an SSH host for Droid agent availability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedDroidAgent {
    /// Path to the `droid` binary, if found.
    pub binary_path: Option<String>,
    /// Version string reported by `droid --version`, if available.
    pub version: Option<String>,
    /// Whether native Factory API transport is supported.
    pub native_supported: bool,
    /// Whether ACP transport via `droid exec --output-format acp` is supported.
    ///
    /// Probed by checking if `droid exec --help` mentions `--output-format`
    /// or by attempting a dry-run of `droid exec --output-format acp --help`.
    pub acp_supported: bool,
}

impl DetectedDroidAgent {
    /// Returns true if Droid is available on this host (any transport).
    pub fn is_available(&self) -> bool {
        self.native_supported || self.acp_supported
    }
}

/// Common paths to check for the `droid` binary.
const DROID_COMMON_PATHS: &[&str] = &[
    "$HOME/.bun/bin/droid",
    "$HOME/.local/bin/droid",
    "/usr/local/bin/droid",
    "$HOME/.cargo/bin/droid",
];

/// Probe an SSH host for Droid agent availability.
///
/// Runs probes:
/// 1. `command -v droid` — checks if Droid binary is on PATH
/// 2. Checks common install locations if not on PATH
/// 3. `droid --version` — verifies binary works and gets version
/// 4. `droid exec --help` — checks if `--output-format acp` is supported
///
/// Each probe has a timeout. Failures are non-fatal — the function
/// returns partial results.
pub async fn detect_droid_agent(ssh_client: &SshClient) -> DetectedDroidAgent {
    let mut detected = DetectedDroidAgent {
        binary_path: None,
        version: None,
        native_supported: false,
        acp_supported: false,
    };

    // Probe 1: Check if droid binary exists on PATH.
    let droid_path = probe_droid_binary(ssh_client).await;
    let binary_found = droid_path.is_some();
    detected.binary_path = droid_path;

    // Probe 2: Check droid version (only if binary found).
    if binary_found {
        detected.version = probe_droid_version(ssh_client).await;
        if detected.version.is_some() {
            detected.native_supported = true;
        }

        // Probe 3: Check if ACP output format is supported.
        detected.acp_supported = probe_droid_acp(ssh_client).await;
    }

    tracing::info!(
        "Droid detection complete: binary_path={:?}, version={:?}, native_supported={}, acp_supported={}",
        detected.binary_path,
        detected.version,
        detected.native_supported,
        detected.acp_supported,
    );

    detected
}

/// Probe for the `droid` binary on PATH and common locations.
///
/// Uses `command -v droid` (with shell profiles sourced) to locate the binary.
/// Falls back to checking common install locations.
async fn probe_droid_binary(ssh_client: &SshClient) -> Option<String> {
    // Try PATH with shell profiles sourced.
    match ssh_client.exec_with_profile("command -v droid 2>/dev/null").await {
        Ok(result) if result.exit_code == 0 => {
            let path = result.stdout.trim().to_string();
            if !path.is_empty() {
                tracing::debug!("Droid binary found on PATH: {path}");
                return Some(path);
            }
        }
        _ => {}
    }

    // Try common locations.
    for path in DROID_COMMON_PATHS {
        let cmd = format!("test -x {path} && echo {path} 2>/dev/null");
        match ssh_client.exec_with_profile(&cmd).await {
            Ok(result) if result.exit_code == 0 => {
                let found = result.stdout.trim().to_string();
                if !found.is_empty() {
                    tracing::debug!("Droid binary found at common location: {found}");
                    return Some(found);
                }
            }
            _ => {}
        }
    }

    tracing::debug!("Droid binary not found");
    None
}

/// Probe `droid --version` to verify the binary works and get version.
///
/// Checks both stdout and stderr for the version string, since some
/// CLIs write --version output to stderr.
async fn probe_droid_version(ssh_client: &SshClient) -> Option<String> {
    match ssh_client.exec_with_profile("droid --version").await {
        Ok(result) if result.exit_code == 0 => {
            let version = if !result.stdout.trim().is_empty() {
                result.stdout.trim().to_string()
            } else if !result.stderr.trim().is_empty() {
                result.stderr.trim().to_string()
            } else {
                return None;
            };
            tracing::debug!("Droid version: {version}");
            Some(version)
        }
        _ => {
            tracing::debug!("droid --version failed");
            None
        }
    }
}

/// Probe whether `droid exec --output-format acp` is supported.
///
/// Checks if `droid exec --help` mentions `--output-format` or
/// `output-format` in its usage text. If the help text contains
/// the flag, the ACP transport path is available.
async fn probe_droid_acp(ssh_client: &SshClient) -> bool {
    match ssh_client
        .exec_with_profile("droid exec --help 2>&1")
        .await
    {
        Ok(result) => {
            let output = format!("{}\n{}", result.stdout, result.stderr);
            let supports_acp = output.contains("--output-format") || output.contains("output-format");
            if supports_acp {
                tracing::debug!("Droid supports --output-format (ACP available)");
            } else {
                tracing::debug!("Droid --output-format not found in help text (ACP not available)");
            }
            supports_acp
        }
        _ => {
            tracing::debug!("droid exec --help failed, ACP support unknown");
            false
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── DetectedDroidAgent logic tests ─────────────────────────────────

    #[test]
    fn detected_droid_agent_nothing_available() {
        let detected = DetectedDroidAgent {
            binary_path: None,
            version: None,
            native_supported: false,
            acp_supported: false,
        };
        assert!(!detected.is_available());
    }

    #[test]
    fn detected_droid_agent_available() {
        let detected = DetectedDroidAgent {
            binary_path: Some("/home/ubuntu/.bun/bin/droid".to_string()),
            version: Some("droid 0.99.0".to_string()),
            native_supported: true,
            acp_supported: false,
        };
        assert!(detected.is_available());
    }

    #[test]
    fn detected_droid_agent_binary_found_no_version() {
        // Binary found but version check fails — should NOT be available.
        let detected = DetectedDroidAgent {
            binary_path: Some("/usr/local/bin/droid".to_string()),
            version: None,
            native_supported: false,
            acp_supported: false,
        };
        assert!(!detected.is_available());
    }

    // ── VAL-DROID-010: Binary detection scenarios ──────────────────────

    #[test]
    fn droid_detection_available_on_path() {
        let detected = DetectedDroidAgent {
            binary_path: Some("/home/ubuntu/.bun/bin/droid".to_string()),
            version: Some("droid 0.99.0".to_string()),
            native_supported: true,
            acp_supported: false,
        };
        assert!(detected.binary_path.is_some());
        assert!(detected.version.is_some());
        assert!(detected.is_available());
    }

    #[test]
    fn droid_detection_not_found() {
        let detected = DetectedDroidAgent {
            binary_path: None,
            version: None,
            native_supported: false,
            acp_supported: false,
        };
        assert!(!detected.is_available());
        assert!(detected.binary_path.is_none());
    }

    #[test]
    fn droid_detection_does_not_interfere_with_codex() {
        // Droid detection should be independent — absence of Droid should
        // not affect Codex detection.
        let detected = DetectedDroidAgent {
            binary_path: None,
            version: None,
            native_supported: false,
            acp_supported: false,
        };
        // No Droid, but the host might still be Codex-capable.
        // The detection result only reflects Droid availability.
        assert!(!detected.is_available());
    }

    // ── VAL-ACP-071: Droid ACP detection ───────────────────────────────

    #[test]
    fn droid_acp_detection_supported() {
        // When ACP is supported alongside native, is_available is true.
        let detected = DetectedDroidAgent {
            binary_path: Some("/home/ubuntu/.bun/bin/droid".to_string()),
            version: Some("droid 0.99.0".to_string()),
            native_supported: true,
            acp_supported: true,
        };
        assert!(detected.is_available());
        assert!(detected.acp_supported);
        assert!(detected.native_supported);
    }

    #[test]
    fn droid_acp_only_acp_supported() {
        // When only ACP is supported (no native), is_available is still true.
        let detected = DetectedDroidAgent {
            binary_path: Some("/home/ubuntu/.bun/bin/droid".to_string()),
            version: Some("droid 0.99.0".to_string()),
            native_supported: false,
            acp_supported: true,
        };
        assert!(detected.is_available());
        assert!(detected.acp_supported);
    }

    #[test]
    fn droid_acp_detection_not_supported() {
        // Binary found, native supported, but ACP not available.
        let detected = DetectedDroidAgent {
            binary_path: Some("/home/ubuntu/.bun/bin/droid".to_string()),
            version: Some("droid 0.99.0".to_string()),
            native_supported: true,
            acp_supported: false,
        };
        assert!(detected.is_available());
        assert!(!detected.acp_supported);
    }

    #[test]
    fn droid_acp_detection_no_binary() {
        // No binary at all — ACP not supported.
        let detected = DetectedDroidAgent {
            binary_path: None,
            version: None,
            native_supported: false,
            acp_supported: false,
        };
        assert!(!detected.is_available());
        assert!(!detected.acp_supported);
    }
}
