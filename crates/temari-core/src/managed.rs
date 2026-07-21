use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use crate::{
    Error, FileFingerprint, FsIdentity, SourceLock,
    artifact::normalize_relative_path,
    filesystem::{canonical_directory, fingerprint, identity, io_error, path_exists},
};

pub const MANAGED_AREAS: [&str; 3] = ["Kept", "Inbox", "Library"];

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryFingerprint {
    pub identity: FsIdentity,
    pub manifest_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ManagedEntryFingerprint {
    File { fingerprint: FileFingerprint },
    Directory { fingerprint: DirectoryFingerprint },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedSetupMove {
    pub source_path: String,
    pub destination_path: String,
    pub fingerprint: ManagedEntryFingerprint,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedSetupPlan {
    pub version: u32,
    pub source: String,
    pub source_identity: FsIdentity,
    pub areas: Vec<String>,
    pub moves: Vec<ManagedSetupMove>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedSetupState {
    Running,
    Completed,
    Failed,
    PartialFailure,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ManagedAreaOutcome {
    Pending,
    Creating,
    Created { identity: FsIdentity },
    Conflict { message: String },
    Failed { message: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ManagedMoveOutcome {
    Pending,
    Moving,
    Moved,
    Conflict { message: String },
    Failed { message: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedAreaRecord {
    pub path: String,
    pub outcome: ManagedAreaOutcome,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedMoveRecord {
    pub source_path: String,
    pub destination_path: String,
    pub fingerprint: ManagedEntryFingerprint,
    pub outcome: ManagedMoveOutcome,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedSetupSession {
    pub version: u32,
    pub id: String,
    pub plan_sha256: String,
    pub source: String,
    pub source_identity: FsIdentity,
    pub state: ManagedSetupState,
    pub started_unix_ms: u128,
    pub finished_unix_ms: Option<u128>,
    pub areas: Vec<ManagedAreaRecord>,
    pub moves: Vec<ManagedMoveRecord>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedSetupUndoState {
    Running,
    Completed,
    PartialFailure,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ManagedUndoMoveOutcome {
    Pending,
    Restoring,
    Restored,
    NotApplied,
    Conflict { message: String },
    Failed { message: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ManagedUndoAreaOutcome {
    Pending,
    Removing,
    Removed,
    NotPresent,
    NotEmpty,
    Conflict { message: String },
    Failed { message: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedUndoMoveRecord {
    pub source_path: String,
    pub destination_path: String,
    pub outcome: ManagedUndoMoveOutcome,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedUndoAreaRecord {
    pub path: String,
    pub outcome: ManagedUndoAreaOutcome,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedSetupUndoSession {
    pub version: u32,
    pub setup_session_id: String,
    pub source: String,
    pub source_identity: FsIdentity,
    pub state: ManagedSetupUndoState,
    pub started_unix_ms: u128,
    pub finished_unix_ms: Option<u128>,
    pub moves: Vec<ManagedUndoMoveRecord>,
    pub areas: Vec<ManagedUndoAreaRecord>,
}

impl ManagedSetupPlan {
    pub fn load(path: &Path) -> Result<Self, Error> {
        let plan: Self = load_json(path)?;
        plan.validate()?;
        Ok(plan)
    }

    pub fn validate(&self) -> Result<(), Error> {
        validate_source_header(self.version, &self.source)?;
        if self.areas != MANAGED_AREAS {
            return Err(Error::InvalidArtifact(
                "managed setup areas must be Kept, Inbox, and Library in that order".into(),
            ));
        }
        let mut sources = HashSet::new();
        let mut destinations = HashSet::new();
        for movement in &self.moves {
            validate_move(movement)?;
            if !sources.insert(&movement.source_path)
                || !destinations.insert(&movement.destination_path)
            {
                return Err(Error::InvalidArtifact(
                    "managed setup plan contains duplicate paths".into(),
                ));
            }
        }
        validate_move_order(self.moves.iter().map(|movement| &movement.fingerprint))?;
        Ok(())
    }

    pub fn sha256(&self) -> Result<String, Error> {
        self.validate()?;
        let bytes = serde_json::to_vec(self)?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }
}

impl ManagedSetupSession {
    pub fn load(path: &Path) -> Result<Self, Error> {
        let session: Self = load_json(path)?;
        session.validate()?;
        Ok(session)
    }

    pub fn validate(&self) -> Result<(), Error> {
        validate_source_header(self.version, &self.source)?;
        validate_digest(&self.plan_sha256)?;
        if self.id.trim().is_empty() {
            return Err(Error::InvalidArtifact(
                "managed setup session ID must not be empty".into(),
            ));
        }
        if self
            .areas
            .iter()
            .map(|area| area.path.as_str())
            .collect::<Vec<_>>()
            != MANAGED_AREAS
        {
            return Err(Error::InvalidArtifact(
                "managed setup session contains invalid areas".into(),
            ));
        }
        let mut sources = HashSet::new();
        let mut destinations = HashSet::new();
        for movement in &self.moves {
            validate_move(&ManagedSetupMove {
                source_path: movement.source_path.clone(),
                destination_path: movement.destination_path.clone(),
                fingerprint: movement.fingerprint.clone(),
            })?;
            if !sources.insert(&movement.source_path)
                || !destinations.insert(&movement.destination_path)
            {
                return Err(Error::InvalidArtifact(
                    "managed setup session contains duplicate paths".into(),
                ));
            }
            if movement.outcome != ManagedMoveOutcome::Pending {
                let area = movement
                    .destination_path
                    .split_once('/')
                    .map(|(area, _)| area)
                    .ok_or_else(|| {
                        Error::InvalidArtifact("managed move destination has no area".into())
                    })?;
                if !self.areas.iter().any(|record| {
                    record.path == area
                        && matches!(record.outcome, ManagedAreaOutcome::Created { .. })
                }) {
                    return Err(Error::InvalidArtifact(
                        "started managed move has no created destination area".into(),
                    ));
                }
            }
        }
        validate_move_order(self.moves.iter().map(|movement| &movement.fingerprint))?;
        validate_terminal_state(&self.state, self.finished_unix_ms)?;
        if self.state != ManagedSetupState::Running
            && (self
                .areas
                .iter()
                .any(|area| area.outcome == ManagedAreaOutcome::Creating)
                || self
                    .moves
                    .iter()
                    .any(|movement| movement.outcome == ManagedMoveOutcome::Moving))
        {
            return Err(Error::InvalidArtifact(
                "terminal managed setup contains an in-progress operation".into(),
            ));
        }
        if self.state == ManagedSetupState::Completed
            && (self
                .areas
                .iter()
                .any(|area| !matches!(area.outcome, ManagedAreaOutcome::Created { .. }))
                || self
                    .moves
                    .iter()
                    .any(|movement| movement.outcome != ManagedMoveOutcome::Moved))
        {
            return Err(Error::InvalidArtifact(
                "completed managed setup contains unfinished operations".into(),
            ));
        }
        let changed = self
            .areas
            .iter()
            .any(|area| matches!(area.outcome, ManagedAreaOutcome::Created { .. }))
            || self
                .moves
                .iter()
                .any(|movement| movement.outcome == ManagedMoveOutcome::Moved);
        let has_problem = self.areas.iter().any(|area| {
            matches!(
                area.outcome,
                ManagedAreaOutcome::Conflict { .. } | ManagedAreaOutcome::Failed { .. }
            )
        }) || self.moves.iter().any(|movement| {
            matches!(
                movement.outcome,
                ManagedMoveOutcome::Conflict { .. } | ManagedMoveOutcome::Failed { .. }
            )
        });
        match self.state {
            ManagedSetupState::Failed if changed || !has_problem => {
                return Err(Error::InvalidArtifact(
                    "failed managed setup must contain a failure and no applied changes".into(),
                ));
            }
            ManagedSetupState::PartialFailure if !changed || !has_problem => {
                return Err(Error::InvalidArtifact(
                    "partial managed setup must contain both a change and a failure".into(),
                ));
            }
            _ => {}
        }
        Ok(())
    }
}

impl ManagedSetupUndoSession {
    pub fn load(path: &Path) -> Result<Self, Error> {
        let session: Self = load_json(path)?;
        session.validate()?;
        Ok(session)
    }

    pub fn validate(&self) -> Result<(), Error> {
        validate_source_header(self.version, &self.source)?;
        if self.setup_session_id.trim().is_empty() {
            return Err(Error::InvalidArtifact(
                "managed setup undo session ID must not be empty".into(),
            ));
        }
        let mut sources = HashSet::new();
        let mut destinations = HashSet::new();
        for movement in &self.moves {
            normalize_relative_path(&movement.source_path)?;
            normalize_relative_path(&movement.destination_path)?;
            if !sources.insert(&movement.source_path)
                || !destinations.insert(&movement.destination_path)
            {
                return Err(Error::InvalidArtifact(
                    "managed setup undo contains duplicate move paths".into(),
                ));
            }
        }
        if self
            .areas
            .iter()
            .map(|area| area.path.as_str())
            .collect::<Vec<_>>()
            != ["Library", "Inbox", "Kept"]
        {
            return Err(Error::InvalidArtifact(
                "managed setup undo contains invalid areas".into(),
            ));
        }
        match self.state {
            ManagedSetupUndoState::Running if self.finished_unix_ms.is_some() => {
                return Err(Error::InvalidArtifact(
                    "running managed setup undo must not have a finish time".into(),
                ));
            }
            ManagedSetupUndoState::Completed | ManagedSetupUndoState::PartialFailure
                if self.finished_unix_ms.is_none() =>
            {
                return Err(Error::InvalidArtifact(
                    "terminal managed setup undo must have a finish time".into(),
                ));
            }
            _ => {}
        }
        if self.state != ManagedSetupUndoState::Running
            && (self.moves.iter().any(|movement| {
                matches!(
                    movement.outcome,
                    ManagedUndoMoveOutcome::Pending | ManagedUndoMoveOutcome::Restoring
                )
            }) || self.areas.iter().any(|area| {
                matches!(
                    area.outcome,
                    ManagedUndoAreaOutcome::Pending | ManagedUndoAreaOutcome::Removing
                )
            }))
        {
            return Err(Error::InvalidArtifact(
                "terminal managed setup undo contains an unfinished operation".into(),
            ));
        }
        if self.state == ManagedSetupUndoState::Completed
            && (self.moves.iter().any(|movement| {
                !matches!(
                    movement.outcome,
                    ManagedUndoMoveOutcome::Restored | ManagedUndoMoveOutcome::NotApplied
                )
            }) || self.areas.iter().any(|area| {
                !matches!(
                    area.outcome,
                    ManagedUndoAreaOutcome::Removed | ManagedUndoAreaOutcome::NotPresent
                )
            }))
        {
            return Err(Error::InvalidArtifact(
                "completed managed setup undo contains conflicts".into(),
            ));
        }
        if self.state == ManagedSetupUndoState::PartialFailure
            && !self.moves.iter().any(|movement| {
                matches!(
                    movement.outcome,
                    ManagedUndoMoveOutcome::Conflict { .. } | ManagedUndoMoveOutcome::Failed { .. }
                )
            })
            && !self.areas.iter().any(|area| {
                matches!(
                    area.outcome,
                    ManagedUndoAreaOutcome::NotEmpty
                        | ManagedUndoAreaOutcome::Conflict { .. }
                        | ManagedUndoAreaOutcome::Failed { .. }
                )
            })
        {
            return Err(Error::InvalidArtifact(
                "partial managed setup undo must contain a conflict or failure".into(),
            ));
        }
        Ok(())
    }
}

pub fn build_managed_setup_plan(source: &Path) -> Result<ManagedSetupPlan, Error> {
    let (source, source_identity) = canonical_directory(source)?;
    for area in MANAGED_AREAS {
        if path_exists(&source.join(area))? {
            return Err(Error::InvalidArtifact(format!(
                "managed area already exists: {area:?}"
            )));
        }
    }

    let mut entries = read_directory_sorted(&source)?;
    let mut moves = Vec::with_capacity(entries.len());
    for (name, path) in entries.drain(..) {
        let metadata =
            fs::symlink_metadata(&path).map_err(|error| io_error("inspect", &path, error))?;
        let (area, value) = if metadata.file_type().is_file() && !metadata.file_type().is_symlink()
        {
            (
                "Inbox",
                ManagedEntryFingerprint::File {
                    fingerprint: fingerprint(&path)?,
                },
            )
        } else if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
            if metadata.dev() != source_identity.device {
                return Err(Error::InvalidArtifact(format!(
                    "managed setup requires same-filesystem moves: {name:?}"
                )));
            }
            (
                "Kept",
                ManagedEntryFingerprint::Directory {
                    fingerprint: fingerprint_directory(&path)?,
                },
            )
        } else {
            return Err(Error::InvalidArtifact(format!(
                "managed setup source contains an unsupported root entry: {name:?}"
            )));
        };
        if metadata.dev() != source_identity.device {
            return Err(Error::InvalidArtifact(format!(
                "managed setup requires same-filesystem moves: {name:?}"
            )));
        }
        moves.push(ManagedSetupMove {
            source_path: name.clone(),
            destination_path: format!("{area}/{name}"),
            fingerprint: value,
        });
    }
    moves.sort_by(|left, right| {
        managed_entry_order(&left.fingerprint)
            .cmp(&managed_entry_order(&right.fingerprint))
            .then_with(|| {
                left.source_path
                    .as_bytes()
                    .cmp(right.source_path.as_bytes())
            })
    });

    let plan = ManagedSetupPlan {
        version: 1,
        source: path_text(&source)?,
        source_identity,
        areas: MANAGED_AREAS
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
        moves,
    };
    plan.validate()?;
    Ok(plan)
}

/// Build a read-only plan that adopts newly created root directories into
/// Kept after managed workspace setup has completed.
pub fn build_managed_directory_adoption_plan(source: &Path) -> Result<ManagedSetupPlan, Error> {
    let (source, source_identity) = canonical_directory(source)?;
    for area in MANAGED_AREAS {
        let path = source.join(area);
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| io_error("inspect managed area", &path, error))?;
        if !metadata.file_type().is_dir()
            || metadata.file_type().is_symlink()
            || metadata.dev() != source_identity.device
        {
            return Err(Error::InvalidArtifact(format!(
                "managed area must be a real same-filesystem directory: {area:?}"
            )));
        }
    }

    let mut moves = Vec::new();
    for (name, path) in read_directory_sorted(&source)? {
        if MANAGED_AREAS.contains(&name.as_str()) {
            continue;
        }
        let metadata =
            fs::symlink_metadata(&path).map_err(|error| io_error("inspect", &path, error))?;
        if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.dev() != source_identity.device {
            return Err(Error::InvalidArtifact(format!(
                "managed directory adoption requires a same-filesystem move: {name:?}"
            )));
        }
        let destination = source.join("Kept").join(&name);
        if path_exists(&destination)? {
            return Err(Error::InvalidArtifact(format!(
                "managed directory adoption destination is occupied: {:?}",
                format!("Kept/{name}")
            )));
        }
        moves.push(ManagedSetupMove {
            source_path: name.clone(),
            destination_path: format!("Kept/{name}"),
            fingerprint: ManagedEntryFingerprint::Directory {
                fingerprint: fingerprint_directory(&path)?,
            },
        });
    }
    moves.sort_by(|left, right| {
        left.source_path
            .as_bytes()
            .cmp(right.source_path.as_bytes())
    });
    let plan = ManagedSetupPlan {
        version: 1,
        source: path_text(&source)?,
        source_identity,
        areas: MANAGED_AREAS
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
        moves,
    };
    plan.validate()?;
    Ok(plan)
}

/// Apply a reviewed directory-adoption plan using the same durable journal,
/// fingerprint checks, lock, and resume path as managed setup.
pub fn apply_managed_directory_adoption(
    plan: &ManagedSetupPlan,
    journal_path: &Path,
) -> Result<ManagedSetupSession, Error> {
    plan.validate()?;
    if plan.moves.iter().any(|movement| {
        !matches!(
            movement.fingerprint,
            ManagedEntryFingerprint::Directory { .. }
        )
    }) {
        return Err(Error::InvalidArtifact(
            "managed directory adoption plan may contain only directories".into(),
        ));
    }
    let lock = SourceLock::acquire(Path::new(&plan.source))?;
    lock.validate_source(&plan.source, &plan.source_identity)?;
    let current = build_managed_directory_adoption_plan(Path::new(&plan.source))?;
    if current != *plan {
        return Err(Error::InvalidArtifact(
            "managed source changed after directory adoption planning".into(),
        ));
    }
    validate_new_journal(journal_path, Path::new(&plan.source))?;
    let root = Path::new(&plan.source);
    let mut areas = Vec::with_capacity(MANAGED_AREAS.len());
    for area in MANAGED_AREAS {
        let metadata = fs::symlink_metadata(root.join(area))
            .map_err(|error| io_error("inspect managed area", &root.join(area), error))?;
        areas.push(ManagedAreaRecord {
            path: area.into(),
            outcome: ManagedAreaOutcome::Created {
                identity: identity(&metadata),
            },
        });
    }
    let mut session = ManagedSetupSession {
        version: 1,
        id: format!("{}-{}", now_unix_ms()?, std::process::id()),
        plan_sha256: plan.sha256()?,
        source: plan.source.clone(),
        source_identity: plan.source_identity.clone(),
        state: ManagedSetupState::Running,
        started_unix_ms: now_unix_ms()?,
        finished_unix_ms: None,
        areas,
        moves: plan
            .moves
            .iter()
            .map(|movement| ManagedMoveRecord {
                source_path: movement.source_path.clone(),
                destination_path: movement.destination_path.clone(),
                fingerprint: movement.fingerprint.clone(),
                outcome: ManagedMoveOutcome::Pending,
            })
            .collect(),
    };
    create_journal(journal_path, &session, root)?;
    continue_setup(&mut session, journal_path)?;
    Ok(session)
}

pub fn fingerprint_directory(path: &Path) -> Result<DirectoryFingerprint, Error> {
    let metadata = fs::symlink_metadata(path).map_err(|error| io_error("inspect", path, error))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(Error::InvalidArtifact(format!(
            "managed directory must be a real directory: {:?}",
            path.display().to_string()
        )));
    }
    let mut hasher = Sha256::new();
    hash_manifest(path, path, &mut hasher)?;
    Ok(DirectoryFingerprint {
        identity: identity(&metadata),
        manifest_sha256: format!("{:x}", hasher.finalize()),
    })
}

pub fn preflight_managed_setup(plan: &ManagedSetupPlan, journal_path: &Path) -> Result<(), Error> {
    plan.validate()?;
    let current = build_managed_setup_plan(Path::new(&plan.source))?;
    if current != *plan {
        return Err(Error::InvalidArtifact(
            "managed source changed after setup planning".into(),
        ));
    }
    validate_new_journal(journal_path, Path::new(&plan.source))
}

pub fn apply_managed_setup(
    plan: &ManagedSetupPlan,
    journal_path: &Path,
) -> Result<ManagedSetupSession, Error> {
    let lock = SourceLock::acquire(Path::new(&plan.source))?;
    apply_managed_setup_with_lock(plan, journal_path, &lock)
}

pub fn apply_managed_setup_with_lock(
    plan: &ManagedSetupPlan,
    journal_path: &Path,
    lock: &SourceLock,
) -> Result<ManagedSetupSession, Error> {
    lock.validate_source(&plan.source, &plan.source_identity)?;
    preflight_managed_setup(plan, journal_path)?;
    let mut session = ManagedSetupSession {
        version: 1,
        id: format!("{}-{}", now_unix_ms()?, std::process::id()),
        plan_sha256: plan.sha256()?,
        source: plan.source.clone(),
        source_identity: plan.source_identity.clone(),
        state: ManagedSetupState::Running,
        started_unix_ms: now_unix_ms()?,
        finished_unix_ms: None,
        areas: plan
            .areas
            .iter()
            .map(|path| ManagedAreaRecord {
                path: path.clone(),
                outcome: ManagedAreaOutcome::Pending,
            })
            .collect(),
        moves: plan
            .moves
            .iter()
            .map(|movement| ManagedMoveRecord {
                source_path: movement.source_path.clone(),
                destination_path: movement.destination_path.clone(),
                fingerprint: movement.fingerprint.clone(),
                outcome: ManagedMoveOutcome::Pending,
            })
            .collect(),
    };
    create_journal(journal_path, &session, Path::new(&plan.source))?;
    continue_setup(&mut session, journal_path)?;
    Ok(session)
}

pub fn preflight_managed_resume(
    session: &ManagedSetupSession,
    journal_path: &Path,
) -> Result<(), Error> {
    session.validate()?;
    if session.state != ManagedSetupState::Running {
        return Err(Error::InvalidArtifact(format!(
            "only a running managed setup can be resumed; found {:?}",
            session.state
        )));
    }
    verify_source(&session.source, &session.source_identity, true)?;
    validate_existing_journal(journal_path, Path::new(&session.source))
}

pub fn resume_managed_setup(journal_path: &Path) -> Result<ManagedSetupSession, Error> {
    let session = ManagedSetupSession::load(journal_path)?;
    let lock = SourceLock::acquire(Path::new(&session.source))?;
    resume_managed_setup_with_lock(journal_path, &lock)
}

pub fn resume_managed_setup_with_lock(
    journal_path: &Path,
    lock: &SourceLock,
) -> Result<ManagedSetupSession, Error> {
    let mut session = ManagedSetupSession::load(journal_path)?;
    lock.validate_recovery_source(&session.source, &session.source_identity)?;
    preflight_managed_resume(&session, journal_path)?;
    reconcile_setup(&mut session, journal_path)?;
    if session.state == ManagedSetupState::Running {
        continue_setup(&mut session, journal_path)?;
    }
    Ok(session)
}

pub fn preflight_managed_undo(
    setup: &ManagedSetupSession,
    journal_path: &Path,
) -> Result<(), Error> {
    setup.validate()?;
    if setup.state == ManagedSetupState::Running {
        return Err(Error::InvalidArtifact(
            "running managed setup must be resumed before undo".into(),
        ));
    }
    verify_source(&setup.source, &setup.source_identity, true)?;
    validate_new_journal(journal_path, Path::new(&setup.source))
}

pub fn undo_managed_setup(
    setup: &ManagedSetupSession,
    journal_path: &Path,
) -> Result<ManagedSetupUndoSession, Error> {
    let lock = SourceLock::acquire(Path::new(&setup.source))?;
    undo_managed_setup_with_lock(setup, journal_path, &lock)
}

pub fn undo_managed_setup_with_lock(
    setup: &ManagedSetupSession,
    journal_path: &Path,
    lock: &SourceLock,
) -> Result<ManagedSetupUndoSession, Error> {
    undo_managed_session_with_lock(setup, journal_path, lock, true)
}

/// Undo a directory-adoption session without removing the three managed areas,
/// which predated the adoption and belong to the workspace setup.
pub fn undo_managed_directory_adoption(
    setup: &ManagedSetupSession,
    journal_path: &Path,
) -> Result<ManagedSetupUndoSession, Error> {
    let lock = SourceLock::acquire(Path::new(&setup.source))?;
    undo_managed_session_with_lock(setup, journal_path, &lock, false)
}

fn undo_managed_session_with_lock(
    setup: &ManagedSetupSession,
    journal_path: &Path,
    lock: &SourceLock,
    remove_areas: bool,
) -> Result<ManagedSetupUndoSession, Error> {
    lock.validate_recovery_source(&setup.source, &setup.source_identity)?;
    preflight_managed_undo(setup, journal_path)?;
    let mut undo = ManagedSetupUndoSession {
        version: 1,
        setup_session_id: setup.id.clone(),
        source: setup.source.clone(),
        source_identity: setup.source_identity.clone(),
        state: ManagedSetupUndoState::Running,
        started_unix_ms: now_unix_ms()?,
        finished_unix_ms: None,
        moves: setup
            .moves
            .iter()
            .rev()
            .map(|movement| ManagedUndoMoveRecord {
                source_path: movement.source_path.clone(),
                destination_path: movement.destination_path.clone(),
                outcome: ManagedUndoMoveOutcome::Pending,
            })
            .collect(),
        areas: setup
            .areas
            .iter()
            .rev()
            .map(|area| ManagedUndoAreaRecord {
                path: area.path.clone(),
                outcome: if remove_areas {
                    ManagedUndoAreaOutcome::Pending
                } else {
                    ManagedUndoAreaOutcome::NotPresent
                },
            })
            .collect(),
    };
    create_journal(journal_path, &undo, Path::new(&setup.source))?;
    continue_undo(setup, &mut undo, journal_path, remove_areas)?;
    Ok(undo)
}

fn continue_setup(session: &mut ManagedSetupSession, journal_path: &Path) -> Result<(), Error> {
    let root = PathBuf::from(&session.source);
    let (_, current_source_identity) = canonical_directory(&root)?;
    for index in 0..session.areas.len() {
        if matches!(
            session.areas[index].outcome,
            ManagedAreaOutcome::Created { .. }
        ) {
            continue;
        }
        if session.areas[index].outcome != ManagedAreaOutcome::Pending {
            return Err(Error::InvalidArtifact(
                "managed area is not resumable".into(),
            ));
        }
        session.areas[index].outcome = ManagedAreaOutcome::Creating;
        update_journal(journal_path, session)?;
        let path = root.join(&session.areas[index].path);
        let result = if path_exists(&path)? {
            Err(Error::InvalidArtifact(format!(
                "managed area is occupied: {:?}",
                session.areas[index].path
            )))
        } else {
            fs::create_dir(&path).map_err(|error| io_error("create", &path, error))?;
            sync_directory(&root)?;
            let metadata =
                fs::symlink_metadata(&path).map_err(|error| io_error("inspect", &path, error))?;
            Ok(identity(&metadata))
        };
        match result {
            Ok(value) => {
                session.areas[index].outcome = ManagedAreaOutcome::Created { identity: value }
            }
            Err(error) => {
                session.areas[index].outcome = ManagedAreaOutcome::Failed {
                    message: error.to_string(),
                };
                finish_setup_failure(session, journal_path)?;
                return Ok(());
            }
        }
        update_journal(journal_path, session)?;
    }

    for index in 0..session.moves.len() {
        if session.moves[index].outcome == ManagedMoveOutcome::Moved {
            continue;
        }
        if session.moves[index].outcome != ManagedMoveOutcome::Pending {
            return Err(Error::InvalidArtifact(
                "managed move is not resumable".into(),
            ));
        }
        session.moves[index].outcome = ManagedMoveOutcome::Moving;
        update_journal(journal_path, session)?;
        let original = root.join(&session.moves[index].source_path);
        let destination = root.join(&session.moves[index].destination_path);
        let result = (|| {
            verify_destination_area(
                &root,
                &session.areas,
                &session.moves[index].destination_path,
                session.source_identity.device,
                current_source_identity.device,
            )?;
            if path_exists(&destination)? {
                return Err(Error::InvalidArtifact(format!(
                    "managed destination is occupied: {:?}",
                    session.moves[index].destination_path
                )));
            }
            verify_entry_for_recovery(
                &original,
                &session.moves[index].fingerprint,
                session.source_identity.device,
                current_source_identity.device,
            )?;
            fs::rename(&original, &destination).map_err(|error| {
                if error.raw_os_error() == Some(18) {
                    Error::InvalidArtifact(
                        "managed setup requires same-filesystem atomic rename".into(),
                    )
                } else {
                    io_error("move", &original, error)
                }
            })?;
            sync_directory(&root)?;
            sync_directory(destination.parent().expect("destination has parent"))
        })();
        match result {
            Ok(()) => session.moves[index].outcome = ManagedMoveOutcome::Moved,
            Err(error) => {
                session.moves[index].outcome = ManagedMoveOutcome::Failed {
                    message: error.to_string(),
                };
                finish_setup_failure(session, journal_path)?;
                return Ok(());
            }
        }
        update_journal(journal_path, session)?;
    }
    session.state = ManagedSetupState::Completed;
    session.finished_unix_ms = Some(now_unix_ms()?);
    update_journal(journal_path, session)
}

fn reconcile_setup(session: &mut ManagedSetupSession, journal_path: &Path) -> Result<(), Error> {
    let root = PathBuf::from(&session.source);
    let (_, current_source_identity) = canonical_directory(&root)?;
    for index in 0..session.areas.len() {
        let path = root.join(&session.areas[index].path);
        let replacement = match &session.areas[index].outcome {
            ManagedAreaOutcome::Creating => match fs::symlink_metadata(&path) {
                Ok(metadata)
                    if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() =>
                {
                    Some(ManagedAreaOutcome::Created {
                        identity: identity(&metadata),
                    })
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    Some(ManagedAreaOutcome::Pending)
                }
                _ => Some(ManagedAreaOutcome::Conflict {
                    message: "in-progress managed area has an unsafe type".into(),
                }),
            },
            ManagedAreaOutcome::Created { identity: expected } => match fs::symlink_metadata(&path)
            {
                Ok(metadata)
                    if metadata.file_type().is_dir()
                        && !metadata.file_type().is_symlink()
                        && identity_matches_for_recovery(
                            &identity(&metadata),
                            expected,
                            session.source_identity.device,
                            current_source_identity.device,
                        ) =>
                {
                    None
                }
                _ => Some(ManagedAreaOutcome::Conflict {
                    message: "created managed area changed before resume".into(),
                }),
            },
            ManagedAreaOutcome::Pending => {
                if path_exists(&path)? {
                    Some(ManagedAreaOutcome::Conflict {
                        message: "pending managed area is occupied".into(),
                    })
                } else {
                    None
                }
            }
            ManagedAreaOutcome::Failed { .. } | ManagedAreaOutcome::Conflict { .. } => {
                finalize_setup_conflict(session, journal_path)?;
                return Ok(());
            }
        };
        if let Some(outcome) = replacement {
            let conflict = matches!(outcome, ManagedAreaOutcome::Conflict { .. });
            session.areas[index].outcome = outcome;
            update_journal(journal_path, session)?;
            if conflict {
                finalize_setup_conflict(session, journal_path)?;
                return Ok(());
            }
        }
    }
    for index in 0..session.moves.len() {
        let original = root.join(&session.moves[index].source_path);
        let destination = root.join(&session.moves[index].destination_path);
        if let Err(error) = verify_destination_area(
            &root,
            &session.areas,
            &session.moves[index].destination_path,
            session.source_identity.device,
            current_source_identity.device,
        ) {
            session.moves[index].outcome = ManagedMoveOutcome::Conflict {
                message: error.to_string(),
            };
            update_journal(journal_path, session)?;
            finalize_setup_conflict(session, journal_path)?;
            return Ok(());
        }
        let location = entry_location_for_recovery(
            &original,
            &destination,
            &session.moves[index].fingerprint,
            session.source_identity.device,
            current_source_identity.device,
        )?;
        let replacement = match (&session.moves[index].outcome, location) {
            (ManagedMoveOutcome::Moving, EntryLocation::Destination) => {
                Some(ManagedMoveOutcome::Moved)
            }
            (ManagedMoveOutcome::Moving, EntryLocation::Original) => {
                Some(ManagedMoveOutcome::Pending)
            }
            (ManagedMoveOutcome::Moved, EntryLocation::Destination)
            | (ManagedMoveOutcome::Pending, EntryLocation::Original) => None,
            _ => Some(ManagedMoveOutcome::Conflict {
                message: "managed move state is ambiguous during resume".into(),
            }),
        };
        if let Some(outcome) = replacement {
            let conflict = matches!(outcome, ManagedMoveOutcome::Conflict { .. });
            session.moves[index].outcome = outcome;
            update_journal(journal_path, session)?;
            if conflict {
                finalize_setup_conflict(session, journal_path)?;
                return Ok(());
            }
        }
    }
    Ok(())
}

fn continue_undo(
    setup: &ManagedSetupSession,
    undo: &mut ManagedSetupUndoSession,
    journal_path: &Path,
    remove_areas: bool,
) -> Result<(), Error> {
    let root = PathBuf::from(&setup.source);
    let (_, current_source_identity) = canonical_directory(&root)?;
    let mut partial = false;
    for index in 0..undo.moves.len() {
        let setup_move = &setup.moves[setup.moves.len() - 1 - index];
        if setup_move.outcome != ManagedMoveOutcome::Moved {
            undo.moves[index].outcome = ManagedUndoMoveOutcome::NotApplied;
            update_journal(journal_path, undo)?;
            continue;
        }
        let original = root.join(&setup_move.source_path);
        let destination = root.join(&setup_move.destination_path);
        if let Err(error) = verify_destination_area(
            &root,
            &setup.areas,
            &setup_move.destination_path,
            setup.source_identity.device,
            current_source_identity.device,
        ) {
            undo.moves[index].outcome = ManagedUndoMoveOutcome::Conflict {
                message: error.to_string(),
            };
            partial = true;
        } else if path_exists(&original)? {
            undo.moves[index].outcome = ManagedUndoMoveOutcome::Conflict {
                message: "original path is occupied; refusing to overwrite".into(),
            };
            partial = true;
        } else if !path_exists(&destination)? {
            undo.moves[index].outcome = ManagedUndoMoveOutcome::Conflict {
                message: "managed destination is missing".into(),
            };
            partial = true;
        } else if let Err(error) = verify_entry_for_recovery(
            &destination,
            &setup_move.fingerprint,
            setup.source_identity.device,
            current_source_identity.device,
        ) {
            undo.moves[index].outcome = ManagedUndoMoveOutcome::Conflict {
                message: error.to_string(),
            };
            partial = true;
        } else {
            undo.moves[index].outcome = ManagedUndoMoveOutcome::Restoring;
            update_journal(journal_path, undo)?;
            match fs::rename(&destination, &original) {
                Ok(()) => {
                    sync_directory(&root)?;
                    sync_directory(destination.parent().expect("destination has parent"))?;
                    undo.moves[index].outcome = ManagedUndoMoveOutcome::Restored;
                }
                Err(error) => {
                    undo.moves[index].outcome = ManagedUndoMoveOutcome::Failed {
                        message: error.to_string(),
                    };
                    partial = true;
                }
            }
        }
        update_journal(journal_path, undo)?;
    }
    for index in 0..undo.areas.len() {
        if !remove_areas {
            continue;
        }
        let setup_area = &setup.areas[setup.areas.len() - 1 - index];
        let path = root.join(&setup_area.path);
        let ManagedAreaOutcome::Created { identity: expected } = &setup_area.outcome else {
            undo.areas[index].outcome = ManagedUndoAreaOutcome::NotPresent;
            update_journal(journal_path, undo)?;
            continue;
        };
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                undo.areas[index].outcome = ManagedUndoAreaOutcome::NotPresent
            }
            Ok(metadata)
                if metadata.file_type().is_dir()
                    && !metadata.file_type().is_symlink()
                    && identity_matches_for_recovery(
                        &identity(&metadata),
                        expected,
                        setup.source_identity.device,
                        current_source_identity.device,
                    ) =>
            {
                if fs::read_dir(&path)
                    .map_err(|error| io_error("read", &path, error))?
                    .next()
                    .is_some()
                {
                    undo.areas[index].outcome = ManagedUndoAreaOutcome::NotEmpty;
                    partial = true;
                } else {
                    undo.areas[index].outcome = ManagedUndoAreaOutcome::Removing;
                    update_journal(journal_path, undo)?;
                    match fs::remove_dir(&path) {
                        Ok(()) => {
                            sync_directory(&root)?;
                            undo.areas[index].outcome = ManagedUndoAreaOutcome::Removed;
                        }
                        Err(error) => {
                            undo.areas[index].outcome = ManagedUndoAreaOutcome::Failed {
                                message: error.to_string(),
                            };
                            partial = true;
                        }
                    }
                }
            }
            Ok(_) => {
                undo.areas[index].outcome = ManagedUndoAreaOutcome::Conflict {
                    message: "managed area identity or type changed".into(),
                };
                partial = true;
            }
            Err(error) => {
                undo.areas[index].outcome = ManagedUndoAreaOutcome::Failed {
                    message: error.to_string(),
                };
                partial = true;
            }
        }
        update_journal(journal_path, undo)?;
    }
    undo.state = if partial {
        ManagedSetupUndoState::PartialFailure
    } else {
        ManagedSetupUndoState::Completed
    };
    undo.finished_unix_ms = Some(now_unix_ms()?);
    update_journal(journal_path, undo)
}

#[derive(Clone, Copy)]
enum EntryLocation {
    Original,
    Destination,
    Ambiguous,
}

fn entry_location_for_recovery(
    original: &Path,
    destination: &Path,
    expected: &ManagedEntryFingerprint,
    recorded_source_device: u64,
    current_source_device: u64,
) -> Result<EntryLocation, Error> {
    let original_exists = path_exists(original)?;
    let destination_exists = path_exists(destination)?;
    if original_exists
        && !destination_exists
        && verify_entry_for_recovery(
            original,
            expected,
            recorded_source_device,
            current_source_device,
        )
        .is_ok()
    {
        return Ok(EntryLocation::Original);
    }
    if !original_exists
        && destination_exists
        && verify_entry_for_recovery(
            destination,
            expected,
            recorded_source_device,
            current_source_device,
        )
        .is_ok()
    {
        return Ok(EntryLocation::Destination);
    }
    Ok(EntryLocation::Ambiguous)
}

fn verify_entry_for_recovery(
    path: &Path,
    expected: &ManagedEntryFingerprint,
    recorded_source_device: u64,
    current_source_device: u64,
) -> Result<(), Error> {
    let matches = match expected {
        ManagedEntryFingerprint::File {
            fingerprint: expected,
        } => {
            let actual = fingerprint(path)?;
            actual.size == expected.size
                && actual.sha256 == expected.sha256
                && identity_matches_for_recovery(
                    &actual.identity,
                    &expected.identity,
                    recorded_source_device,
                    current_source_device,
                )
        }
        ManagedEntryFingerprint::Directory {
            fingerprint: expected,
        } => {
            let actual = fingerprint_directory(path)?;
            actual.manifest_sha256 == expected.manifest_sha256
                && identity_matches_for_recovery(
                    &actual.identity,
                    &expected.identity,
                    recorded_source_device,
                    current_source_device,
                )
        }
    };
    if !matches {
        return Err(Error::InvalidArtifact(format!(
            "managed entry fingerprint changed: {:?}",
            path.display().to_string()
        )));
    }
    Ok(())
}

fn identity_matches_for_recovery(
    actual: &FsIdentity,
    expected: &FsIdentity,
    recorded_source_device: u64,
    current_source_device: u64,
) -> bool {
    let expected_device = if expected.device == recorded_source_device {
        current_source_device
    } else {
        expected.device
    };
    actual.device == expected_device && actual.inode == expected.inode
}

fn verify_destination_area(
    root: &Path,
    areas: &[ManagedAreaRecord],
    destination_path: &str,
    recorded_source_device: u64,
    current_source_device: u64,
) -> Result<(), Error> {
    let area_name = destination_path
        .split_once('/')
        .map(|(area, _)| area)
        .ok_or_else(|| Error::InvalidArtifact("managed destination has no area".into()))?;
    let area = areas
        .iter()
        .find(|area| area.path == area_name)
        .ok_or_else(|| Error::InvalidArtifact("managed destination area is unknown".into()))?;
    let ManagedAreaOutcome::Created { identity: expected } = &area.outcome else {
        return Err(Error::InvalidArtifact(
            "managed destination area was not created by this setup".into(),
        ));
    };
    let path = root.join(area_name);
    let metadata =
        fs::symlink_metadata(&path).map_err(|error| io_error("inspect", &path, error))?;
    if !metadata.file_type().is_dir()
        || metadata.file_type().is_symlink()
        || !identity_matches_for_recovery(
            &identity(&metadata),
            expected,
            recorded_source_device,
            current_source_device,
        )
    {
        return Err(Error::InvalidArtifact(
            "managed destination area identity or type changed".into(),
        ));
    }
    Ok(())
}

fn hash_manifest(root: &Path, path: &Path, hasher: &mut Sha256) -> Result<(), Error> {
    let relative = path
        .strip_prefix(root)
        .expect("manifest path is below root");
    let metadata = fs::symlink_metadata(path).map_err(|error| io_error("inspect", path, error))?;
    if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
        hash_field(hasher, b'd', relative.as_os_str().as_bytes());
        for (_, child) in read_directory_sorted(path)? {
            hash_manifest(root, &child, hasher)?;
        }
    } else if metadata.file_type().is_file() && !metadata.file_type().is_symlink() {
        let value = fingerprint(path)?;
        hash_field(hasher, b'f', relative.as_os_str().as_bytes());
        hasher.update(value.size.to_le_bytes());
        hasher.update(value.sha256.as_bytes());
    } else if metadata.file_type().is_symlink() {
        let target = fs::read_link(path).map_err(|error| io_error("read link", path, error))?;
        hash_field(hasher, b'l', relative.as_os_str().as_bytes());
        hash_field(hasher, b't', target.as_os_str().as_bytes());
    } else {
        return Err(Error::InvalidArtifact(format!(
            "managed directory contains a special filesystem entry: {:?}",
            path.display().to_string()
        )));
    }
    Ok(())
}

fn hash_field(hasher: &mut Sha256, tag: u8, value: &[u8]) {
    hasher.update([tag]);
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value);
}

fn read_directory_sorted(path: &Path) -> Result<Vec<(String, PathBuf)>, Error> {
    let mut values = Vec::new();
    for entry in fs::read_dir(path).map_err(|error| io_error("read", path, error))? {
        let entry = entry.map_err(|error| io_error("read", path, error))?;
        let name = entry.file_name().into_string().map_err(|_| {
            Error::InvalidArtifact(format!(
                "managed path is not valid UTF-8 below {:?}",
                path.display().to_string()
            ))
        })?;
        if name.chars().any(char::is_control) {
            return Err(Error::InvalidArtifact(
                "managed path contains control characters".into(),
            ));
        }
        values.push((name, entry.path()));
    }
    values.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
    Ok(values)
}

fn validate_move(movement: &ManagedSetupMove) -> Result<(), Error> {
    normalize_relative_path(&movement.source_path)?;
    normalize_relative_path(&movement.destination_path)?;
    if movement.source_path.contains('/') {
        return Err(Error::InvalidArtifact(
            "managed setup sources must be root entries".into(),
        ));
    }
    let expected_area = match movement.fingerprint {
        ManagedEntryFingerprint::File { .. } => "Inbox",
        ManagedEntryFingerprint::Directory { .. } => "Kept",
    };
    if movement.destination_path != format!("{expected_area}/{}", movement.source_path) {
        return Err(Error::InvalidArtifact(
            "managed setup destination does not match its entry type".into(),
        ));
    }
    match &movement.fingerprint {
        ManagedEntryFingerprint::File { fingerprint } => validate_digest(&fingerprint.sha256),
        ManagedEntryFingerprint::Directory { fingerprint } => {
            validate_digest(&fingerprint.manifest_sha256)
        }
    }
}

fn managed_entry_order(fingerprint: &ManagedEntryFingerprint) -> u8 {
    match fingerprint {
        ManagedEntryFingerprint::Directory { .. } => 0,
        ManagedEntryFingerprint::File { .. } => 1,
    }
}

fn validate_move_order<'a>(
    fingerprints: impl IntoIterator<Item = &'a ManagedEntryFingerprint>,
) -> Result<(), Error> {
    let mut saw_file = false;
    for fingerprint in fingerprints {
        match fingerprint {
            ManagedEntryFingerprint::File { .. } => saw_file = true,
            ManagedEntryFingerprint::Directory { .. } if saw_file => {
                return Err(Error::InvalidArtifact(
                    "managed setup must move every directory to Kept before staging files".into(),
                ));
            }
            ManagedEntryFingerprint::Directory { .. } => {}
        }
    }
    Ok(())
}

fn validate_source_header(version: u32, source: &str) -> Result<(), Error> {
    if version != 1 || !Path::new(source).is_absolute() || source.chars().any(char::is_control) {
        return Err(Error::InvalidArtifact(
            "managed setup artifact must be version 1 with an absolute source".into(),
        ));
    }
    Ok(())
}

fn validate_terminal_state(state: &ManagedSetupState, finished: Option<u128>) -> Result<(), Error> {
    match state {
        ManagedSetupState::Running if finished.is_some() => Err(Error::InvalidArtifact(
            "running managed setup must not have a finish time".into(),
        )),
        ManagedSetupState::Completed
        | ManagedSetupState::Failed
        | ManagedSetupState::PartialFailure
            if finished.is_none() =>
        {
            Err(Error::InvalidArtifact(
                "terminal managed setup must have a finish time".into(),
            ))
        }
        _ => Ok(()),
    }
}

fn finish_setup_failure(
    session: &mut ManagedSetupSession,
    journal_path: &Path,
) -> Result<(), Error> {
    let changed = session
        .moves
        .iter()
        .any(|value| value.outcome == ManagedMoveOutcome::Moved)
        || session
            .areas
            .iter()
            .any(|value| matches!(value.outcome, ManagedAreaOutcome::Created { .. }));
    session.state = if changed {
        ManagedSetupState::PartialFailure
    } else {
        ManagedSetupState::Failed
    };
    session.finished_unix_ms = Some(now_unix_ms()?);
    update_journal(journal_path, session)
}

fn finalize_setup_conflict(
    session: &mut ManagedSetupSession,
    journal_path: &Path,
) -> Result<(), Error> {
    let changed = session
        .moves
        .iter()
        .any(|value| value.outcome == ManagedMoveOutcome::Moved)
        || session
            .areas
            .iter()
            .any(|value| matches!(value.outcome, ManagedAreaOutcome::Created { .. }));
    session.state = if changed {
        ManagedSetupState::PartialFailure
    } else {
        ManagedSetupState::Failed
    };
    session.finished_unix_ms = Some(now_unix_ms()?);
    update_journal(journal_path, session)
}

fn verify_source(source: &str, expected: &FsIdentity, recovery: bool) -> Result<PathBuf, Error> {
    let requested = Path::new(source);
    let (canonical, actual) = canonical_directory(requested)?;
    let identity_matches = if recovery {
        actual.inode == expected.inode
    } else {
        actual == *expected
    };
    if canonical != requested || !identity_matches {
        return Err(Error::InvalidArtifact(
            "managed source path or identity changed".into(),
        ));
    }
    Ok(canonical)
}

fn validate_new_journal(path: &Path, source: &Path) -> Result<(), Error> {
    let output = canonical_journal_target(path)?;
    if output.starts_with(source) {
        return Err(Error::InvalidArtifact(
            "managed journal must be outside the source".into(),
        ));
    }
    if path_exists(&output)? {
        return Err(Error::InvalidArtifact(format!(
            "managed journal already exists: {:?}",
            output.display().to_string()
        )));
    }
    Ok(())
}

fn validate_existing_journal(path: &Path, source: &Path) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(path).map_err(|error| io_error("inspect", path, error))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(Error::InvalidArtifact(
            "managed resume journal must be a regular non-symlink file".into(),
        ));
    }
    if canonical_journal_target(path)?.starts_with(source) {
        return Err(Error::InvalidArtifact(
            "managed resume journal must remain outside the source".into(),
        ));
    }
    Ok(())
}

fn canonical_journal_target(path: &Path) -> Result<PathBuf, Error> {
    if path == Path::new("-") {
        return Err(Error::InvalidArtifact(
            "managed setup requires a persistent journal path".into(),
        ));
    }
    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = fs::canonicalize(parent).map_err(|error| io_error("resolve", parent, error))?;
    let name = path
        .file_name()
        .ok_or_else(|| Error::InvalidArtifact("managed journal path needs a file name".into()))?;
    Ok(parent.join(name))
}

fn create_journal<T: Serialize>(path: &Path, value: &T, source: &Path) -> Result<(), Error> {
    validate_new_journal(path, source)?;
    write_journal(path, value, true)
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
    sync_directory(parent)
}

fn load_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, Error> {
    let text = fs::read_to_string(path).map_err(|source| Error::ReadFile {
        path: path.display().to_string(),
        source,
    })?;
    Ok(serde_json::from_str(&text)?)
}

fn sync_directory(path: &Path) -> Result<(), Error> {
    File::open(path)
        .and_then(|value| value.sync_all())
        .map_err(|error| io_error("sync directory", path, error))
}

fn now_unix_ms() -> Result<u128, Error> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis())
        .map_err(|error| {
            Error::InvalidArtifact(format!("system clock is before Unix epoch: {error}"))
        })
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

fn path_text(path: &Path) -> Result<String, Error> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        Error::InvalidArtifact(format!(
            "managed source is not valid UTF-8: {:?}",
            path.display().to_string()
        ))
    })
}

#[cfg(test)]
mod tests {
    use std::os::unix::{fs::symlink, net::UnixDatagram};

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn plans_root_files_and_directories_without_mutation() {
        let source = tempdir().unwrap();
        fs::write(source.path().join("a.txt"), b"loose").unwrap();
        fs::create_dir(source.path().join("Zed")).unwrap();
        fs::write(source.path().join("Zed/readme.md"), b"readme").unwrap();
        symlink("readme.md", source.path().join("Zed/latest")).unwrap();

        let plan = build_managed_setup_plan(source.path()).unwrap();

        assert_eq!(plan.version, 1);
        assert_eq!(plan.areas, MANAGED_AREAS);
        assert_eq!(
            plan.moves
                .iter()
                .map(|value| value.destination_path.as_str())
                .collect::<Vec<_>>(),
            ["Kept/Zed", "Inbox/a.txt"]
        );
        assert!(source.path().join("a.txt").is_file());
        assert!(!source.path().join("Inbox").exists());
        assert_eq!(plan.sha256().unwrap().len(), 64);
        let artifacts = tempdir().unwrap();
        let artifact = artifacts.path().join("plan.json");
        fs::write(&artifact, serde_json::to_vec_pretty(&plan).unwrap()).unwrap();
        assert_eq!(ManagedSetupPlan::load(&artifact).unwrap(), plan);

        let mut reordered = plan;
        reordered.moves.reverse();
        assert!(reordered.validate().is_err());
    }

    #[test]
    fn rejects_existing_areas_and_special_entries() {
        let source = tempdir().unwrap();
        fs::create_dir(source.path().join("Kept")).unwrap();
        assert!(build_managed_setup_plan(source.path()).is_err());

        let source = tempdir().unwrap();
        let _socket = UnixDatagram::bind(source.path().join("socket")).unwrap();
        assert!(build_managed_setup_plan(source.path()).is_err());
    }

    #[test]
    fn directory_manifest_detects_content_and_link_changes() {
        let source = tempdir().unwrap();
        fs::write(source.path().join("file"), b"one").unwrap();
        symlink("file", source.path().join("link")).unwrap();
        let first = fingerprint_directory(source.path()).unwrap();
        fs::write(source.path().join("file"), b"two").unwrap();
        let second = fingerprint_directory(source.path()).unwrap();
        assert_ne!(first.manifest_sha256, second.manifest_sha256);
        fs::remove_file(source.path().join("link")).unwrap();
        symlink("other", source.path().join("link")).unwrap();
        let third = fingerprint_directory(source.path()).unwrap();
        assert_ne!(second.manifest_sha256, third.manifest_sha256);
    }

    #[test]
    fn adopts_only_new_root_directories_into_existing_kept_area() {
        let source = tempdir().unwrap();
        for area in MANAGED_AREAS {
            fs::create_dir(source.path().join(area)).unwrap();
        }
        fs::create_dir(source.path().join("Project")).unwrap();
        fs::write(source.path().join("Project/readme.md"), b"kept").unwrap();
        fs::write(source.path().join("loose.txt"), b"staged separately").unwrap();
        let artifacts = tempdir().unwrap();

        let plan = build_managed_directory_adoption_plan(source.path()).unwrap();
        assert_eq!(plan.moves.len(), 1);
        assert_eq!(plan.moves[0].source_path, "Project");
        assert_eq!(plan.moves[0].destination_path, "Kept/Project");

        let session = apply_managed_directory_adoption(
            &plan,
            &artifacts.path().join("directory-adoption.json"),
        )
        .unwrap();
        assert_eq!(session.state, ManagedSetupState::Completed);
        assert!(source.path().join("Kept/Project/readme.md").is_file());
        assert!(source.path().join("loose.txt").is_file());
        assert!(source.path().join("Inbox").is_dir());
        assert!(source.path().join("Library").is_dir());
    }

    #[test]
    fn directory_adoption_undo_restores_directories_without_removing_managed_areas() {
        let source = tempdir().unwrap();
        for area in MANAGED_AREAS {
            fs::create_dir(source.path().join(area)).unwrap();
        }
        fs::create_dir(source.path().join("Project")).unwrap();
        fs::write(source.path().join("Project/readme.md"), b"kept").unwrap();
        let artifacts = tempdir().unwrap();
        let plan = build_managed_directory_adoption_plan(source.path()).unwrap();
        let session = apply_managed_directory_adoption(
            &plan,
            &artifacts.path().join("directory-adoption.json"),
        )
        .unwrap();

        let undo = undo_managed_directory_adoption(
            &session,
            &artifacts.path().join("directory-adoption-undo.json"),
        )
        .unwrap();

        assert_eq!(undo.state, ManagedSetupUndoState::Completed);
        assert!(source.path().join("Project/readme.md").is_file());
        assert!(!source.path().join("Kept/Project").exists());
        for area in MANAGED_AREAS {
            assert!(source.path().join(area).is_dir());
        }
    }

    #[test]
    fn directory_adoption_rejects_source_changes_after_planning() {
        let source = tempdir().unwrap();
        for area in MANAGED_AREAS {
            fs::create_dir(source.path().join(area)).unwrap();
        }
        fs::create_dir(source.path().join("Project")).unwrap();
        let plan = build_managed_directory_adoption_plan(source.path()).unwrap();
        fs::write(source.path().join("Project/new.txt"), b"changed").unwrap();
        let artifacts = tempdir().unwrap();

        let error = apply_managed_directory_adoption(
            &plan,
            &artifacts.path().join("directory-adoption.json"),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("changed after directory adoption planning")
        );
        assert!(source.path().join("Project/new.txt").is_file());
        assert!(!source.path().join("Kept/Project").exists());
    }

    #[test]
    fn applies_and_undoes_setup_with_durable_journals() {
        let source = tempdir().unwrap();
        let journals = tempdir().unwrap();
        fs::write(source.path().join("loose.txt"), b"loose").unwrap();
        fs::create_dir(source.path().join("Projects")).unwrap();
        fs::write(source.path().join("Projects/readme.md"), b"readme").unwrap();
        let plan = build_managed_setup_plan(source.path()).unwrap();
        let apply_path = journals.path().join("setup.json");

        let setup = apply_managed_setup(&plan, &apply_path).unwrap();
        assert_eq!(setup.state, ManagedSetupState::Completed);
        assert!(source.path().join("Inbox/loose.txt").is_file());
        assert!(source.path().join("Kept/Projects/readme.md").is_file());
        assert!(source.path().join("Library").is_dir());
        assert_eq!(
            fs::metadata(&apply_path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let undo = undo_managed_setup(&setup, &journals.path().join("undo.json")).unwrap();
        assert_eq!(undo.state, ManagedSetupUndoState::Completed);
        assert!(source.path().join("loose.txt").is_file());
        assert!(source.path().join("Projects/readme.md").is_file());
        for area in MANAGED_AREAS {
            assert!(!source.path().join(area).exists());
        }
        let mut invalid = undo;
        invalid.moves[0].outcome = ManagedUndoMoveOutcome::Pending;
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn resume_reconciles_an_atomic_move_before_its_checkpoint() {
        let source = tempdir().unwrap();
        let journals = tempdir().unwrap();
        fs::write(source.path().join("loose.txt"), b"loose").unwrap();
        let plan = build_managed_setup_plan(source.path()).unwrap();
        let mut areas = Vec::new();
        for name in MANAGED_AREAS {
            let path = source.path().join(name);
            fs::create_dir(&path).unwrap();
            areas.push(ManagedAreaRecord {
                path: name.into(),
                outcome: ManagedAreaOutcome::Created {
                    identity: identity(&fs::symlink_metadata(path).unwrap()),
                },
            });
        }
        let session = ManagedSetupSession {
            version: 1,
            id: "interrupted".into(),
            plan_sha256: plan.sha256().unwrap(),
            source: plan.source.clone(),
            source_identity: plan.source_identity.clone(),
            state: ManagedSetupState::Running,
            started_unix_ms: now_unix_ms().unwrap(),
            finished_unix_ms: None,
            areas,
            moves: plan
                .moves
                .iter()
                .map(|movement| ManagedMoveRecord {
                    source_path: movement.source_path.clone(),
                    destination_path: movement.destination_path.clone(),
                    fingerprint: movement.fingerprint.clone(),
                    outcome: ManagedMoveOutcome::Moving,
                })
                .collect(),
        };
        let journal = journals.path().join("setup.json");
        create_journal(&journal, &session, source.path()).unwrap();
        fs::rename(
            source.path().join("loose.txt"),
            source.path().join("Inbox/loose.txt"),
        )
        .unwrap();

        let resumed = resume_managed_setup(&journal).unwrap();

        assert_eq!(resumed.state, ManagedSetupState::Completed);
        assert_eq!(resumed.moves[0].outcome, ManagedMoveOutcome::Moved);
    }

    #[test]
    fn resume_accepts_a_consistent_source_device_renumber() {
        let source = tempdir().unwrap();
        let journals = tempdir().unwrap();
        fs::write(source.path().join("loose.txt"), b"loose").unwrap();
        let plan = build_managed_setup_plan(source.path()).unwrap();
        let journal = journals.path().join("setup.json");
        let mut session = apply_managed_setup(&plan, &journal).unwrap();
        session.state = ManagedSetupState::Running;
        session.finished_unix_ms = None;
        session.moves[0].outcome = ManagedMoveOutcome::Moving;
        let old_device = session.source_identity.device;
        let recorded_device = old_device.wrapping_add(1);
        session.source_identity.device = recorded_device;
        for area in &mut session.areas {
            if let ManagedAreaOutcome::Created { identity } = &mut area.outcome
                && identity.device == old_device
            {
                identity.device = recorded_device;
            }
        }
        for movement in &mut session.moves {
            match &mut movement.fingerprint {
                ManagedEntryFingerprint::File { fingerprint }
                    if fingerprint.identity.device == old_device =>
                {
                    fingerprint.identity.device = recorded_device;
                }
                ManagedEntryFingerprint::Directory { fingerprint }
                    if fingerprint.identity.device == old_device =>
                {
                    fingerprint.identity.device = recorded_device;
                }
                _ => {}
            }
        }
        update_journal(&journal, &session).unwrap();

        let resumed = resume_managed_setup(&journal).unwrap();

        assert_eq!(resumed.state, ManagedSetupState::Completed);
        assert_eq!(resumed.moves[0].outcome, ManagedMoveOutcome::Moved);
    }

    #[test]
    fn undo_refuses_a_replaced_destination_area_symlink() {
        let source = tempdir().unwrap();
        let journals = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::write(source.path().join("loose.txt"), b"loose").unwrap();
        let setup = apply_managed_setup(
            &build_managed_setup_plan(source.path()).unwrap(),
            &journals.path().join("setup.json"),
        )
        .unwrap();
        fs::rename(
            source.path().join("Inbox"),
            journals.path().join("original-inbox"),
        )
        .unwrap();
        fs::write(outside.path().join("loose.txt"), b"outside").unwrap();
        symlink(outside.path(), source.path().join("Inbox")).unwrap();

        let undo = undo_managed_setup(&setup, &journals.path().join("undo.json")).unwrap();

        assert_eq!(undo.state, ManagedSetupUndoState::PartialFailure);
        assert_eq!(
            fs::read(outside.path().join("loose.txt")).unwrap(),
            b"outside"
        );
        assert!(!source.path().join("loose.txt").exists());
    }

    #[test]
    fn undo_refuses_changed_directory_and_occupied_original() {
        let source = tempdir().unwrap();
        let journals = tempdir().unwrap();
        fs::create_dir(source.path().join("Projects")).unwrap();
        fs::write(source.path().join("Projects/readme.md"), b"readme").unwrap();
        fs::write(source.path().join("loose.txt"), b"loose").unwrap();
        let setup = apply_managed_setup(
            &build_managed_setup_plan(source.path()).unwrap(),
            &journals.path().join("setup.json"),
        )
        .unwrap();
        fs::write(source.path().join("Kept/Projects/new.txt"), b"new").unwrap();
        fs::write(source.path().join("loose.txt"), b"occupied").unwrap();

        let undo = undo_managed_setup(&setup, &journals.path().join("undo.json")).unwrap();

        assert_eq!(undo.state, ManagedSetupUndoState::PartialFailure);
        assert!(source.path().join("Kept/Projects/new.txt").is_file());
        assert_eq!(
            fs::read(source.path().join("loose.txt")).unwrap(),
            b"occupied"
        );
        assert!(source.path().join("Inbox/loose.txt").is_file());
    }

    #[test]
    fn undo_accepts_a_consistent_source_device_renumber() {
        let source = tempdir().unwrap();
        let journals = tempdir().unwrap();
        fs::write(source.path().join("loose.txt"), b"loose").unwrap();
        fs::create_dir(source.path().join("Projects")).unwrap();
        let mut setup = apply_managed_setup(
            &build_managed_setup_plan(source.path()).unwrap(),
            &journals.path().join("setup.json"),
        )
        .unwrap();
        let recorded_device = setup.source_identity.device;
        let replacement = recorded_device.wrapping_add(1);
        setup.source_identity.device = replacement;
        for area in &mut setup.areas {
            if let ManagedAreaOutcome::Created { identity } = &mut area.outcome
                && identity.device == recorded_device
            {
                identity.device = replacement;
            }
        }
        for movement in &mut setup.moves {
            match &mut movement.fingerprint {
                ManagedEntryFingerprint::File { fingerprint }
                    if fingerprint.identity.device == recorded_device =>
                {
                    fingerprint.identity.device = replacement;
                }
                ManagedEntryFingerprint::Directory { fingerprint }
                    if fingerprint.identity.device == recorded_device =>
                {
                    fingerprint.identity.device = replacement;
                }
                _ => {}
            }
        }

        let undo = undo_managed_setup(&setup, &journals.path().join("undo.json")).unwrap();

        assert_eq!(undo.state, ManagedSetupUndoState::Completed);
        assert!(source.path().join("loose.txt").is_file());
        assert!(source.path().join("Projects").is_dir());
    }

    #[test]
    fn terminal_setup_cannot_resume_and_journal_must_be_outside_source() {
        let source = tempdir().unwrap();
        let journals = tempdir().unwrap();
        fs::write(source.path().join("loose.txt"), b"loose").unwrap();
        let plan = build_managed_setup_plan(source.path()).unwrap();
        assert!(preflight_managed_setup(&plan, &source.path().join("journal.json")).is_err());
        let path = journals.path().join("setup.json");
        let setup = apply_managed_setup(&plan, &path).unwrap();
        assert!(preflight_managed_resume(&setup, &path).is_err());
    }
}
