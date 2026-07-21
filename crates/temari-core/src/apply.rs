use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use crate::{
    Error, FileFingerprint, FsIdentity, Plan,
    artifact::normalize_relative_path,
    filesystem::{
        canonical_directory, checked_join, fingerprint, identity, io_error, path_exists,
        verify_directory_chain,
    },
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApplySession {
    pub version: u32,
    pub id: String,
    pub plan_sha256: String,
    pub source: String,
    pub source_identity: FsIdentity,
    pub state: ApplyState,
    pub started_unix_ms: u128,
    pub finished_unix_ms: Option<u128>,
    pub directories: Vec<DirectoryRecord>,
    pub moves: Vec<MoveRecord>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyState {
    Running,
    Completed,
    Failed,
    PartialFailure,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryRecord {
    pub path: String,
    pub outcome: DirectoryOutcome,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum DirectoryOutcome {
    Pending,
    Creating,
    Created { identity: FsIdentity },
    AlreadyPresent,
    Conflict { message: String },
    Failed { message: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MoveRecord {
    pub file_id: String,
    pub source_path: String,
    pub destination_path: String,
    pub fingerprint: FileFingerprint,
    pub outcome: MoveOutcome,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum MoveOutcome {
    Pending,
    Moving,
    Moved,
    Conflict { message: String },
    Failed { message: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UndoSession {
    pub version: u32,
    pub apply_session_id: String,
    pub source: String,
    pub source_identity: FsIdentity,
    pub state: UndoState,
    pub started_unix_ms: u128,
    pub finished_unix_ms: Option<u128>,
    pub moves: Vec<UndoMoveRecord>,
    pub directories: Vec<UndoDirectoryRecord>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UndoState {
    Running,
    Completed,
    PartialFailure,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UndoMoveRecord {
    pub file_id: String,
    pub source_path: String,
    pub destination_path: String,
    pub outcome: UndoMoveOutcome,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum UndoMoveOutcome {
    Pending,
    Restoring,
    Restored,
    AlreadyRestored,
    NotApplied,
    Conflict { message: String },
    Failed { message: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UndoDirectoryRecord {
    pub path: String,
    pub outcome: UndoDirectoryOutcome,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum UndoDirectoryOutcome {
    Pending,
    Removing,
    Removed,
    NotPresent,
    NotEmpty,
    NotCreatedBySession,
    Conflict { message: String },
    Failed { message: String },
}

impl ApplySession {
    pub fn load(path: &Path) -> Result<Self, Error> {
        let text = fs::read_to_string(path).map_err(|source| Error::ReadFile {
            path: path.display().to_string(),
            source,
        })?;
        let session: Self = serde_json::from_str(&text)?;
        session.validate()?;
        Ok(session)
    }

    pub fn validate(&self) -> Result<(), Error> {
        if self.version != 1
            || !Path::new(&self.source).is_absolute()
            || self.source.chars().any(char::is_control)
        {
            return Err(Error::InvalidArtifact(
                "apply session must be version 1 with an absolute source".into(),
            ));
        }
        validate_digest(&self.plan_sha256)?;
        if self.id.trim().is_empty() {
            return Err(Error::InvalidArtifact(
                "apply session ID must not be empty".into(),
            ));
        }
        let mut paths = HashSet::new();
        for directory in &self.directories {
            normalize_relative_path(&directory.path)?;
            if !paths.insert(directory.path.as_str()) {
                return Err(Error::InvalidArtifact(format!(
                    "duplicate session directory {:?}",
                    directory.path
                )));
            }
        }
        let mut files = HashSet::new();
        let mut destinations = HashSet::new();
        for record in &self.moves {
            if record.file_id.trim().is_empty() || !files.insert(record.source_path.as_str()) {
                return Err(Error::InvalidArtifact(format!(
                    "duplicate or invalid session source {:?}",
                    record.source_path
                )));
            }
            validate_direct_file_name(&record.source_path)?;
            normalize_relative_path(&record.destination_path)?;
            if !destinations.insert(record.destination_path.as_str()) {
                return Err(Error::InvalidArtifact(format!(
                    "duplicate session destination {:?}",
                    record.destination_path
                )));
            }
            validate_fingerprint(&record.fingerprint)?;
        }
        if self.state == ApplyState::Completed
            && (self
                .moves
                .iter()
                .any(|record| record.outcome != MoveOutcome::Moved)
                || self.directories.iter().any(|record| {
                    matches!(
                        record.outcome,
                        DirectoryOutcome::Pending
                            | DirectoryOutcome::Creating
                            | DirectoryOutcome::Conflict { .. }
                            | DirectoryOutcome::Failed { .. }
                    )
                }))
        {
            return Err(Error::InvalidArtifact(
                "completed apply session contains unfinished operations".into(),
            ));
        }
        match self.state {
            ApplyState::Running if self.finished_unix_ms.is_some() => {
                return Err(Error::InvalidArtifact(
                    "running apply session must not have a finish time".into(),
                ));
            }
            ApplyState::Completed | ApplyState::Failed | ApplyState::PartialFailure
                if self.finished_unix_ms.is_none() =>
            {
                return Err(Error::InvalidArtifact(
                    "finalized apply session must have a finish time".into(),
                ));
            }
            _ => {}
        }
        if self.state != ApplyState::Running
            && (self
                .moves
                .iter()
                .any(|record| record.outcome == MoveOutcome::Moving)
                || self
                    .directories
                    .iter()
                    .any(|record| record.outcome == DirectoryOutcome::Creating))
        {
            return Err(Error::InvalidArtifact(
                "finalized apply session contains an in-progress operation".into(),
            ));
        }
        Ok(())
    }
}

impl UndoSession {
    pub fn load(path: &Path) -> Result<Self, Error> {
        let text = fs::read_to_string(path).map_err(|source| Error::ReadFile {
            path: path.display().to_string(),
            source,
        })?;
        let session: Self = serde_json::from_str(&text)?;
        if session.version != 1 || !Path::new(&session.source).is_absolute() {
            return Err(Error::InvalidArtifact(
                "undo session must be version 1 with an absolute source".into(),
            ));
        }
        Ok(session)
    }
}

pub fn apply_plan(plan: &Plan, journal_path: &Path) -> Result<ApplySession, Error> {
    preflight_apply(plan, journal_path)?;
    let mut session = ApplySession {
        version: 1,
        id: format!("{}-{}", now_unix_ms()?, std::process::id()),
        plan_sha256: plan.sha256()?,
        source: plan.source.clone(),
        source_identity: plan.source_identity.clone(),
        state: ApplyState::Running,
        started_unix_ms: now_unix_ms()?,
        finished_unix_ms: None,
        directories: plan
            .directories
            .iter()
            .map(|path| DirectoryRecord {
                path: path.clone(),
                outcome: DirectoryOutcome::Pending,
            })
            .collect(),
        moves: plan
            .entries
            .iter()
            .map(|entry| MoveRecord {
                file_id: entry.file_id.clone(),
                source_path: entry.file_name.clone(),
                destination_path: entry.destination_path.clone(),
                fingerprint: entry.source_fingerprint.clone(),
                outcome: MoveOutcome::Pending,
            })
            .collect(),
    };
    create_journal(journal_path, &session, Path::new(&plan.source))?;
    continue_apply(&mut session, journal_path)?;
    Ok(session)
}

pub fn resume_apply_session(journal_path: &Path) -> Result<ApplySession, Error> {
    let mut session = ApplySession::load(journal_path)?;
    preflight_resume(&session, journal_path)?;
    reconcile_running_session(&mut session, journal_path)?;
    if session.state == ApplyState::Running {
        continue_apply(&mut session, journal_path)?;
    }
    Ok(session)
}

fn continue_apply(session: &mut ApplySession, journal_path: &Path) -> Result<(), Error> {
    let root_path = session.source.clone();
    let root = Path::new(&root_path);
    for move_index in 0..session.moves.len() {
        if session.moves[move_index].outcome == MoveOutcome::Moved {
            continue;
        }
        if session.moves[move_index].outcome != MoveOutcome::Pending {
            return Err(Error::InvalidArtifact(format!(
                "move {:?} is not resumable from {:?}",
                session.moves[move_index].file_id, session.moves[move_index].outcome
            )));
        }
        let destination_parent =
            relative_parent(&session.moves[move_index].destination_path)?.to_owned();
        if let Err(error) = ensure_directories(root, &destination_parent, session, journal_path) {
            if session
                .directories
                .iter()
                .any(|record| record.outcome == DirectoryOutcome::Creating)
            {
                return Err(error);
            }
            session.moves[move_index].outcome = MoveOutcome::Failed {
                message: error.to_string(),
            };
            finish_apply_failure(session, journal_path)?;
            return Ok(());
        }

        session.moves[move_index].outcome = MoveOutcome::Moving;
        update_journal(journal_path, session)?;
        let source_path = checked_join(root, &session.moves[move_index].source_path)?;
        let destination_path = checked_join(root, &session.moves[move_index].destination_path)?;
        let move_result = (|| {
            verify_directory_chain(root, &destination_parent)?;
            if fingerprint(&source_path)? != session.moves[move_index].fingerprint {
                return Err(Error::InvalidArtifact(format!(
                    "source changed after planning: {:?}",
                    session.moves[move_index].source_path
                )));
            }
            if path_exists(&destination_path)? {
                return Err(Error::InvalidArtifact(format!(
                    "planned destination is now occupied: {:?}",
                    session.moves[move_index].destination_path
                )));
            }
            fs::rename(&source_path, &destination_path)
                .map_err(|source| io_error("move", &source_path, source))?;
            Ok(())
        })();
        match move_result {
            Ok(()) => {
                sync_directory(root)?;
                sync_directory(&checked_join(root, &destination_parent)?)?;
                session.moves[move_index].outcome = MoveOutcome::Moved;
                update_journal(journal_path, session)?;
            }
            Err(error) => {
                session.moves[move_index].outcome = MoveOutcome::Failed {
                    message: error.to_string(),
                };
                finish_apply_failure(session, journal_path)?;
                return Ok(());
            }
        }
    }

    session.state = ApplyState::Completed;
    session.finished_unix_ms = Some(now_unix_ms()?);
    update_journal(journal_path, session)?;
    Ok(())
}

pub fn undo_session(apply: &ApplySession, journal_path: &Path) -> Result<UndoSession, Error> {
    preflight_undo(apply, journal_path)?;
    let root = PathBuf::from(&apply.source);
    let mut undo = UndoSession {
        version: 1,
        apply_session_id: apply.id.clone(),
        source: apply.source.clone(),
        source_identity: apply.source_identity.clone(),
        state: UndoState::Running,
        started_unix_ms: now_unix_ms()?,
        finished_unix_ms: None,
        moves: apply
            .moves
            .iter()
            .rev()
            .map(|record| UndoMoveRecord {
                file_id: record.file_id.clone(),
                source_path: record.source_path.clone(),
                destination_path: record.destination_path.clone(),
                outcome: UndoMoveOutcome::Pending,
            })
            .collect(),
        directories: apply
            .directories
            .iter()
            .rev()
            .map(|record| UndoDirectoryRecord {
                path: record.path.clone(),
                outcome: UndoDirectoryOutcome::Pending,
            })
            .collect(),
    };
    create_journal(journal_path, &undo, &root)?;
    let mut partial = false;

    for undo_index in 0..undo.moves.len() {
        let apply_record = &apply.moves[apply.moves.len() - 1 - undo_index];
        if !matches!(
            apply_record.outcome,
            MoveOutcome::Moved | MoveOutcome::Moving
        ) {
            undo.moves[undo_index].outcome = UndoMoveOutcome::NotApplied;
            update_journal(journal_path, &undo)?;
            continue;
        }
        let original = checked_join(&root, &apply_record.source_path)?;
        let destination = checked_join(&root, &apply_record.destination_path)?;
        if let Err(error) =
            verify_directory_chain(&root, relative_parent(&apply_record.destination_path)?)
        {
            undo.moves[undo_index].outcome = UndoMoveOutcome::Conflict {
                message: error.to_string(),
            };
            partial = true;
            update_journal(journal_path, &undo)?;
            continue;
        }
        match reconcile_move(&original, &destination, &apply_record.fingerprint)? {
            ReconciledMove::AlreadyRestored => {
                undo.moves[undo_index].outcome = UndoMoveOutcome::AlreadyRestored;
            }
            ReconciledMove::Conflict(message) => {
                undo.moves[undo_index].outcome = UndoMoveOutcome::Conflict { message };
                partial = true;
            }
            ReconciledMove::AtDestination => {
                undo.moves[undo_index].outcome = UndoMoveOutcome::Restoring;
                update_journal(journal_path, &undo)?;
                match fs::rename(&destination, &original) {
                    Ok(()) => {
                        sync_directory(&root)?;
                        sync_directory(&checked_join(
                            &root,
                            relative_parent(&apply_record.destination_path)?,
                        )?)?;
                        undo.moves[undo_index].outcome = UndoMoveOutcome::Restored;
                    }
                    Err(error) => {
                        undo.moves[undo_index].outcome = UndoMoveOutcome::Failed {
                            message: error.to_string(),
                        };
                        partial = true;
                    }
                }
            }
        }
        update_journal(journal_path, &undo)?;
    }

    for undo_index in 0..undo.directories.len() {
        let apply_record = &apply.directories[apply.directories.len() - 1 - undo_index];
        let DirectoryOutcome::Created {
            identity: expected_identity,
        } = &apply_record.outcome
        else {
            undo.directories[undo_index].outcome = UndoDirectoryOutcome::NotCreatedBySession;
            update_journal(journal_path, &undo)?;
            continue;
        };
        let path = checked_join(&root, &apply_record.path)?;
        if let Some(parent) = apply_record.path.rsplit_once('/').map(|(parent, _)| parent)
            && let Err(error) = verify_directory_chain(&root, parent)
        {
            undo.directories[undo_index].outcome = UndoDirectoryOutcome::Conflict {
                message: error.to_string(),
            };
            partial = true;
            update_journal(journal_path, &undo)?;
            continue;
        }
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                undo.directories[undo_index].outcome = UndoDirectoryOutcome::NotPresent;
            }
            Err(error) => {
                undo.directories[undo_index].outcome = UndoDirectoryOutcome::Failed {
                    message: error.to_string(),
                };
                partial = true;
            }
            Ok(metadata)
                if metadata.file_type().is_dir()
                    && !metadata.file_type().is_symlink()
                    && identity(&metadata) == *expected_identity =>
            {
                if fs::read_dir(&path)
                    .map_err(|source| io_error("read", &path, source))?
                    .next()
                    .is_some()
                {
                    undo.directories[undo_index].outcome = UndoDirectoryOutcome::NotEmpty;
                    partial = true;
                } else {
                    undo.directories[undo_index].outcome = UndoDirectoryOutcome::Removing;
                    update_journal(journal_path, &undo)?;
                    match fs::remove_dir(&path) {
                        Ok(()) => {
                            let parent = path.parent().ok_or_else(|| {
                                Error::InvalidArtifact("created directory has no parent".into())
                            })?;
                            sync_directory(parent)?;
                            undo.directories[undo_index].outcome = UndoDirectoryOutcome::Removed
                        }
                        Err(error) => {
                            undo.directories[undo_index].outcome = UndoDirectoryOutcome::Failed {
                                message: error.to_string(),
                            };
                            partial = true;
                        }
                    }
                }
            }
            Ok(_) => {
                undo.directories[undo_index].outcome = UndoDirectoryOutcome::Conflict {
                    message: "directory identity or type changed after apply".into(),
                };
                partial = true;
            }
        }
        update_journal(journal_path, &undo)?;
    }

    undo.state = if partial {
        UndoState::PartialFailure
    } else {
        UndoState::Completed
    };
    undo.finished_unix_ms = Some(now_unix_ms()?);
    update_journal(journal_path, &undo)?;
    Ok(undo)
}

pub fn preflight_apply(plan: &Plan, journal_path: &Path) -> Result<(), Error> {
    plan.validate()?;
    let root = verify_source(&plan.source, &plan.source_identity)?;
    for entry in &plan.entries {
        let source = checked_join(&root, &entry.file_name)?;
        if fingerprint(&source)? != entry.source_fingerprint {
            return Err(Error::InvalidArtifact(format!(
                "source changed after planning: {:?}",
                entry.file_name
            )));
        }
        let parent = relative_parent(&entry.destination_path)?;
        verify_directory_chain(&root, parent)?;
        let destination = checked_join(&root, &entry.destination_path)?;
        if path_exists(&destination)? {
            return Err(Error::InvalidArtifact(format!(
                "planned destination is now occupied: {:?}",
                entry.destination_path
            )));
        }
    }
    validate_journal_target(journal_path, &root)?;
    Ok(())
}

pub fn preflight_undo(apply: &ApplySession, journal_path: &Path) -> Result<(), Error> {
    apply.validate()?;
    if apply.state == ApplyState::Running {
        return Err(Error::InvalidArtifact(
            "running apply session must be resumed before undo".into(),
        ));
    }
    let root = verify_source(&apply.source, &apply.source_identity)?;
    validate_journal_target(journal_path, &root)
}

pub fn preflight_resume(apply: &ApplySession, journal_path: &Path) -> Result<(), Error> {
    apply.validate()?;
    if apply.state != ApplyState::Running {
        return Err(Error::InvalidArtifact(format!(
            "only a running apply session can be resumed; found {:?}",
            apply.state
        )));
    }
    let root = verify_source(&apply.source, &apply.source_identity)?;
    validate_existing_journal(journal_path, &root)
}

fn reconcile_running_session(session: &mut ApplySession, journal_path: &Path) -> Result<(), Error> {
    let root_path = session.source.clone();
    let root = Path::new(&root_path);
    for index in 0..session.directories.len() {
        let path = checked_join(root, &session.directories[index].path)?;
        if let Some(parent) = session.directories[index]
            .path
            .rsplit_once('/')
            .map(|(parent, _)| parent)
            && let Err(error) = verify_directory_chain(root, parent)
        {
            session.directories[index].outcome = DirectoryOutcome::Conflict {
                message: error.to_string(),
            };
            finalize_resume_conflict(session, journal_path)?;
            return Ok(());
        }
        let reconciled = match &session.directories[index].outcome {
            DirectoryOutcome::Creating => match fs::symlink_metadata(&path) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    Some(DirectoryOutcome::Pending)
                }
                Ok(metadata)
                    if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() =>
                {
                    Some(DirectoryOutcome::AlreadyPresent)
                }
                Ok(_) => Some(DirectoryOutcome::Conflict {
                    message: "in-progress directory became an unsafe filesystem object".into(),
                }),
                Err(error) => Some(DirectoryOutcome::Conflict {
                    message: error.to_string(),
                }),
            },
            DirectoryOutcome::Created { identity: expected } => match fs::symlink_metadata(&path) {
                Ok(metadata)
                    if metadata.file_type().is_dir()
                        && !metadata.file_type().is_symlink()
                        && identity(&metadata) == *expected =>
                {
                    None
                }
                _ => Some(DirectoryOutcome::Conflict {
                    message: "created directory identity changed before resume".into(),
                }),
            },
            DirectoryOutcome::AlreadyPresent => match fs::symlink_metadata(&path) {
                Ok(metadata)
                    if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() =>
                {
                    None
                }
                _ => Some(DirectoryOutcome::Conflict {
                    message: "pre-existing destination directory changed before resume".into(),
                }),
            },
            DirectoryOutcome::Pending => match fs::symlink_metadata(&path) {
                Ok(metadata)
                    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() =>
                {
                    Some(DirectoryOutcome::Conflict {
                        message: "pending destination path is occupied by an unsafe object".into(),
                    })
                }
                Ok(_) => None,
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => Some(DirectoryOutcome::Conflict {
                    message: error.to_string(),
                }),
            },
            DirectoryOutcome::Failed { .. } => {
                finish_apply_failure(session, journal_path)?;
                return Ok(());
            }
            DirectoryOutcome::Conflict { .. } => {
                finalize_resume_conflict(session, journal_path)?;
                return Ok(());
            }
        };
        if let Some(outcome) = reconciled {
            let conflict = matches!(outcome, DirectoryOutcome::Conflict { .. });
            session.directories[index].outcome = outcome;
            if conflict {
                finalize_resume_conflict(session, journal_path)?;
                return Ok(());
            }
            update_journal(journal_path, session)?;
        }
    }

    for index in 0..session.moves.len() {
        let source = checked_join(root, &session.moves[index].source_path)?;
        let destination = checked_join(root, &session.moves[index].destination_path)?;
        if let Err(error) = verify_directory_chain(
            root,
            relative_parent(&session.moves[index].destination_path)?,
        ) {
            session.moves[index].outcome = MoveOutcome::Conflict {
                message: error.to_string(),
            };
            finalize_resume_conflict(session, journal_path)?;
            return Ok(());
        }
        let expected = &session.moves[index].fingerprint;
        let reconciled = match &session.moves[index].outcome {
            MoveOutcome::Moving => match reconcile_move(&source, &destination, expected)? {
                ReconciledMove::AtDestination => Some(MoveOutcome::Moved),
                ReconciledMove::AlreadyRestored => Some(MoveOutcome::Pending),
                ReconciledMove::Conflict(message) => Some(MoveOutcome::Conflict { message }),
            },
            MoveOutcome::Moved => match reconcile_move(&source, &destination, expected)? {
                ReconciledMove::AtDestination => None,
                ReconciledMove::AlreadyRestored => Some(MoveOutcome::Conflict {
                    message: "a completed move was manually restored before resume".into(),
                }),
                ReconciledMove::Conflict(message) => Some(MoveOutcome::Conflict { message }),
            },
            MoveOutcome::Pending => match reconcile_move(&source, &destination, expected)? {
                ReconciledMove::AlreadyRestored => None,
                ReconciledMove::AtDestination => Some(MoveOutcome::Conflict {
                    message: "pending move appears applied without a durable intent record".into(),
                }),
                ReconciledMove::Conflict(message) => Some(MoveOutcome::Conflict { message }),
            },
            MoveOutcome::Failed { .. } => {
                finish_apply_failure(session, journal_path)?;
                return Ok(());
            }
            MoveOutcome::Conflict { .. } => {
                finalize_resume_conflict(session, journal_path)?;
                return Ok(());
            }
        };
        if let Some(outcome) = reconciled {
            let conflict = matches!(outcome, MoveOutcome::Conflict { .. });
            session.moves[index].outcome = outcome;
            if conflict {
                finalize_resume_conflict(session, journal_path)?;
                return Ok(());
            }
            update_journal(journal_path, session)?;
        }
    }
    Ok(())
}

fn ensure_directories(
    root: &Path,
    destination_parent: &str,
    session: &mut ApplySession,
    journal_path: &Path,
) -> Result<(), Error> {
    for index in 0..session.directories.len() {
        let directory = &session.directories[index].path;
        if !(destination_parent == directory
            || destination_parent.starts_with(&format!("{directory}/")))
            || !matches!(
                session.directories[index].outcome,
                DirectoryOutcome::Pending
            )
        {
            continue;
        }
        session.directories[index].outcome = DirectoryOutcome::Creating;
        update_journal(journal_path, session)?;
        let path = checked_join(root, &session.directories[index].path)?;
        let outcome = match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
                DirectoryOutcome::AlreadyPresent
            }
            Ok(_) => DirectoryOutcome::Failed {
                message: "destination component is not a real directory".into(),
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => match fs::create_dir(&path) {
                Ok(()) => {
                    let metadata = fs::symlink_metadata(&path)
                        .map_err(|source| io_error("inspect", &path, source))?;
                    let parent = path.parent().ok_or_else(|| {
                        Error::InvalidArtifact("created directory has no parent".into())
                    })?;
                    sync_directory(parent)?;
                    DirectoryOutcome::Created {
                        identity: identity(&metadata),
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    match fs::symlink_metadata(&path) {
                        Ok(metadata)
                            if metadata.file_type().is_dir()
                                && !metadata.file_type().is_symlink() =>
                        {
                            DirectoryOutcome::AlreadyPresent
                        }
                        _ => DirectoryOutcome::Failed {
                            message: "destination component appeared with an unsafe type".into(),
                        },
                    }
                }
                Err(error) => DirectoryOutcome::Failed {
                    message: error.to_string(),
                },
            },
            Err(error) => DirectoryOutcome::Failed {
                message: error.to_string(),
            },
        };
        let failed = matches!(outcome, DirectoryOutcome::Failed { .. });
        session.directories[index].outcome = outcome;
        update_journal(journal_path, session)?;
        if failed {
            return Err(Error::InvalidArtifact(format!(
                "could not prepare destination directory {:?}",
                session.directories[index].path
            )));
        }
    }
    Ok(())
}

fn finish_apply_failure(session: &mut ApplySession, journal_path: &Path) -> Result<(), Error> {
    let has_effect = session
        .moves
        .iter()
        .any(|record| record.outcome == MoveOutcome::Moved)
        || session
            .directories
            .iter()
            .any(|record| matches!(record.outcome, DirectoryOutcome::Created { .. }));
    session.state = if has_effect {
        ApplyState::PartialFailure
    } else {
        ApplyState::Failed
    };
    session.finished_unix_ms = Some(now_unix_ms()?);
    update_journal(journal_path, session)
}

fn finalize_resume_conflict(session: &mut ApplySession, journal_path: &Path) -> Result<(), Error> {
    session.state = ApplyState::PartialFailure;
    session.finished_unix_ms = Some(now_unix_ms()?);
    update_journal(journal_path, session)
}

fn verify_source(source: &str, expected: &FsIdentity) -> Result<PathBuf, Error> {
    let requested = Path::new(source);
    let (canonical, actual) = canonical_directory(requested)?;
    if canonical != requested || actual != *expected {
        return Err(Error::InvalidArtifact(
            "source path or identity changed after planning".into(),
        ));
    }
    Ok(canonical)
}

enum ReconciledMove {
    AtDestination,
    AlreadyRestored,
    Conflict(String),
}

fn reconcile_move(
    original: &Path,
    destination: &Path,
    expected: &FileFingerprint,
) -> Result<ReconciledMove, Error> {
    let original_exists = path_exists(original)?;
    let destination_exists = path_exists(destination)?;
    if !original_exists && destination_exists {
        return Ok(if fingerprint(destination)? == *expected {
            ReconciledMove::AtDestination
        } else {
            ReconciledMove::Conflict("destination file changed after apply".into())
        });
    }
    if original_exists && !destination_exists && fingerprint(original)? == *expected {
        return Ok(ReconciledMove::AlreadyRestored);
    }
    Ok(ReconciledMove::Conflict(
        "source and destination state is ambiguous; refusing to overwrite".into(),
    ))
}

fn create_journal<T: Serialize>(path: &Path, value: &T, source: &Path) -> Result<(), Error> {
    validate_journal_target(path, source)?;
    write_journal(path, value, true)
}

fn validate_journal_target(path: &Path, source: &Path) -> Result<(), Error> {
    if path == Path::new("-") {
        return Err(Error::InvalidArtifact(
            "apply and undo journals require a persistent --out path".into(),
        ));
    }
    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let canonical_parent =
        fs::canonicalize(parent).map_err(|error| io_error("resolve", parent, error))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| Error::InvalidArtifact("journal output must include a file name".into()))?;
    let canonical_output = canonical_parent.join(file_name);
    if canonical_output.starts_with(source) {
        return Err(Error::InvalidArtifact(
            "journal output must be outside the organized source".into(),
        ));
    }
    if path_exists(&canonical_output)? {
        return Err(Error::InvalidArtifact(format!(
            "journal output already exists: {:?}",
            canonical_output.display().to_string()
        )));
    }
    Ok(())
}

fn validate_existing_journal(path: &Path, source: &Path) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(path).map_err(|error| io_error("inspect", path, error))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(Error::InvalidArtifact(
            "resume journal must be a regular non-symlink file".into(),
        ));
    }
    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let canonical_parent =
        fs::canonicalize(parent).map_err(|error| io_error("resolve", parent, error))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| Error::InvalidArtifact("resume journal has no file name".into()))?;
    if canonical_parent.join(file_name).starts_with(source) {
        return Err(Error::InvalidArtifact(
            "resume journal must remain outside the organized source".into(),
        ));
    }
    Ok(())
}

fn update_journal<T: Serialize>(path: &Path, value: &T) -> Result<(), Error> {
    write_journal(path, value, false)
}

fn write_journal<T: Serialize>(path: &Path, value: &T, no_clobber: bool) -> Result<(), Error> {
    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temporary = NamedTempFile::new_in(parent)
        .map_err(|error| io_error("create journal temporary file in", parent, error))?;
    temporary
        .as_file_mut()
        .set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| io_error("set permissions on", temporary.path(), error))?;
    serde_json::to_writer_pretty(temporary.as_file_mut(), value)?;
    writeln!(temporary.as_file_mut())
        .map_err(|error| io_error("write", temporary.path(), error))?;
    temporary
        .as_file_mut()
        .sync_all()
        .map_err(|error| io_error("sync", temporary.path(), error))?;
    if no_clobber {
        temporary
            .persist_noclobber(path)
            .map_err(|error| io_error("create", path, error.error))?;
    } else {
        temporary
            .persist(path)
            .map_err(|error| io_error("update", path, error.error))?;
    }
    sync_directory(parent)?;
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), Error> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| io_error("sync directory", path, error))
}

fn now_unix_ms() -> Result<u128, Error> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .map_err(|error| {
            Error::InvalidArtifact(format!("system clock is before Unix epoch: {error}"))
        })
}

fn relative_parent(path: &str) -> Result<&str, Error> {
    path.rsplit_once('/')
        .map(|(parent, _)| parent)
        .ok_or_else(|| {
            Error::InvalidArtifact(format!("destination has no parent directory: {path:?}"))
        })
}

fn validate_direct_file_name(name: &str) -> Result<(), Error> {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name == "." || name == ".." {
        return Err(Error::InvalidArtifact(format!(
            "session source must be one file-name component: {name:?}"
        )));
    }
    Ok(())
}

fn validate_fingerprint(value: &FileFingerprint) -> Result<(), Error> {
    validate_digest(&value.sha256)
}

fn validate_digest(value: &str) -> Result<(), Error> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(Error::InvalidArtifact(
            "expected a lowercase SHA-256 digest".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use tempfile::tempdir;

    use crate::{ApprovedFolder, Classification, FileCandidate, build_plan};

    use super::*;

    fn plan(root: &Path) -> Plan {
        fs::write(root.join("report.txt"), b"report").unwrap();
        build_plan(
            root,
            &[FileCandidate {
                id: "f000001".into(),
                name: "report.txt".into(),
                extension: "txt".into(),
            }],
            &[ApprovedFolder {
                id: "d000001".into(),
                path: "Documents/Reports".into(),
                description: "Reports".into(),
            }],
            vec![Classification {
                file_id: "f000001".into(),
                destination_id: "d000001".into(),
                reasoning: None,
            }],
        )
        .unwrap()
    }

    #[test]
    fn applies_and_undoes_moves_and_created_directories() {
        let root = tempdir().unwrap();
        let journals = tempdir().unwrap();
        let plan = plan(root.path());
        let apply_path = journals.path().join("apply.json");
        let undo_path = journals.path().join("undo.json");

        let apply = apply_plan(&plan, &apply_path).unwrap();
        assert_eq!(apply.state, ApplyState::Completed);
        assert_eq!(
            fs::metadata(&apply_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(!root.path().join("report.txt").exists());
        assert!(root.path().join("Documents/Reports/report.txt").exists());

        let original_bytes = fs::read(&apply_path).unwrap();
        let undo = undo_session(&apply, &undo_path).unwrap();
        assert_eq!(undo.state, UndoState::Completed);
        assert!(root.path().join("report.txt").exists());
        assert!(!root.path().join("Documents").exists());
        assert_eq!(fs::read(&apply_path).unwrap(), original_bytes);
    }

    #[test]
    fn rejects_stale_source_before_creating_journal() {
        let root = tempdir().unwrap();
        let journals = tempdir().unwrap();
        let plan = plan(root.path());
        fs::write(root.path().join("report.txt"), b"changed").unwrap();
        let journal = journals.path().join("apply.json");

        assert!(apply_plan(&plan, &journal).is_err());
        assert!(!journal.exists());
        assert!(!root.path().join("Documents").exists());
    }

    #[test]
    fn refuses_to_overwrite_existing_journal() {
        let root = tempdir().unwrap();
        let journals = tempdir().unwrap();
        let plan = plan(root.path());
        let journal = journals.path().join("apply.json");
        fs::write(&journal, b"keep").unwrap();

        assert!(apply_plan(&plan, &journal).is_err());
        assert_eq!(fs::read(&journal).unwrap(), b"keep");
        assert!(root.path().join("report.txt").exists());
    }

    #[test]
    fn undo_refuses_modified_destination() {
        let root = tempdir().unwrap();
        let journals = tempdir().unwrap();
        let plan = plan(root.path());
        let apply = apply_plan(&plan, &journals.path().join("apply.json")).unwrap();
        fs::write(root.path().join("Documents/Reports/report.txt"), b"changed").unwrap();

        let undo = undo_session(&apply, &journals.path().join("undo.json")).unwrap();

        assert_eq!(undo.state, UndoState::PartialFailure);
        assert!(!root.path().join("report.txt").exists());
        assert!(matches!(
            undo.moves[0].outcome,
            UndoMoveOutcome::Conflict { .. }
        ));
    }

    #[test]
    fn rejects_destination_occupied_after_planning() {
        let root = tempdir().unwrap();
        let journals = tempdir().unwrap();
        let plan = plan(root.path());
        fs::create_dir_all(root.path().join("Documents/Reports")).unwrap();
        fs::write(
            root.path().join("Documents/Reports/report.txt"),
            b"existing",
        )
        .unwrap();
        let journal = journals.path().join("apply.json");

        assert!(apply_plan(&plan, &journal).is_err());
        assert!(!journal.exists());
        assert_eq!(fs::read(root.path().join("report.txt")).unwrap(), b"report");
    }

    #[test]
    fn rejects_journal_inside_organized_source() {
        let root = tempdir().unwrap();
        let plan = plan(root.path());

        assert!(apply_plan(&plan, &root.path().join("apply.json")).is_err());
        assert!(root.path().join("report.txt").exists());
        assert!(!root.path().join("Documents").exists());
    }

    #[test]
    fn undo_never_overwrites_recreated_original() {
        let root = tempdir().unwrap();
        let journals = tempdir().unwrap();
        let plan = plan(root.path());
        let apply = apply_plan(&plan, &journals.path().join("apply.json")).unwrap();
        fs::write(root.path().join("report.txt"), b"new file").unwrap();

        let undo = undo_session(&apply, &journals.path().join("undo.json")).unwrap();

        assert_eq!(undo.state, UndoState::PartialFailure);
        assert_eq!(
            fs::read(root.path().join("report.txt")).unwrap(),
            b"new file"
        );
        assert!(root.path().join("Documents/Reports/report.txt").exists());
    }

    #[test]
    fn resume_reconciles_a_move_completed_before_its_checkpoint() {
        let root = tempdir().unwrap();
        let journals = tempdir().unwrap();
        let plan = plan(root.path());
        let journal = journals.path().join("apply.json");
        let mut session = apply_plan(&plan, &journal).unwrap();
        session.state = ApplyState::Running;
        session.finished_unix_ms = None;
        session.moves[0].outcome = MoveOutcome::Moving;
        update_journal(&journal, &session).unwrap();

        let resumed = resume_apply_session(&journal).unwrap();

        assert_eq!(resumed.state, ApplyState::Completed);
        assert_eq!(resumed.moves[0].outcome, MoveOutcome::Moved);
        assert!(root.path().join("Documents/Reports/report.txt").exists());
    }

    #[test]
    fn resume_retries_a_move_that_never_happened() {
        let root = tempdir().unwrap();
        let journals = tempdir().unwrap();
        let plan = plan(root.path());
        let journal = journals.path().join("apply.json");
        let mut session = apply_plan(&plan, &journal).unwrap();
        fs::rename(
            root.path().join("Documents/Reports/report.txt"),
            root.path().join("report.txt"),
        )
        .unwrap();
        session.state = ApplyState::Running;
        session.finished_unix_ms = None;
        session.moves[0].outcome = MoveOutcome::Moving;
        update_journal(&journal, &session).unwrap();

        let resumed = resume_apply_session(&journal).unwrap();

        assert_eq!(resumed.state, ApplyState::Completed);
        assert!(root.path().join("Documents/Reports/report.txt").exists());
        assert!(!root.path().join("report.txt").exists());
    }

    #[test]
    fn resume_finalizes_ambiguous_move_as_conflict() {
        let root = tempdir().unwrap();
        let journals = tempdir().unwrap();
        let plan = plan(root.path());
        let journal = journals.path().join("apply.json");
        let mut session = apply_plan(&plan, &journal).unwrap();
        fs::write(root.path().join("report.txt"), b"new file").unwrap();
        session.state = ApplyState::Running;
        session.finished_unix_ms = None;
        session.moves[0].outcome = MoveOutcome::Moving;
        update_journal(&journal, &session).unwrap();

        let resumed = resume_apply_session(&journal).unwrap();

        assert_eq!(resumed.state, ApplyState::PartialFailure);
        assert!(matches!(
            resumed.moves[0].outcome,
            MoveOutcome::Conflict { .. }
        ));
        assert_eq!(
            fs::read(root.path().join("report.txt")).unwrap(),
            b"new file"
        );
        assert!(root.path().join("Documents/Reports/report.txt").exists());
    }

    #[test]
    fn undo_rejects_a_running_apply_session() {
        let root = tempdir().unwrap();
        let journals = tempdir().unwrap();
        let plan = plan(root.path());
        let mut session = apply_plan(&plan, &journals.path().join("apply.json")).unwrap();
        session.state = ApplyState::Running;
        session.finished_unix_ms = None;

        assert!(preflight_undo(&session, &journals.path().join("undo.json")).is_err());
    }
}
