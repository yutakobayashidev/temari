mod apply;
mod artifact;
mod classification;
mod config;
mod extraction;
mod filesystem;
mod lock;
mod managed;
mod managed_area_migration;
mod managed_cycle;
mod managed_library;
mod managed_service;
mod model;
mod monitor;
mod plan;
mod rules;
mod scan;
mod state;

pub use apply::{
    ApplySession, ApplyState, DirectoryOutcome, DirectoryRecord, MoveOutcome, MoveRecord,
    UndoDirectoryOutcome, UndoDirectoryRecord, UndoMoveOutcome, UndoMoveRecord, UndoSession,
    UndoState, apply_plan, apply_plan_with_lock, preflight_apply, preflight_resume, preflight_undo,
    resume_apply_session, resume_apply_session_with_lock, undo_session, undo_session_files,
    undo_session_files_with_lock, undo_session_with_lock,
};
pub use artifact::{
    ApprovedFolder, FallbackCategory, FolderProposal, FolderSet, Proposal, ScanScope,
};
pub use classification::{
    ClassificationOptions, ClassificationSummary, ContentDecision, ContentExtractor, NamePass,
    classify_file_names, complete_classification,
};
pub use config::{Config, ContentPolicy, ExtractionConfig, ModelConfig, OcrConfig, PrivacyConfig};
pub use extraction::LocalContentExtractor;
pub use filesystem::{
    FileFingerprint, FsIdentity, canonical_source_identity, fingerprint_candidate,
};
pub use lock::SourceLock;
pub use managed::{
    DirectoryFingerprint, MANAGED_AREAS, ManagedAreaOutcome, ManagedAreaRecord,
    ManagedEntryFingerprint, ManagedMoveOutcome, ManagedMoveRecord, ManagedSetupMove,
    ManagedSetupPlan, ManagedSetupSession, ManagedSetupState, ManagedSetupUndoSession,
    ManagedSetupUndoState, ManagedUndoAreaOutcome, ManagedUndoAreaRecord, ManagedUndoMoveOutcome,
    ManagedUndoMoveRecord, apply_managed_directory_adoption, apply_managed_setup,
    apply_managed_setup_with_lock, build_managed_directory_adoption_plan, build_managed_setup_plan,
    fingerprint_directory, preflight_managed_resume, preflight_managed_setup,
    preflight_managed_undo, resume_managed_setup, resume_managed_setup_with_lock,
    undo_managed_directory_adoption, undo_managed_setup, undo_managed_setup_with_lock,
};
pub use managed_area_migration::{
    CURRENT_MANAGED_AREAS, LEGACY_MANAGED_AREAS, ManagedAreaLayout, ManagedAreaMigrationMove,
    ManagedAreaMigrationOutcome, ManagedAreaMigrationPlan, ManagedAreaMigrationRecord,
    ManagedAreaMigrationSession, ManagedAreaMigrationState, ManagedAreaMigrationUndoSession,
    apply_managed_area_migration, apply_managed_area_migration_with_lock,
    detect_managed_area_layout, resume_managed_area_migration, resume_managed_area_migration_undo,
    undo_managed_area_migration,
};
pub use managed_cycle::{
    INBOX_DIRECTORY, KEPT_DIRECTORY, LIBRARY_DIRECTORY, ManagedReprocessArea,
    ManagedReprocessSelection, STAGE_TO_INBOX_RULE_ID, build_reprocess_to_inbox_plan,
    build_stage_to_inbox_plan, filter_inbox_candidates, inbox_file_candidates, library_folder_set,
    reprocess_file_candidates, root_file_candidates,
};
pub use managed_library::{
    ManagedLibraryEdit, ManagedLibraryEditPlan, ManagedLibraryEditSession, ManagedLibraryEditState,
    ManagedLibraryEditUndoSession,
};
pub use managed_service::{
    ManagedActivationResult, ManagedAreaMigrationResult, ManagedAreaMigrationUndoResult,
    ManagedCycleResult, ManagedDirectoryAdoption, ManagedLibraryEditResult,
    ManagedLibraryEditUndoResult, ManagedService,
};
pub use model::{
    Classification, ClassificationBasis, Classifier, ContentCandidate, FolderProposer,
    NameClassification, NameDecision, OpenAiCompatibleModel,
};
pub use monitor::{
    MonitoringOptions, MonitoringPlan, MonitoringStats, apply_monitoring_plan,
    persist_monitoring_plan, plan_monitor_candidates, plan_monitor_cycle, processing_signature,
};
pub use plan::{Plan, PlanEntry, build_plan};
pub use rules::{LocalRule, RuleMatch, RuleSet};
pub use scan::{FileCandidate, scan_directory, select_representative_files};
pub use state::{
    InboxItem, InboxReconcileSummary, InboxState, ManagedRun, ManagedRunKind, ManagedWorkspace,
    MonitorRecord, MonitoringRun, ProcessedFileRecord, ReconcileSummary, RunState,
    StagedFileRecord, StateStore,
};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("could not read {path}: {source}")]
    ReadFile {
        path: String,
        source: std::io::Error,
    },
    #[error("could not parse configuration: {0}")]
    ParseConfig(#[from] toml::de::Error),
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    #[error("invalid artifact: {0}")]
    InvalidArtifact(String),
    #[error("could not {action} {path}: {source}")]
    FileSystem {
        action: &'static str,
        path: String,
        source: std::io::Error,
    },
    #[error("could not scan {path}: {source}")]
    Scan {
        path: String,
        source: std::io::Error,
    },
    #[error("model request failed: {0}")]
    ModelRequest(#[from] reqwest::Error),
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("model response was rejected: {0}")]
    InvalidModelResponse(String),
    #[error("state database error: {0}")]
    StateDatabase(#[from] rusqlite::Error),
    #[error("invalid monitoring state: {0}")]
    InvalidState(String),
}
