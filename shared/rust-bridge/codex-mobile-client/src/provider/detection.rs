//! Combined agent detection helper.
//!
//! `detect_all_agents()` runs Pi and Droid detection in parallel via
//! `tokio::join!`, wraps both in a 5-second timeout, and always includes
//! Codex as a baseline agent. Returns a unified `Vec<AgentInfo>` with
//! correct `AgentType` variants.

use std::time::Duration;

use crate::provider::droid::detection::DetectedDroidAgent;
use crate::provider::pi::detection::DetectedPiAgent;
use crate::provider::{AgentInfo, AgentType};
use crate::ssh::SshClient;

/// Maximum time for each individual detection probe (Pi or Droid)
/// before falling back to partial results for that probe.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Aggregated result of probing an SSH host for all agent types.
///
/// Always includes Codex as a baseline (it is always available via the
/// standard bootstrap path). Pi and Droid entries are included when their
/// respective binaries are found on the remote host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedAgents {
    /// All detected agents, including Codex as a baseline.
    pub agents: Vec<AgentInfo>,
}

impl DetectedAgents {
    /// Returns `true` if at least one agent was detected.
    pub fn has_any_agent(&self) -> bool {
        !self.agents.is_empty()
    }

    /// Returns `true` if more than one agent type is available.
    pub fn has_multiple_agents(&self) -> bool {
        self.agents.len() > 1
    }

    /// Returns the first agent with the given type, if present.
    pub fn find_agent(&self, agent_type: AgentType) -> Option<&AgentInfo> {
        self.agents.iter().find(|a| a.detected_transports.contains(&agent_type))
    }

    /// Returns `true` if any Pi agent was detected.
    pub fn has_pi(&self) -> bool {
        self.agents.iter().any(|a| {
            a.detected_transports.contains(&AgentType::PiNative)
                || a.detected_transports.contains(&AgentType::PiAcp)
        })
    }

    /// Returns `true` if any Droid agent was detected.
    pub fn has_droid(&self) -> bool {
        self.agents.iter().any(|a| {
            a.detected_transports.contains(&AgentType::DroidNative)
                || a.detected_transports.contains(&AgentType::DroidAcp)
        })
    }
}

/// Map a detected Pi agent into `AgentInfo` entries.
///
/// Returns one entry for Native and/or one for ACP, depending on what
/// transports were detected.
fn map_pi_agent(detected: &DetectedPiAgent) -> Vec<AgentInfo> {
    let mut agents = Vec::new();

    if detected.detected_transports.contains(&super::pi::detection::PiTransportKind::Native) {
        let version_suffix = detected
            .pi_version
            .as_ref()
            .map(|v| format!(" ({v})"))
            .unwrap_or_default();
        agents.push(AgentInfo {
            id: "pi-native".to_string(),
            display_name: "Pi (Native)".to_string(),
            description: format!("Pi agent over native JSONL RPC{version_suffix}"),
            detected_transports: vec![AgentType::PiNative],
            capabilities: vec!["streaming".to_string(), "tools".to_string()],
        });
    }

    if detected.detected_transports.contains(&super::pi::detection::PiTransportKind::Acp) {
        agents.push(AgentInfo {
            id: "pi-acp".to_string(),
            display_name: "Pi (ACP)".to_string(),
            description: "Pi agent over ACP protocol".to_string(),
            detected_transports: vec![AgentType::PiAcp],
            capabilities: vec!["streaming".to_string(), "tools".to_string()],
        });
    }

    agents
}

/// Map a detected Droid agent into `AgentInfo` entries.
///
/// Returns entries for native and/or ACP transports depending on what
/// was detected. If ACP is supported, a separate `DroidAcp` entry is
/// included alongside or instead of the native entry.
fn map_droid_agent(detected: &DetectedDroidAgent) -> Vec<AgentInfo> {
    let mut agents = Vec::new();

    if detected.native_supported {
        let version_suffix = detected
            .version
            .as_ref()
            .map(|v| format!(" ({v})"))
            .unwrap_or_default();
        agents.push(AgentInfo {
            id: "droid-native".to_string(),
            display_name: "Droid (Native)".to_string(),
            description: format!("Droid agent over Factory API JSON-RPC{version_suffix}"),
            detected_transports: vec![AgentType::DroidNative],
            capabilities: vec!["streaming".to_string(), "tools".to_string()],
        });
    }

    if detected.acp_supported {
        agents.push(AgentInfo {
            id: "droid-acp".to_string(),
            display_name: "Droid (ACP)".to_string(),
            description: "Droid agent over standard ACP protocol".to_string(),
            detected_transports: vec![AgentType::DroidAcp],
            capabilities: vec!["streaming".to_string(), "tools".to_string(), "plans".to_string()],
        });
    }

    agents
}

/// Detect all agent types on an SSH host in parallel.
///
/// Runs `detect_pi_agent` and `detect_droid_agent` concurrently with
/// **independent timeouts**. Each probe gets `PROBE_TIMEOUT` seconds.
/// A slow Pi probe does NOT discard Droid results (and vice versa).
///
/// Also includes GenericAcp agents from user-configured ACP profiles,
/// when provided. GenericAcp agents don't require SSH probing — they
/// are included based on the configured remote commands.
///
/// # Timeout Behaviour
///
/// Each probe is independently wrapped in a timeout. If a probe
/// exceeds `PROBE_TIMEOUT`, its results are empty (but the other probe
/// may still succeed). Codex is always included as a baseline.
///
/// # Error Handling
///
/// Individual probe failures are logged and the corresponding agent
/// entries are simply omitted. Detection errors never block the
/// connection — callers can always fall back to Codex-only.
pub async fn detect_all_agents(ssh_client: &SshClient) -> DetectedAgents {
    detect_all_agents_with_profiles(ssh_client, &[]).await
}

/// Detect all agent types including GenericAcp from user-configured profiles.
///
/// In addition to the standard Pi and Droid probes, this function includes
/// GenericAcp agents from the provided ACP profiles. Each profile represents
/// a user-configured remote command that speaks the ACP protocol.
///
/// # Arguments
///
/// * `ssh_client` — SSH client for probing the remote host
/// * `acp_profiles` — User-configured ACP profiles with remote commands.
///   Each profile becomes a `GenericAcp` entry in the detected agents list.
pub async fn detect_all_agents_with_profiles(
    ssh_client: &SshClient,
    acp_profiles: &[AcpProfile],
) -> DetectedAgents {
    // Run both detection probes concurrently with independent timeouts.
    // Using independent timeouts ensures a slow Pi probe (e.g. npx pi-acp)
    // does not discard a fast Droid result.
    let pi_future = async {
        match tokio::time::timeout(
            PROBE_TIMEOUT,
            crate::provider::pi::detection::detect_pi_agent(ssh_client),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => {
                tracing::warn!(
                    "Pi detection timed out after {:.1}s",
                    PROBE_TIMEOUT.as_secs_f64()
                );
                DetectedPiAgent {
                    pi_binary_path: None,
                    pi_version: None,
                    pi_acp_available: false,
                    detected_transports: Vec::new(),
                }
            }
        }
    };

    let droid_future = async {
        match tokio::time::timeout(
            PROBE_TIMEOUT,
            crate::provider::droid::detection::detect_droid_agent(ssh_client),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => {
                tracing::warn!(
                    "Droid detection timed out after {:.1}s",
                    PROBE_TIMEOUT.as_secs_f64()
                );
                DetectedDroidAgent {
                    binary_path: None,
                    version: None,
                    native_supported: false,
                    acp_supported: false,
                }
            }
        }
    };

    let (pi_result, droid_result) = tokio::join!(pi_future, droid_future);

    // Build unified agent list.
    // Only include agents that were actually detected on the host.
    // Codex is NOT added as a baseline — it has its own install/bootstrap
    // path in the guided SSH connect flow. If no Pi/Droid agents are
    // found, the guided flow proceeds to Codex bootstrap automatically.
    let mut agents = Vec::new();
    agents.extend(map_pi_agent(&pi_result));
    agents.extend(map_droid_agent(&droid_result));

    // Include GenericAcp agents from user-configured profiles.
    // Each profile becomes a separate agent entry with the profile's
    // display name and remote command.
    agents.extend(map_acp_profiles(acp_profiles));

    tracing::info!(
        "Agent detection complete: {} agent(s) found — types: {:?}",
        agents.len(),
        agents
            .iter()
            .flat_map(|a| a.detected_transports.clone())
            .collect::<Vec<_>>(),
    );

    DetectedAgents { agents }
}

/// A user-configured ACP provider profile.
///
/// Represents a custom ACP-compatible agent with a user-specified
/// remote command. Used during detection to populate GenericAcp entries
/// in the detected agents list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpProfile {
    /// Unique identifier for this profile.
    pub id: String,
    /// Human-readable name displayed in the agent picker.
    pub display_name: String,
    /// The remote command to spawn over SSH for this ACP agent.
    pub remote_command: String,
}

/// Map user-configured ACP profiles into `AgentInfo` entries.
///
/// Each profile produces a GenericAcp agent entry with the profile's
/// display name and remote command in the description.
fn map_acp_profiles(profiles: &[AcpProfile]) -> Vec<AgentInfo> {
    profiles
        .iter()
        .map(|profile| AgentInfo {
            id: format!("generic-acp-{}", profile.id),
            display_name: profile.display_name.clone(),
            description: format!("Custom ACP agent: {}", profile.remote_command),
            detected_transports: vec![AgentType::GenericAcp],
            capabilities: vec![
                "streaming".to_string(),
                "tools".to_string(),
                "plans".to_string(),
            ],
        })
        .collect()
}

// ── Tests ───────────────────────────────────────────────────────────────────


// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::pi::detection::PiTransportKind;

    fn pi_native_info() -> AgentInfo {
        AgentInfo {
            id: "pi-native".to_string(),
            display_name: "Pi (Native)".to_string(),
            description: "Pi agent".to_string(),
            detected_transports: vec![AgentType::PiNative],
            capabilities: vec![],
        }
    }

    fn droid_native_info() -> AgentInfo {
        AgentInfo {
            id: "droid-native".to_string(),
            display_name: "Droid (Native)".to_string(),
            description: "Droid agent".to_string(),
            detected_transports: vec![AgentType::DroidNative],
            capabilities: vec![],
        }
    }

    #[test]
    fn val_detect_001_no_agents_found() {
        let pi = DetectedPiAgent {
            pi_binary_path: None,
            pi_version: None,
            pi_acp_available: false,
            detected_transports: Vec::new(),
        };
        let droid = DetectedDroidAgent {
            binary_path: None,
            version: None,
            native_supported: false,
            acp_supported: false,
        };
        let mut agents = Vec::new();
        agents.extend(map_pi_agent(&pi));
        agents.extend(map_droid_agent(&droid));
        let detected = DetectedAgents { agents };
        assert!(detected.agents.is_empty());
        assert!(!detected.has_any_agent());
        assert!(!detected.has_multiple_agents());
    }

    #[test]
    fn val_detect_002_pi_detected() {
        let pi = DetectedPiAgent {
            pi_binary_path: Some("/usr/local/bin/pi".to_string()),
            pi_version: Some("pi 0.66.1".to_string()),
            pi_acp_available: false,
            detected_transports: vec![PiTransportKind::Native],
        };
        let droid = DetectedDroidAgent {
            binary_path: None,
            version: None,
            native_supported: false,
            acp_supported: false,
        };
        let mut agents = Vec::new();
        agents.extend(map_pi_agent(&pi));
        agents.extend(map_droid_agent(&droid));
        let detected = DetectedAgents { agents };
        assert_eq!(detected.agents.len(), 1);
        assert!(detected.has_any_agent());
        assert!(detected.has_pi());
    }

    #[test]
    fn val_detect_003_droid_detected() {
        let pi = DetectedPiAgent {
            pi_binary_path: None,
            pi_version: None,
            pi_acp_available: false,
            detected_transports: Vec::new(),
        };
        let droid = DetectedDroidAgent {
            binary_path: Some("/home/ubuntu/.bun/bin/droid".to_string()),
            version: Some("droid 0.99.0".to_string()),
            native_supported: true,
            acp_supported: false,
        };
        let mut agents = Vec::new();
        agents.extend(map_pi_agent(&pi));
        agents.extend(map_droid_agent(&droid));
        let detected = DetectedAgents { agents };
        assert_eq!(detected.agents.len(), 1);
        assert!(detected.has_any_agent());
        assert!(detected.has_droid());
    }

    #[test]
    fn val_detect_004_both_detected() {
        let pi = DetectedPiAgent {
            pi_binary_path: Some("/home/ubuntu/.bun/bin/pi".to_string()),
            pi_version: Some("pi 0.66.1".to_string()),
            pi_acp_available: true,
            detected_transports: vec![PiTransportKind::Native, PiTransportKind::Acp],
        };
        let droid = DetectedDroidAgent {
            binary_path: Some("/home/ubuntu/.bun/bin/droid".to_string()),
            version: Some("droid 0.99.0".to_string()),
            native_supported: true,
            acp_supported: true,
        };
        let mut agents = Vec::new();
        agents.extend(map_pi_agent(&pi));
        agents.extend(map_droid_agent(&droid));
        let detected = DetectedAgents { agents };
        // PiNative + PiAcp + DroidNative + DroidAcp = 4.
        assert_eq!(detected.agents.len(), 4);
        assert!(detected.has_any_agent());
        assert!(detected.has_multiple_agents());
        assert!(detected.find_agent(AgentType::PiNative).is_some());
        assert!(detected.find_agent(AgentType::PiAcp).is_some());
        assert!(detected.find_agent(AgentType::DroidNative).is_some());
        assert!(detected.find_agent(AgentType::DroidAcp).is_some());
    }

    #[test]
    fn val_detect_006_timeout_returns_empty() {
        let mut agents = Vec::new();
        agents.extend(map_pi_agent(&DetectedPiAgent {
            pi_binary_path: None,
            pi_version: None,
            pi_acp_available: false,
            detected_transports: Vec::new(),
        }));
        agents.extend(map_droid_agent(&DetectedDroidAgent {
            binary_path: None,
            version: None,
            native_supported: false,
            acp_supported: false,
        }));
        let detected = DetectedAgents { agents };
        assert!(detected.agents.is_empty());
        assert!(!detected.has_any_agent());
    }

    #[test]
    fn val_detect_006_probe_timeout_is_10_seconds() {
        assert_eq!(PROBE_TIMEOUT, Duration::from_secs(10));
    }

    // ── Mapping edge cases ─────────────────────────────────────────────

    #[test]
    fn map_pi_agent_native_only() {
        let detected = DetectedPiAgent {
            pi_binary_path: Some("/usr/local/bin/pi".to_string()),
            pi_version: Some("pi 0.66.1".to_string()),
            pi_acp_available: false,
            detected_transports: vec![PiTransportKind::Native],
        };
        let agents = map_pi_agent(&detected);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].id, "pi-native");
    }

    #[test]
    fn map_pi_agent_acp_only() {
        let detected = DetectedPiAgent {
            pi_binary_path: None,
            pi_version: None,
            pi_acp_available: true,
            detected_transports: vec![PiTransportKind::Acp],
        };
        let agents = map_pi_agent(&detected);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].id, "pi-acp");
    }

    #[test]
    fn map_pi_agent_both_transports() {
        let detected = DetectedPiAgent {
            pi_binary_path: Some("/usr/local/bin/pi".to_string()),
            pi_version: Some("pi 0.66.1".to_string()),
            pi_acp_available: true,
            detected_transports: vec![PiTransportKind::Native, PiTransportKind::Acp],
        };
        let agents = map_pi_agent(&detected);
        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0].id, "pi-native");
        assert_eq!(agents[1].id, "pi-acp");
    }

    #[test]
    fn map_pi_agent_nothing_found() {
        let detected = DetectedPiAgent {
            pi_binary_path: None,
            pi_version: None,
            pi_acp_available: false,
            detected_transports: Vec::new(),
        };
        let agents = map_pi_agent(&detected);
        assert!(agents.is_empty());
    }

    #[test]
    fn map_droid_agent_available() {
        let detected = DetectedDroidAgent {
            binary_path: Some("/home/ubuntu/.bun/bin/droid".to_string()),
            version: Some("droid 0.99.0".to_string()),
            native_supported: true,
            acp_supported: false,
        };
        let agents = map_droid_agent(&detected);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].id, "droid-native");
    }

    #[test]
    fn map_droid_agent_not_available() {
        let detected = DetectedDroidAgent {
            binary_path: None,
            version: None,
            native_supported: false,
            acp_supported: false,
        };
        let agents = map_droid_agent(&detected);
        assert!(agents.is_empty());
    }

    #[test]
    fn map_droid_agent_acp_only() {
        let detected = DetectedDroidAgent {
            binary_path: Some("/home/ubuntu/.bun/bin/droid".to_string()),
            version: Some("droid 0.99.0".to_string()),
            native_supported: false,
            acp_supported: true,
        };
        let agents = map_droid_agent(&detected);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].id, "droid-acp");
        assert_eq!(agents[0].display_name, "Droid (ACP)");
        assert!(agents[0].detected_transports.contains(&AgentType::DroidAcp));
    }

    #[test]
    fn map_droid_agent_both_native_and_acp() {
        let detected = DetectedDroidAgent {
            binary_path: Some("/home/ubuntu/.bun/bin/droid".to_string()),
            version: Some("droid 0.99.0".to_string()),
            native_supported: true,
            acp_supported: true,
        };
        let agents = map_droid_agent(&detected);
        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0].id, "droid-native");
        assert_eq!(agents[1].id, "droid-acp");
    }

    // ── DetectedAgents helpers ─────────────────────────────────────────

    #[test]
    fn detected_agents_has_any_agent() {
        let detected = DetectedAgents { agents: vec![pi_native_info()] };
        assert!(detected.has_any_agent());
        assert!(!detected.has_multiple_agents());
    }

    #[test]
    fn detected_agents_has_multiple_agents_true() {
        let detected = DetectedAgents {
            agents: vec![pi_native_info(), droid_native_info()],
        };
        assert!(detected.has_any_agent());
        assert!(detected.has_multiple_agents());
    }

    #[test]
    fn detected_agents_find_agent() {
        let detected = DetectedAgents {
            agents: vec![pi_native_info(), droid_native_info()],
        };
        assert!(detected.find_agent(AgentType::PiNative).is_some());
        assert!(detected.find_agent(AgentType::DroidNative).is_some());
        assert!(detected.find_agent(AgentType::Codex).is_none());
    }

    #[test]
    fn detected_agents_has_pi_and_has_droid() {
        let detected = DetectedAgents {
            agents: vec![pi_native_info(), droid_native_info()],
        };
        assert!(detected.has_pi());
        assert!(detected.has_droid());
    }

    // ══════════════════════════════════════════════════════════════════════
    // VAL-ACP-070: GenericAcp detection
    // ══════════════════════════════════════════════════════════════════════

    #[test]
    fn acp_detection_generic_acp_from_profiles() {
        // When ACP profiles are provided, GenericAcp entries should appear
        // in the detected agents list.
        let profiles = vec![
            AcpProfile {
                id: "my-agent-1".to_string(),
                display_name: "My Custom Agent".to_string(),
                remote_command: "my-agent --acp --port 8080".to_string(),
            },
        ];

        let agents = map_acp_profiles(&profiles);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].id, "generic-acp-my-agent-1");
        assert_eq!(agents[0].display_name, "My Custom Agent");
        assert!(agents[0].detected_transports.contains(&AgentType::GenericAcp));
        assert!(agents[0].capabilities.contains(&"streaming".to_string()));
        assert!(agents[0].capabilities.contains(&"tools".to_string()));
    }

    #[test]
    fn acp_detection_generic_acp_multiple_profiles() {
        let profiles = vec![
            AcpProfile {
                id: "agent-1".to_string(),
                display_name: "Agent One".to_string(),
                remote_command: "agent-one --acp".to_string(),
            },
            AcpProfile {
                id: "agent-2".to_string(),
                display_name: "Agent Two".to_string(),
                remote_command: "agent-two --acp".to_string(),
            },
        ];

        let agents = map_acp_profiles(&profiles);
        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0].display_name, "Agent One");
        assert_eq!(agents[1].display_name, "Agent Two");
    }

    #[test]
    fn acp_detection_generic_acp_no_profiles() {
        // When no profiles are provided, no GenericAcp entries appear.
        let agents = map_acp_profiles(&[]);
        assert!(agents.is_empty());
    }

    #[test]
    fn acp_detection_generic_acp_in_combined_detection() {
        // GenericAcp entries from profiles are combined with Pi/Droid results.
        let pi = DetectedPiAgent {
            pi_binary_path: Some("/usr/local/bin/pi".to_string()),
            pi_version: Some("pi 0.66.1".to_string()),
            pi_acp_available: false,
            detected_transports: vec![PiTransportKind::Native],
        };
        let droid = DetectedDroidAgent {
            binary_path: Some("/home/ubuntu/.bun/bin/droid".to_string()),
            version: Some("droid 0.99.0".to_string()),
            native_supported: true,
            acp_supported: true,
        };
        let profiles = vec![AcpProfile {
            id: "my-agent".to_string(),
            display_name: "My Agent".to_string(),
            remote_command: "my-agent --acp".to_string(),
        }];

        let mut agents = Vec::new();
        agents.extend(map_pi_agent(&pi));
        agents.extend(map_droid_agent(&droid));
        agents.extend(map_acp_profiles(&profiles));

        let detected = DetectedAgents { agents };
        // PiNative + DroidNative + DroidAcp + GenericAcp = 4.
        assert_eq!(detected.agents.len(), 4);
        assert!(detected.find_agent(AgentType::PiNative).is_some());
        assert!(detected.find_agent(AgentType::DroidNative).is_some());
        assert!(detected.find_agent(AgentType::DroidAcp).is_some());
        assert!(detected.find_agent(AgentType::GenericAcp).is_some());
        assert!(detected.has_multiple_agents());
    }

    // ══════════════════════════════════════════════════════════════════════
    // VAL-ACP-071: Droid ACP detection updated for standard ACP
    // ══════════════════════════════════════════════════════════════════════

    #[test]
    fn acp_detection_droid_acp_appears_in_droid_mapping() {
        let detected = DetectedDroidAgent {
            binary_path: Some("/home/ubuntu/.bun/bin/droid".to_string()),
            version: Some("droid 0.99.0".to_string()),
            native_supported: true,
            acp_supported: true,
        };
        let agents = map_droid_agent(&detected);
        assert_eq!(agents.len(), 2);

        let droid_acp = agents.iter().find(|a| a.id == "droid-acp").unwrap();
        assert_eq!(droid_acp.display_name, "Droid (ACP)");
        assert!(droid_acp.detected_transports.contains(&AgentType::DroidAcp));
        assert!(droid_acp.capabilities.contains(&"plans".to_string()));
    }

    #[test]
    fn acp_detection_droid_acp_only_no_native() {
        // Droid with only ACP support (no native) still produces an agent entry.
        let detected = DetectedDroidAgent {
            binary_path: Some("/home/ubuntu/.bun/bin/droid".to_string()),
            version: Some("droid 0.99.0".to_string()),
            native_supported: false,
            acp_supported: true,
        };
        let agents = map_droid_agent(&detected);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].id, "droid-acp");
    }

    #[test]
    fn acp_detection_droid_has_droid_acp() {
        let droid_info = AgentInfo {
            id: "droid-acp".to_string(),
            display_name: "Droid (ACP)".to_string(),
            description: "Droid over ACP".to_string(),
            detected_transports: vec![AgentType::DroidAcp],
            capabilities: vec![],
        };
        let detected = DetectedAgents {
            agents: vec![droid_info],
        };
        assert!(detected.has_droid());
        assert!(detected.find_agent(AgentType::DroidAcp).is_some());
    }

    // ══════════════════════════════════════════════════════════════════════
    // AcpProfile construction tests
    // ══════════════════════════════════════════════════════════════════════

    #[test]
    fn acp_profile_construction() {
        let profile = AcpProfile {
            id: "test-1".to_string(),
            display_name: "Test Agent".to_string(),
            remote_command: "test-agent --acp".to_string(),
        };
        assert_eq!(profile.id, "test-1");
        assert_eq!(profile.display_name, "Test Agent");
        assert_eq!(profile.remote_command, "test-agent --acp");
    }

    #[test]
    fn acp_profile_equality() {
        let a = AcpProfile {
            id: "1".to_string(),
            display_name: "A".to_string(),
            remote_command: "cmd".to_string(),
        };
        let b = AcpProfile {
            id: "1".to_string(),
            display_name: "A".to_string(),
            remote_command: "cmd".to_string(),
        };
        assert_eq!(a, b);
    }

    #[test]
    fn acp_profile_inequality() {
        let a = AcpProfile {
            id: "1".to_string(),
            display_name: "A".to_string(),
            remote_command: "cmd".to_string(),
        };
        let b = AcpProfile {
            id: "2".to_string(),
            display_name: "B".to_string(),
            remote_command: "other".to_string(),
        };
        assert_ne!(a, b);
    }
}
