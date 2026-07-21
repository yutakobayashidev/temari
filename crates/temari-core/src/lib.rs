mod apply;
mod artifact;
mod classification;
mod config;
mod extraction;
mod filesystem;
mod model;
mod plan;
mod scan;

pub use apply::{
    ApplySession, ApplyState, DirectoryOutcome, DirectoryRecord, MoveOutcome, MoveRecord,
    UndoDirectoryOutcome, UndoDirectoryRecord, UndoMoveOutcome, UndoMoveRecord, UndoSession,
    UndoState, apply_plan, preflight_apply, preflight_resume, preflight_undo, resume_apply_session,
    undo_session,
};
pub use artifact::{
    ApprovedFolder, FallbackCategory, FolderProposal, FolderSet, Proposal, ScanScope,
};
pub use classification::{
    ClassificationOptions, ClassificationSummary, ContentExtractor, classify_files,
};
pub use config::{Config, ContentPolicy, ExtractionConfig, ModelConfig, OcrConfig, PrivacyConfig};
pub use extraction::LocalContentExtractor;
pub use filesystem::{FileFingerprint, FsIdentity};
pub use model::{
    Classification, ClassificationBasis, Classifier, ContentCandidate, FolderProposer,
    NameClassification, NameDecision, OpenAiCompatibleModel,
};
pub use plan::{Plan, PlanEntry, build_plan};
pub use scan::{FileCandidate, scan_directory, select_representative_files};

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
}
