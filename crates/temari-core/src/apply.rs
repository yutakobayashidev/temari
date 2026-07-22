use std::{
    cmp::Ordering,
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
    Error, FileFingerprint, FsIdentity, Plan, SourceLock,
    artifact::normalize_relative_path,
    filesystem::{
        canonical_directory, checked_join, fingerprint, identity, io_error, path_exists,
        verify_directory_chain, verify_existing_directory_chain,
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

/// An owned, already-authorized set of filesystem moves.
///
/// Callers must validate their workflow-specific artifact before constructing
/// this manifest. The executor still validates its filesystem-facing shape and
/// stale-state checks before creating a journal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ValidatedMoveManifest {
    pub(crate) digest: String,
    pub(crate) source: String,
    pub(crate) source_identity: FsIdentity,
    pub(crate) directories: Vec<String>,
    pub(crate) moves: Vec<ValidatedMove>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ValidatedMove {
    pub(crate) file_id: String,
    pub(crate) source_path: String,
    pub(crate) destination_path: String,
    pub(crate) fingerprint: FileFingerprint,
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
        if self.version != 2
            || !Path::new(&self.source).is_absolute()
            || self.source.chars().any(char::is_control)
        {
            return Err(Error::InvalidArtifact(
                "apply session must be version 2 with an absolute source".into(),
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
        let mut file_ids = HashSet::new();
        let mut sources = HashSet::new();
        let mut destinations = HashSet::new();
        for record in &self.moves {
            if record.file_id.trim().is_empty()
                || record.file_id.chars().any(char::is_control)
                || !file_ids.insert(record.file_id.as_str())
            {
                return Err(Error::InvalidArtifact(format!(
                    "duplicate or invalid session file ID {:?}",
                    record.file_id
                )));
            }
            if !sources.insert(record.source_path.as_str()) {
                return Err(Error::InvalidArtifact(format!(
                    "duplicate session source {:?}",
                    record.source_path
                )));
            }
            normalize_relative_path(&record.source_path)?;
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
        session.validate()?;
        Ok(session)
    }

    pub fn validate(&self) -> Result<(), Error> {
        if self.version != 2
            || !Path::new(&self.source).is_absolute()
            || self.source.chars().any(char::is_control)
        {
            return Err(Error::InvalidArtifact(
                "undo session must be version 2 with an absolute source".into(),
            ));
        }
        if self.apply_session_id.trim().is_empty()
            || self.apply_session_id.chars().any(char::is_control)
        {
            return Err(Error::InvalidArtifact(
                "undo session apply ID must not be empty or contain control characters".into(),
            ));
        }
        let mut file_ids = HashSet::new();
        let mut sources = HashSet::new();
        let mut destinations = HashSet::new();
        for record in &self.moves {
            if record.file_id.trim().is_empty()
                || record.file_id.chars().any(char::is_control)
                || !file_ids.insert(record.file_id.as_str())
            {
                return Err(Error::InvalidArtifact(
                    "undo session contains a duplicate or invalid file ID".into(),
                ));
            }
            normalize_relative_path(&record.source_path)?;
            normalize_relative_path(&record.destination_path)?;
            if !sources.insert(record.source_path.as_str())
                || !destinations.insert(record.destination_path.as_str())
            {
                return Err(Error::InvalidArtifact(
                    "undo session contains duplicate move paths".into(),
                ));
            }
        }
        let mut directories = HashSet::new();
        for record in &self.directories {
            normalize_relative_path(&record.path)?;
            if !directories.insert(record.path.as_str()) {
                return Err(Error::InvalidArtifact(
                    "undo session contains duplicate directory paths".into(),
                ));
            }
        }
        match self.state {
            UndoState::Running if self.finished_unix_ms.is_some() => {
                return Err(Error::InvalidArtifact(
                    "running undo session must not have a finish time".into(),
                ));
            }
            UndoState::Completed | UndoState::PartialFailure if self.finished_unix_ms.is_none() => {
                return Err(Error::InvalidArtifact(
                    "finalized undo session must have a finish time".into(),
                ));
            }
            _ => {}
        }
        if self.state != UndoState::Running
            && (self.moves.iter().any(|record| {
                matches!(
                    record.outcome,
                    UndoMoveOutcome::Pending | UndoMoveOutcome::Restoring
                )
            }) || self.directories.iter().any(|record| {
                matches!(
                    record.outcome,
                    UndoDirectoryOutcome::Pending | UndoDirectoryOutcome::Removing
                )
            }))
        {
            return Err(Error::InvalidArtifact(
                "finalized undo session contains an in-progress operation".into(),
            ));
        }
        let has_problem = self.moves.iter().any(|record| {
            matches!(
                record.outcome,
                UndoMoveOutcome::Conflict { .. } | UndoMoveOutcome::Failed { .. }
            )
        }) || self.directories.iter().any(|record| {
            matches!(
                record.outcome,
                UndoDirectoryOutcome::NotEmpty
                    | UndoDirectoryOutcome::Conflict { .. }
                    | UndoDirectoryOutcome::Failed { .. }
            )
        });
        if self.state == UndoState::Completed && has_problem {
            return Err(Error::InvalidArtifact(
                "completed undo session contains a failed operation".into(),
            ));
        }
        if self.state == UndoState::PartialFailure && !has_problem {
            return Err(Error::InvalidArtifact(
                "partial undo session contains no failed operation".into(),
            ));
        }
        Ok(())
    }
}

pub fn apply_plan(plan: &Plan, journal_path: &Path) -> Result<ApplySession, Error> {
    let lock = SourceLock::acquire(Path::new(&plan.source))?;
    apply_plan_with_lock(plan, journal_path, &lock)
}

pub fn apply_plan_with_lock(
    plan: &Plan,
    journal_path: &Path,
    lock: &SourceLock,
) -> Result<ApplySession, Error> {
    lock.validate_source(&plan.source, &plan.source_identity)?;
    preflight_apply(plan, journal_path)?;
    apply_preflighted_move_manifest(move_manifest_from_plan(plan)?, journal_path)
}

/// Applies a separately validated workflow artifact through the standard move
/// journal and recovery engine while reusing a caller-held source lock.
pub(crate) fn apply_validated_move_manifest(
    manifest: ValidatedMoveManifest,
    journal_path: &Path,
    lock: &SourceLock,
) -> Result<ApplySession, Error> {
    lock.validate_source(&manifest.source, &manifest.source_identity)?;
    preflight_validated_move_manifest(&manifest, journal_path)?;
    apply_preflighted_move_manifest(manifest, journal_path)
}

fn apply_preflighted_move_manifest(
    manifest: ValidatedMoveManifest,
    journal_path: &Path,
) -> Result<ApplySession, Error> {
    let mut session = ApplySession {
        version: 2,
        id: format!("{}-{}", now_unix_ms()?, std::process::id()),
        plan_sha256: manifest.digest,
        source: manifest.source,
        source_identity: manifest.source_identity,
        state: ApplyState::Running,
        started_unix_ms: now_unix_ms()?,
        finished_unix_ms: None,
        directories: manifest
            .directories
            .into_iter()
            .map(|path| DirectoryRecord {
                path,
                outcome: DirectoryOutcome::Pending,
            })
            .collect(),
        moves: manifest
            .moves
            .into_iter()
            .map(|movement| MoveRecord {
                file_id: movement.file_id,
                source_path: movement.source_path,
                destination_path: movement.destination_path,
                fingerprint: movement.fingerprint,
                outcome: MoveOutcome::Pending,
            })
            .collect(),
    };
    create_journal(journal_path, &session, Path::new(&session.source))?;
    continue_apply(&mut session, journal_path)?;
    Ok(session)
}

pub fn resume_apply_session(journal_path: &Path) -> Result<ApplySession, Error> {
    let session = ApplySession::load(journal_path)?;
    let lock = SourceLock::acquire(Path::new(&session.source))?;
    resume_apply_session_with_lock(journal_path, &lock)
}

pub fn resume_apply_session_with_lock(
    journal_path: &Path,
    lock: &SourceLock,
) -> Result<ApplySession, Error> {
    let mut session = ApplySession::load(journal_path)?;
    lock.validate_source(&session.source, &session.source_identity)?;
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
            verify_source_parent(root, &session.moves[move_index].source_path)?;
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
                sync_source_parent(root, &session.moves[move_index].source_path)?;
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
    let lock = SourceLock::acquire(Path::new(&apply.source))?;
    undo_session_with_lock(apply, journal_path, &lock)
}

/// Resumes a running Undo journal after conservatively reconciling each
/// in-progress filesystem operation against its Apply Session.
pub fn resume_undo_session(
    apply: &ApplySession,
    journal_path: &Path,
) -> Result<UndoSession, Error> {
    let lock = SourceLock::acquire(Path::new(&apply.source))?;
    resume_undo_session_with_lock(apply, journal_path, &lock)
}

/// Resumes a running Undo journal while reusing a caller-held source lock.
pub fn resume_undo_session_with_lock(
    apply: &ApplySession,
    journal_path: &Path,
    lock: &SourceLock,
) -> Result<UndoSession, Error> {
    apply.validate()?;
    lock.validate_recovery_source(&apply.source, &apply.source_identity)?;
    let mut undo = UndoSession::load(journal_path)?;
    if undo.state != UndoState::Running {
        return Err(Error::InvalidArtifact(format!(
            "only a running undo session can be resumed; found {:?}",
            undo.state
        )));
    }
    validate_undo_provenance(apply, &undo)?;
    validate_existing_journal(journal_path, Path::new(&apply.source))?;
    reconcile_running_undo(apply, &mut undo, journal_path, lock)?;
    if undo.state == UndoState::Running {
        continue_undo_session(apply, &mut undo, journal_path, lock)?;
    }
    Ok(undo)
}

/// Restores selected applied files from a terminal apply session without
/// removing any directories created by that session.
pub fn undo_session_files(
    apply: &ApplySession,
    file_ids: &[String],
    journal_path: &Path,
) -> Result<UndoSession, Error> {
    let lock = SourceLock::acquire(Path::new(&apply.source))?;
    undo_session_files_with_lock(apply, file_ids, journal_path, &lock)
}

/// Restores selected applied files while reusing a caller-held source lock.
pub fn undo_session_files_with_lock(
    apply: &ApplySession,
    file_ids: &[String],
    journal_path: &Path,
    lock: &SourceLock,
) -> Result<UndoSession, Error> {
    apply.validate()?;
    if file_ids.is_empty() {
        return Err(Error::InvalidArtifact(
            "individual undo requires at least one file ID".into(),
        ));
    }
    let requested: HashSet<_> = file_ids.iter().map(String::as_str).collect();
    if requested.len() != file_ids.len() {
        return Err(Error::InvalidArtifact(
            "individual undo file IDs must be unique".into(),
        ));
    }
    let moves = apply
        .moves
        .iter()
        .filter(|record| requested.contains(record.file_id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if moves.len() != requested.len() {
        return Err(Error::InvalidArtifact(
            "individual undo references an unknown file ID".into(),
        ));
    }
    if moves
        .iter()
        .any(|record| !matches!(record.outcome, MoveOutcome::Moved | MoveOutcome::Moving))
    {
        return Err(Error::InvalidArtifact(
            "individual undo can only select files that were applied".into(),
        ));
    }
    let mut selected = apply.clone();
    selected.moves = moves;
    selected.directories.clear();
    undo_session_with_lock(&selected, journal_path, lock)
}

pub fn undo_session_with_lock(
    apply: &ApplySession,
    journal_path: &Path,
    lock: &SourceLock,
) -> Result<UndoSession, Error> {
    lock.validate_recovery_source(&apply.source, &apply.source_identity)?;
    preflight_undo(apply, journal_path)?;
    let root = PathBuf::from(&apply.source);
    let mut undo = UndoSession {
        version: 2,
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
    continue_undo_session(apply, &mut undo, journal_path, lock)?;
    Ok(undo)
}

fn continue_undo_session(
    apply: &ApplySession,
    undo: &mut UndoSession,
    journal_path: &Path,
    lock: &SourceLock,
) -> Result<(), Error> {
    let root = PathBuf::from(&apply.source);
    let recorded_source_device = apply.source_identity.device;
    let current_source_device = lock.identity().device;
    let mut partial = false;

    for undo_index in 0..undo.moves.len() {
        let apply_record = apply_move_for_undo(apply, &undo.moves[undo_index])?;
        match &undo.moves[undo_index].outcome {
            UndoMoveOutcome::Restored
            | UndoMoveOutcome::AlreadyRestored
            | UndoMoveOutcome::NotApplied => continue,
            UndoMoveOutcome::Conflict { .. } | UndoMoveOutcome::Failed { .. } => {
                partial = true;
                continue;
            }
            UndoMoveOutcome::Restoring => {
                return Err(Error::InvalidArtifact(
                    "in-progress Undo move was not reconciled before continuation".into(),
                ));
            }
            UndoMoveOutcome::Pending => {}
        }
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
        if let Err(error) = verify_source_parent(&root, &apply_record.source_path).and_then(|()| {
            verify_directory_chain(&root, relative_parent(&apply_record.destination_path)?)
        }) {
            undo.moves[undo_index].outcome = UndoMoveOutcome::Conflict {
                message: error.to_string(),
            };
            partial = true;
            update_journal(journal_path, &undo)?;
            continue;
        }
        match reconcile_move_for_recovery(
            &original,
            &destination,
            &apply_record.fingerprint,
            recorded_source_device,
            current_source_device,
        )? {
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
                        sync_source_parent(&root, &apply_record.source_path)?;
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
        let apply_record = apply_directory_for_undo(apply, &undo.directories[undo_index])?;
        match &undo.directories[undo_index].outcome {
            UndoDirectoryOutcome::Removed
            | UndoDirectoryOutcome::NotPresent
            | UndoDirectoryOutcome::NotCreatedBySession => continue,
            UndoDirectoryOutcome::NotEmpty
            | UndoDirectoryOutcome::Conflict { .. }
            | UndoDirectoryOutcome::Failed { .. } => {
                partial = true;
                continue;
            }
            UndoDirectoryOutcome::Removing => {
                return Err(Error::InvalidArtifact(
                    "in-progress Undo directory was not reconciled before continuation".into(),
                ));
            }
            UndoDirectoryOutcome::Pending => {}
        }
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
                    && identity_matches_for_recovery(
                        &identity(&metadata),
                        expected_identity,
                        recorded_source_device,
                        current_source_device,
                    ) =>
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
    Ok(())
}

fn validate_undo_provenance(apply: &ApplySession, undo: &UndoSession) -> Result<(), Error> {
    if undo.apply_session_id != apply.id
        || undo.source != apply.source
        || undo.source_identity != apply.source_identity
    {
        return Err(Error::InvalidArtifact(
            "Undo journal does not match its Apply Session".into(),
        ));
    }
    let mut previous_move_index = apply.moves.len();
    for movement in &undo.moves {
        let index = apply
            .moves
            .iter()
            .position(|record| record.file_id == movement.file_id)
            .ok_or_else(|| {
                Error::InvalidArtifact("Undo references an unknown Apply move".into())
            })?;
        let record = &apply.moves[index];
        if index >= previous_move_index
            || movement.source_path != record.source_path
            || movement.destination_path != record.destination_path
        {
            return Err(Error::InvalidArtifact(
                "Undo move order or paths do not match the Apply Session".into(),
            ));
        }
        previous_move_index = index;
    }
    if !undo.directories.is_empty()
        && (undo.directories.len() != apply.directories.len()
            || undo
                .directories
                .iter()
                .zip(apply.directories.iter().rev())
                .any(|(undo, apply)| undo.path != apply.path))
    {
        return Err(Error::InvalidArtifact(
            "Undo directories do not match the reversed Apply Session".into(),
        ));
    }
    Ok(())
}

fn apply_move_for_undo<'a>(
    apply: &'a ApplySession,
    undo: &UndoMoveRecord,
) -> Result<&'a MoveRecord, Error> {
    apply
        .moves
        .iter()
        .find(|record| {
            record.file_id == undo.file_id
                && record.source_path == undo.source_path
                && record.destination_path == undo.destination_path
        })
        .ok_or_else(|| Error::InvalidArtifact("Undo move does not match its Apply Session".into()))
}

fn apply_directory_for_undo<'a>(
    apply: &'a ApplySession,
    undo: &UndoDirectoryRecord,
) -> Result<&'a DirectoryRecord, Error> {
    apply
        .directories
        .iter()
        .find(|record| record.path == undo.path)
        .ok_or_else(|| {
            Error::InvalidArtifact("Undo directory does not match its Apply Session".into())
        })
}

fn reconcile_running_undo(
    apply: &ApplySession,
    undo: &mut UndoSession,
    journal_path: &Path,
    lock: &SourceLock,
) -> Result<(), Error> {
    let root = Path::new(&apply.source);
    let recorded_source_device = apply.source_identity.device;
    let current_source_device = lock.identity().device;
    for index in 0..undo.moves.len() {
        let apply_record = apply_move_for_undo(apply, &undo.moves[index])?;
        let original = checked_join(root, &apply_record.source_path)?;
        let destination = checked_join(root, &apply_record.destination_path)?;
        verify_source_parent(root, &apply_record.source_path)?;
        verify_directory_chain(root, relative_parent(&apply_record.destination_path)?)?;
        let next = match &undo.moves[index].outcome {
            UndoMoveOutcome::Pending => match reconcile_move_for_recovery(
                &original,
                &destination,
                &apply_record.fingerprint,
                recorded_source_device,
                current_source_device,
            )? {
                ReconciledMove::AtDestination => None,
                ReconciledMove::AlreadyRestored => Some(UndoMoveOutcome::AlreadyRestored),
                ReconciledMove::Conflict(message) => Some(UndoMoveOutcome::Conflict { message }),
            },
            UndoMoveOutcome::Restoring => match reconcile_move_for_recovery(
                &original,
                &destination,
                &apply_record.fingerprint,
                recorded_source_device,
                current_source_device,
            )? {
                ReconciledMove::AtDestination => Some(UndoMoveOutcome::Pending),
                ReconciledMove::AlreadyRestored => Some(UndoMoveOutcome::Restored),
                ReconciledMove::Conflict(message) => Some(UndoMoveOutcome::Conflict { message }),
            },
            UndoMoveOutcome::Restored | UndoMoveOutcome::AlreadyRestored => {
                match reconcile_move_for_recovery(
                    &original,
                    &destination,
                    &apply_record.fingerprint,
                    recorded_source_device,
                    current_source_device,
                )? {
                    ReconciledMove::AlreadyRestored => None,
                    ReconciledMove::AtDestination => Some(UndoMoveOutcome::Conflict {
                        message: "restored file returned to its applied destination".into(),
                    }),
                    ReconciledMove::Conflict(message) => {
                        Some(UndoMoveOutcome::Conflict { message })
                    }
                }
            }
            UndoMoveOutcome::NotApplied => {
                if matches!(
                    apply_record.outcome,
                    MoveOutcome::Moved | MoveOutcome::Moving
                ) {
                    return Err(Error::InvalidArtifact(
                        "Undo marks an applied move as not applied".into(),
                    ));
                }
                None
            }
            UndoMoveOutcome::Conflict { .. } | UndoMoveOutcome::Failed { .. } => None,
        };
        if let Some(outcome) = next {
            undo.moves[index].outcome = outcome;
            update_journal(journal_path, undo)?;
        }
    }

    for index in 0..undo.directories.len() {
        let apply_record = apply_directory_for_undo(apply, &undo.directories[index])?;
        let DirectoryOutcome::Created {
            identity: expected_identity,
        } = &apply_record.outcome
        else {
            if undo.directories[index].outcome != UndoDirectoryOutcome::NotCreatedBySession {
                undo.directories[index].outcome = UndoDirectoryOutcome::NotCreatedBySession;
                update_journal(journal_path, undo)?;
            }
            continue;
        };
        let path = checked_join(root, &apply_record.path)?;
        if let Some(parent) = apply_record.path.rsplit_once('/').map(|(parent, _)| parent) {
            verify_directory_chain(root, parent)?;
        }
        let state = match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(io_error("inspect", &path, error)),
            Ok(metadata)
                if metadata.file_type().is_dir()
                    && !metadata.file_type().is_symlink()
                    && identity_matches_for_recovery(
                        &identity(&metadata),
                        expected_identity,
                        recorded_source_device,
                        current_source_device,
                    ) =>
            {
                Some(
                    fs::read_dir(&path)
                        .map_err(|source| io_error("read", &path, source))?
                        .next()
                        .is_some(),
                )
            }
            Ok(_) => {
                undo.directories[index].outcome = UndoDirectoryOutcome::Conflict {
                    message: "directory identity or type changed during Undo".into(),
                };
                update_journal(journal_path, undo)?;
                continue;
            }
        };
        let next = match (&undo.directories[index].outcome, state) {
            (UndoDirectoryOutcome::Pending, None) => Some(UndoDirectoryOutcome::NotPresent),
            (UndoDirectoryOutcome::Pending, Some(_)) => None,
            (UndoDirectoryOutcome::Removing, None) => Some(UndoDirectoryOutcome::Removed),
            (UndoDirectoryOutcome::Removing, Some(false)) => Some(UndoDirectoryOutcome::Pending),
            (UndoDirectoryOutcome::Removing, Some(true)) => Some(UndoDirectoryOutcome::NotEmpty),
            (UndoDirectoryOutcome::Removed | UndoDirectoryOutcome::NotPresent, None) => None,
            (UndoDirectoryOutcome::Removed | UndoDirectoryOutcome::NotPresent, Some(_)) => {
                Some(UndoDirectoryOutcome::Conflict {
                    message: "removed Undo directory reappeared".into(),
                })
            }
            (
                UndoDirectoryOutcome::NotEmpty
                | UndoDirectoryOutcome::Conflict { .. }
                | UndoDirectoryOutcome::Failed { .. },
                _,
            ) => None,
            (UndoDirectoryOutcome::NotCreatedBySession, _) => {
                return Err(Error::InvalidArtifact(
                    "Undo directory provenance contradicts the Apply Session".into(),
                ));
            }
        };
        if let Some(outcome) = next {
            undo.directories[index].outcome = outcome;
            update_journal(journal_path, undo)?;
        }
    }
    Ok(())
}

pub fn preflight_apply(plan: &Plan, journal_path: &Path) -> Result<(), Error> {
    plan.validate()?;
    preflight_validated_move_manifest(&move_manifest_from_plan(plan)?, journal_path)
}

/// Performs the common stale-file, destination, and journal checks for a
/// workflow-specific manifest that has already passed its semantic validation.
pub(crate) fn preflight_validated_move_manifest(
    manifest: &ValidatedMoveManifest,
    journal_path: &Path,
) -> Result<(), Error> {
    validate_move_manifest(manifest)?;
    let root = verify_source(&manifest.source, &manifest.source_identity)?;
    for movement in &manifest.moves {
        verify_source_parent(&root, &movement.source_path)?;
        let source = checked_join(&root, &movement.source_path)?;
        if fingerprint(&source)? != movement.fingerprint {
            return Err(Error::InvalidArtifact(format!(
                "source changed after planning: {:?}",
                movement.source_path
            )));
        }
        let parent = relative_parent(&movement.destination_path)?;
        verify_directory_chain(&root, parent)?;
        let destination = checked_join(&root, &movement.destination_path)?;
        if path_exists(&destination)? {
            return Err(Error::InvalidArtifact(format!(
                "planned destination is now occupied: {:?}",
                movement.destination_path
            )));
        }
    }
    validate_journal_target(journal_path, &root)?;
    Ok(())
}

fn move_manifest_from_plan(plan: &Plan) -> Result<ValidatedMoveManifest, Error> {
    Ok(ValidatedMoveManifest {
        digest: plan.sha256()?,
        source: plan.source.clone(),
        source_identity: plan.source_identity.clone(),
        directories: plan.directories.clone(),
        moves: plan
            .entries
            .iter()
            .map(|entry| ValidatedMove {
                file_id: entry.file_id.clone(),
                source_path: entry.source_path.clone(),
                destination_path: entry.destination_path.clone(),
                fingerprint: entry.source_fingerprint.clone(),
            })
            .collect(),
    })
}

fn validate_move_manifest(manifest: &ValidatedMoveManifest) -> Result<(), Error> {
    validate_digest(&manifest.digest)?;
    if !Path::new(&manifest.source).is_absolute() || manifest.source.chars().any(char::is_control) {
        return Err(Error::InvalidArtifact(
            "move manifest source must be an absolute path without control characters".into(),
        ));
    }

    let mut previous_directory: Option<&str> = None;
    let mut directories = HashSet::new();
    for directory in &manifest.directories {
        normalize_relative_path(directory)?;
        if !directories.insert(directory.as_str()) {
            return Err(Error::InvalidArtifact(format!(
                "duplicate move manifest directory {directory:?}"
            )));
        }
        if let Some(previous) = previous_directory
            && move_directory_order(previous, directory) == Ordering::Greater
        {
            return Err(Error::InvalidArtifact(
                "move manifest directories must be in parent-first lexical order".into(),
            ));
        }
        previous_directory = Some(directory);
    }

    let mut file_ids = HashSet::new();
    let mut sources = HashSet::new();
    let mut destinations = HashSet::new();
    for movement in &manifest.moves {
        if movement.file_id.trim().is_empty()
            || movement.file_id.chars().any(char::is_control)
            || !file_ids.insert(movement.file_id.as_str())
        {
            return Err(Error::InvalidArtifact(format!(
                "duplicate or invalid move manifest file ID {:?}",
                movement.file_id
            )));
        }
        normalize_relative_path(&movement.source_path)?;
        normalize_relative_path(&movement.destination_path)?;
        if movement.source_path == movement.destination_path {
            return Err(Error::InvalidArtifact(format!(
                "move manifest source and destination must differ: {:?}",
                movement.source_path
            )));
        }
        if !sources.insert(movement.source_path.as_str()) {
            return Err(Error::InvalidArtifact(format!(
                "duplicate move manifest source {:?}",
                movement.source_path
            )));
        }
        if !destinations.insert(movement.destination_path.as_str()) {
            return Err(Error::InvalidArtifact(format!(
                "duplicate move manifest destination {:?}",
                movement.destination_path
            )));
        }
        validate_fingerprint(&movement.fingerprint)?;
    }
    for directory in &manifest.directories {
        if !manifest.moves.iter().any(|movement| {
            relative_parent(&movement.destination_path).is_ok_and(|parent| {
                parent == directory || parent.starts_with(&format!("{directory}/"))
            })
        }) {
            return Err(Error::InvalidArtifact(format!(
                "move manifest directory is not required by any move: {directory:?}"
            )));
        }
    }
    Ok(())
}

fn move_directory_order(left: &str, right: &str) -> Ordering {
    left.split('/')
        .count()
        .cmp(&right.split('/').count())
        .then_with(|| left.cmp(right))
}

pub fn preflight_undo(apply: &ApplySession, journal_path: &Path) -> Result<(), Error> {
    apply.validate()?;
    if apply.state == ApplyState::Running {
        return Err(Error::InvalidArtifact(
            "running apply session must be resumed before undo".into(),
        ));
    }
    let root = verify_recovery_source(&apply.source, &apply.source_identity)?;
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
        if let Err(error) = verify_source_parent(root, &session.moves[index].source_path) {
            session.moves[index].outcome = MoveOutcome::Conflict {
                message: error.to_string(),
            };
            finalize_resume_conflict(session, journal_path)?;
            return Ok(());
        }
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

fn verify_recovery_source(source: &str, expected: &FsIdentity) -> Result<PathBuf, Error> {
    let requested = Path::new(source);
    let (canonical, actual) = canonical_directory(requested)?;
    if canonical != requested || actual.inode != expected.inode {
        return Err(Error::InvalidArtifact(
            "source path or inode changed after apply".into(),
        ));
    }
    Ok(canonical)
}

fn identity_matches_for_recovery(
    actual: &FsIdentity,
    expected: &FsIdentity,
    recorded_source_device: u64,
    current_source_device: u64,
) -> bool {
    let expected_current_device = if expected.device == recorded_source_device {
        current_source_device
    } else {
        expected.device
    };
    actual.device == expected_current_device && actual.inode == expected.inode
}

fn fingerprint_matches_for_recovery(
    actual: &FileFingerprint,
    expected: &FileFingerprint,
    recorded_source_device: u64,
    current_source_device: u64,
) -> bool {
    actual.size == expected.size
        && actual.sha256 == expected.sha256
        && identity_matches_for_recovery(
            &actual.identity,
            &expected.identity,
            recorded_source_device,
            current_source_device,
        )
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
    reconcile_move_for_recovery(
        original,
        destination,
        expected,
        expected.identity.device,
        expected.identity.device,
    )
}

fn reconcile_move_for_recovery(
    original: &Path,
    destination: &Path,
    expected: &FileFingerprint,
    recorded_source_device: u64,
    current_source_device: u64,
) -> Result<ReconciledMove, Error> {
    let original_exists = path_exists(original)?;
    let destination_exists = path_exists(destination)?;
    if !original_exists && destination_exists {
        let actual = fingerprint(destination)?;
        return Ok(
            if fingerprint_matches_for_recovery(
                &actual,
                expected,
                recorded_source_device,
                current_source_device,
            ) {
                ReconciledMove::AtDestination
            } else {
                ReconciledMove::Conflict("destination file changed after apply".into())
            },
        );
    }
    if original_exists && !destination_exists {
        let actual = fingerprint(original)?;
        if fingerprint_matches_for_recovery(
            &actual,
            expected,
            recorded_source_device,
            current_source_device,
        ) {
            return Ok(ReconciledMove::AlreadyRestored);
        }
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

fn verify_source_parent(root: &Path, source_path: &str) -> Result<(), Error> {
    normalize_relative_path(source_path)?;
    if let Some((parent, _)) = source_path.rsplit_once('/') {
        verify_existing_directory_chain(root, parent)?;
    }
    Ok(())
}

fn sync_source_parent(root: &Path, source_path: &str) -> Result<(), Error> {
    match source_path.rsplit_once('/') {
        Some((parent, _)) => sync_directory(&checked_join(root, parent)?),
        None => sync_directory(root),
    }
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
    use std::os::unix::fs::{PermissionsExt, symlink};

    use tempfile::tempdir;

    use crate::{
        Classification, ClassificationBasis, FileCandidate, FolderProposal, Proposal, build_plan,
    };

    use super::*;

    fn plan(root: &Path) -> Plan {
        fs::write(root.join("report.txt"), b"report").unwrap();
        let folders = Proposal {
            version: 2,
            source: root.display().to_string(),
            scope: crate::ScanScope::default(),
            files_considered: 1,
            folders: vec![FolderProposal {
                path: "Documents/Reports".into(),
                description: "Reports".into(),
            }],
        }
        .approve()
        .unwrap()
        .folders;
        build_plan(
            root,
            &crate::ScanScope::default(),
            &[FileCandidate {
                id: "f000001".into(),
                source_path: "report.txt".into(),
                extension: "txt".into(),
            }],
            &folders,
            vec![Classification {
                file_id: "f000001".into(),
                destination_id: "d000001".into(),
                reasoning: None,
                basis: ClassificationBasis::Name,
                rule_id: None,
            }],
        )
        .unwrap()
    }

    fn nested_plan(root: &Path) -> Plan {
        fs::create_dir_all(root.join("incoming/deep")).unwrap();
        fs::write(root.join("incoming/deep/report.txt"), b"report").unwrap();
        let scope = crate::ScanScope::new(vec!["incoming".into()]).unwrap();
        let folders = Proposal {
            version: 2,
            source: root.display().to_string(),
            scope: scope.clone(),
            files_considered: 1,
            folders: vec![FolderProposal {
                path: "Documents/Reports".into(),
                description: "Reports".into(),
            }],
        }
        .approve()
        .unwrap()
        .folders;
        build_plan(
            root,
            &scope,
            &[FileCandidate {
                id: "f000001".into(),
                source_path: "incoming/deep/report.txt".into(),
                extension: "txt".into(),
            }],
            &folders,
            vec![Classification {
                file_id: "f000001".into(),
                destination_id: "d000001".into(),
                reasoning: None,
                basis: ClassificationBasis::Name,
                rule_id: None,
            }],
        )
        .unwrap()
    }

    fn renumber_apply_devices(apply: &mut ApplySession) {
        let recorded_device = apply.source_identity.device.wrapping_add(1);
        apply.source_identity.device = recorded_device;
        for record in &mut apply.moves {
            record.fingerprint.identity.device = recorded_device;
        }
        for record in &mut apply.directories {
            if let DirectoryOutcome::Created { identity } = &mut record.outcome {
                identity.device = recorded_device;
            }
        }
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
    fn resumes_undo_from_a_journal_created_before_filesystem_mutation() {
        let root = tempdir().unwrap();
        let journals = tempdir().unwrap();
        let apply = apply_plan(&plan(root.path()), &journals.path().join("apply.json")).unwrap();
        let undo_path = journals.path().join("undo.json");
        let undo = UndoSession {
            version: 2,
            apply_session_id: apply.id.clone(),
            source: apply.source.clone(),
            source_identity: apply.source_identity.clone(),
            state: UndoState::Running,
            started_unix_ms: now_unix_ms().unwrap(),
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
        create_journal(&undo_path, &undo, root.path()).unwrap();

        let resumed = resume_undo_session(&apply, &undo_path).unwrap();

        assert_eq!(resumed.state, UndoState::Completed);
        assert!(root.path().join("report.txt").is_file());
        assert!(!root.path().join("Documents").exists());
    }

    #[test]
    fn resume_reconciles_completed_undo_operations_and_keeps_terminal_journal_immutable() {
        let root = tempdir().unwrap();
        let journals = tempdir().unwrap();
        let apply = apply_plan(&plan(root.path()), &journals.path().join("apply.json")).unwrap();
        let undo_path = journals.path().join("undo.json");
        let mut interrupted = undo_session(&apply, &undo_path).unwrap();
        interrupted.state = UndoState::Running;
        interrupted.finished_unix_ms = None;
        for movement in &mut interrupted.moves {
            if movement.outcome == UndoMoveOutcome::Restored {
                movement.outcome = UndoMoveOutcome::Restoring;
            }
        }
        for directory in &mut interrupted.directories {
            if directory.outcome == UndoDirectoryOutcome::Removed {
                directory.outcome = UndoDirectoryOutcome::Removing;
            }
        }
        update_journal(&undo_path, &interrupted).unwrap();

        let resumed = resume_undo_session(&apply, &undo_path).unwrap();

        assert_eq!(resumed.state, UndoState::Completed);
        assert!(
            resumed
                .moves
                .iter()
                .all(|record| record.outcome == UndoMoveOutcome::Restored)
        );
        assert!(
            resumed
                .directories
                .iter()
                .filter(|record| apply
                    .directories
                    .iter()
                    .any(|apply| apply.path == record.path
                        && matches!(apply.outcome, DirectoryOutcome::Created { .. })))
                .all(|record| record.outcome == UndoDirectoryOutcome::Removed)
        );
        let terminal_bytes = fs::read(&undo_path).unwrap();
        assert!(resume_undo_session(&apply, &undo_path).is_err());
        assert_eq!(fs::read(&undo_path).unwrap(), terminal_bytes);
    }

    #[test]
    fn validated_manifest_moves_within_an_approved_style_tree() {
        let root = tempdir().unwrap();
        let journals = tempdir().unwrap();
        fs::create_dir_all(root.path().join("AI Library/Old")).unwrap();
        fs::write(root.path().join("AI Library/Old/report.txt"), b"report").unwrap();
        let (_, source_identity) = canonical_directory(root.path()).unwrap();
        let manifest = ValidatedMoveManifest {
            digest: "a".repeat(64),
            source: root.path().display().to_string(),
            source_identity,
            directories: vec!["AI Library/New".into()],
            moves: vec![ValidatedMove {
                file_id: "f000001".into(),
                source_path: "AI Library/Old/report.txt".into(),
                destination_path: "AI Library/New/report.txt".into(),
                fingerprint: fingerprint(&root.path().join("AI Library/Old/report.txt")).unwrap(),
            }],
        };
        let apply_path = journals.path().join("apply.json");
        let lock = SourceLock::acquire(root.path()).unwrap();

        preflight_validated_move_manifest(&manifest, &apply_path).unwrap();
        let apply = apply_validated_move_manifest(manifest, &apply_path, &lock).unwrap();

        assert_eq!(apply.state, ApplyState::Completed);
        assert!(!root.path().join("AI Library/Old/report.txt").exists());
        assert!(root.path().join("AI Library/New/report.txt").exists());
        drop(lock);

        let undo = undo_session(&apply, &journals.path().join("undo.json")).unwrap();
        assert_eq!(undo.state, UndoState::Completed);
        assert!(root.path().join("AI Library/Old/report.txt").exists());
        assert!(!root.path().join("AI Library/New").exists());
    }

    #[test]
    fn validated_manifest_rejects_unsafe_shape_before_journaling() {
        let root = tempdir().unwrap();
        let journals = tempdir().unwrap();
        fs::write(root.path().join("report.txt"), b"report").unwrap();
        let (_, source_identity) = canonical_directory(root.path()).unwrap();
        let file_fingerprint = fingerprint(&root.path().join("report.txt")).unwrap();
        let movement = ValidatedMove {
            file_id: "f000001".into(),
            source_path: "report.txt".into(),
            destination_path: "Documents/report.txt".into(),
            fingerprint: file_fingerprint,
        };
        let manifest = ValidatedMoveManifest {
            digest: "a".repeat(64),
            source: root.path().display().to_string(),
            source_identity,
            directories: vec!["Documents".into()],
            moves: vec![movement.clone(), movement],
        };
        let journal = journals.path().join("apply.json");
        let lock = SourceLock::acquire(root.path()).unwrap();

        assert!(apply_validated_move_manifest(manifest, &journal, &lock).is_err());
        assert!(!journal.exists());
        assert!(root.path().join("report.txt").exists());
    }

    #[test]
    fn individual_undo_restores_the_file_but_keeps_shared_directories() {
        let root = tempdir().unwrap();
        let journals = tempdir().unwrap();
        let plan = plan(root.path());
        let apply = apply_plan(&plan, &journals.path().join("apply.json")).unwrap();

        let undo = undo_session_files(
            &apply,
            &["f000001".into()],
            &journals.path().join("undo.json"),
        )
        .unwrap();

        assert_eq!(undo.state, UndoState::Completed);
        assert!(root.path().join("report.txt").exists());
        assert!(root.path().join("Documents/Reports").is_dir());
        assert!(undo.directories.is_empty());
    }

    #[test]
    fn individual_undo_rejects_a_file_that_was_not_applied() {
        let root = tempdir().unwrap();
        let journals = tempdir().unwrap();
        let mut apply =
            apply_plan(&plan(root.path()), &journals.path().join("apply.json")).unwrap();
        apply.state = ApplyState::PartialFailure;
        apply.moves[0].outcome = MoveOutcome::Failed {
            message: "not applied".into(),
        };

        let result = undo_session_files(
            &apply,
            &["f000001".into()],
            &journals.path().join("undo.json"),
        );

        assert!(result.is_err());
        assert!(!journals.path().join("undo.json").exists());
    }

    #[test]
    fn apply_artifact_rejects_duplicate_file_ids() {
        let root = tempdir().unwrap();
        let journals = tempdir().unwrap();
        let mut apply =
            apply_plan(&plan(root.path()), &journals.path().join("apply.json")).unwrap();
        apply.state = ApplyState::Running;
        apply.finished_unix_ms = None;
        let mut duplicate = apply.moves[0].clone();
        duplicate.source_path = "other.txt".into();
        duplicate.destination_path = "Documents/Reports/other.txt".into();
        duplicate.outcome = MoveOutcome::Pending;
        apply.moves.push(duplicate);

        assert!(apply.validate().is_err());
    }

    #[test]
    fn undo_artifact_rejects_inconsistent_terminal_states() {
        let root = tempdir().unwrap();
        let journals = tempdir().unwrap();
        let apply = apply_plan(&plan(root.path()), &journals.path().join("apply.json")).unwrap();
        let mut undo = undo_session(&apply, &journals.path().join("undo.json")).unwrap();
        let artifact = journals.path().join("forged-undo.json");

        undo.moves[0].outcome = UndoMoveOutcome::Pending;
        fs::write(&artifact, serde_json::to_vec(&undo).unwrap()).unwrap();
        assert!(UndoSession::load(&artifact).is_err());

        undo.moves[0].outcome = UndoMoveOutcome::Restored;
        undo.state = UndoState::PartialFailure;
        fs::write(&artifact, serde_json::to_vec(&undo).unwrap()).unwrap();
        assert!(UndoSession::load(&artifact).is_err());
    }

    #[test]
    fn undo_accepts_a_consistent_source_device_renumber() {
        let root = tempdir().unwrap();
        let journals = tempdir().unwrap();
        let plan = plan(root.path());
        let mut apply = apply_plan(&plan, &journals.path().join("apply.json")).unwrap();
        renumber_apply_devices(&mut apply);

        let undo = undo_session(&apply, &journals.path().join("undo.json")).unwrap();

        assert_eq!(undo.state, UndoState::Completed);
        assert!(root.path().join("report.txt").exists());
        assert!(!root.path().join("Documents").exists());
    }

    #[test]
    fn recovery_rejects_an_identity_left_on_the_recorded_source_device() {
        let expected = FsIdentity {
            device: 38,
            inode: 42,
        };
        let stale = expected.clone();

        assert!(!identity_matches_for_recovery(&stale, &expected, 38, 37));
        assert!(identity_matches_for_recovery(
            &FsIdentity {
                device: 37,
                inode: 42,
            },
            &expected,
            38,
            37,
        ));
    }

    #[test]
    fn undo_recognizes_an_already_restored_file_after_device_renumber() {
        let root = tempdir().unwrap();
        let journals = tempdir().unwrap();
        let plan = plan(root.path());
        let mut apply = apply_plan(&plan, &journals.path().join("apply.json")).unwrap();
        renumber_apply_devices(&mut apply);
        fs::rename(
            root.path().join("Documents/Reports/report.txt"),
            root.path().join("report.txt"),
        )
        .unwrap();

        let undo = undo_session(&apply, &journals.path().join("undo.json")).unwrap();

        assert_eq!(undo.state, UndoState::Completed);
        assert_eq!(undo.moves[0].outcome, UndoMoveOutcome::AlreadyRestored);
        assert!(!root.path().join("Documents").exists());
    }

    #[test]
    fn undo_rejects_a_same_content_replacement_after_device_renumber() {
        let root = tempdir().unwrap();
        let journals = tempdir().unwrap();
        let plan = plan(root.path());
        let mut apply = apply_plan(&plan, &journals.path().join("apply.json")).unwrap();
        renumber_apply_devices(&mut apply);
        let destination = root.path().join("Documents/Reports/report.txt");
        fs::rename(&destination, root.path().join("original-inode.txt")).unwrap();
        fs::write(&destination, b"report").unwrap();

        let undo = undo_session(&apply, &journals.path().join("undo.json")).unwrap();

        assert_eq!(undo.state, UndoState::PartialFailure);
        assert!(matches!(
            undo.moves[0].outcome,
            UndoMoveOutcome::Conflict { .. }
        ));
        assert!(!root.path().join("report.txt").exists());
    }

    #[test]
    fn undo_rejects_a_changed_source_inode_after_device_renumber() {
        let root = tempdir().unwrap();
        let journals = tempdir().unwrap();
        let plan = plan(root.path());
        let mut apply = apply_plan(&plan, &journals.path().join("apply.json")).unwrap();
        renumber_apply_devices(&mut apply);
        apply.source_identity.inode = apply.source_identity.inode.wrapping_add(1);
        let undo_path = journals.path().join("undo.json");

        assert!(undo_session(&apply, &undo_path).is_err());
        assert!(!undo_path.exists());
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
    fn rejects_competing_source_lock_and_accepts_an_existing_lock() {
        let root = tempdir().unwrap();
        let journals = tempdir().unwrap();
        let plan = plan(root.path());
        let apply_path = journals.path().join("apply.json");
        let lock = SourceLock::acquire(root.path()).unwrap();

        let error = apply_plan(&plan, &apply_path).unwrap_err();
        assert!(error.to_string().contains("already locked"));
        assert!(!apply_path.exists());

        let session = apply_plan_with_lock(&plan, &apply_path, &lock).unwrap();
        assert_eq!(session.state, ApplyState::Completed);
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

    #[test]
    fn applies_and_undoes_a_nested_source_without_removing_source_directories() {
        let root = tempdir().unwrap();
        let journals = tempdir().unwrap();
        let plan = nested_plan(root.path());
        let apply_path = journals.path().join("apply.json");
        let undo_path = journals.path().join("undo.json");

        let apply = apply_plan(&plan, &apply_path).unwrap();
        assert!(root.path().join("incoming/deep").is_dir());
        assert!(root.path().join("Documents/Reports/report.txt").is_file());

        let undo = undo_session(&apply, &undo_path).unwrap();
        assert_eq!(undo.state, UndoState::Completed);
        assert!(root.path().join("incoming/deep/report.txt").is_file());
        assert!(root.path().join("incoming/deep").is_dir());
    }

    #[test]
    fn preflight_rejects_a_nested_source_parent_replaced_by_a_symlink() {
        let root = tempdir().unwrap();
        let journals = tempdir().unwrap();
        let plan = nested_plan(root.path());
        fs::rename(
            root.path().join("incoming"),
            root.path().join("real-incoming"),
        )
        .unwrap();
        symlink(
            root.path().join("real-incoming"),
            root.path().join("incoming"),
        )
        .unwrap();

        let error = apply_plan(&plan, &journals.path().join("apply.json")).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("source component must be a real directory")
        );
        assert!(!journals.path().join("apply.json").exists());
    }
}
