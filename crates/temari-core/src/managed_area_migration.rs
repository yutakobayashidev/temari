use std::{
    collections::HashSet,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use crate::{
    DirectoryFingerprint, Error, FolderSet, FsIdentity, SourceLock,
    artifact::normalize_relative_path,
    filesystem::{canonical_directory, io_error, path_exists},
    fingerprint_directory,
};

pub const LEGACY_MANAGED_AREAS: [&str; 3] = ["Kept", "Inbox", "Library"];
pub const CURRENT_MANAGED_AREAS: [&str; 3] = ["Manual Library", "Recents", "AI Library"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagedAreaLayout {
    Legacy,
    Current,
}

impl ManagedAreaLayout {
    pub fn areas(self) -> [&'static str; 3] {
        match self {
            Self::Legacy => LEGACY_MANAGED_AREAS,
            Self::Current => CURRENT_MANAGED_AREAS,
        }
    }

    pub fn manual(self) -> &'static str {
        self.areas()[0]
    }

    pub fn recents(self) -> &'static str {
        self.areas()[1]
    }

    pub fn library(self) -> &'static str {
        self.areas()[2]
    }
}

pub fn detect_managed_area_layout(source: &Path) -> Result<ManagedAreaLayout, Error> {
    let legacy = LEGACY_MANAGED_AREAS
        .iter()
        .filter(|area| source.join(area).is_dir())
        .count();
    let current = CURRENT_MANAGED_AREAS
        .iter()
        .filter(|area| source.join(area).is_dir())
        .count();
    match (legacy, current) {
        (0, 0) => Ok(ManagedAreaLayout::Current),
        (1..=3, 0) => Ok(ManagedAreaLayout::Legacy),
        (0, 1..=3) => Ok(ManagedAreaLayout::Current),
        _ => Err(Error::InvalidState(
            "managed source does not contain exactly one complete area layout".into(),
        )),
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedAreaMigrationMove {
    pub source_path: String,
    pub destination_path: String,
    pub fingerprint: DirectoryFingerprint,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedAreaMigrationPlan {
    pub version: u32,
    pub workspace_id: String,
    pub source: String,
    pub source_identity: FsIdentity,
    pub before_folder_set_path: String,
    pub before_folder_set_sha256: String,
    pub before_folders: FolderSet,
    pub after_folders: FolderSet,
    pub moves: Vec<ManagedAreaMigrationMove>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedAreaMigrationState {
    Running,
    Completed,
    PartialFailure,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ManagedAreaMigrationOutcome {
    Pending,
    Moving,
    Moved,
    Conflict { message: String },
    Failed { message: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedAreaMigrationRecord {
    pub source_path: String,
    pub destination_path: String,
    pub fingerprint: DirectoryFingerprint,
    pub outcome: ManagedAreaMigrationOutcome,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedAreaMigrationSession {
    pub version: u32,
    pub id: String,
    pub plan_sha256: String,
    pub workspace_id: String,
    pub source: String,
    pub source_identity: FsIdentity,
    pub state: ManagedAreaMigrationState,
    pub started_unix_ms: u128,
    pub finished_unix_ms: Option<u128>,
    pub moves: Vec<ManagedAreaMigrationRecord>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedAreaMigrationUndoSession {
    pub version: u32,
    pub migration_session_id: String,
    pub workspace_id: String,
    pub source: String,
    pub source_identity: FsIdentity,
    pub state: ManagedAreaMigrationState,
    pub started_unix_ms: u128,
    pub finished_unix_ms: Option<u128>,
    pub moves: Vec<ManagedAreaMigrationRecord>,
}

impl ManagedAreaMigrationPlan {
    pub fn build(
        workspace_id: &str,
        source: &Path,
        before_folder_set_path: &Path,
        before_folders: &FolderSet,
    ) -> Result<Self, Error> {
        let (source, source_identity) = canonical_directory(source)?;
        if workspace_id.trim().is_empty() {
            return Err(Error::InvalidArtifact(
                "managed area migration workspace ID must not be empty".into(),
            ));
        }
        before_folders.validate()?;
        if before_folders.source != path_text(&source)? {
            return Err(Error::InvalidArtifact(
                "managed area migration FolderSet belongs to another source".into(),
            ));
        }
        let before_folder_set_path = canonical_file(before_folder_set_path)?;
        if before_folder_set_path.starts_with(&source) {
            return Err(Error::InvalidArtifact(
                "managed area migration FolderSet must be outside the source".into(),
            ));
        }
        let mut moves = Vec::with_capacity(3);
        for (legacy, current) in LEGACY_MANAGED_AREAS.into_iter().zip(CURRENT_MANAGED_AREAS) {
            let legacy_path = source.join(legacy);
            if path_exists(&source.join(current))? {
                return Err(Error::InvalidArtifact(format!(
                    "managed area migration destination is occupied: {current:?}"
                )));
            }
            moves.push(ManagedAreaMigrationMove {
                source_path: legacy.into(),
                destination_path: current.into(),
                fingerprint: fingerprint_directory(&legacy_path)?,
            });
        }
        let after_folders = rewrite_folder_set(before_folders)?;
        let plan = Self {
            version: 1,
            workspace_id: workspace_id.into(),
            source: path_text(&source)?,
            source_identity,
            before_folder_set_path: path_text(&before_folder_set_path)?,
            before_folder_set_sha256: before_folders.sha256()?,
            before_folders: before_folders.clone(),
            after_folders,
            moves,
        };
        plan.validate()?;
        Ok(plan)
    }

    pub fn load(path: &Path) -> Result<Self, Error> {
        let plan: Self = load_json(path)?;
        plan.validate()?;
        Ok(plan)
    }

    pub fn validate(&self) -> Result<(), Error> {
        if self.version != 1 || self.workspace_id.trim().is_empty() {
            return Err(Error::InvalidArtifact(
                "invalid managed area migration Plan header".into(),
            ));
        }
        let source = Path::new(&self.source);
        if !source.is_absolute() || !Path::new(&self.before_folder_set_path).is_absolute() {
            return Err(Error::InvalidArtifact(
                "managed area migration paths must be absolute".into(),
            ));
        }
        if Path::new(&self.before_folder_set_path).starts_with(source) {
            return Err(Error::InvalidArtifact(
                "managed area migration FolderSet must be outside the source".into(),
            ));
        }
        self.before_folders.validate()?;
        self.after_folders.validate()?;
        if self.before_folders.source != self.source || self.after_folders.source != self.source {
            return Err(Error::InvalidArtifact(
                "managed area migration FolderSets belong to another source".into(),
            ));
        }
        if self.before_folders.sha256()? != self.before_folder_set_sha256 {
            return Err(Error::InvalidArtifact(
                "managed area migration before FolderSet digest does not match".into(),
            ));
        }
        if rewrite_folder_set(&self.before_folders)? != self.after_folders {
            return Err(Error::InvalidArtifact(
                "managed area migration after FolderSet is not the exact path rewrite".into(),
            ));
        }
        if self.moves.len() != 3 {
            return Err(Error::InvalidArtifact(
                "managed area migration must contain exactly three moves".into(),
            ));
        }
        let expected = LEGACY_MANAGED_AREAS.into_iter().zip(CURRENT_MANAGED_AREAS);
        for (movement, (legacy, current)) in self.moves.iter().zip(expected) {
            normalize_relative_path(&movement.source_path)?;
            normalize_relative_path(&movement.destination_path)?;
            validate_digest(&movement.fingerprint.manifest_sha256)?;
            if movement.source_path != legacy || movement.destination_path != current {
                return Err(Error::InvalidArtifact(
                    "managed area migration contains an unexpected rename".into(),
                ));
            }
        }
        Ok(())
    }

    pub fn sha256(&self) -> Result<String, Error> {
        self.validate()?;
        Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(self)?)))
    }
}

impl ManagedAreaMigrationSession {
    pub fn load(path: &Path) -> Result<Self, Error> {
        let session: Self = load_json(path)?;
        session.validate()?;
        Ok(session)
    }

    pub fn validate(&self) -> Result<(), Error> {
        validate_session(&SessionValidation {
            version: self.version,
            id: &self.id,
            workspace_id: &self.workspace_id,
            source: &self.source,
            state: &self.state,
            finished_unix_ms: self.finished_unix_ms,
            moves: &self.moves,
            undo: false,
        })?;
        validate_digest(&self.plan_sha256)
    }
}

impl ManagedAreaMigrationUndoSession {
    pub fn load(path: &Path) -> Result<Self, Error> {
        let session: Self = load_json(path)?;
        session.validate()?;
        Ok(session)
    }

    pub fn validate(&self) -> Result<(), Error> {
        validate_session(&SessionValidation {
            version: self.version,
            id: &self.migration_session_id,
            workspace_id: &self.workspace_id,
            source: &self.source,
            state: &self.state,
            finished_unix_ms: self.finished_unix_ms,
            moves: &self.moves,
            undo: true,
        })
    }
}

pub fn apply_managed_area_migration(
    plan: &ManagedAreaMigrationPlan,
    journal_path: &Path,
) -> Result<ManagedAreaMigrationSession, Error> {
    let lock = SourceLock::acquire(Path::new(&plan.source))?;
    apply_managed_area_migration_with_lock(plan, journal_path, &lock)
}

pub fn apply_managed_area_migration_with_lock(
    plan: &ManagedAreaMigrationPlan,
    journal_path: &Path,
    lock: &SourceLock,
) -> Result<ManagedAreaMigrationSession, Error> {
    plan.validate()?;
    lock.validate_source(&plan.source, &plan.source_identity)?;
    preflight_plan(plan, journal_path)?;
    let mut session = ManagedAreaMigrationSession {
        version: 1,
        id: format!("{}-{}", now_unix_ms()?, std::process::id()),
        plan_sha256: plan.sha256()?,
        workspace_id: plan.workspace_id.clone(),
        source: plan.source.clone(),
        source_identity: plan.source_identity.clone(),
        state: ManagedAreaMigrationState::Running,
        started_unix_ms: now_unix_ms()?,
        finished_unix_ms: None,
        moves: plan
            .moves
            .iter()
            .map(|movement| ManagedAreaMigrationRecord {
                source_path: movement.source_path.clone(),
                destination_path: movement.destination_path.clone(),
                fingerprint: movement.fingerprint.clone(),
                outcome: ManagedAreaMigrationOutcome::Pending,
            })
            .collect(),
    };
    create_journal(journal_path, &session, Path::new(&plan.source))?;
    continue_migration(&mut session, journal_path)?;
    Ok(session)
}

pub fn resume_managed_area_migration(
    journal_path: &Path,
) -> Result<ManagedAreaMigrationSession, Error> {
    let mut session = ManagedAreaMigrationSession::load(journal_path)?;
    if session.state != ManagedAreaMigrationState::Running {
        return Err(Error::InvalidArtifact(
            "only a running managed area migration can be resumed".into(),
        ));
    }
    let lock = SourceLock::acquire(Path::new(&session.source))?;
    lock.validate_recovery_source(&session.source, &session.source_identity)?;
    validate_existing_journal(journal_path, Path::new(&session.source))?;
    reconcile_records(&mut session.moves, Path::new(&session.source))?;
    replace_json(journal_path, &session)?;
    continue_migration(&mut session, journal_path)?;
    Ok(session)
}

pub fn undo_managed_area_migration(
    migration: &ManagedAreaMigrationSession,
    journal_path: &Path,
) -> Result<ManagedAreaMigrationUndoSession, Error> {
    migration.validate()?;
    if migration.state != ManagedAreaMigrationState::Completed {
        return Err(Error::InvalidArtifact(
            "only a completed managed area migration can be undone".into(),
        ));
    }
    let lock = SourceLock::acquire(Path::new(&migration.source))?;
    lock.validate_recovery_source(&migration.source, &migration.source_identity)?;
    validate_new_journal(journal_path, Path::new(&migration.source))?;
    let mut undo = ManagedAreaMigrationUndoSession {
        version: 1,
        migration_session_id: migration.id.clone(),
        workspace_id: migration.workspace_id.clone(),
        source: migration.source.clone(),
        source_identity: migration.source_identity.clone(),
        state: ManagedAreaMigrationState::Running,
        started_unix_ms: now_unix_ms()?,
        finished_unix_ms: None,
        moves: migration
            .moves
            .iter()
            .rev()
            .map(|movement| ManagedAreaMigrationRecord {
                source_path: movement.destination_path.clone(),
                destination_path: movement.source_path.clone(),
                fingerprint: movement.fingerprint.clone(),
                outcome: ManagedAreaMigrationOutcome::Pending,
            })
            .collect(),
    };
    create_journal(journal_path, &undo, Path::new(&migration.source))?;
    continue_undo(&mut undo, journal_path)?;
    Ok(undo)
}

pub fn resume_managed_area_migration_undo(
    journal_path: &Path,
) -> Result<ManagedAreaMigrationUndoSession, Error> {
    let mut undo = ManagedAreaMigrationUndoSession::load(journal_path)?;
    if undo.state != ManagedAreaMigrationState::Running {
        return Err(Error::InvalidArtifact(
            "only a running managed area migration Undo can be resumed".into(),
        ));
    }
    let lock = SourceLock::acquire(Path::new(&undo.source))?;
    lock.validate_recovery_source(&undo.source, &undo.source_identity)?;
    validate_existing_journal(journal_path, Path::new(&undo.source))?;
    reconcile_undo_records(&mut undo, journal_path)?;
    continue_undo(&mut undo, journal_path)?;
    Ok(undo)
}

fn rewrite_folder_set(before: &FolderSet) -> Result<FolderSet, Error> {
    let mut after = before.clone();
    for folder in &mut after.folders {
        if folder.path == "Library" {
            folder.path = "AI Library".into();
        } else if let Some(suffix) = folder.path.strip_prefix("Library/") {
            folder.path = format!("AI Library/{suffix}");
        } else {
            return Err(Error::InvalidArtifact(format!(
                "legacy managed FolderSet contains a destination outside Library: {:?}",
                folder.path
            )));
        }
    }
    after.validate()?;
    Ok(after)
}

fn preflight_plan(plan: &ManagedAreaMigrationPlan, journal_path: &Path) -> Result<(), Error> {
    let (source, source_identity) = canonical_directory(Path::new(&plan.source))?;
    if source_identity != plan.source_identity {
        return Err(Error::InvalidArtifact(
            "managed area migration source identity changed".into(),
        ));
    }
    for movement in &plan.moves {
        if fingerprint_directory(&source.join(&movement.source_path))? != movement.fingerprint {
            return Err(Error::InvalidArtifact(format!(
                "managed area changed after migration planning: {:?}",
                movement.source_path
            )));
        }
        if path_exists(&source.join(&movement.destination_path))? {
            return Err(Error::InvalidArtifact(format!(
                "managed area migration destination is occupied: {:?}",
                movement.destination_path
            )));
        }
    }
    validate_new_journal(journal_path, &source)
}

fn continue_migration(
    session: &mut ManagedAreaMigrationSession,
    journal_path: &Path,
) -> Result<(), Error> {
    let root = Path::new(&session.source);
    for index in 0..session.moves.len() {
        if session.moves[index].outcome == ManagedAreaMigrationOutcome::Moved {
            continue;
        }
        if session.moves[index].outcome != ManagedAreaMigrationOutcome::Pending {
            return Err(Error::InvalidArtifact(
                "managed area migration record is not resumable".into(),
            ));
        }
        session.moves[index].outcome = ManagedAreaMigrationOutcome::Moving;
        replace_json(journal_path, session)?;
        let from = root.join(&session.moves[index].source_path);
        let to = root.join(&session.moves[index].destination_path);
        let result = verify_and_rename(&from, &to, &session.moves[index].fingerprint);
        match result {
            Ok(()) => session.moves[index].outcome = ManagedAreaMigrationOutcome::Moved,
            Err(error) => {
                session.moves[index].outcome = ManagedAreaMigrationOutcome::Conflict {
                    message: error.to_string(),
                };
                for movement in &mut session.moves[index + 1..] {
                    movement.outcome = ManagedAreaMigrationOutcome::Failed {
                        message: "not attempted after an earlier conflict".into(),
                    };
                }
                session.state = ManagedAreaMigrationState::PartialFailure;
                session.finished_unix_ms = Some(now_unix_ms()?);
                replace_json(journal_path, session)?;
                return Ok(());
            }
        }
        replace_json(journal_path, session)?;
    }
    session.state = ManagedAreaMigrationState::Completed;
    session.finished_unix_ms = Some(now_unix_ms()?);
    replace_json(journal_path, session)
}

fn continue_undo(
    undo: &mut ManagedAreaMigrationUndoSession,
    journal_path: &Path,
) -> Result<(), Error> {
    let root = Path::new(&undo.source);
    for index in 0..undo.moves.len() {
        if undo.moves[index].outcome == ManagedAreaMigrationOutcome::Moved {
            continue;
        }
        if undo.moves[index].outcome != ManagedAreaMigrationOutcome::Pending {
            return Err(Error::InvalidArtifact(
                "managed area migration Undo record is not resumable".into(),
            ));
        }
        undo.moves[index].outcome = ManagedAreaMigrationOutcome::Moving;
        replace_json(journal_path, undo)?;
        let from = root.join(&undo.moves[index].source_path);
        let to = root.join(&undo.moves[index].destination_path);
        match verify_and_rename(&from, &to, &undo.moves[index].fingerprint) {
            Ok(()) => undo.moves[index].outcome = ManagedAreaMigrationOutcome::Moved,
            Err(error) => {
                undo.moves[index].outcome = ManagedAreaMigrationOutcome::Conflict {
                    message: error.to_string(),
                };
                for movement in &mut undo.moves[index + 1..] {
                    movement.outcome = ManagedAreaMigrationOutcome::Failed {
                        message: "not attempted after an earlier conflict".into(),
                    };
                }
                undo.state = ManagedAreaMigrationState::PartialFailure;
                undo.finished_unix_ms = Some(now_unix_ms()?);
                replace_json(journal_path, undo)?;
                return Ok(());
            }
        }
        replace_json(journal_path, undo)?;
    }
    undo.state = ManagedAreaMigrationState::Completed;
    undo.finished_unix_ms = Some(now_unix_ms()?);
    replace_json(journal_path, undo)
}

fn verify_and_rename(from: &Path, to: &Path, expected: &DirectoryFingerprint) -> Result<(), Error> {
    if path_exists(to)? {
        return Err(Error::InvalidArtifact(format!(
            "managed area migration destination is occupied: {:?}",
            to.display().to_string()
        )));
    }
    if fingerprint_directory(from)? != *expected {
        return Err(Error::InvalidArtifact(format!(
            "managed area changed before rename: {:?}",
            from.display().to_string()
        )));
    }
    fs::rename(from, to).map_err(|error| io_error("rename managed area", from, error))?;
    sync_directory(
        from.parent()
            .ok_or_else(|| Error::InvalidArtifact("managed area has no parent".into()))?,
    )
}

fn reconcile_records(records: &mut [ManagedAreaMigrationRecord], root: &Path) -> Result<(), Error> {
    for record in records {
        if record.outcome != ManagedAreaMigrationOutcome::Moving {
            continue;
        }
        let source_exists = path_exists(&root.join(&record.source_path))?;
        let destination = root.join(&record.destination_path);
        let destination_matches = path_exists(&destination)?
            && fingerprint_directory(&destination)? == record.fingerprint;
        record.outcome = match (source_exists, destination_matches) {
            (false, true) => ManagedAreaMigrationOutcome::Moved,
            (true, false) => ManagedAreaMigrationOutcome::Pending,
            _ => {
                return Err(Error::InvalidArtifact(
                    "managed area migration cannot reconcile an interrupted rename".into(),
                ));
            }
        };
    }
    Ok(())
}

fn reconcile_undo_records(
    undo: &mut ManagedAreaMigrationUndoSession,
    journal_path: &Path,
) -> Result<(), Error> {
    let root = PathBuf::from(&undo.source);
    for record in &mut undo.moves {
        if record.outcome != ManagedAreaMigrationOutcome::Moving {
            continue;
        }
        let source_exists = path_exists(&root.join(&record.source_path))?;
        let destination = root.join(&record.destination_path);
        let destination_matches = path_exists(&destination)?
            && fingerprint_directory(&destination)? == record.fingerprint;
        record.outcome = match (source_exists, destination_matches) {
            (false, true) => ManagedAreaMigrationOutcome::Moved,
            (true, false) => ManagedAreaMigrationOutcome::Pending,
            _ => {
                return Err(Error::InvalidArtifact(
                    "managed area migration Undo cannot reconcile an interrupted rename".into(),
                ));
            }
        };
    }
    replace_json(journal_path, undo)
}

struct SessionValidation<'a> {
    version: u32,
    id: &'a str,
    workspace_id: &'a str,
    source: &'a str,
    state: &'a ManagedAreaMigrationState,
    finished_unix_ms: Option<u128>,
    moves: &'a [ManagedAreaMigrationRecord],
    undo: bool,
}

fn validate_session(session: &SessionValidation<'_>) -> Result<(), Error> {
    if session.version != 1
        || session.id.trim().is_empty()
        || session.workspace_id.trim().is_empty()
    {
        return Err(Error::InvalidArtifact(
            "invalid managed area migration Session header".into(),
        ));
    }
    if !Path::new(session.source).is_absolute() || session.moves.len() != 3 {
        return Err(Error::InvalidArtifact(
            "invalid managed area migration Session paths".into(),
        ));
    }
    let expected = if session.undo {
        CURRENT_MANAGED_AREAS
            .into_iter()
            .rev()
            .zip(LEGACY_MANAGED_AREAS.into_iter().rev())
            .collect::<Vec<_>>()
    } else {
        LEGACY_MANAGED_AREAS
            .into_iter()
            .zip(CURRENT_MANAGED_AREAS)
            .collect::<Vec<_>>()
    };
    let mut paths = HashSet::new();
    for (movement, (expected_source, expected_destination)) in session.moves.iter().zip(expected) {
        normalize_relative_path(&movement.source_path)?;
        normalize_relative_path(&movement.destination_path)?;
        validate_digest(&movement.fingerprint.manifest_sha256)?;
        if movement.source_path != expected_source
            || movement.destination_path != expected_destination
        {
            return Err(Error::InvalidArtifact(
                "managed area migration Session contains an unexpected rename".into(),
            ));
        }
        if !paths.insert((&movement.source_path, &movement.destination_path)) {
            return Err(Error::InvalidArtifact(
                "managed area migration Session contains duplicate moves".into(),
            ));
        }
    }
    match session.state {
        ManagedAreaMigrationState::Running if session.finished_unix_ms.is_some() => Err(
            Error::InvalidArtifact("running managed area migration has a finish time".into()),
        ),
        ManagedAreaMigrationState::Completed | ManagedAreaMigrationState::PartialFailure
            if session.finished_unix_ms.is_none() =>
        {
            Err(Error::InvalidArtifact(
                "terminal managed area migration has no finish time".into(),
            ))
        }
        ManagedAreaMigrationState::Completed
            if session
                .moves
                .iter()
                .any(|movement| movement.outcome != ManagedAreaMigrationOutcome::Moved) =>
        {
            Err(Error::InvalidArtifact(
                "completed managed area migration contains unfinished moves".into(),
            ))
        }
        ManagedAreaMigrationState::PartialFailure
            if session.moves.iter().any(|movement| {
                matches!(
                    movement.outcome,
                    ManagedAreaMigrationOutcome::Pending | ManagedAreaMigrationOutcome::Moving
                )
            }) || !session.moves.iter().any(|movement| {
                matches!(
                    movement.outcome,
                    ManagedAreaMigrationOutcome::Conflict { .. }
                        | ManagedAreaMigrationOutcome::Failed { .. }
                )
            }) =>
        {
            Err(Error::InvalidArtifact(
                "partial managed area migration has invalid outcomes".into(),
            ))
        }
        ManagedAreaMigrationState::Running
            if session.moves.iter().any(|movement| {
                matches!(
                    movement.outcome,
                    ManagedAreaMigrationOutcome::Conflict { .. }
                        | ManagedAreaMigrationOutcome::Failed { .. }
                )
            }) =>
        {
            Err(Error::InvalidArtifact(
                "running managed area migration contains a terminal outcome".into(),
            ))
        }
        _ => Ok(()),
    }
}

fn canonical_file(path: &Path) -> Result<PathBuf, Error> {
    let resolved = fs::canonicalize(path).map_err(|error| io_error("resolve", path, error))?;
    let metadata =
        fs::symlink_metadata(&resolved).map_err(|error| io_error("inspect", &resolved, error))?;
    if !metadata.is_file() {
        return Err(Error::InvalidArtifact(format!(
            "managed area migration FolderSet is not a regular file: {:?}",
            resolved.display().to_string()
        )));
    }
    Ok(resolved)
}

fn validate_digest(value: &str) -> Result<(), Error> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(Error::InvalidArtifact(
            "managed area migration digest must be lowercase SHA-256".into(),
        ));
    }
    Ok(())
}

fn validate_new_journal(path: &Path, source: &Path) -> Result<(), Error> {
    if !path.is_absolute() || path.starts_with(source) || path_exists(path)? {
        return Err(Error::InvalidArtifact(
            "managed area migration journal must be a new absolute path outside the source".into(),
        ));
    }
    let parent = path.parent().ok_or_else(|| {
        Error::InvalidArtifact("managed area migration journal has no parent".into())
    })?;
    canonical_directory(parent)?;
    Ok(())
}

fn validate_existing_journal(path: &Path, source: &Path) -> Result<(), Error> {
    if !path.is_absolute() || path.starts_with(source) || !path_exists(path)? {
        return Err(Error::InvalidArtifact(
            "managed area migration recovery journal is invalid".into(),
        ));
    }
    Ok(())
}

fn create_journal<T: Serialize>(path: &Path, value: &T, source: &Path) -> Result<(), Error> {
    validate_new_journal(path, source)?;
    let parent = path.parent().expect("validated journal parent");
    let mut temporary = NamedTempFile::new_in(parent)
        .map_err(|error| io_error("create migration journal", path, error))?;
    serde_json::to_writer_pretty(&mut temporary, value)?;
    temporary
        .write_all(b"\n")
        .map_err(|error| io_error("write migration journal", path, error))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| io_error("sync migration journal", path, error))?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| io_error("persist migration journal", path, error.error))?;
    sync_directory(parent)
}

fn replace_json<T: Serialize>(path: &Path, value: &T) -> Result<(), Error> {
    let parent = path.parent().ok_or_else(|| {
        Error::InvalidArtifact("managed area migration journal has no parent".into())
    })?;
    let mut temporary = NamedTempFile::new_in(parent)
        .map_err(|error| io_error("create migration journal update", path, error))?;
    serde_json::to_writer_pretty(&mut temporary, value)?;
    temporary
        .write_all(b"\n")
        .map_err(|error| io_error("write migration journal update", path, error))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| io_error("sync migration journal update", path, error))?;
    temporary
        .persist(path)
        .map_err(|error| io_error("replace migration journal", path, error.error))?;
    sync_directory(parent)
}

fn load_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, Error> {
    serde_json::from_reader(File::open(path).map_err(|source| Error::ReadFile {
        path: path.display().to_string(),
        source,
    })?)
    .map_err(Error::from)
}

fn sync_directory(path: &Path) -> Result<(), Error> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| io_error("sync directory", path, error))
}

fn path_text(path: &Path) -> Result<String, Error> {
    path.to_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| Error::InvalidArtifact("managed area path must be valid UTF-8".into()))
}

fn now_unix_ms() -> Result<u128, Error> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .map_err(|_| Error::InvalidArtifact("system clock is before the Unix epoch".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FolderProposal, Proposal, ScanScope, library_folder_set};

    fn fixture() -> (tempfile::TempDir, tempfile::TempDir, FolderSet, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        for area in LEGACY_MANAGED_AREAS {
            fs::create_dir(root.path().join(area)).unwrap();
        }
        fs::write(root.path().join("Kept/manual.txt"), b"manual").unwrap();
        fs::write(root.path().join("Inbox/recent.txt"), b"recent").unwrap();
        fs::create_dir(root.path().join("Library/Documents")).unwrap();
        fs::write(root.path().join("Library/Documents/report.txt"), b"report").unwrap();
        let artifacts = tempfile::tempdir().unwrap();
        let raw = Proposal {
            version: 2,
            source: root.path().to_str().unwrap().into(),
            scope: ScanScope::default(),
            files_considered: 1,
            folders: vec![FolderProposal {
                path: "Documents".into(),
                description: "Documents".into(),
            }],
        };
        let mut folders = library_folder_set(&raw.approve().unwrap()).unwrap();
        folders.scope = ScanScope::new(vec!["Inbox".into()]).unwrap();
        for folder in &mut folders.folders {
            folder.path = folder.path.replacen("AI Library/", "Library/", 1);
        }
        folders.validate().unwrap();
        let folders_path = artifacts.path().join("folders.json");
        fs::write(&folders_path, serde_json::to_vec_pretty(&folders).unwrap()).unwrap();
        (root, artifacts, folders, folders_path)
    }

    #[test]
    fn migration_apply_and_undo_preserve_contents_and_identity() {
        let (root, artifacts, folders, folders_path) = fixture();
        let plan =
            ManagedAreaMigrationPlan::build("workspace-1", root.path(), &folders_path, &folders)
                .unwrap();
        let identities = plan
            .moves
            .iter()
            .map(|movement| movement.fingerprint.identity.clone())
            .collect::<Vec<_>>();
        let apply_path = artifacts.path().join("apply.json");
        let applied = apply_managed_area_migration(&plan, &apply_path).unwrap();
        assert_eq!(applied.state, ManagedAreaMigrationState::Completed);
        assert!(root.path().join("Manual Library/manual.txt").is_file());
        assert!(root.path().join("Recents/recent.txt").is_file());
        assert!(
            root.path()
                .join("AI Library/Documents/report.txt")
                .is_file()
        );
        for (area, expected) in CURRENT_MANAGED_AREAS.into_iter().zip(identities) {
            assert_eq!(
                fingerprint_directory(&root.path().join(area))
                    .unwrap()
                    .identity,
                expected
            );
        }
        let undo_path = artifacts.path().join("undo.json");
        let undone = undo_managed_area_migration(&applied, &undo_path).unwrap();
        assert_eq!(undone.state, ManagedAreaMigrationState::Completed);
        assert!(root.path().join("Kept/manual.txt").is_file());
        assert!(root.path().join("Inbox/recent.txt").is_file());
        assert!(root.path().join("Library/Documents/report.txt").is_file());
    }

    #[test]
    fn migration_rejects_stale_area_and_occupied_destination() {
        let (root, artifacts, folders, folders_path) = fixture();
        let plan =
            ManagedAreaMigrationPlan::build("workspace-1", root.path(), &folders_path, &folders)
                .unwrap();
        fs::write(root.path().join("Inbox/late.txt"), b"late").unwrap();
        let error =
            apply_managed_area_migration(&plan, &artifacts.path().join("apply.json")).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("changed after migration planning")
        );

        let (root, artifacts, folders, folders_path) = fixture();
        fs::create_dir(root.path().join("Recents")).unwrap();
        let error =
            ManagedAreaMigrationPlan::build("workspace-1", root.path(), &folders_path, &folders)
                .unwrap_err();
        assert!(error.to_string().contains("destination is occupied"));
        drop(artifacts);
    }

    #[test]
    fn migration_plan_rewrites_only_library_prefix_and_preserves_ids() {
        let (root, _artifacts, folders, folders_path) = fixture();
        let plan =
            ManagedAreaMigrationPlan::build("workspace-1", root.path(), &folders_path, &folders)
                .unwrap();
        assert_eq!(
            plan.after_folders.folders[0].id,
            plan.before_folders.folders[0].id
        );
        assert_eq!(plan.after_folders.folders[0].path, "AI Library/Documents");
        let mut tampered = plan.clone();
        tampered.after_folders.folders[0].path = "AI Library/Tampered".into();
        assert!(tampered.validate().is_err());
    }

    #[test]
    fn migration_and_undo_resume_reconcile_a_completed_rename() {
        let (root, artifacts, folders, folders_path) = fixture();
        let plan =
            ManagedAreaMigrationPlan::build("workspace-1", root.path(), &folders_path, &folders)
                .unwrap();
        let apply_path = artifacts.path().join("apply.json");
        let mut apply = ManagedAreaMigrationSession {
            version: 1,
            id: "migration-1".into(),
            plan_sha256: plan.sha256().unwrap(),
            workspace_id: plan.workspace_id.clone(),
            source: plan.source.clone(),
            source_identity: plan.source_identity.clone(),
            state: ManagedAreaMigrationState::Running,
            started_unix_ms: 1,
            finished_unix_ms: None,
            moves: plan
                .moves
                .iter()
                .map(|movement| ManagedAreaMigrationRecord {
                    source_path: movement.source_path.clone(),
                    destination_path: movement.destination_path.clone(),
                    fingerprint: movement.fingerprint.clone(),
                    outcome: ManagedAreaMigrationOutcome::Pending,
                })
                .collect(),
        };
        apply.moves[0].outcome = ManagedAreaMigrationOutcome::Moving;
        fs::rename(root.path().join("Kept"), root.path().join("Manual Library")).unwrap();
        fs::write(&apply_path, serde_json::to_vec_pretty(&apply).unwrap()).unwrap();
        let applied = resume_managed_area_migration(&apply_path).unwrap();
        assert_eq!(applied.state, ManagedAreaMigrationState::Completed);
        assert!(
            root.path()
                .join("AI Library/Documents/report.txt")
                .is_file()
        );

        let undo_path = artifacts.path().join("undo.json");
        let mut undo = ManagedAreaMigrationUndoSession {
            version: 1,
            migration_session_id: applied.id.clone(),
            workspace_id: applied.workspace_id.clone(),
            source: applied.source.clone(),
            source_identity: applied.source_identity.clone(),
            state: ManagedAreaMigrationState::Running,
            started_unix_ms: 2,
            finished_unix_ms: None,
            moves: applied
                .moves
                .iter()
                .rev()
                .map(|movement| ManagedAreaMigrationRecord {
                    source_path: movement.destination_path.clone(),
                    destination_path: movement.source_path.clone(),
                    fingerprint: movement.fingerprint.clone(),
                    outcome: ManagedAreaMigrationOutcome::Pending,
                })
                .collect(),
        };
        undo.moves[0].outcome = ManagedAreaMigrationOutcome::Moving;
        fs::rename(root.path().join("AI Library"), root.path().join("Library")).unwrap();
        fs::write(&undo_path, serde_json::to_vec_pretty(&undo).unwrap()).unwrap();
        let undone = resume_managed_area_migration_undo(&undo_path).unwrap();
        assert_eq!(undone.state, ManagedAreaMigrationState::Completed);
        assert!(root.path().join("Library/Documents/report.txt").is_file());
    }
}
