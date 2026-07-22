use serde::Serialize;

use crate::{ManagedRun, ManagedRunKind, ManagedWorkspace, RecentsItem, RecentsState, RunState};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedWorkspaceSnapshot {
    pub workspace_id: String,
    pub enabled: bool,
    pub queue: ManagedQueueSnapshot,
    pub activity: ManagedActivitySnapshot,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedQueueSnapshot {
    pub pending_runs: usize,
    pub waiting_files: Vec<WaitingFileSnapshot>,
    pub eligible_files: usize,
    pub next_eligible_unix_ms: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitingFileSnapshot {
    pub relative_path: String,
    pub size_bytes: u64,
    pub eligible_unix_ms: i64,
    pub reasons: Vec<WaitingReason>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    tag = "kind"
)]
pub enum WaitingReason {
    Retention { until_unix_ms: i64 },
    Settling { until_unix_ms: i64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedActivity {
    Idle,
    Running,
    Failed,
    Recoverable,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedActivitySnapshot {
    pub state: ManagedActivity,
    pub run: Option<ManagedActivityRunSnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedActivityRunSnapshot {
    pub id: String,
    pub kind: ManagedRunKind,
    pub state: RunState,
    pub error: Option<String>,
}

impl From<&ManagedRun> for ManagedActivityRunSnapshot {
    fn from(run: &ManagedRun) -> Self {
        Self {
            id: run.id.clone(),
            kind: run.kind,
            state: run.state,
            error: run.error.clone(),
        }
    }
}

pub fn build_workspace_snapshot(
    workspace: &ManagedWorkspace,
    items: &[RecentsItem],
    runs: &[ManagedRun],
    now_unix_ms: i64,
) -> ManagedWorkspaceSnapshot {
    let waiting_files = items
        .iter()
        .filter(|item| item.state == RecentsState::Pending && item.eligible_unix_ms > now_unix_ms)
        .map(|item| {
            let retention_until = item.first_seen_unix_ms.saturating_add(
                i64::try_from(workspace.retention_seconds)
                    .unwrap_or(i64::MAX)
                    .saturating_mul(1000),
            );
            let settling_until = item.stable_since_unix_ms.saturating_add(
                i64::try_from(workspace.settle_seconds)
                    .unwrap_or(i64::MAX)
                    .saturating_mul(1000),
            );
            let mut reasons = Vec::new();
            if retention_until > now_unix_ms {
                reasons.push(WaitingReason::Retention {
                    until_unix_ms: retention_until,
                });
            }
            if settling_until > now_unix_ms {
                reasons.push(WaitingReason::Settling {
                    until_unix_ms: settling_until,
                });
            }
            WaitingFileSnapshot {
                relative_path: item.relative_path.clone(),
                size_bytes: item.size_bytes,
                eligible_unix_ms: item.eligible_unix_ms,
                reasons,
            }
        })
        .collect::<Vec<_>>();
    let eligible_files = items
        .iter()
        .filter(|item| item.state == RecentsState::Pending && item.eligible_unix_ms <= now_unix_ms)
        .count();
    let next_eligible_unix_ms = waiting_files.iter().map(|item| item.eligible_unix_ms).min();
    let activity_run = runs.iter().find(|run| {
        matches!(
            run.state,
            RunState::Applying | RunState::NeedsResume | RunState::Failed | RunState::Planning
        )
    });
    let activity = match activity_run.map(|run| run.state) {
        Some(RunState::Applying | RunState::NeedsResume) => ManagedActivity::Recoverable,
        Some(RunState::Failed) => ManagedActivity::Failed,
        Some(RunState::Planning) => ManagedActivity::Running,
        _ => ManagedActivity::Idle,
    };
    ManagedWorkspaceSnapshot {
        workspace_id: workspace.id.clone(),
        enabled: workspace.enabled,
        queue: ManagedQueueSnapshot {
            pending_runs: runs
                .iter()
                .filter(|run| run.state == RunState::Planned)
                .count(),
            waiting_files,
            eligible_files,
            next_eligible_unix_ms,
        },
        activity: ManagedActivitySnapshot {
            state: activity,
            run: activity_run.map(ManagedActivityRunSnapshot::from),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FsIdentity, ManagedRunKind};

    fn workspace() -> ManagedWorkspace {
        ManagedWorkspace {
            id: "workspace-1".into(),
            monitor_id: "monitor-1".into(),
            source: "/tmp/source".into(),
            source_identity: FsIdentity {
                device: 1,
                inode: 2,
            },
            folder_set_path: "/tmp/folders.json".into(),
            folder_set_sha256: "a".repeat(64),
            config_path: "/tmp/config.toml".into(),
            retention_seconds: 100,
            settle_seconds: 30,
            enabled: true,
            setup_session_path: None,
            created_unix_ms: 0,
            updated_unix_ms: 0,
        }
    }

    fn item(path: &str, first: i64, stable: i64, eligible: i64) -> RecentsItem {
        RecentsItem {
            workspace_id: "workspace-1".into(),
            file_identity: FsIdentity {
                device: 1,
                inode: u64::try_from(eligible).unwrap_or(1),
            },
            relative_path: path.into(),
            content_sha256: "b".repeat(64),
            size_bytes: 42,
            first_seen_unix_ms: first,
            stable_since_unix_ms: stable,
            eligible_unix_ms: eligible,
            state: RecentsState::Pending,
            last_run_id: None,
        }
    }

    fn run(state: RunState) -> ManagedRun {
        ManagedRun {
            id: format!("run-{state:?}"),
            workspace_id: "workspace-1".into(),
            kind: ManagedRunKind::Stage,
            state,
            plan_path: None,
            apply_path: None,
            undo_path: None,
            started_unix_ms: 1,
            finished_unix_ms: None,
            move_count: 0,
            error: None,
        }
    }

    #[test]
    fn reports_waiting_reasons_and_moves_due_files_to_eligible() {
        let mut moved = item("Recents/moved.txt", 10_000, 90_000, 120_000);
        moved.state = RecentsState::Moved;
        let snapshot = build_workspace_snapshot(
            &workspace(),
            &[
                item("Recents/both.txt", 10_000, 90_000, 120_000),
                item("Recents/ready.txt", 0, 0, 50_000),
                moved,
            ],
            &[],
            100_000,
        );

        assert_eq!(snapshot.queue.eligible_files, 1);
        assert_eq!(snapshot.queue.next_eligible_unix_ms, Some(120_000));
        assert_eq!(
            snapshot.queue.waiting_files[0].reasons,
            vec![
                WaitingReason::Retention {
                    until_unix_ms: 110_000,
                },
                WaitingReason::Settling {
                    until_unix_ms: 120_000,
                },
            ]
        );
        assert_eq!(
            serde_json::to_value(&snapshot.queue.waiting_files[0].reasons[0]).unwrap(),
            serde_json::json!({"kind": "retention", "untilUnixMs": 110_000})
        );
    }

    #[test]
    fn keeps_queue_and_failure_or_recovery_state_independent() {
        let failed = build_workspace_snapshot(
            &workspace(),
            &[item("Recents/ready.txt", 0, 0, 50_000)],
            &[run(RunState::Failed), run(RunState::Planned)],
            100_000,
        );
        assert_eq!(failed.queue.eligible_files, 1);
        assert_eq!(failed.queue.pending_runs, 1);
        assert_eq!(failed.activity.state, ManagedActivity::Failed);

        let recoverable =
            build_workspace_snapshot(&workspace(), &[], &[run(RunState::NeedsResume)], 100_000);
        assert_eq!(recoverable.activity.state, ManagedActivity::Recoverable);
        let value = serde_json::to_value(&recoverable.activity).unwrap();
        assert_eq!(value["run"]["id"], "run-NeedsResume");
        assert!(value["run"].get("plan_path").is_none());
        assert!(value["run"].get("planPath").is_none());
    }
}
