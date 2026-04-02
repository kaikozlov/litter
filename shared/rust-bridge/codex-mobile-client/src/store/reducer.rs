use std::collections::HashSet;
use std::sync::RwLock;

use tokio::sync::broadcast;

use crate::conversation::{make_error_item, make_model_rerouted_item, make_turn_diff_item};
use crate::conversation_uniffi::{
    HydratedAssistantMessageData, HydratedConversationItem, HydratedConversationItemContent,
    HydratedUserInputResponseData, HydratedUserInputResponseOptionData,
    HydratedUserInputResponseQuestionData,
};
use crate::session::connection::ServerConfig;
use crate::session::events::UiEvent;
use crate::types::{
    AppModeKind, AppOperationStatus, AppPlanProgressSnapshot, AppPlanStep, AppVoiceSessionPhase,
    AppVoiceTranscriptEntry, AppVoiceTranscriptUpdate,
};
use crate::types::{
    PendingApproval, PendingApprovalKey, PendingApprovalSeed, PendingApprovalWithSeed,
    PendingUserInputAnswer, PendingUserInputRequest, ThreadInfo, ThreadKey, ThreadSummaryStatus,
};

use super::actions::{
    conversation_item_from_upstream, thread_info_from_upstream,
    thread_info_from_upstream_status_change,
};
use super::boundary::{
    current_agent_directory_version, project_thread_state_update, project_thread_update,
};
use super::snapshot::{
    AppConnectionProgressSnapshot, AppQueuedFollowUpPreview, AppSnapshot, AppVoiceSessionSnapshot,
    QueuedFollowUpDraft, ServerHealthSnapshot, ServerSnapshot, ThreadSnapshot,
};
use super::updates::{AppStoreUpdateRecord, ThreadStreamingDeltaKind};
use super::voice::{VoiceDerivedUpdate, VoiceRealtimeState};

pub struct AppStoreReducer {
    snapshot: RwLock<AppSnapshot>,
    updates_tx: broadcast::Sender<AppStoreUpdateRecord>,
    voice_state: VoiceRealtimeState,
}

enum ItemMutationUpdate {
    Upsert(HydratedConversationItem),
    CommandExecutionUpdated {
        item_id: String,
        status: AppOperationStatus,
        exit_code: Option<i32>,
        duration_ms: Option<i64>,
        process_id: Option<String>,
        output_delta: Option<String>,
    },
}

impl AppStoreReducer {
    pub fn new() -> Self {
        // Streaming turns can burst small deltas quickly; keep enough headroom so
        // native subscribers do not immediately fall into lagged/full-resync mode.
        let (updates_tx, _) = broadcast::channel(1024);
        Self {
            snapshot: RwLock::new(AppSnapshot::default()),
            updates_tx,
            voice_state: VoiceRealtimeState::default(),
        }
    }

    pub fn snapshot(&self) -> AppSnapshot {
        self.snapshot
            .read()
            .expect("app store lock poisoned")
            .clone()
    }

    pub(crate) fn thread_snapshot(&self, key: &ThreadKey) -> Option<ThreadSnapshot> {
        self.snapshot
            .read()
            .expect("app store lock poisoned")
            .threads
            .get(key)
            .cloned()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AppStoreUpdateRecord> {
        self.updates_tx.subscribe()
    }

    pub fn upsert_server(&self, config: &ServerConfig, health: ServerHealthSnapshot) {
        {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            let (
                existing_account,
                requires_openai_auth,
                existing_rate_limits,
                existing_available_models,
                existing_has_ipc,
                existing_connection_progress,
            ) = if let Some(existing) = snapshot.servers.get(&config.server_id) {
                (
                    existing.account.clone(),
                    existing.requires_openai_auth,
                    existing.rate_limits.clone(),
                    existing.available_models.clone(),
                    existing.has_ipc,
                    existing.connection_progress.clone(),
                )
            } else {
                (None, false, None, None, false, None)
            };
            snapshot.servers.insert(
                config.server_id.clone(),
                ServerSnapshot {
                    server_id: config.server_id.clone(),
                    display_name: config.display_name.clone(),
                    host: config.host.clone(),
                    port: config.port,
                    is_local: config.is_local,
                    has_ipc: existing_has_ipc,
                    health,
                    account: existing_account,
                    requires_openai_auth,
                    rate_limits: existing_rate_limits,
                    available_models: existing_available_models,
                    connection_progress: existing_connection_progress,
                },
            );
        }
        self.emit(AppStoreUpdateRecord::ServerChanged {
            server_id: config.server_id.clone(),
        });
    }

    pub fn remove_server(&self, server_id: &str) {
        let mut removed_thread_keys = Vec::new();
        let agent_directory_version;
        {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            snapshot.servers.remove(server_id);
            snapshot.threads.retain(|key, _| {
                let keep = key.server_id != server_id;
                if !keep {
                    removed_thread_keys.push(key.clone());
                }
                keep
            });
            if snapshot
                .active_thread
                .as_ref()
                .is_some_and(|key| key.server_id == server_id)
            {
                snapshot.active_thread = None;
            }
            snapshot.pending_approvals.retain(|approval| {
                approval
                    .thread_id
                    .as_deref()
                    .is_none_or(|tid| !removed_thread_keys.iter().any(|key| key.thread_id == tid))
            });
            snapshot
                .pending_approval_seeds
                .retain(|key, _| key.server_id != server_id);
            snapshot
                .pending_user_inputs
                .retain(|request| request.server_id != server_id);
            if snapshot
                .voice_session
                .active_thread
                .as_ref()
                .is_some_and(|key| key.server_id == server_id)
            {
                snapshot.voice_session = AppVoiceSessionSnapshot::default();
            }
            agent_directory_version = current_agent_directory_version(&snapshot);
        }
        self.emit(AppStoreUpdateRecord::ServerRemoved {
            server_id: server_id.to_string(),
        });
        for key in removed_thread_keys {
            self.emit(AppStoreUpdateRecord::ThreadRemoved {
                key,
                agent_directory_version,
            });
        }
        self.emit(AppStoreUpdateRecord::ActiveThreadChanged { key: None });
    }

    pub fn sync_thread_list(&self, server_id: &str, threads: &[ThreadInfo]) {
        let incoming_ids = threads
            .iter()
            .map(|info| info.id.clone())
            .collect::<HashSet<_>>();
        let mut upserted_thread_keys = Vec::new();
        let mut updated_thread_keys = Vec::new();
        let mut removed_thread_keys = Vec::new();
        let mut active_thread_cleared = false;
        let mut pending_approvals = None;
        let mut pending_user_inputs = None;
        let mut voice_session_changed = false;
        let agent_directory_version;
        {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            let active_thread_key = snapshot.active_thread.clone();
            snapshot.threads.retain(|key, _| {
                let keep = key.server_id != server_id
                    || incoming_ids.contains(&key.thread_id)
                    || active_thread_key.as_ref() == Some(key);
                if !keep {
                    removed_thread_keys.push(key.clone());
                }
                keep
            });
            for info in threads {
                let key = ThreadKey {
                    server_id: server_id.to_string(),
                    thread_id: info.id.clone(),
                };
                if let Some(entry) = snapshot.threads.get_mut(&key) {
                    let mut next_info = info.clone();
                    preserve_thread_title(&entry.info, &mut next_info);
                    let next_model = next_info.model.clone().or_else(|| entry.model.clone());
                    let info_changed = entry.info != next_info;
                    let model_changed = entry.model != next_model;
                    if info_changed || model_changed {
                        entry.info = next_info;
                        entry.model = next_model;
                        updated_thread_keys.push(key);
                    }
                } else {
                    snapshot.threads.insert(
                        key.clone(),
                        ThreadSnapshot::from_info(server_id, info.clone()),
                    );
                    upserted_thread_keys.push(key);
                }
            }
            if snapshot.active_thread.as_ref().is_some_and(|key| {
                key.server_id == server_id && !incoming_ids.contains(&key.thread_id)
            }) {
                let should_clear = snapshot
                    .active_thread
                    .as_ref()
                    .is_some_and(|key| !snapshot.threads.contains_key(key));
                if should_clear {
                    snapshot.active_thread = None;
                    active_thread_cleared = true;
                }
            }
            let approvals_before = snapshot.pending_approvals.len();
            snapshot.pending_approvals.retain(|approval| {
                approval.thread_id.as_deref().is_none_or(|tid| {
                    !removed_thread_keys
                        .iter()
                        .any(|key| key.thread_id.as_str() == tid)
                })
            });
            let remaining_approval_keys = snapshot
                .pending_approvals
                .iter()
                .map(|approval| PendingApprovalKey {
                    server_id: approval.server_id.clone(),
                    request_id: approval.id.clone(),
                })
                .collect::<HashSet<_>>();
            snapshot
                .pending_approval_seeds
                .retain(|key, _| remaining_approval_keys.contains(key));
            if snapshot.pending_approvals.len() != approvals_before {
                pending_approvals = Some(snapshot.pending_approvals.clone());
            }
            let pending_user_inputs_before = snapshot.pending_user_inputs.len();
            snapshot.pending_user_inputs.retain(|request| {
                !(request.server_id == server_id
                    && removed_thread_keys
                        .iter()
                        .any(|key| key.thread_id == request.thread_id))
            });
            if snapshot.pending_user_inputs.len() != pending_user_inputs_before {
                pending_user_inputs = Some(snapshot.pending_user_inputs.clone());
            }
            if snapshot
                .voice_session
                .active_thread
                .as_ref()
                .is_some_and(|key| {
                    key.server_id == server_id && !incoming_ids.contains(&key.thread_id)
                })
            {
                snapshot.voice_session = AppVoiceSessionSnapshot::default();
                voice_session_changed = true;
            }
            agent_directory_version = current_agent_directory_version(&snapshot);
        }
        for key in removed_thread_keys {
            self.emit(AppStoreUpdateRecord::ThreadRemoved {
                key,
                agent_directory_version,
            });
        }
        for key in upserted_thread_keys {
            self.emit_thread_upsert(&key);
        }
        for key in updated_thread_keys {
            self.emit_thread_state_update(&key);
        }
        if let Some(approvals) = pending_approvals {
            self.emit(AppStoreUpdateRecord::PendingApprovalsChanged { approvals });
        }
        if let Some(requests) = pending_user_inputs {
            self.emit(AppStoreUpdateRecord::PendingUserInputsChanged { requests });
        }
        if voice_session_changed {
            self.emit(AppStoreUpdateRecord::VoiceSessionChanged);
        }
        if active_thread_cleared {
            self.emit(AppStoreUpdateRecord::ActiveThreadChanged { key: None });
        }
    }

    pub fn upsert_thread_snapshot(&self, mut thread: ThreadSnapshot) {
        let key = thread.key.clone();
        {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            if let Some(existing) = snapshot.threads.get(&key) {
                preserve_thread_title(&existing.info, &mut thread.info);
                preserve_thread_runtime_state(existing, &mut thread);
                preserve_local_overlay_items(existing, &mut thread);
                preserve_queued_follow_ups(existing, &mut thread);
            }
            if !thread.queued_follow_up_drafts.is_empty() || thread.queued_follow_ups.is_empty() {
                sync_thread_follow_up_projection(&mut thread);
            }
            snapshot.threads.insert(key.clone(), thread);
        }
        self.emit_thread_upsert(&key);
    }

    pub fn enqueue_thread_follow_up_preview(
        &self,
        key: &ThreadKey,
        preview: AppQueuedFollowUpPreview,
    ) {
        self.enqueue_thread_follow_up_draft(
            key,
            QueuedFollowUpDraft {
                preview,
                inputs: Vec::new(),
                source_message_json: None,
            },
        );
    }

    pub(crate) fn enqueue_thread_follow_up_draft(
        &self,
        key: &ThreadKey,
        draft: QueuedFollowUpDraft,
    ) {
        if self
            .mutate_thread_with_result(key, |thread| {
                thread.queued_follow_up_drafts.push(draft);
                sync_thread_follow_up_projection(thread);
            })
            .is_some()
        {
            self.emit_thread_state_update(key);
        }
    }

    pub fn set_thread_collaboration_mode(&self, key: &ThreadKey, mode: AppModeKind) {
        if self
            .mutate_thread_with_result(key, |thread| {
                if thread.collaboration_mode == mode {
                    return false;
                }
                thread.collaboration_mode = mode;
                true
            })
            .unwrap_or(false)
        {
            self.emit_thread_state_update(key);
        }
    }

    pub fn dismiss_plan_implementation_prompt(&self, key: &ThreadKey) {
        if self
            .mutate_thread_with_result(key, |thread| {
                let had_prompt = thread.pending_plan_implementation_turn_id.is_some();
                thread.pending_plan_implementation_turn_id = None;
                had_prompt
            })
            .unwrap_or(false)
        {
            self.emit_thread_state_update(key);
        }
    }

    pub fn remove_thread_follow_up_preview(&self, key: &ThreadKey, preview_id: &str) {
        self.remove_thread_follow_up_draft(key, preview_id);
    }

    pub(crate) fn remove_thread_follow_up_draft(&self, key: &ThreadKey, preview_id: &str) {
        if self
            .mutate_thread_with_result(key, |thread| {
                thread
                    .queued_follow_up_drafts
                    .retain(|draft| draft.preview.id != preview_id);
                sync_thread_follow_up_projection(thread);
            })
            .is_some()
        {
            self.emit_thread_state_update(key);
        }
    }

    pub fn set_thread_follow_up_previews(
        &self,
        key: &ThreadKey,
        previews: Vec<AppQueuedFollowUpPreview>,
    ) {
        let drafts = previews
            .into_iter()
            .map(|preview| QueuedFollowUpDraft {
                preview,
                inputs: Vec::new(),
                source_message_json: None,
            })
            .collect();
        self.set_thread_follow_up_drafts(key, drafts);
    }

    pub(crate) fn set_thread_follow_up_drafts(
        &self,
        key: &ThreadKey,
        drafts: Vec<QueuedFollowUpDraft>,
    ) {
        if self
            .mutate_thread_with_result(key, |thread| {
                if thread.queued_follow_up_drafts == drafts {
                    return false;
                }
                thread.queued_follow_up_drafts = drafts;
                sync_thread_follow_up_projection(thread);
                true
            })
            .unwrap_or(false)
        {
            self.emit_thread_state_update(key);
        }
    }

    pub fn remove_thread(&self, key: &ThreadKey) {
        let agent_directory_version;
        {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            snapshot.threads.remove(key);
            if snapshot.active_thread.as_ref() == Some(key) {
                snapshot.active_thread = None;
            }
            if snapshot.voice_session.active_thread.as_ref() == Some(key) {
                snapshot.voice_session = AppVoiceSessionSnapshot::default();
            }
            snapshot
                .pending_approvals
                .retain(|approval| approval.thread_id.as_deref() != Some(key.thread_id.as_str()));
            snapshot.pending_user_inputs.retain(|request| {
                !(request.server_id == key.server_id && request.thread_id == key.thread_id)
            });
            agent_directory_version = current_agent_directory_version(&snapshot);
        }
        self.emit(AppStoreUpdateRecord::ThreadRemoved {
            key: key.clone(),
            agent_directory_version,
        });
    }

    pub fn set_active_thread(&self, key: Option<ThreadKey>) {
        {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            snapshot.active_thread = key.clone();
        }
        self.emit(AppStoreUpdateRecord::ActiveThreadChanged { key });
    }

    pub fn set_voice_handoff_thread(&self, key: Option<ThreadKey>) {
        {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            snapshot.voice_session.handoff_thread_key = key;
        }
        self.emit(AppStoreUpdateRecord::VoiceSessionChanged);
    }

    pub fn replace_pending_approvals(&self, approvals: Vec<PendingApproval>) {
        {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            snapshot.pending_approvals = approvals.clone();
            snapshot.pending_approval_seeds.clear();
        }
        self.emit(AppStoreUpdateRecord::PendingApprovalsChanged { approvals });
    }

    pub(crate) fn replace_pending_approvals_with_seeds(
        &self,
        approvals: Vec<PendingApprovalWithSeed>,
    ) {
        let public_approvals = {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            snapshot.pending_approvals = approvals
                .iter()
                .map(|entry| entry.approval.clone())
                .collect();
            snapshot.pending_approval_seeds = approvals
                .into_iter()
                .map(|entry| {
                    (
                        PendingApprovalKey {
                            server_id: entry.approval.server_id.clone(),
                            request_id: entry.approval.id.clone(),
                        },
                        entry.seed,
                    )
                })
                .collect();
            snapshot.pending_approvals.clone()
        };
        self.emit(AppStoreUpdateRecord::PendingApprovalsChanged {
            approvals: public_approvals,
        });
    }

    pub fn replace_pending_user_inputs(&self, requests: Vec<PendingUserInputRequest>) {
        {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            snapshot.pending_user_inputs = requests.clone();
        }
        self.emit(AppStoreUpdateRecord::PendingUserInputsChanged { requests });
    }

    pub fn resolve_approval(&self, request_id: &str) {
        let approvals = {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            snapshot
                .pending_approvals
                .retain(|approval| approval.id != request_id);
            snapshot
                .pending_approval_seeds
                .retain(|key, _| key.request_id != request_id);
            snapshot.pending_approvals.clone()
        };
        self.emit(AppStoreUpdateRecord::PendingApprovalsChanged { approvals });
    }

    pub(crate) fn pending_approval_seed(
        &self,
        server_id: &str,
        request_id: &str,
    ) -> Option<PendingApprovalSeed> {
        self.snapshot
            .read()
            .expect("app store lock poisoned")
            .pending_approval_seeds
            .get(&PendingApprovalKey {
                server_id: server_id.to_string(),
                request_id: request_id.to_string(),
            })
            .cloned()
    }

    pub fn resolve_pending_user_input(&self, request_id: &str) {
        let requests = {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            snapshot
                .pending_user_inputs
                .retain(|request| request.id != request_id);
            snapshot.pending_user_inputs.clone()
        };
        self.emit(AppStoreUpdateRecord::PendingUserInputsChanged { requests });
    }

    pub fn resolve_pending_user_input_with_response(
        &self,
        request_id: &str,
        answers: Vec<PendingUserInputAnswer>,
    ) {
        let (requests, thread_key) = {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            let request = snapshot
                .pending_user_inputs
                .iter()
                .find(|request| request.id == request_id)
                .cloned();

            let mut thread_key = None;
            if let Some(request) = request {
                thread_key = Some(ThreadKey {
                    server_id: request.server_id.clone(),
                    thread_id: request.thread_id.clone(),
                });
                if let Some(thread) = snapshot.threads.get_mut(&ThreadKey {
                    server_id: request.server_id.clone(),
                    thread_id: request.thread_id.clone(),
                }) {
                    let item = answered_user_input_item(&request, &answers);
                    thread
                        .local_overlay_items
                        .retain(|existing| !is_duplicate_overlay_item(&item, existing));
                    thread.local_overlay_items.push(item);
                }
            }

            snapshot
                .pending_user_inputs
                .retain(|request| request.id != request_id);
            (snapshot.pending_user_inputs.clone(), thread_key)
        };
        self.emit(AppStoreUpdateRecord::PendingUserInputsChanged { requests });
        if let Some(key) = thread_key {
            self.emit_thread_upsert(&key);
        }
    }

    pub fn update_server_account(
        &self,
        server_id: &str,
        account: Option<crate::types::Account>,
        requires_openai_auth: bool,
    ) {
        {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            if let Some(server) = snapshot.servers.get_mut(server_id) {
                server.account = account;
                server.requires_openai_auth = requires_openai_auth;
            }
        }
        self.emit(AppStoreUpdateRecord::ServerChanged {
            server_id: server_id.to_string(),
        });
    }

    pub fn update_server_rate_limits(
        &self,
        server_id: &str,
        rate_limits: Option<crate::types::RateLimitSnapshot>,
    ) {
        {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            if let Some(server) = snapshot.servers.get_mut(server_id) {
                server.rate_limits = rate_limits;
            }
        }
        self.emit(AppStoreUpdateRecord::ServerChanged {
            server_id: server_id.to_string(),
        });
    }

    pub fn update_server_models(
        &self,
        server_id: &str,
        models: Option<Vec<crate::types::ModelInfo>>,
    ) {
        {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            if let Some(server) = snapshot.servers.get_mut(server_id) {
                server.available_models = models;
            }
        }
        self.emit(AppStoreUpdateRecord::ServerChanged {
            server_id: server_id.to_string(),
        });
    }

    pub fn update_server_ipc_state(&self, server_id: &str, has_ipc: bool) {
        {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            if let Some(server) = snapshot.servers.get_mut(server_id) {
                server.has_ipc = has_ipc;
            }
        }
        self.emit(AppStoreUpdateRecord::ServerChanged {
            server_id: server_id.to_string(),
        });
    }

    pub fn update_server_health(&self, server_id: &str, health: ServerHealthSnapshot) {
        {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            if let Some(server) = snapshot.servers.get_mut(server_id) {
                server.health = health;
            }
        }
        self.emit(AppStoreUpdateRecord::ServerChanged {
            server_id: server_id.to_string(),
        });
    }

    pub fn update_server_connection_progress(
        &self,
        server_id: &str,
        connection_progress: Option<AppConnectionProgressSnapshot>,
    ) {
        {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            if let Some(server) = snapshot.servers.get_mut(server_id) {
                server.connection_progress = connection_progress;
            }
        }
        self.emit(AppStoreUpdateRecord::ServerChanged {
            server_id: server_id.to_string(),
        });
    }

    pub(crate) fn apply_ui_event(&self, event: &UiEvent) {
        match event {
            UiEvent::ThreadStarted { key, notification } => {
                let info = thread_info_from_upstream(notification.thread.clone());
                self.upsert_or_merge_thread(key.clone(), info, |thread| {
                    thread.info.status = ThreadSummaryStatus::Active;
                    if thread.info.parent_thread_id.is_some() {
                        thread.info.agent_status = Some("running".to_string());
                    }
                });
            }
            UiEvent::ThreadArchived { key } => {
                self.remove_thread(key);
            }
            UiEvent::ThreadNameUpdated { key, thread_name } => {
                self.mutate_thread(key, |thread| {
                    thread.info.title = thread_name.clone();
                });
            }
            UiEvent::ThreadStatusChanged { key, notification } => {
                let info = thread_info_from_upstream_status_change(
                    &notification.thread_id,
                    notification.status.clone(),
                );
                self.upsert_or_merge_thread(key.clone(), info, |thread| {
                    if thread.info.parent_thread_id.is_some() {
                        thread.info.agent_status = match thread.info.status {
                            ThreadSummaryStatus::Active => Some("running".to_string()),
                            ThreadSummaryStatus::SystemError => Some("errored".to_string()),
                            ThreadSummaryStatus::Idle => thread
                                .info
                                .agent_status
                                .clone()
                                .or(Some("completed".to_string())),
                            ThreadSummaryStatus::NotLoaded => thread.info.agent_status.clone(),
                        };
                    }
                });
            }
            UiEvent::ModelRerouted { key, notification } => {
                let item = make_model_rerouted_item(
                    &notification.turn_id,
                    Some(notification.from_model.clone()),
                    notification.to_model.clone(),
                    Some(format_model_reroute_reason(&notification.reason)),
                    Some(&notification.turn_id),
                );
                if self
                    .mutate_thread_with_result(key, |thread| {
                        thread.model = Some(notification.to_model.clone());
                        thread.info.model = Some(notification.to_model.clone());
                        upsert_item(thread, item.clone());
                    })
                    .is_some()
                {
                    self.emit_thread_state_update(key);
                    self.emit_thread_item_upsert_owned(key, item);
                }
            }
            UiEvent::TurnStarted { key, turn_id } => {
                if self
                    .mutate_thread_with_result(key, |thread| {
                        remove_first_queued_follow_up(thread);
                        thread.active_turn_id = Some(turn_id.clone());
                        thread.active_plan_progress = None;
                        thread.pending_plan_implementation_turn_id = None;
                        thread.info.status = ThreadSummaryStatus::Active;
                        if thread.info.parent_thread_id.is_some() {
                            thread.info.agent_status = Some("running".to_string());
                        }
                    })
                    .is_some()
                {
                    self.emit_thread_state_update(key);
                }
            }
            UiEvent::TurnCompleted { key, .. } => {
                if self
                    .mutate_thread_with_result(key, |thread| {
                        thread.active_turn_id = None;
                        thread.active_plan_progress = None;
                        thread.info.status = ThreadSummaryStatus::Idle;
                        if thread.info.parent_thread_id.is_some() {
                            thread.info.agent_status = Some("completed".to_string());
                        }
                    })
                    .is_some()
                {
                    self.emit_thread_state_update(key);
                }
            }
            UiEvent::TurnPlanUpdated { key, notification } => {
                if self
                    .mutate_thread_with_result(key, |thread| {
                        thread.active_plan_progress = Some(AppPlanProgressSnapshot {
                            turn_id: notification.turn_id.clone(),
                            explanation: notification.explanation.clone(),
                            plan: notification
                                .plan
                                .iter()
                                .cloned()
                                .map(AppPlanStep::from)
                                .collect(),
                        });
                    })
                    .is_some()
                {
                    self.emit_thread_state_update(key);
                }
            }
            UiEvent::ItemStarted { key, notification } => {
                if let Some(item) = conversation_item_from_upstream(notification.item.clone()) {
                    self.apply_item_update(key, item);
                }
            }
            UiEvent::ItemCompleted { key, notification } => {
                if let Some(item) = conversation_item_from_upstream(notification.item.clone()) {
                    self.apply_item_update(key, item);
                }
                if matches!(
                    notification.item,
                    codex_app_server_protocol::ThreadItem::Plan { .. }
                ) && self
                    .mutate_thread_with_result(key, |thread| {
                        if thread.collaboration_mode != AppModeKind::Plan {
                            return false;
                        }
                        thread.pending_plan_implementation_turn_id =
                            Some(notification.turn_id.clone());
                        true
                    })
                    .unwrap_or(false)
                {
                    self.emit_thread_state_update(key);
                }
            }
            UiEvent::MessageDelta {
                key,
                item_id,
                delta,
            } => {
                let inserted_placeholder = self
                    .mutate_thread_with_result(key, |thread| {
                        append_assistant_delta(thread, item_id, delta)
                    })
                    .unwrap_or(false);
                if inserted_placeholder {
                    self.emit_thread_item_upsert_by_id(key, item_id);
                } else {
                    self.emit_thread_streaming_delta(
                        key,
                        item_id,
                        ThreadStreamingDeltaKind::AssistantText,
                        delta,
                    );
                }
            }
            UiEvent::ReasoningDelta {
                key,
                item_id,
                delta,
            } => {
                let updated = self
                    .mutate_thread_with_result(key, |thread| {
                        append_reasoning_delta(thread, item_id, delta)
                    })
                    .unwrap_or(false);
                if updated {
                    self.emit_thread_streaming_delta(
                        key,
                        item_id,
                        ThreadStreamingDeltaKind::ReasoningText,
                        delta,
                    );
                } else {
                    self.emit_thread_upsert(key);
                }
            }
            UiEvent::PlanDelta {
                key,
                item_id,
                delta,
            } => {
                let updated = self
                    .mutate_thread_with_result(key, |thread| {
                        append_plan_delta(thread, item_id, delta)
                    })
                    .unwrap_or(false);
                if updated {
                    self.emit_thread_streaming_delta(
                        key,
                        item_id,
                        ThreadStreamingDeltaKind::PlanText,
                        delta,
                    );
                } else {
                    self.emit_thread_upsert(key);
                }
            }
            UiEvent::CommandOutputDelta {
                key,
                item_id,
                delta,
            } => {
                let updated = self
                    .mutate_thread_with_result(key, |thread| {
                        append_command_output_delta(thread, item_id, delta)
                    })
                    .unwrap_or(false);
                if updated {
                    self.emit_thread_streaming_delta(
                        key,
                        item_id,
                        ThreadStreamingDeltaKind::CommandOutput,
                        delta,
                    );
                } else {
                    self.emit_thread_upsert(key);
                }
            }
            UiEvent::TurnDiffUpdated { key, notification } => {
                let item = make_turn_diff_item(
                    &notification.turn_id,
                    notification.diff.clone(),
                    Some(&notification.turn_id),
                );
                if self
                    .mutate_thread_with_result(key, |thread| upsert_item(thread, item.clone()))
                    .is_some()
                {
                    self.emit_thread_item_upsert_owned(key, item);
                }
            }
            UiEvent::McpToolCallProgress { key, notification } => {
                let updated = self
                    .mutate_thread_with_result(key, |thread| {
                        append_mcp_progress(thread, &notification.item_id, &notification.message)
                    })
                    .unwrap_or(false);
                if updated {
                    self.emit_thread_streaming_delta(
                        key,
                        &notification.item_id,
                        ThreadStreamingDeltaKind::McpProgress,
                        &notification.message,
                    );
                } else {
                    self.emit_thread_upsert(key);
                }
            }
            UiEvent::ApprovalRequested { approval, .. } => {
                let approvals = {
                    let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
                    if !snapshot
                        .pending_approvals
                        .iter()
                        .any(|existing| existing.id == approval.approval.id)
                    {
                        snapshot.pending_approvals.push(approval.approval.clone());
                        snapshot.pending_approval_seeds.insert(
                            PendingApprovalKey {
                                server_id: approval.approval.server_id.clone(),
                                request_id: approval.approval.id.clone(),
                            },
                            approval.seed.clone(),
                        );
                    }
                    snapshot.pending_approvals.clone()
                };
                self.emit(AppStoreUpdateRecord::PendingApprovalsChanged { approvals });
            }
            UiEvent::ServerRequestResolved { notification, .. } => {
                let request_id = match &notification.request_id {
                    codex_app_server_protocol::RequestId::String(value) => value.as_str(),
                    codex_app_server_protocol::RequestId::Integer(_) => return,
                };
                self.resolve_approval(request_id);
                self.resolve_pending_user_input(request_id);
            }
            UiEvent::AccountRateLimitsUpdated {
                server_id,
                notification,
            } => {
                let rate_limits = notification.rate_limits.clone().into();
                self.update_server_rate_limits(server_id, Some(rate_limits));
            }
            UiEvent::ConnectionStateChanged { server_id, health } => {
                {
                    let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
                    if let Some(server) = snapshot.servers.get_mut(server_id) {
                        let next_health = ServerHealthSnapshot::from_wire(health);
                        server.health = match (&server.connection_progress, next_health) {
                            // During SSH session replacement the old session can report a transient
                            // disconnect while the new transport is still bootstrapping. Keep the
                            // server visible as connecting until that progress finishes or an
                            // explicit reconnect failure updates the store directly.
                            (Some(_), ServerHealthSnapshot::Disconnected) => {
                                ServerHealthSnapshot::Connecting
                            }
                            (_, health) => health,
                        };
                    }
                }
                self.emit(AppStoreUpdateRecord::ServerChanged {
                    server_id: server_id.clone(),
                });
            }
            UiEvent::ContextTokensUpdated { key, used, limit } => {
                if self
                    .mutate_thread_with_result(key, |thread| {
                        thread.context_tokens_used = Some(*used);
                        thread.model_context_window = Some(*limit);
                    })
                    .is_some()
                {
                    self.emit_thread_state_update(key);
                }
            }
            UiEvent::RealtimeStarted { key, notification } => {
                self.voice_state.reset_thread(key);
                {
                    let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
                    snapshot.voice_session.active_thread = Some(key.clone());
                    snapshot.voice_session.session_id = notification.session_id.clone();
                    snapshot.voice_session.phase = Some(AppVoiceSessionPhase::Listening);
                    snapshot.voice_session.last_error = None;
                    snapshot.voice_session.transcript_entries.clear();
                    snapshot.voice_session.handoff_thread_key = None;
                    if let Some(thread) = snapshot.threads.get_mut(key) {
                        thread.realtime_session_id = notification.session_id.clone();
                    }
                }
                self.emit(AppStoreUpdateRecord::VoiceSessionChanged);
                let protocol_notification = crate::types::AppRealtimeStartedNotification {
                    thread_id: notification.thread_id.clone(),
                    session_id: notification.session_id.clone(),
                    version: match notification.version {
                        codex_protocol::protocol::RealtimeConversationVersion::V1 => {
                            "v1".to_string()
                        }
                        codex_protocol::protocol::RealtimeConversationVersion::V2 => {
                            "v2".to_string()
                        }
                    },
                };
                self.emit(AppStoreUpdateRecord::RealtimeStarted {
                    key: key.clone(),
                    notification: protocol_notification,
                });
                self.emit_thread_state_update(key);
            }
            UiEvent::RealtimeTranscriptUpdated { key, role, text } => {
                for update in self
                    .voice_state
                    .handle_typed_transcript_delta(key, role, text)
                {
                    match update {
                        VoiceDerivedUpdate::Transcript(update) => {
                            self.apply_voice_transcript_update(key, &update);
                            self.emit(AppStoreUpdateRecord::RealtimeTranscriptUpdated {
                                key: key.clone(),
                                update,
                            });
                        }
                        _ => {}
                    }
                }
            }
            UiEvent::RealtimeItemAdded { key, notification } => {
                for update in self.voice_state.handle_item(key, &notification.item) {
                    match update {
                        VoiceDerivedUpdate::Transcript(update) => {
                            self.apply_voice_transcript_update(key, &update);
                            self.emit(AppStoreUpdateRecord::RealtimeTranscriptUpdated {
                                key: key.clone(),
                                update,
                            });
                        }
                        VoiceDerivedUpdate::HandoffRequest(request) => {
                            {
                                let mut snapshot =
                                    self.snapshot.write().expect("app store lock poisoned");
                                snapshot.voice_session.phase = Some(AppVoiceSessionPhase::Handoff);
                            }
                            self.emit(AppStoreUpdateRecord::VoiceSessionChanged);
                            self.emit(AppStoreUpdateRecord::RealtimeHandoffRequested {
                                key: key.clone(),
                                request,
                            });
                        }
                        VoiceDerivedUpdate::SpeechStarted => {
                            {
                                let mut snapshot =
                                    self.snapshot.write().expect("app store lock poisoned");
                                snapshot.voice_session.phase =
                                    Some(AppVoiceSessionPhase::Listening);
                            }
                            self.emit(AppStoreUpdateRecord::VoiceSessionChanged);
                            self.emit(AppStoreUpdateRecord::RealtimeSpeechStarted {
                                key: key.clone(),
                            });
                        }
                    }
                }
            }
            UiEvent::RealtimeOutputAudioDelta { key, notification } => {
                {
                    let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
                    if snapshot.voice_session.active_thread.as_ref() == Some(key) {
                        snapshot.voice_session.phase = Some(AppVoiceSessionPhase::Speaking);
                    }
                }
                self.emit(AppStoreUpdateRecord::VoiceSessionChanged);
                let protocol_notification = crate::types::AppRealtimeOutputAudioDeltaNotification {
                    thread_id: notification.thread_id.clone(),
                    audio: crate::types::AppRealtimeAudioChunk {
                        item_id: notification.audio.item_id.clone(),
                        data: notification.audio.data.clone(),
                        sample_rate: notification.audio.sample_rate,
                        num_channels: notification.audio.num_channels.into(),
                        samples_per_channel: notification.audio.samples_per_channel,
                    },
                };
                self.emit(AppStoreUpdateRecord::RealtimeOutputAudioDelta {
                    key: key.clone(),
                    notification: protocol_notification,
                });
            }
            UiEvent::RealtimeError { key, notification } => {
                {
                    let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
                    snapshot.voice_session.phase = Some(AppVoiceSessionPhase::Error);
                    snapshot.voice_session.last_error = Some(notification.message.clone());
                }
                self.emit(AppStoreUpdateRecord::VoiceSessionChanged);
                let protocol_notification = crate::types::AppRealtimeErrorNotification {
                    thread_id: notification.thread_id.clone(),
                    message: notification.message.clone(),
                };
                self.emit(AppStoreUpdateRecord::RealtimeError {
                    key: key.clone(),
                    notification: protocol_notification,
                });
            }
            UiEvent::RealtimeClosed { key, notification } => {
                self.voice_state.clear_thread(key);
                {
                    let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
                    if let Some(thread) = snapshot.threads.get_mut(key) {
                        thread.realtime_session_id = None;
                    }
                    let reason = notification.reason.as_deref().unwrap_or("").trim();
                    if reason.is_empty() || reason == "requested" {
                        snapshot.voice_session = AppVoiceSessionSnapshot::default();
                    } else {
                        snapshot.voice_session.active_thread = Some(key.clone());
                        snapshot.voice_session.session_id = None;
                        snapshot.voice_session.phase = Some(AppVoiceSessionPhase::Error);
                        snapshot.voice_session.last_error = Some(reason.to_string());
                        snapshot.voice_session.handoff_thread_key = None;
                    }
                }
                self.emit(AppStoreUpdateRecord::VoiceSessionChanged);
                let protocol_notification = crate::types::AppRealtimeClosedNotification {
                    thread_id: notification.thread_id.clone(),
                    reason: notification.reason.clone(),
                };
                self.emit(AppStoreUpdateRecord::RealtimeClosed {
                    key: key.clone(),
                    notification: protocol_notification,
                });
                self.emit_thread_state_update(key);
            }
            UiEvent::Error { key, message, code } => {
                if let Some(key) = key {
                    let item = {
                        let mut item = None;
                        self.mutate_thread_with_result(key, |thread| {
                            let next = make_error_item(
                                format!("error-{}-{}", key.thread_id, thread.items.len()),
                                message.clone(),
                                *code,
                            );
                            thread.items.push(next.clone());
                            item = Some(next);
                        });
                        item
                    };
                    if let Some(item) = item {
                        self.emit_thread_item_upsert_owned(key, item);
                    }
                }
            }
            UiEvent::UserInputRequested { request } => {
                let requests = {
                    let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
                    snapshot
                        .pending_user_inputs
                        .retain(|existing| existing.id != request.id);
                    snapshot.pending_user_inputs.push(request.clone());
                    snapshot.pending_user_inputs.clone()
                };
                self.emit(AppStoreUpdateRecord::PendingUserInputsChanged { requests });
                let key = ThreadKey {
                    server_id: request.server_id.clone(),
                    thread_id: request.thread_id.clone(),
                };
                self.emit_thread_state_update(&key);
            }
            UiEvent::RawNotification { .. } => {}
        }
    }

    fn upsert_or_merge_thread<F>(&self, key: ThreadKey, info: ThreadInfo, mutate: F)
    where
        F: FnOnce(&mut ThreadSnapshot),
    {
        {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            let thread = snapshot
                .threads
                .entry(key.clone())
                .or_insert_with(|| ThreadSnapshot::from_info(&key.server_id, info.clone()));
            thread.info.id = info.id;
            if info.title.is_some() {
                thread.info.title = info.title;
            }
            if info.preview.is_some() {
                thread.info.preview = info.preview;
            }
            if info.cwd.is_some() {
                thread.info.cwd = info.cwd;
            }
            if info.path.is_some() {
                thread.info.path = info.path;
            }
            if info.model_provider.is_some() {
                thread.info.model_provider = info.model_provider;
            }
            if info.agent_nickname.is_some() {
                thread.info.agent_nickname = info.agent_nickname;
            }
            if info.agent_role.is_some() {
                thread.info.agent_role = info.agent_role;
            }
            if info.created_at.is_some() {
                thread.info.created_at = info.created_at;
            }
            if info.updated_at.is_some() {
                thread.info.updated_at = info.updated_at;
            }
            thread.info.status = info.status;
            mutate(thread);
        }
        self.emit_thread_upsert(&key);
    }

    pub(crate) fn mutate_thread<F>(&self, key: &ThreadKey, mutate: F)
    where
        F: FnOnce(&mut ThreadSnapshot),
    {
        if self
            .mutate_thread_with_result(key, |thread| {
                mutate(thread);
            })
            .is_some()
        {
            self.emit_thread_upsert(key);
        }
    }

    pub(crate) fn mutate_thread_with_result<F, R>(&self, key: &ThreadKey, mutate: F) -> Option<R>
    where
        F: FnOnce(&mut ThreadSnapshot) -> R,
    {
        let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
        let thread = snapshot.threads.get_mut(key)?;
        Some(mutate(thread))
    }

    pub(crate) fn emit_thread_state_update(&self, key: &ThreadKey) {
        let update = {
            let snapshot = self.snapshot.read().expect("app store lock poisoned");
            match project_thread_state_update(&snapshot, key) {
                Ok(Some((state, session_summary, agent_directory_version))) => {
                    Some(AppStoreUpdateRecord::ThreadStateUpdated {
                        state,
                        session_summary,
                        agent_directory_version,
                    })
                }
                Ok(None) => None,
                Err(error) => {
                    tracing::error!(
                        target: "store",
                        server_id = key.server_id,
                        thread_id = key.thread_id,
                        %error,
                        "failed to project ThreadStateUpdated"
                    );
                    Some(AppStoreUpdateRecord::FullResync)
                }
            }
        };
        if let Some(update) = update {
            self.emit(update);
        }
    }

    pub(crate) fn emit_thread_upsert(&self, key: &ThreadKey) {
        let update = {
            let snapshot = self.snapshot.read().expect("app store lock poisoned");
            match project_thread_update(&snapshot, key) {
                Ok(Some((thread, session_summary, agent_directory_version))) => {
                    Some(AppStoreUpdateRecord::ThreadUpserted {
                        thread,
                        session_summary,
                        agent_directory_version,
                    })
                }
                Ok(None) => None,
                Err(error) => {
                    tracing::error!(
                        target: "store",
                        server_id = key.server_id,
                        thread_id = key.thread_id,
                        %error,
                        "failed to project ThreadUpserted"
                    );
                    Some(AppStoreUpdateRecord::FullResync)
                }
            }
        };
        if let Some(update) = update {
            self.emit(update);
        }
    }

    pub(crate) fn emit_thread_item_upsert(&self, key: &ThreadKey, item: &HydratedConversationItem) {
        self.emit_thread_item_upsert_owned(key, item.clone());
    }

    pub(crate) fn emit_thread_item_upsert_owned(
        &self,
        key: &ThreadKey,
        item: HydratedConversationItem,
    ) {
        self.emit(AppStoreUpdateRecord::ThreadItemUpserted {
            key: key.clone(),
            item: HydratedConversationItem::from(item),
        });
    }

    pub(crate) fn emit_thread_item_upsert_by_id(&self, key: &ThreadKey, item_id: &str) {
        let item = {
            let snapshot = self.snapshot.read().expect("app store lock poisoned");
            snapshot
                .threads
                .get(key)
                .and_then(|thread| thread.items.iter().find(|item| item.id == item_id).cloned())
        };
        if let Some(item) = item {
            self.emit_thread_item_upsert_owned(key, item);
        }
    }

    pub(crate) fn emit_thread_command_execution_updated(
        &self,
        key: &ThreadKey,
        item_id: &str,
        status: AppOperationStatus,
        exit_code: Option<i32>,
        duration_ms: Option<i64>,
        process_id: Option<String>,
    ) {
        self.emit(AppStoreUpdateRecord::ThreadCommandExecutionUpdated {
            key: key.clone(),
            item_id: item_id.to_string(),
            status,
            exit_code,
            duration_ms,
            process_id,
        });
    }

    pub(crate) fn emit_thread_streaming_delta(
        &self,
        key: &ThreadKey,
        item_id: &str,
        kind: ThreadStreamingDeltaKind,
        text: &str,
    ) {
        self.emit(AppStoreUpdateRecord::ThreadStreamingDelta {
            key: key.clone(),
            item_id: item_id.to_string(),
            kind,
            text: text.to_string(),
        });
    }

    fn apply_item_update(&self, key: &ThreadKey, item: HydratedConversationItem) {
        let result = self.mutate_thread_with_result(key, |thread| {
            let existing = thread
                .items
                .iter()
                .find(|existing| existing.id == item.id)
                .cloned();
            let queued_count_before = thread.queued_follow_ups.len();
            let mutation = classify_item_mutation(existing.as_ref(), &item);
            let clears_queued_follow_up = item.is_from_user_turn_boundary
                && matches!(&item.content, HydratedConversationItemContent::User(_));
            upsert_item(thread, item);
            if clears_queued_follow_up {
                remove_first_queued_follow_up(thread);
            }
            (
                mutation,
                queued_count_before != thread.queued_follow_ups.len(),
            )
        });

        match result {
            Some((Some(ItemMutationUpdate::Upsert(item)), queued_changed)) => {
                if queued_changed {
                    self.emit_thread_state_update(key);
                }
                self.emit(AppStoreUpdateRecord::ThreadItemUpserted {
                    key: key.clone(),
                    item,
                });
            }
            Some((
                Some(ItemMutationUpdate::CommandExecutionUpdated {
                    item_id,
                    status,
                    exit_code,
                    duration_ms,
                    process_id,
                    output_delta,
                }),
                queued_changed,
            )) => {
                if queued_changed {
                    self.emit_thread_state_update(key);
                }
                if let Some(delta) = output_delta.filter(|delta| !delta.is_empty()) {
                    self.emit_thread_streaming_delta(
                        key,
                        &item_id,
                        ThreadStreamingDeltaKind::CommandOutput,
                        &delta,
                    );
                }
                self.emit_thread_command_execution_updated(
                    key,
                    &item_id,
                    status,
                    exit_code,
                    duration_ms,
                    process_id,
                );
            }
            Some((None, queued_changed)) => {
                if queued_changed {
                    self.emit_thread_state_update(key);
                }
            }
            None => {}
        }
    }

    fn apply_voice_transcript_update(&self, key: &ThreadKey, update: &AppVoiceTranscriptUpdate) {
        let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
        if snapshot.voice_session.active_thread.as_ref() != Some(key) {
            return;
        }

        let entry = AppVoiceTranscriptEntry {
            item_id: update.item_id.clone(),
            speaker: update.speaker,
            text: update.text.clone(),
        };
        if let Some(existing) = snapshot
            .voice_session
            .transcript_entries
            .iter_mut()
            .find(|existing| existing.item_id == entry.item_id)
        {
            *existing = entry;
        } else {
            snapshot.voice_session.transcript_entries.push(entry);
        }

        snapshot.voice_session.phase = Some(match (update.speaker, update.is_final) {
            (_, false) => match update.speaker {
                crate::types::AppVoiceSpeaker::User => AppVoiceSessionPhase::Listening,
                crate::types::AppVoiceSpeaker::Assistant => AppVoiceSessionPhase::Speaking,
            },
            (crate::types::AppVoiceSpeaker::Assistant, true) => AppVoiceSessionPhase::Thinking,
            (crate::types::AppVoiceSpeaker::User, true) => AppVoiceSessionPhase::Listening,
        });
    }

    fn emit(&self, update: AppStoreUpdateRecord) {
        match &update {
            AppStoreUpdateRecord::FullResync => tracing::debug!(target: "store", "emit FullResync"),
            AppStoreUpdateRecord::ServerChanged { server_id } => {
                tracing::debug!(target: "store", server_id, "emit ServerChanged")
            }
            AppStoreUpdateRecord::ServerRemoved { server_id } => {
                tracing::debug!(target: "store", server_id, "emit ServerRemoved")
            }
            AppStoreUpdateRecord::ThreadUpserted { thread, .. } => {
                tracing::debug!(
                    target: "store",
                    server_id = thread.key.server_id,
                    thread_id = thread.key.thread_id,
                    "emit ThreadUpserted"
                )
            }
            AppStoreUpdateRecord::ThreadStateUpdated { state, .. } => {
                tracing::debug!(
                    target: "store",
                    server_id = state.key.server_id,
                    thread_id = state.key.thread_id,
                    "emit ThreadStateUpdated"
                )
            }
            AppStoreUpdateRecord::ThreadItemUpserted { key, item } => {
                tracing::debug!(
                    target: "store",
                    server_id = key.server_id,
                    thread_id = key.thread_id,
                    item_id = item.id,
                    "emit ThreadItemUpserted"
                )
            }
            AppStoreUpdateRecord::ThreadCommandExecutionUpdated { key, item_id, .. } => {
                tracing::debug!(
                    target: "store",
                    server_id = key.server_id,
                    thread_id = key.thread_id,
                    item_id,
                    "emit ThreadCommandExecutionUpdated"
                )
            }
            AppStoreUpdateRecord::ThreadStreamingDelta {
                key, item_id, kind, ..
            } => {
                tracing::trace!(
                    target: "store",
                    server_id = key.server_id,
                    thread_id = key.thread_id,
                    item_id,
                    kind = ?kind,
                    "emit ThreadStreamingDelta"
                )
            }
            AppStoreUpdateRecord::ThreadRemoved { key, .. } => {
                tracing::debug!(target: "store", server_id = key.server_id, thread_id = key.thread_id, "emit ThreadRemoved")
            }
            AppStoreUpdateRecord::ActiveThreadChanged { key } => {
                tracing::debug!(target: "store", thread_id = ?key.as_ref().map(|k| &k.thread_id), "emit ActiveThreadChanged")
            }
            AppStoreUpdateRecord::PendingApprovalsChanged { approvals } => {
                tracing::debug!(target: "store", count = approvals.len(), "emit PendingApprovalsChanged")
            }
            AppStoreUpdateRecord::PendingUserInputsChanged { requests } => {
                tracing::debug!(target: "store", count = requests.len(), "emit PendingUserInputsChanged")
            }
            AppStoreUpdateRecord::VoiceSessionChanged => {
                tracing::debug!(target: "store", "emit VoiceSessionChanged")
            }
            AppStoreUpdateRecord::RealtimeTranscriptUpdated { key, .. } => {
                tracing::trace!(target: "store", server_id = key.server_id, thread_id = key.thread_id, "emit RealtimeTranscriptUpdated")
            }
            AppStoreUpdateRecord::RealtimeHandoffRequested { key, .. } => {
                tracing::debug!(target: "store", server_id = key.server_id, thread_id = key.thread_id, "emit RealtimeHandoffRequested")
            }
            AppStoreUpdateRecord::RealtimeSpeechStarted { key } => {
                tracing::debug!(target: "store", server_id = key.server_id, thread_id = key.thread_id, "emit RealtimeSpeechStarted")
            }
            AppStoreUpdateRecord::RealtimeStarted { key, .. } => {
                tracing::debug!(target: "store", server_id = key.server_id, thread_id = key.thread_id, "emit RealtimeStarted")
            }
            AppStoreUpdateRecord::RealtimeOutputAudioDelta { .. } => {} // too noisy even for trace
            AppStoreUpdateRecord::RealtimeError { key, .. } => {
                tracing::warn!(target: "store", server_id = key.server_id, thread_id = key.thread_id, "emit RealtimeError")
            }
            AppStoreUpdateRecord::RealtimeClosed { key, .. } => {
                tracing::debug!(target: "store", server_id = key.server_id, thread_id = key.thread_id, "emit RealtimeClosed")
            }
        }
        let _ = self.updates_tx.send(update);
    }
}

fn upsert_item(
    thread: &mut ThreadSnapshot,
    item: crate::conversation_uniffi::HydratedConversationItem,
) {
    if let Some(existing) = thread
        .items
        .iter_mut()
        .find(|existing| existing.id == item.id)
    {
        *existing = item;
    } else {
        thread.items.push(item);
    }
}

fn append_assistant_delta(thread: &mut ThreadSnapshot, item_id: &str, delta: &str) -> bool {
    let mut inserted_placeholder = false;
    if !thread.items.iter().any(|item| item.id == item_id) {
        thread.items.push(HydratedConversationItem {
            id: item_id.to_string(),
            content: HydratedConversationItemContent::Assistant(HydratedAssistantMessageData {
                text: String::new(),
                agent_nickname: None,
                agent_role: None,
                phase: None,
            }),
            source_turn_id: thread.active_turn_id.clone(),
            source_turn_index: None,
            timestamp: None,
            is_from_user_turn_boundary: false,
        });
        inserted_placeholder = true;
    }

    let Some(item) = thread.items.iter_mut().find(|item| item.id == item_id) else {
        return inserted_placeholder;
    };
    if let HydratedConversationItemContent::Assistant(message) = &mut item.content {
        message.text.push_str(delta);
    }
    inserted_placeholder
}

const USER_INPUT_RESPONSE_ITEM_PREFIX: &str = "user-input-response:";

fn preserve_local_overlay_items(source: &ThreadSnapshot, target: &mut ThreadSnapshot) {
    for item in &source.local_overlay_items {
        if target
            .items
            .iter()
            .all(|existing| !is_duplicate_overlay_item(item, existing))
            && target
                .local_overlay_items
                .iter()
                .all(|existing| !is_duplicate_overlay_item(item, existing))
        {
            target.local_overlay_items.push(item.clone());
        }
    }
}

fn preserve_thread_runtime_state(source: &ThreadSnapshot, target: &mut ThreadSnapshot) {
    target.collaboration_mode = source.collaboration_mode;
    if target.active_plan_progress.is_none() {
        target.active_plan_progress = source.active_plan_progress.clone();
    }
    if target.pending_plan_implementation_turn_id.is_none() {
        target.pending_plan_implementation_turn_id =
            source.pending_plan_implementation_turn_id.clone();
    }
}

fn preserve_thread_title(existing: &ThreadInfo, incoming: &mut ThreadInfo) {
    let incoming_blank = incoming
        .title
        .as_deref()
        .map(str::trim)
        .is_none_or(str::is_empty);
    if !incoming_blank {
        return;
    }

    let existing_title = existing
        .title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if existing_title.is_some() {
        incoming.title = existing_title;
    }
}

fn preserve_queued_follow_ups(source: &ThreadSnapshot, target: &mut ThreadSnapshot) {
    if target.queued_follow_ups.is_empty() {
        target.queued_follow_ups = source.queued_follow_ups.clone();
    }
    if target.queued_follow_up_drafts.is_empty() {
        target.queued_follow_up_drafts = source.queued_follow_up_drafts.clone();
    }
}

fn sync_thread_follow_up_projection(thread: &mut ThreadSnapshot) {
    thread.queued_follow_ups = thread
        .queued_follow_up_drafts
        .iter()
        .map(|draft| draft.preview.clone())
        .collect();
}

fn remove_first_queued_follow_up(thread: &mut ThreadSnapshot) {
    if !thread.queued_follow_up_drafts.is_empty() {
        thread.queued_follow_up_drafts.remove(0);
        sync_thread_follow_up_projection(thread);
        return;
    }
    if !thread.queued_follow_ups.is_empty() {
        thread.queued_follow_ups.remove(0);
    }
}

fn is_duplicate_overlay_item(
    local: &HydratedConversationItem,
    existing: &HydratedConversationItem,
) -> bool {
    if local.id == existing.id && local.id.starts_with(USER_INPUT_RESPONSE_ITEM_PREFIX) {
        return true;
    }

    match (&local.content, &existing.content) {
        (
            HydratedConversationItemContent::UserInputResponse(local_data),
            HydratedConversationItemContent::UserInputResponse(existing_data),
        ) => local.source_turn_id == existing.source_turn_id && local_data == existing_data,
        _ => false,
    }
}

fn answered_user_input_item(
    request: &PendingUserInputRequest,
    answers: &[PendingUserInputAnswer],
) -> HydratedConversationItem {
    let content =
        HydratedConversationItemContent::UserInputResponse(HydratedUserInputResponseData {
            questions: request
                .questions
                .iter()
                .map(|question| {
                    let answer = answers
                        .iter()
                        .find(|answer| answer.question_id == question.id)
                        .map(|answer| answer.answers.join("\n"))
                        .unwrap_or_default();
                    HydratedUserInputResponseQuestionData {
                        id: question.id.clone(),
                        header: question.header.clone(),
                        question: question.question.clone(),
                        answer,
                        options: question
                            .options
                            .iter()
                            .map(|option| HydratedUserInputResponseOptionData {
                                label: option.label.clone(),
                                description: option.description.clone(),
                            })
                            .collect(),
                    }
                })
                .collect(),
        });

    HydratedConversationItem {
        id: format!("{USER_INPUT_RESPONSE_ITEM_PREFIX}{}", request.id),
        content,
        source_turn_id: Some(request.turn_id.clone()),
        source_turn_index: None,
        timestamp: None,
        is_from_user_turn_boundary: false,
    }
}

fn append_reasoning_delta(thread: &mut ThreadSnapshot, item_id: &str, delta: &str) -> bool {
    let Some(item) = thread.items.iter_mut().find(|item| item.id == item_id) else {
        return false;
    };
    if let HydratedConversationItemContent::Reasoning(reasoning) = &mut item.content {
        if let Some(last) = reasoning.content.last_mut() {
            last.push_str(delta);
        } else {
            reasoning.content.push(delta.to_string());
        }
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation_uniffi::HydratedDividerData;
    use crate::types::{PendingUserInputOption, PendingUserInputQuestion};
    use codex_app_server_protocol::{
        McpToolCallProgressNotification, ModelRerouteReason, ModelReroutedNotification,
        TurnDiffUpdatedNotification, TurnPlanStep, TurnPlanStepStatus, TurnPlanUpdatedNotification,
    };
    use tokio::sync::broadcast::error::TryRecvError;

    fn make_thread_info(id: &str) -> ThreadInfo {
        ThreadInfo {
            id: id.to_string(),
            title: Some(format!("Thread {id}")),
            model: None,
            status: ThreadSummaryStatus::Idle,
            preview: None,
            cwd: Some("/tmp".to_string()),
            path: None,
            model_provider: None,
            agent_nickname: None,
            agent_role: None,
            parent_thread_id: None,
            agent_status: None,
            created_at: None,
            updated_at: None,
        }
    }

    fn drain_updates(
        receiver: &mut tokio::sync::broadcast::Receiver<AppStoreUpdateRecord>,
    ) -> Vec<AppStoreUpdateRecord> {
        let mut updates = Vec::new();
        loop {
            match receiver.try_recv() {
                Ok(update) => updates.push(update),
                Err(TryRecvError::Empty) => break,
                Err(error) => panic!("unexpected broadcast receive error: {error:?}"),
            }
        }
        updates
    }

    #[test]
    fn sync_thread_list_preserves_active_missing_thread() {
        let reducer = AppStoreReducer::new();
        let active_key = ThreadKey {
            server_id: "srv".to_string(),
            thread_id: "active".to_string(),
        };
        reducer
            .upsert_thread_snapshot(ThreadSnapshot::from_info("srv", make_thread_info("active")));
        reducer.set_active_thread(Some(active_key.clone()));
        let mut receiver = reducer.subscribe();

        reducer.sync_thread_list("srv", &[make_thread_info("other")]);

        let snapshot = reducer.snapshot();
        assert!(snapshot.threads.contains_key(&active_key));
        assert_eq!(snapshot.active_thread, Some(active_key));
        let updates = drain_updates(&mut receiver);
        assert!(
            updates
                .iter()
                .all(|update| !matches!(update, AppStoreUpdateRecord::FullResync))
        );
        assert!(updates.iter().any(|update| matches!(
            update,
            AppStoreUpdateRecord::ThreadUpserted { thread, .. } if thread.key.thread_id == "other"
        )));
    }

    #[test]
    fn sync_thread_list_emits_incremental_updates_without_full_resync() {
        let reducer = AppStoreReducer::new();
        let existing_key = ThreadKey {
            server_id: "srv".to_string(),
            thread_id: "existing".to_string(),
        };
        reducer.upsert_thread_snapshot(ThreadSnapshot::from_info(
            "srv",
            make_thread_info("existing"),
        ));
        let mut receiver = reducer.subscribe();

        let mut updated_existing = make_thread_info("existing");
        updated_existing.title = Some("Updated existing".to_string());
        updated_existing.model = Some("gpt-5.4".to_string());
        updated_existing.status = ThreadSummaryStatus::Active;

        let mut inserted = make_thread_info("inserted");
        inserted.model = Some("gpt-5.4".to_string());

        reducer.sync_thread_list("srv", &[updated_existing.clone(), inserted.clone()]);

        let updates = drain_updates(&mut receiver);
        assert!(
            updates
                .iter()
                .all(|update| !matches!(update, AppStoreUpdateRecord::FullResync))
        );
        assert!(updates.iter().any(|update| matches!(
            update,
            AppStoreUpdateRecord::ThreadStateUpdated { state, .. }
                if state.key == existing_key
                    && state.info == updated_existing
                    && state.model.as_deref() == Some("gpt-5.4")
        )));
        assert!(updates.iter().any(|update| matches!(
            update,
            AppStoreUpdateRecord::ThreadUpserted { thread, .. }
                if thread.key.thread_id == "inserted"
                    && thread.info == inserted
                    && thread.model.as_deref() == Some("gpt-5.4")
        )));
    }

    #[test]
    fn sync_thread_list_preserves_existing_title_when_incoming_title_missing() {
        let reducer = AppStoreReducer::new();
        let key = ThreadKey {
            server_id: "srv".to_string(),
            thread_id: "thread".to_string(),
        };
        reducer
            .upsert_thread_snapshot(ThreadSnapshot::from_info("srv", make_thread_info("thread")));

        let mut incoming = make_thread_info("thread");
        incoming.title = None;
        incoming.preview = Some("First user message".to_string());

        reducer.sync_thread_list("srv", &[incoming]);

        let snapshot = reducer.snapshot();
        let thread = snapshot.threads.get(&key).expect("thread exists");
        assert_eq!(thread.info.title.as_deref(), Some("Thread thread"));
        assert_eq!(thread.info.preview.as_deref(), Some("First user message"));
    }

    #[test]
    fn turn_diff_updates_become_conversation_items() {
        let reducer = AppStoreReducer::new();
        let key = ThreadKey {
            server_id: "srv".to_string(),
            thread_id: "thread".to_string(),
        };
        reducer
            .upsert_thread_snapshot(ThreadSnapshot::from_info("srv", make_thread_info("thread")));

        reducer.apply_ui_event(&UiEvent::TurnDiffUpdated {
            key: key.clone(),
            notification: TurnDiffUpdatedNotification {
                thread_id: key.thread_id.clone(),
                turn_id: "turn-1".to_string(),
                diff: "@@ -1 +1 @@\n-old\n+new".to_string(),
            },
        });

        let snapshot = reducer.snapshot();
        let thread = snapshot.threads.get(&key).expect("thread exists");
        let diff_item = thread
            .items
            .iter()
            .find(|item| item.id == "turn-diff-turn-1")
            .expect("turn diff item exists");
        match &diff_item.content {
            HydratedConversationItemContent::TurnDiff(data) => assert!(data.diff.contains("+new")),
            other => panic!("expected turn diff item, got {other:?}"),
        }
    }

    #[test]
    fn turn_plan_updates_populate_active_plan_progress_without_timeline_items() {
        let reducer = AppStoreReducer::new();
        let key = ThreadKey {
            server_id: "srv".to_string(),
            thread_id: "thread".to_string(),
        };
        reducer
            .upsert_thread_snapshot(ThreadSnapshot::from_info("srv", make_thread_info("thread")));

        reducer.apply_ui_event(&UiEvent::TurnPlanUpdated {
            key: key.clone(),
            notification: TurnPlanUpdatedNotification {
                thread_id: key.thread_id.clone(),
                turn_id: "turn-1".to_string(),
                explanation: Some("working".to_string()),
                plan: vec![
                    TurnPlanStep {
                        step: "Inspect renderer".to_string(),
                        status: TurnPlanStepStatus::Completed,
                    },
                    TurnPlanStep {
                        step: "Restore task cards".to_string(),
                        status: TurnPlanStepStatus::InProgress,
                    },
                ],
            },
        });

        let snapshot = reducer.snapshot();
        let thread = snapshot.threads.get(&key).expect("thread exists");
        assert!(
            thread
                .items
                .iter()
                .all(|item| item.id != "turn-plan-turn-1"),
            "turn/plan/updated should not create historical timeline items"
        );
        let progress = thread
            .active_plan_progress
            .as_ref()
            .expect("active plan progress should exist");
        assert_eq!(progress.turn_id, "turn-1");
        assert_eq!(progress.explanation.as_deref(), Some("working"));
        assert_eq!(progress.plan.len(), 2);
        assert_eq!(
            progress.plan[0].status,
            crate::types::AppPlanStepStatus::Completed
        );
        assert_eq!(
            progress.plan[1].status,
            crate::types::AppPlanStepStatus::InProgress
        );
        assert_eq!(progress.plan[1].step, "Restore task cards");
    }

    #[test]
    fn mcp_progress_updates_append_to_existing_item() {
        let reducer = AppStoreReducer::new();
        let key = ThreadKey {
            server_id: "srv".to_string(),
            thread_id: "thread".to_string(),
        };
        let mut thread = ThreadSnapshot::from_info("srv", make_thread_info("thread"));
        thread
            .items
            .push(crate::conversation_uniffi::HydratedConversationItem {
                id: "mcp-1".to_string(),
                content: HydratedConversationItemContent::McpToolCall(
                    crate::conversation_uniffi::HydratedMcpToolCallData {
                        server: "github".to_string(),
                        tool: "search".to_string(),
                        status: crate::types::AppOperationStatus::InProgress,
                        duration_ms: None,
                        arguments_json: None,
                        content_summary: None,
                        structured_content_json: None,
                        raw_output_json: None,
                        error_message: None,
                        progress_messages: Vec::new(),
                    },
                ),
                source_turn_id: Some("turn-1".to_string()),
                source_turn_index: None,
                timestamp: None,
                is_from_user_turn_boundary: false,
            });
        reducer.upsert_thread_snapshot(thread);

        reducer.apply_ui_event(&UiEvent::McpToolCallProgress {
            key: key.clone(),
            notification: McpToolCallProgressNotification {
                thread_id: key.thread_id.clone(),
                turn_id: "turn-1".to_string(),
                item_id: "mcp-1".to_string(),
                message: "Fetched 3 results".to_string(),
            },
        });

        let snapshot = reducer.snapshot();
        let thread = snapshot.threads.get(&key).expect("thread exists");
        let mcp_item = thread.items.iter().find(|item| item.id == "mcp-1").unwrap();
        match &mcp_item.content {
            HydratedConversationItemContent::McpToolCall(data) => {
                assert_eq!(
                    data.progress_messages,
                    vec!["Fetched 3 results".to_string()]
                );
            }
            other => panic!("expected mcp tool item, got {other:?}"),
        }
    }

    #[test]
    fn model_reroutes_become_divider_items() {
        let reducer = AppStoreReducer::new();
        let key = ThreadKey {
            server_id: "srv".to_string(),
            thread_id: "thread".to_string(),
        };
        reducer
            .upsert_thread_snapshot(ThreadSnapshot::from_info("srv", make_thread_info("thread")));

        reducer.apply_ui_event(&UiEvent::ModelRerouted {
            key: key.clone(),
            notification: ModelReroutedNotification {
                thread_id: key.thread_id.clone(),
                turn_id: "turn-1".to_string(),
                from_model: "gpt-5".to_string(),
                to_model: "gpt-5-mini".to_string(),
                reason: ModelRerouteReason::HighRiskCyberActivity,
            },
        });

        let snapshot = reducer.snapshot();
        let thread = snapshot.threads.get(&key).expect("thread exists");
        let reroute_item = thread
            .items
            .iter()
            .find(|item| item.id == "model-rerouted-turn-1")
            .expect("model reroute item exists");
        match &reroute_item.content {
            HydratedConversationItemContent::Divider(HydratedDividerData::ModelRerouted {
                from_model,
                to_model,
                reason,
            }) => {
                assert_eq!(from_model.as_deref(), Some("gpt-5"));
                assert_eq!(to_model, "gpt-5-mini");
                assert_eq!(reason.as_deref(), Some("High Risk Cyber Activity"));
            }
            other => panic!("expected model reroute divider, got {other:?}"),
        }
    }

    #[test]
    fn resolved_user_input_appends_response_item() {
        let reducer = AppStoreReducer::new();
        let key = ThreadKey {
            server_id: "srv".to_string(),
            thread_id: "thread".to_string(),
        };
        reducer
            .upsert_thread_snapshot(ThreadSnapshot::from_info("srv", make_thread_info("thread")));
        reducer.replace_pending_user_inputs(vec![PendingUserInputRequest {
            id: "req-1".to_string(),
            server_id: key.server_id.clone(),
            thread_id: key.thread_id.clone(),
            turn_id: "turn-1".to_string(),
            item_id: "tool-1".to_string(),
            questions: vec![PendingUserInputQuestion {
                id: "q-1".to_string(),
                header: Some("Choice".to_string()),
                question: "Pick one".to_string(),
                is_other_allowed: false,
                is_secret: false,
                options: vec![PendingUserInputOption {
                    label: "A".to_string(),
                    description: Some("First".to_string()),
                }],
            }],
            requester_agent_nickname: None,
            requester_agent_role: None,
        }]);

        reducer.resolve_pending_user_input_with_response(
            "req-1",
            vec![PendingUserInputAnswer {
                question_id: "q-1".to_string(),
                answers: vec!["A".to_string()],
            }],
        );

        let snapshot = reducer.snapshot();
        let thread = snapshot.threads.get(&key).expect("thread exists");
        let item = thread
            .local_overlay_items
            .iter()
            .find(|item| item.id == "user-input-response:req-1")
            .expect("response item exists");
        match &item.content {
            HydratedConversationItemContent::UserInputResponse(data) => {
                assert_eq!(data.questions.len(), 1);
                assert_eq!(data.questions[0].answer, "A");
            }
            other => panic!("expected user input response item, got {other:?}"),
        }
    }

    #[test]
    fn server_backed_user_input_response_supersedes_local_synthetic_copy() {
        let reducer = AppStoreReducer::new();
        let key = ThreadKey {
            server_id: "srv".to_string(),
            thread_id: "thread".to_string(),
        };
        let mut local = ThreadSnapshot::from_info("srv", make_thread_info("thread"));
        local.items.push(HydratedConversationItem {
            id: "user-input-response:req-1".to_string(),
            content: HydratedConversationItemContent::UserInputResponse(
                HydratedUserInputResponseData {
                    questions: vec![HydratedUserInputResponseQuestionData {
                        id: "q-1".to_string(),
                        header: Some("Choice".to_string()),
                        question: "Pick one".to_string(),
                        answer: "A".to_string(),
                        options: vec![],
                    }],
                },
            ),
            source_turn_id: Some("turn-1".to_string()),
            source_turn_index: None,
            timestamp: None,
            is_from_user_turn_boundary: false,
        });
        reducer.upsert_thread_snapshot(local);

        let mut server = ThreadSnapshot::from_info("srv", make_thread_info("thread"));
        server.items.push(HydratedConversationItem {
            id: "server-item-1".to_string(),
            content: HydratedConversationItemContent::UserInputResponse(
                HydratedUserInputResponseData {
                    questions: vec![HydratedUserInputResponseQuestionData {
                        id: "q-1".to_string(),
                        header: Some("Choice".to_string()),
                        question: "Pick one".to_string(),
                        answer: "A".to_string(),
                        options: vec![],
                    }],
                },
            ),
            source_turn_id: Some("turn-1".to_string()),
            source_turn_index: None,
            timestamp: None,
            is_from_user_turn_boundary: false,
        });
        reducer.upsert_thread_snapshot(server);

        let snapshot = reducer.snapshot();
        let thread = snapshot.threads.get(&key).expect("thread exists");
        assert!(thread.local_overlay_items.is_empty());
        assert_eq!(thread.items.len(), 1);
        assert_eq!(thread.items[0].id, "server-item-1");
    }

    #[test]
    fn upsert_thread_snapshot_preserves_existing_title_when_incoming_title_missing() {
        let reducer = AppStoreReducer::new();
        let key = ThreadKey {
            server_id: "srv".to_string(),
            thread_id: "thread".to_string(),
        };

        reducer
            .upsert_thread_snapshot(ThreadSnapshot::from_info("srv", make_thread_info("thread")));

        let mut incoming = ThreadSnapshot::from_info("srv", make_thread_info("thread"));
        incoming.info.title = None;
        incoming.info.preview = Some("First user message".to_string());

        reducer.upsert_thread_snapshot(incoming);

        let snapshot = reducer.snapshot();
        let thread = snapshot.threads.get(&key).expect("thread exists");
        assert_eq!(thread.info.title.as_deref(), Some("Thread thread"));
        assert_eq!(thread.info.preview.as_deref(), Some("First user message"));
    }

    #[test]
    fn turn_started_consumes_first_queued_follow_up_preview() {
        let reducer = AppStoreReducer::new();
        let key = ThreadKey {
            server_id: "srv".to_string(),
            thread_id: "thread".to_string(),
        };
        reducer
            .upsert_thread_snapshot(ThreadSnapshot::from_info("srv", make_thread_info("thread")));
        reducer.enqueue_thread_follow_up_preview(
            &key,
            AppQueuedFollowUpPreview {
                id: "queued-1".to_string(),
                kind: crate::store::snapshot::AppQueuedFollowUpKind::Message,
                text: "first".to_string(),
            },
        );
        reducer.enqueue_thread_follow_up_preview(
            &key,
            AppQueuedFollowUpPreview {
                id: "queued-2".to_string(),
                kind: crate::store::snapshot::AppQueuedFollowUpKind::Message,
                text: "second".to_string(),
            },
        );

        reducer.apply_ui_event(&UiEvent::TurnStarted {
            key: key.clone(),
            turn_id: "turn-2".to_string(),
        });

        let snapshot = reducer.snapshot();
        let thread = snapshot.threads.get(&key).expect("thread exists");
        assert_eq!(thread.active_turn_id.as_deref(), Some("turn-2"));
        assert_eq!(thread.queued_follow_ups.len(), 1);
        assert_eq!(thread.queued_follow_ups[0].id, "queued-2");
    }

    #[test]
    fn user_turn_boundary_item_consumes_stale_queued_follow_up_preview() {
        let reducer = AppStoreReducer::new();
        let key = ThreadKey {
            server_id: "srv".to_string(),
            thread_id: "thread".to_string(),
        };
        reducer
            .upsert_thread_snapshot(ThreadSnapshot::from_info("srv", make_thread_info("thread")));
        reducer.enqueue_thread_follow_up_preview(
            &key,
            AppQueuedFollowUpPreview {
                id: "queued-1".to_string(),
                kind: crate::store::snapshot::AppQueuedFollowUpKind::Message,
                text: "queued follow-up".to_string(),
            },
        );

        reducer.apply_item_update(
            &key,
            HydratedConversationItem {
                id: "user-1".to_string(),
                content: HydratedConversationItemContent::User(
                    crate::conversation_uniffi::HydratedUserMessageData {
                        text: "queued follow-up".to_string(),
                        image_data_uris: Vec::new(),
                    },
                ),
                source_turn_id: Some("turn-2".to_string()),
                source_turn_index: None,
                timestamp: None,
                is_from_user_turn_boundary: true,
            },
        );

        let snapshot = reducer.snapshot();
        let thread = snapshot.threads.get(&key).expect("thread exists");
        assert!(thread.queued_follow_ups.is_empty());
    }
}

fn append_plan_delta(thread: &mut ThreadSnapshot, item_id: &str, delta: &str) -> bool {
    let Some(item) = thread.items.iter_mut().find(|item| item.id == item_id) else {
        return false;
    };
    if let HydratedConversationItemContent::ProposedPlan(plan) = &mut item.content {
        plan.content.push_str(delta);
        return true;
    }
    false
}

fn append_command_output_delta(thread: &mut ThreadSnapshot, item_id: &str, delta: &str) -> bool {
    let Some(item) = thread.items.iter_mut().find(|item| item.id == item_id) else {
        return false;
    };
    if let HydratedConversationItemContent::CommandExecution(command) = &mut item.content {
        command
            .output
            .get_or_insert_with(String::new)
            .push_str(delta);
        return true;
    }
    false
}

fn append_mcp_progress(thread: &mut ThreadSnapshot, item_id: &str, message: &str) -> bool {
    let Some(item) = thread.items.iter_mut().find(|item| item.id == item_id) else {
        return false;
    };
    if let HydratedConversationItemContent::McpToolCall(call) = &mut item.content {
        if !message.trim().is_empty() {
            call.progress_messages.push(message.to_string());
        }
        return true;
    }
    false
}

fn appended_text_delta(existing: &str, projected: &str) -> Option<String> {
    projected
        .starts_with(existing)
        .then(|| projected[existing.len()..].to_string())
}

fn appended_optional_text_delta(
    existing: &Option<String>,
    projected: &Option<String>,
) -> Option<String> {
    match (existing.as_deref(), projected.as_deref()) {
        (None, None) => Some(String::new()),
        (None, Some(projected)) => Some(projected.to_string()),
        (Some(existing), Some(projected)) => appended_text_delta(existing, projected),
        (Some(_), None) => None,
    }
}

fn classify_item_mutation(
    existing: Option<&HydratedConversationItem>,
    item: &HydratedConversationItem,
) -> Option<ItemMutationUpdate> {
    let Some(existing) = existing else {
        return Some(ItemMutationUpdate::Upsert(HydratedConversationItem::from(
            item.clone(),
        )));
    };

    match (&existing.content, &item.content) {
        (
            HydratedConversationItemContent::CommandExecution(existing_data),
            HydratedConversationItemContent::CommandExecution(projected_data),
        ) => {
            if existing.id != item.id
                || existing.source_turn_id != item.source_turn_id
                || existing.source_turn_index != item.source_turn_index
                || existing.timestamp != item.timestamp
                || existing.is_from_user_turn_boundary != item.is_from_user_turn_boundary
                || existing_data.command != projected_data.command
                || existing_data.cwd != projected_data.cwd
                || existing_data.actions != projected_data.actions
            {
                return Some(ItemMutationUpdate::Upsert(HydratedConversationItem::from(
                    item.clone(),
                )));
            }

            let output_delta =
                appended_optional_text_delta(&existing_data.output, &projected_data.output)?;
            let status = projected_data.status.clone();
            let status_changed = existing_data.status != projected_data.status
                || existing_data.exit_code != projected_data.exit_code
                || existing_data.duration_ms != projected_data.duration_ms
                || existing_data.process_id != projected_data.process_id;
            if output_delta.is_empty() && !status_changed {
                None
            } else {
                Some(ItemMutationUpdate::CommandExecutionUpdated {
                    item_id: item.id.clone(),
                    status,
                    exit_code: projected_data.exit_code,
                    duration_ms: projected_data.duration_ms,
                    process_id: projected_data.process_id.clone(),
                    output_delta: (!output_delta.is_empty()).then_some(output_delta),
                })
            }
        }
        _ if existing.content == item.content => None,
        _ => Some(ItemMutationUpdate::Upsert(HydratedConversationItem::from(
            item.clone(),
        ))),
    }
}

fn format_model_reroute_reason(reason: &codex_app_server_protocol::ModelRerouteReason) -> String {
    let raw = format!("{reason:?}");
    let mut formatted = String::new();
    for (index, ch) in raw.chars().enumerate() {
        if index > 0 && ch.is_uppercase() {
            formatted.push(' ');
        }
        formatted.push(ch);
    }
    formatted
}
