use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{self, IsTerminal},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use clap::{Subcommand, ValueEnum};
use directories::ProjectDirs;
use serde::Serialize;
use temari_core::{
    ApplySession, Config, FolderSet, InboxReconcileSummary, InboxState, LocalRule,
    ManagedLibraryEdit, ManagedLibraryEditPlan, ManagedReprocessArea, ManagedReprocessSelection,
    ManagedRunKind, ManagedService, ManagedSetupPlan, ManagedSetupSession, ManagedSetupState,
    ManagedSetupUndoSession, ManagedUndoMoveOutcome as ManagedSetupUndoMoveOutcome,
    ManagedWorkspace, RuleSet, RunState, SourceLock, StateStore, UndoMoveOutcome, UndoSession,
    UndoState, build_managed_setup_plan, canonical_source_identity, fingerprint_candidate,
    inbox_file_candidates, resume_managed_setup, undo_managed_setup, undo_session_files_with_lock,
    undo_session_with_lock,
};
#[cfg(test)]
use temari_core::{
    ManagedLibraryEditState, ManagedLibraryEditUndoSession, apply_plan, build_stage_to_inbox_plan,
    root_file_candidates,
};

use crate::{
    Cli, approval_mode, confirm_mutation,
    managed_schedule::{
        ScheduleSpec, SchedulerPlatform, install_schedule, render_schedule, schedule_status,
        uninstall_schedule,
    },
    print_output_result, write_artifact,
};

static ID_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ReprocessArea {
    Kept,
    Library,
}

impl From<ReprocessArea> for ManagedReprocessArea {
    fn from(value: ReprocessArea) -> Self {
        match value {
            ReprocessArea::Kept => Self::Kept,
            ReprocessArea::Library => Self::Library,
        }
    }
}

#[derive(Serialize)]
struct ManagedWorkspaceView<'a> {
    id: &'a str,
    source: &'a str,
    folder_set_path: &'a str,
    folder_set_sha256: &'a str,
    retention_seconds: u64,
    settle_seconds: u64,
    enabled: bool,
    setup_session_path: Option<&'a str>,
    created_unix_ms: i64,
    updated_unix_ms: i64,
}

impl<'a> From<&'a ManagedWorkspace> for ManagedWorkspaceView<'a> {
    fn from(workspace: &'a ManagedWorkspace) -> Self {
        Self {
            id: &workspace.id,
            source: &workspace.source,
            folder_set_path: &workspace.folder_set_path,
            folder_set_sha256: &workspace.folder_set_sha256,
            retention_seconds: workspace.retention_seconds,
            settle_seconds: workspace.settle_seconds,
            enabled: workspace.enabled,
            setup_session_path: workspace.setup_session_path.as_deref(),
            created_unix_ms: workspace.created_unix_ms,
            updated_unix_ms: workspace.updated_unix_ms,
        }
    }
}

#[derive(Serialize)]
struct ManagedRuleView<'a> {
    id: &'a str,
    workspace_id: &'a str,
    name_glob: &'a str,
    destination_id: &'a str,
    priority: i32,
    enabled: bool,
}

#[derive(Serialize)]
struct ManagedMoveView {
    run_id: String,
    kind: ManagedRunKind,
    file_id: String,
    source_path: String,
    destination_path: String,
    undone: bool,
    undo_outcome: Option<UndoMoveOutcome>,
    finished_unix_ms: Option<i64>,
}

impl<'a> ManagedRuleView<'a> {
    fn new(rule: &'a LocalRule, workspace_id: &'a str) -> Self {
        Self {
            id: &rule.id,
            workspace_id,
            name_glob: &rule.name_glob,
            destination_id: &rule.destination_id,
            priority: rule.priority,
            enabled: rule.enabled,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum ManagedCommand {
    /// Build a read-only plan for creating a managed three-area workspace.
    Init {
        /// Source directory to manage.
        source: PathBuf,
        /// Path for the generated setup plan JSON.
        #[arg(long)]
        out: PathBuf,
    },
    /// Apply a reviewed setup plan and activate the managed workspace.
    Apply {
        /// Reviewed managed setup plan JSON.
        plan: PathBuf,
        /// Reviewed raw folder set JSON.
        #[arg(long)]
        folders: PathBuf,
        /// New directory for setup artifacts and the recovery journal.
        #[arg(long)]
        out: PathBuf,
        /// Minimum Inbox age before classification.
        #[arg(long, default_value_t = 86_400)]
        retention_seconds: u64,
        /// Minimum unchanged time before classification.
        #[arg(long, default_value_t = 30)]
        settle_seconds: u64,
        /// Confirm the filesystem mutation without prompting.
        #[arg(long)]
        yes: bool,
    },
    /// List managed workspaces.
    List,
    /// Show one workspace, its Inbox state, and indexed runs.
    Status {
        /// Managed workspace ID.
        id: String,
    },
    /// Enable new managed runs for a workspace.
    Enable { id: String },
    /// Disable new managed runs without changing files or recovery artifacts.
    Disable { id: String },
    /// Change Inbox retention and stability windows.
    Edit {
        id: String,
        #[arg(long)]
        retention_seconds: Option<u64>,
        #[arg(long)]
        settle_seconds: Option<u64>,
    },
    /// Remove only the workspace registration and mutable indexes.
    Remove {
        id: String,
        #[arg(long)]
        yes: bool,
    },
    /// Reconcile the Inbox filesystem with its mutable SQLite index.
    Reconcile { id: String },
    /// Configure deterministic local routing rules for managed workspaces.
    Rule {
        #[command(subcommand)]
        command: RuleCommand,
    },
    /// Inspect and edit the approved Library structure.
    Library {
        #[command(subcommand)]
        command: LibraryCommand,
    },
    /// Run one staging and classification cycle.
    Run {
        /// Managed workspace ID.
        id: String,
        /// New directory for cycle artifacts and journals. Defaults below the state directory.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Apply each generated plan after writing it.
        #[arg(long)]
        apply: bool,
        /// Confirm filesystem mutations without prompting.
        #[arg(long)]
        yes: bool,
    },
    /// Move selected protected or classified files back through Inbox.
    Reprocess {
        /// Managed workspace ID.
        id: String,
        /// Managed area containing the selected paths.
        #[arg(long, value_enum)]
        from: ReprocessArea,
        /// Area-relative file or directory to include; repeat for multiple paths.
        #[arg(long = "path", conflicts_with = "all")]
        paths: Vec<String>,
        /// Reprocess every file in Library. Not allowed for Kept.
        #[arg(long, conflicts_with = "paths")]
        all: bool,
        /// New directory for the reviewed Plan and optional Apply journal.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Apply the generated Plan after writing it.
        #[arg(long)]
        apply: bool,
        /// Confirm filesystem mutation without prompting.
        #[arg(long)]
        yes: bool,
    },
    /// Configure explicit per-user scheduling for finite managed runs.
    Schedule {
        #[command(subcommand)]
        command: ScheduleCommand,
    },
    /// Apply one previously reviewed managed run.
    ApplyRun {
        /// Managed run ID containing the reviewed plan.
        run_id: String,
        /// Confirm the filesystem mutation without prompting.
        #[arg(long)]
        yes: bool,
    },
    /// Resume a managed run whose Apply session was interrupted.
    ResumeRun {
        /// Managed run ID with a running Apply session.
        run_id: String,
        /// Confirm the filesystem mutation without prompting.
        #[arg(long)]
        yes: bool,
    },
    /// List recent completed managed moves.
    History {
        /// Managed workspace ID.
        id: String,
        /// Maximum number of moves to return.
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Undo all or selected files from a completed managed run.
    Undo {
        /// Completed managed run ID to undo.
        run_id: String,
        /// New directory for the Undo journal.
        #[arg(long)]
        out: PathBuf,
        /// File ID or original source-relative path to undo; repeat for multiple files.
        #[arg(long = "file")]
        files: Vec<String>,
        /// Confirm the filesystem mutation without prompting.
        #[arg(long)]
        yes: bool,
    },
    /// Undo a terminal managed setup session.
    UndoSetup {
        /// Terminal managed setup session JSON.
        session: PathBuf,
        /// Path for the generated setup Undo journal.
        #[arg(long)]
        out: PathBuf,
        /// Confirm the filesystem mutation without prompting.
        #[arg(long)]
        yes: bool,
    },
    /// Resume a running managed setup session in place.
    ResumeSetup {
        /// Running managed setup session JSON to resume in place.
        session: PathBuf,
        /// Confirm the filesystem mutation without prompting.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum RuleCommand {
    /// Add a basename glob rule to a managed workspace.
    Add {
        /// Managed workspace ID.
        workspace_id: String,
        /// Case-insensitive basename glob to match.
        #[arg(long = "name-glob")]
        name_glob: String,
        /// Approved opaque destination ID from the workspace folder set.
        #[arg(long)]
        destination: String,
        /// Match priority; higher values run first.
        #[arg(long, default_value_t = 50)]
        priority: i32,
        /// Create the rule without activating it.
        #[arg(long)]
        disabled: bool,
    },
    /// List a managed workspace's rules in matching order.
    List {
        /// Managed workspace ID.
        workspace_id: String,
    },
    /// Enable a managed workspace rule.
    Enable {
        /// Rule ID.
        rule_id: String,
    },
    /// Disable a managed workspace rule.
    Disable {
        /// Rule ID.
        rule_id: String,
    },
    /// Remove a managed workspace rule.
    Remove {
        /// Rule ID.
        rule_id: String,
        /// Confirm removal without prompting.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum LibraryCommand {
    /// Write the current approved FolderSet without changing it.
    Show {
        /// Managed workspace ID.
        workspace_id: String,
        /// Output path, or `-` for stdout.
        #[arg(long)]
        out: PathBuf,
    },
    /// Build a read-only Library edit Plan.
    Plan {
        /// Managed workspace ID.
        workspace_id: String,
        /// Output path for the reviewed Plan JSON.
        #[arg(long)]
        out: PathBuf,
        #[command(subcommand)]
        operation: LibraryPlanCommand,
    },
    /// Apply a reviewed Library edit Plan.
    Apply {
        /// Reviewed Library edit Plan JSON.
        plan: PathBuf,
        /// Confirm the binding change without prompting.
        #[arg(long)]
        yes: bool,
    },
    /// Undo a completed Library edit using a run-owned journal.
    Undo {
        /// Managed workspace ID.
        workspace_id: String,
        /// Completed Configure run ID.
        run_id: String,
        /// Confirm the binding change without prompting.
        #[arg(long)]
        yes: bool,
    },
    /// Resume an interrupted Library edit Apply or Undo.
    Resume {
        /// Managed workspace ID.
        workspace_id: String,
        /// Configure run ID requiring recovery.
        run_id: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum LibraryPlanCommand {
    /// Add a model-visible Library destination.
    Add {
        #[arg(long)]
        path: String,
        #[arg(long)]
        description: String,
    },
    /// Change an approved destination path while preserving its opaque ID.
    Rename {
        destination_id: String,
        #[arg(long)]
        path: String,
    },
    /// Change an approved destination description.
    Describe {
        destination_id: String,
        #[arg(long)]
        description: String,
    },
    /// Remove an approved destination from future classification.
    Delete { destination_id: String },
}

#[derive(Debug, Subcommand)]
pub enum ScheduleCommand {
    /// Print the platform scheduler definition without installing it.
    Print {
        workspace_id: String,
        /// Stable executable path used by the scheduler.
        #[arg(long)]
        executable: Option<PathBuf>,
        #[arg(long, default_value_t = 300)]
        every_seconds: u32,
        #[arg(long, value_enum, default_value_t)]
        platform: SchedulerPlatform,
    },
    /// Install and start the per-user scheduler definition.
    Install {
        workspace_id: String,
        /// Stable executable path used by the scheduler.
        #[arg(long)]
        executable: Option<PathBuf>,
        #[arg(long, default_value_t = 300)]
        every_seconds: u32,
        #[arg(long, value_enum, default_value_t)]
        platform: SchedulerPlatform,
        #[arg(long)]
        yes: bool,
    },
    /// Show whether the workspace scheduler is installed and active.
    Status {
        workspace_id: String,
        #[arg(long, value_enum, default_value_t)]
        platform: SchedulerPlatform,
    },
    /// Stop and remove only this workspace's scheduler definition.
    Uninstall {
        workspace_id: String,
        #[arg(long, value_enum, default_value_t)]
        platform: SchedulerPlatform,
        #[arg(long)]
        yes: bool,
    },
}

pub fn run_managed(cli: &Cli, command: &ManagedCommand) -> Result<()> {
    match command {
        ManagedCommand::Init { source, out } => init(cli, source, out),
        ManagedCommand::Apply {
            plan,
            folders,
            out,
            retention_seconds,
            settle_seconds,
            yes,
        } => activate(
            cli,
            plan,
            folders,
            out,
            *retention_seconds,
            *settle_seconds,
            *yes,
        ),
        ManagedCommand::List => list(cli),
        ManagedCommand::Status { id } => status(cli, id),
        ManagedCommand::Enable { id } => set_workspace_enabled(cli, id, true),
        ManagedCommand::Disable { id } => set_workspace_enabled(cli, id, false),
        ManagedCommand::Edit {
            id,
            retention_seconds,
            settle_seconds,
        } => edit_workspace(cli, id, *retention_seconds, *settle_seconds),
        ManagedCommand::Remove { id, yes } => remove_workspace(cli, id, *yes),
        ManagedCommand::Reconcile { id } => reconcile_workspace(cli, id),
        ManagedCommand::Rule { command } => run_rule(cli, command),
        ManagedCommand::Library { command } => run_library(cli, command),
        ManagedCommand::Run {
            id,
            out,
            apply,
            yes,
        } => run_cycle(cli, id, out.as_deref(), *apply, *yes),
        ManagedCommand::Reprocess {
            id,
            from,
            paths,
            all,
            out,
            apply,
            yes,
        } => reprocess(cli, id, *from, paths, *all, out.as_deref(), *apply, *yes),
        ManagedCommand::Schedule { command } => run_schedule(cli, command),
        ManagedCommand::ApplyRun { run_id, yes } => apply_saved_run(cli, run_id, *yes),
        ManagedCommand::ResumeRun { run_id, yes } => resume_run(cli, run_id, *yes),
        ManagedCommand::History { id, limit } => history(cli, id, *limit),
        ManagedCommand::Undo {
            run_id,
            out,
            files,
            yes,
        } => undo_run(cli, run_id, out, files, *yes),
        ManagedCommand::UndoSetup { session, out, yes } => undo_setup(cli, session, out, *yes),
        ManagedCommand::ResumeSetup { session, yes } => resume_setup(cli, session, *yes),
    }
}

fn run_library(cli: &Cli, command: &LibraryCommand) -> Result<()> {
    match command {
        LibraryCommand::Show { workspace_id, out } => library_show(cli, workspace_id, out),
        LibraryCommand::Plan {
            workspace_id,
            out,
            operation,
        } => library_plan(cli, workspace_id, out, operation),
        LibraryCommand::Apply { plan, yes } => library_apply(cli, plan, *yes),
        LibraryCommand::Undo {
            workspace_id,
            run_id,
            yes,
        } => library_undo(cli, workspace_id, run_id, *yes),
        LibraryCommand::Resume {
            workspace_id,
            run_id,
        } => library_resume(cli, workspace_id, run_id),
    }
}

fn library_show(cli: &Cli, workspace_id: &str, out: &Path) -> Result<()> {
    let context = ManagedContext::new(cli)?;
    let store = context.store()?;
    let workspace = require_workspace(&store, workspace_id)?;
    let folders = FolderSet::load(Path::new(&workspace.folder_set_path))?;
    let out = resolve_artifact_output(out, Path::new(&workspace.source), "FolderSet output")?;
    write_artifact(&out, &folders)?;
    print_output_result(cli, &out)
}

fn library_plan(
    cli: &Cli,
    workspace_id: &str,
    out: &Path,
    operation: &LibraryPlanCommand,
) -> Result<()> {
    let context = ManagedContext::new(cli)?;
    let store = context.store()?;
    let workspace = require_workspace(&store, workspace_id)?;
    let out = resolve_artifact_output(out, Path::new(&workspace.source), "Library edit Plan")?;
    let operation = match operation {
        LibraryPlanCommand::Add { path, description } => ManagedLibraryEdit::Add {
            path: path.clone(),
            description: description.clone(),
        },
        LibraryPlanCommand::Rename {
            destination_id,
            path,
        } => ManagedLibraryEdit::Rename {
            id: destination_id.clone(),
            path: path.clone(),
        },
        LibraryPlanCommand::Describe {
            destination_id,
            description,
        } => ManagedLibraryEdit::EditDescription {
            id: destination_id.clone(),
            description: description.clone(),
        },
        LibraryPlanCommand::Delete { destination_id } => ManagedLibraryEdit::Delete {
            id: destination_id.clone(),
        },
    };
    drop(store);
    let plan = ManagedService::new(&context.state).preview_library_edit(workspace_id, operation)?;
    write_artifact(&out, &plan)?;
    print_output_result(cli, &out)
}

fn library_apply(cli: &Cli, plan_path: &Path, yes: bool) -> Result<()> {
    let plan_path = fs::canonicalize(plan_path)
        .with_context(|| format!("failed to resolve {}", plan_path.display()))?;
    let plan = ManagedLibraryEditPlan::load(&plan_path)?;
    ensure_outside(&plan_path, Path::new(&plan.source), "Library edit Plan")?;
    confirm(cli, yes, "Apply this reviewed Library edit Plan? [y/N] ")?;
    let context = ManagedContext::new(cli)?;
    let result = ManagedService::new(&context.state).apply_library_edit(&plan)?;
    print_value(cli, &result, &result.run.id)
}

fn library_undo(cli: &Cli, workspace_id: &str, run_id: &str, yes: bool) -> Result<()> {
    let context = ManagedContext::new(cli)?;
    let store = context.store()?;
    let run = store
        .managed_run(run_id)?
        .ok_or_else(|| anyhow::anyhow!("unknown managed run {run_id:?}"))?;
    if run.workspace_id != workspace_id {
        bail!("managed run {run_id:?} does not belong to workspace {workspace_id:?}");
    }
    if run.kind != ManagedRunKind::Configure {
        bail!("managed run {run_id:?} is not a Library Configure run");
    }
    let apply_path = run
        .apply_path
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("Library Configure run has no Apply Session"))?;
    let journal_path = Path::new(apply_path)
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Library Configure Session has no parent directory"))?
        .join("library-edit-undo.json");
    drop(store);
    confirm(cli, yes, "Undo this completed Library edit? [y/N] ")?;
    let result = ManagedService::new(&context.state).undo_library_edit(run_id, &journal_path)?;
    print_value(cli, &result, &result.run.id)
}

fn library_resume(cli: &Cli, workspace_id: &str, run_id: &str) -> Result<()> {
    let context = ManagedContext::new(cli)?;
    let store = context.store()?;
    let run = store
        .managed_run(run_id)?
        .ok_or_else(|| anyhow::anyhow!("unknown managed run {run_id:?}"))?;
    if run.workspace_id != workspace_id {
        bail!("managed run {run_id:?} does not belong to workspace {workspace_id:?}");
    }
    if run.kind != ManagedRunKind::Configure {
        bail!("managed run {run_id:?} is not a Library Configure run");
    }
    let state = run.state;
    drop(store);
    let service = ManagedService::new(&context.state);
    match state {
        RunState::Applying => {
            let result = service.resume_library_edit(run_id)?;
            print_value(cli, &result, &result.run.id)
        }
        RunState::NeedsResume => {
            let result = service.resume_library_edit_undo(run_id)?;
            print_value(cli, &result, &result.run.id)
        }
        _ => bail!("Library Configure run {run_id:?} is {state:?} and does not require recovery"),
    }
}

fn resolve_artifact_output(out: &Path, source: &Path, label: &str) -> Result<PathBuf> {
    if out == Path::new("-") {
        return Ok(PathBuf::from("-"));
    }
    let out = resolved_target(out)?;
    ensure_outside(&out, source, label)?;
    Ok(out)
}

fn run_rule(cli: &Cli, command: &RuleCommand) -> Result<()> {
    match command {
        RuleCommand::Add {
            workspace_id,
            name_glob,
            destination,
            priority,
            disabled,
        } => add_rule(
            cli,
            workspace_id,
            name_glob,
            destination,
            *priority,
            *disabled,
        ),
        RuleCommand::List { workspace_id } => list_rules(cli, workspace_id),
        RuleCommand::Enable { rule_id } => set_rule_enabled(cli, rule_id, true),
        RuleCommand::Disable { rule_id } => set_rule_enabled(cli, rule_id, false),
        RuleCommand::Remove { rule_id, yes } => remove_rule(cli, rule_id, *yes),
    }
}

fn run_schedule(cli: &Cli, command: &ScheduleCommand) -> Result<()> {
    match command {
        ScheduleCommand::Print {
            workspace_id,
            executable,
            every_seconds,
            platform,
        } => {
            let spec = schedule_spec(
                cli,
                workspace_id,
                executable.as_deref(),
                *every_seconds,
                false,
            )?;
            let definitions = render_schedule(&spec, *platform)?;
            if cli.json {
                print_json(&definitions)
            } else {
                for definition in definitions {
                    println!("# {}", definition.path.display());
                    print!("{}", definition.contents);
                }
                Ok(())
            }
        }
        ScheduleCommand::Install {
            workspace_id,
            executable,
            every_seconds,
            platform,
            yes,
        } => {
            let spec = schedule_spec(
                cli,
                workspace_id,
                executable.as_deref(),
                *every_seconds,
                true,
            )?;
            let resolved_platform = platform.resolve()?;
            let prompt = format!(
                "Install a {resolved_platform:?} user schedule every {} seconds for {}? [y/N] ",
                spec.interval_seconds(),
                spec.source().display()
            );
            confirm(cli, *yes, &prompt)?;
            let status = install_schedule(&spec, *platform)?;
            print_value(cli, &status, "installed")
        }
        ScheduleCommand::Status {
            workspace_id,
            platform,
        } => {
            let status = schedule_status(workspace_id, *platform)?;
            print_value(
                cli,
                &status,
                if status.active { "active" } else { "inactive" },
            )
        }
        ScheduleCommand::Uninstall {
            workspace_id,
            platform,
            yes,
        } => {
            confirm(cli, *yes, "Stop and remove this user schedule? [y/N] ")?;
            let status = uninstall_schedule(workspace_id, *platform)?;
            print_value(cli, &status, "uninstalled")
        }
    }
}

fn schedule_spec(
    cli: &Cli,
    workspace_id: &str,
    executable: Option<&Path>,
    every_seconds: u32,
    reject_environment_key: bool,
) -> Result<ScheduleSpec> {
    let context = ManagedContext::new(cli)?;
    let store = context.store()?;
    let workspace = require_workspace(&store, workspace_id)?;
    validate_workspace(&context, &store, &workspace)?;
    let config_path = Path::new(&workspace.config_path);
    let config = Config::load(config_path)
        .with_context(|| format!("failed to load {}", config_path.display()))?;
    if reject_environment_key && config.model.api_key_env.is_some() {
        bail!(
            "scheduled runs cannot inherit model.api_key_env reliably; use an owner-only config with model.api_key or install the schedule manually with an explicit environment"
        );
    }
    let executable = executable
        .map(Path::to_path_buf)
        .unwrap_or(std::env::current_exe()?);
    ScheduleSpec::new(
        workspace_id,
        &executable,
        config_path,
        &context.state,
        Path::new(&workspace.source),
        every_seconds,
    )
}

fn init(cli: &Cli, source: &Path, out: &Path) -> Result<()> {
    let (source, _) = canonical_source_identity(source)?;
    if out != Path::new("-") {
        ensure_outside(out, &source, "managed setup Plan")?;
    }
    let plan = build_managed_setup_plan(&source)?;
    write_artifact(out, &plan)?;
    print_output_result(cli, out)
}

#[allow(clippy::too_many_arguments)]
fn activate(
    cli: &Cli,
    plan_path: &Path,
    raw_folders_path: &Path,
    run_directory: &Path,
    retention_seconds: u64,
    settle_seconds: u64,
    yes: bool,
) -> Result<()> {
    validate_activation_durations(retention_seconds, settle_seconds)?;
    let plan_path = fs::canonicalize(plan_path)
        .with_context(|| format!("failed to resolve {}", plan_path.display()))?;
    let raw_folders_path = fs::canonicalize(raw_folders_path)
        .with_context(|| format!("failed to resolve {}", raw_folders_path.display()))?;
    let plan = ManagedSetupPlan::load(&plan_path)?;
    let source = Path::new(&plan.source);
    ensure_outside(&plan_path, source, "managed setup Plan")?;
    ensure_outside(&raw_folders_path, source, "raw folder set")?;
    let raw_folders = FolderSet::load(&raw_folders_path)?;
    if raw_folders.source != plan.source {
        bail!("raw folder set does not belong to the managed source");
    }
    let context = ManagedContext::new(cli)?;
    ensure_outside(&context.state, source, "state database")?;
    confirm(cli, yes, "Apply this managed setup plan? [y/N] ")?;
    let service = ManagedService::new(&context.state);
    let activation = service.activate_workspace_in(
        &plan,
        &raw_folders,
        &cli.config,
        retention_seconds,
        settle_seconds,
        Some(run_directory),
    )?;
    print_value(
        cli,
        &serde_json::json!({
            "workspace_id": activation.workspace.id,
            "setup_session": activation.workspace.setup_session_path,
            "folder_set": activation.workspace.folder_set_path,
        }),
        &activation.workspace.id,
    )
}

fn list(cli: &Cli) -> Result<()> {
    let records = ManagedContext::new(cli)?.store()?.managed_workspaces()?;
    if cli.json {
        print_json(
            &records
                .iter()
                .map(ManagedWorkspaceView::from)
                .collect::<Vec<_>>(),
        )
    } else {
        for workspace in records {
            println!(
                "{}\t{}\t{}\tretention={}s\tsettle={}s",
                workspace.id,
                if workspace.enabled {
                    "enabled"
                } else {
                    "disabled"
                },
                workspace.source,
                workspace.retention_seconds,
                workspace.settle_seconds
            );
        }
        Ok(())
    }
}

fn status(cli: &Cli, id: &str) -> Result<()> {
    let context = ManagedContext::new(cli)?;
    let store = context.store()?;
    let workspace = require_workspace(&store, id)?;
    let inbox = store.inbox_items(id)?;
    let runs = store.managed_runs(id)?;
    let now = unix_ms()?;
    let mut issues = Vec::new();
    if let Err(error) = validate_workspace_binding(&context, &store, &workspace) {
        issues.push(error.to_string());
    }
    let physical_inbox_files = match inbox_file_candidates(Path::new(&workspace.source)) {
        Ok(files) => files.len(),
        Err(error) => {
            issues.push(format!("Inbox scan failed: {error}"));
            0
        }
    };
    let count_state = |state| inbox.iter().filter(|item| item.state == state).count();
    let eligible_now = inbox
        .iter()
        .filter(|item| item.state == InboxState::Pending && item.eligible_unix_ms <= now)
        .count();
    let next_eligible_unix_ms = inbox
        .iter()
        .filter(|item| item.state == InboxState::Pending && item.eligible_unix_ms > now)
        .map(|item| item.eligible_unix_ms)
        .min();
    let actionable_runs = runs
        .iter()
        .filter(|run| {
            matches!(
                run.state,
                RunState::Planned | RunState::Applying | RunState::NeedsResume | RunState::Failed
            )
        })
        .collect::<Vec<_>>();
    if !actionable_runs.is_empty() {
        issues.push(format!("{} run(s) need attention", actionable_runs.len()));
    }
    let health = if !issues.is_empty() {
        "attention"
    } else if !workspace.enabled {
        "disabled"
    } else {
        "healthy"
    };
    let value = serde_json::json!({
        "health": health,
        "issues": issues,
        "workspace": ManagedWorkspaceView::from(&workspace),
        "inbox": {
            "physical_files": physical_inbox_files,
            "indexed_pending": count_state(InboxState::Pending),
            "indexed_planned": count_state(InboxState::Planned),
            "indexed_moved": count_state(InboxState::Moved),
            "eligible_now": eligible_now,
            "next_eligible_unix_ms": next_eligible_unix_ms,
        },
        "runs": {
            "total": runs.len(),
            "actionable": actionable_runs,
        },
    });
    if cli.json {
        print_json(&value)
    } else {
        println!("Health: {health}");
        println!("Workspace: {}", workspace.id);
        println!("Source: {}", workspace.source);
        println!(
            "Inbox: {physical_inbox_files} files, {} pending, {} planned, {} eligible now",
            count_state(InboxState::Pending),
            count_state(InboxState::Planned),
            eligible_now
        );
        if let Some(next) = next_eligible_unix_ms {
            println!("Next eligible: {next}");
        }
        for issue in value["issues"].as_array().into_iter().flatten() {
            println!("Attention: {}", issue.as_str().unwrap_or("unknown issue"));
        }
        Ok(())
    }
}

fn set_workspace_enabled(cli: &Cli, id: &str, enabled: bool) -> Result<()> {
    let context = ManagedContext::new(cli)?;
    let mut store = context.store()?;
    let workspace = require_workspace(&store, id)?;
    if enabled {
        validate_workspace_binding(&context, &store, &workspace)?;
    }
    let workspace = store.set_managed_workspace_enabled(id, enabled, unix_ms()?)?;
    print_value(
        cli,
        &ManagedWorkspaceView::from(&workspace),
        if enabled { "enabled" } else { "disabled" },
    )
}

fn edit_workspace(
    cli: &Cli,
    id: &str,
    retention_seconds: Option<u64>,
    settle_seconds: Option<u64>,
) -> Result<()> {
    if retention_seconds.is_none() && settle_seconds.is_none() {
        bail!("managed edit requires --retention-seconds or --settle-seconds");
    }
    let context = ManagedContext::new(cli)?;
    let mut store = context.store()?;
    let current = require_workspace(&store, id)?;
    let retention = retention_seconds.unwrap_or(current.retention_seconds);
    let settle = settle_seconds.unwrap_or(current.settle_seconds);
    validate_activation_durations(retention, settle)?;
    let workspace = store.update_managed_workspace_windows(id, retention, settle, unix_ms()?)?;
    print_value(cli, &ManagedWorkspaceView::from(&workspace), &workspace.id)
}

fn remove_workspace(cli: &Cli, id: &str, yes: bool) -> Result<()> {
    confirm(
        cli,
        yes,
        "Remove this workspace registration and mutable indexes? Files and JSON artifacts remain. [y/N] ",
    )?;
    let context = ManagedContext::new(cli)?;
    let mut store = context.store()?;
    let workspace = require_workspace(&store, id)?;
    let _lock = temari_core::SourceLock::acquire(Path::new(&workspace.source))?;
    store.remove_managed_workspace_registration(id, unix_ms()?)?;
    print_value(
        cli,
        &serde_json::json!({ "id": id, "state": "removed" }),
        "removed",
    )
}

fn reconcile_workspace(cli: &Cli, id: &str) -> Result<()> {
    let context = ManagedContext::new(cli)?;
    let mut store = context.store()?;
    let workspace = require_workspace(&store, id)?;
    validate_workspace_binding(&context, &store, &workspace)?;
    let summary = reconcile_inbox(&mut store, &workspace, unix_ms()?)?;
    print_value(cli, &summary, "reconciled")
}

fn add_rule(
    cli: &Cli,
    workspace_id: &str,
    name_glob: &str,
    destination: &str,
    priority: i32,
    disabled: bool,
) -> Result<()> {
    let context = ManagedContext::new(cli)?;
    let mut store = context.store()?;
    let workspace = require_workspace(&store, workspace_id)?;
    let folders = managed_rule_folders(&store, &workspace)?;
    let rule = LocalRule {
        id: new_id("rule")?,
        monitor_id: workspace.monitor_id,
        name_glob: name_glob.to_owned(),
        destination_id: destination.to_owned(),
        priority,
        enabled: !disabled,
    };
    let mut rules = store.active_rules(&rule.monitor_id)?;
    rules.push(rule.clone());
    RuleSet::compile(&rules, &folders.folders)?;
    store.insert_rule(&rule, unix_ms()?)?;
    print_value(cli, &ManagedRuleView::new(&rule, workspace_id), &rule.id)
}

fn list_rules(cli: &Cli, workspace_id: &str) -> Result<()> {
    let store = ManagedContext::new(cli)?.store()?;
    let workspace = require_workspace(&store, workspace_id)?;
    managed_rule_folders(&store, &workspace)?;
    let rules = store.active_rules(&workspace.monitor_id)?;
    if cli.json {
        print_json(
            &rules
                .iter()
                .map(|rule| ManagedRuleView::new(rule, workspace_id))
                .collect::<Vec<_>>(),
        )
    } else {
        for rule in rules {
            println!(
                "{}\t{}\t{}\t{}\t{}",
                rule.id,
                if rule.enabled { "enabled" } else { "disabled" },
                rule.priority,
                rule.name_glob,
                rule.destination_id
            );
        }
        Ok(())
    }
}

fn set_rule_enabled(cli: &Cli, rule_id: &str, enabled: bool) -> Result<()> {
    let context = ManagedContext::new(cli)?;
    let mut store = context.store()?;
    let mut rule = require_managed_rule(&store, rule_id)?;
    let workspace = workspace_for_monitor(&store, &rule.monitor_id)?;
    let folders = managed_rule_folders(&store, &workspace)?;
    rule.enabled = enabled;
    let mut rules = store.active_rules(&rule.monitor_id)?;
    let candidate = rules
        .iter_mut()
        .find(|candidate| candidate.id == rule.id)
        .ok_or_else(|| anyhow::anyhow!("unknown active rule {rule_id:?}"))?;
    *candidate = rule.clone();
    RuleSet::compile(&rules, &folders.folders)?;
    store.set_rule_enabled(rule_id, enabled, unix_ms()?)?;
    print_value(cli, &ManagedRuleView::new(&rule, &workspace.id), &rule.id)
}

fn remove_rule(cli: &Cli, rule_id: &str, yes: bool) -> Result<()> {
    confirm(
        cli,
        yes,
        &format!("Remove managed rule {rule_id:?}? [y/N] "),
    )?;
    let context = ManagedContext::new(cli)?;
    let mut store = context.store()?;
    let rule = require_managed_rule(&store, rule_id)?;
    let workspace = workspace_for_monitor(&store, &rule.monitor_id)?;
    managed_rule_folders(&store, &workspace)?;
    store.remove_rule(rule_id, unix_ms()?)?;
    print_value(
        cli,
        &serde_json::json!({ "id": rule_id, "state": "removed" }),
        rule_id,
    )
}

fn require_managed_rule(store: &StateStore, rule_id: &str) -> Result<LocalRule> {
    let rule = store
        .rule(rule_id)?
        .ok_or_else(|| anyhow::anyhow!("unknown active rule {rule_id:?}"))?;
    workspace_for_monitor(store, &rule.monitor_id)?;
    Ok(rule)
}

fn workspace_for_monitor(store: &StateStore, monitor_id: &str) -> Result<ManagedWorkspace> {
    store
        .managed_workspaces()?
        .into_iter()
        .find(|workspace| workspace.monitor_id == monitor_id)
        .ok_or_else(|| anyhow::anyhow!("rule does not belong to a managed workspace"))
}

fn managed_rule_folders(store: &StateStore, workspace: &ManagedWorkspace) -> Result<FolderSet> {
    let monitor = store
        .monitor(&workspace.monitor_id)?
        .filter(|monitor| monitor.deleted_unix_ms.is_none())
        .ok_or_else(|| anyhow::anyhow!("managed monitor is missing"))?;
    if monitor.source != workspace.source
        || monitor.source_identity != workspace.source_identity
        || monitor.folder_set_path != workspace.folder_set_path
        || monitor.folder_set_sha256 != workspace.folder_set_sha256
    {
        bail!("managed monitor no longer matches its workspace");
    }
    let folders = FolderSet::load(Path::new(&workspace.folder_set_path))?;
    if folders.source != workspace.source || folders.sha256()? != workspace.folder_set_sha256 {
        bail!("managed workspace folder set changed");
    }
    Ok(folders)
}

fn run_cycle(cli: &Cli, id: &str, out: Option<&Path>, apply: bool, yes: bool) -> Result<()> {
    if apply != yes {
        bail!("--apply and --yes must be supplied together");
    }
    let context = ManagedContext::new(cli)?;
    let service = ManagedService::new(&context.state);
    let result = service.run_workspace_in(id, apply, out)?;

    if cli.json {
        print_json(&result)
    } else {
        println!("Artifacts: {}", result.artifact_directory);
        if let Some(adoption) = result.directory_adoption {
            println!(
                "adopted-directories\t{} moves\t{}",
                adoption.move_count, adoption.plan_path
            );
        }
        for run in result.runs {
            println!(
                "{}\t{:?}\t{:?}\t{} moves{}",
                run.id,
                run.kind,
                run.state,
                run.move_count,
                run.plan_path
                    .as_deref()
                    .map(|path| format!("\t{path}"))
                    .unwrap_or_default()
            );
        }
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn reprocess(
    cli: &Cli,
    id: &str,
    from: ReprocessArea,
    paths: &[String],
    all: bool,
    out: Option<&Path>,
    apply: bool,
    yes: bool,
) -> Result<()> {
    if apply != yes {
        bail!("--apply and --yes must be supplied together");
    }
    let selection = if all {
        ManagedReprocessSelection::All
    } else if paths.is_empty() {
        bail!("reprocess requires at least one --path or --all");
    } else {
        ManagedReprocessSelection::Paths(paths.to_vec())
    };
    let area = ManagedReprocessArea::from(from);
    let context = ManagedContext::new(cli)?;
    let service = ManagedService::new(&context.state);
    let result = service.reprocess_in(id, area, &selection, apply, out)?;
    print_value(
        cli,
        &result,
        result.runs.first().map(|run| run.id.as_str()).unwrap_or(id),
    )
}

fn apply_saved_run(cli: &Cli, run_id: &str, yes: bool) -> Result<()> {
    confirm(cli, yes, "Apply this managed run? [y/N] ")?;
    let context = ManagedContext::new(cli)?;
    let run = ManagedService::new(&context.state).apply_run(run_id)?;
    print_value(cli, &run, &run.id)
}

fn resume_run(cli: &Cli, run_id: &str, yes: bool) -> Result<()> {
    confirm(cli, yes, "Resume this managed run? [y/N] ")?;
    let context = ManagedContext::new(cli)?;
    let run = ManagedService::new(&context.state).resume_run(run_id)?;
    print_value(cli, &run, &run.id)
}

fn history(cli: &Cli, id: &str, limit: u32) -> Result<()> {
    let store = ManagedContext::new(cli)?.store()?;
    require_workspace(&store, id)?;
    let moves = managed_move_history(&store, id, limit)?;
    if cli.json {
        print_json(&moves)
    } else {
        for movement in moves {
            println!(
                "{}\t{:?}\t{}\t{}\t{} -> {}",
                movement.run_id,
                movement.kind,
                if movement.undone { "undone" } else { "active" },
                movement.file_id,
                movement.source_path,
                movement.destination_path,
            );
        }
        Ok(())
    }
}

fn managed_move_history(
    store: &StateStore,
    workspace_id: &str,
    limit: u32,
) -> Result<Vec<ManagedMoveView>> {
    let runs = store.recent_managed_moves(workspace_id, limit)?;
    let mut moves = Vec::new();
    for run in runs {
        let apply_path = run
            .apply_path
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("managed run {:?} has no Apply session", run.id))?;
        if run.kind == ManagedRunKind::Adopt {
            let apply = ManagedSetupSession::load(Path::new(apply_path))?;
            let restored = match run.undo_path.as_deref() {
                Some(path) => ManagedSetupUndoSession::load(Path::new(path))?
                    .moves
                    .into_iter()
                    .filter(|movement| movement.outcome == ManagedSetupUndoMoveOutcome::Restored)
                    .map(|movement| movement.source_path)
                    .collect::<HashSet<_>>(),
                None => HashSet::new(),
            };
            for movement in apply.moves {
                moves.push(ManagedMoveView {
                    run_id: run.id.clone(),
                    kind: run.kind,
                    file_id: movement.source_path.clone(),
                    undone: restored.contains(&movement.source_path),
                    source_path: movement.source_path,
                    destination_path: movement.destination_path,
                    undo_outcome: None,
                    finished_unix_ms: run.finished_unix_ms,
                });
                if moves.len() == limit as usize {
                    return Ok(moves);
                }
            }
            continue;
        }
        let apply = ApplySession::load(Path::new(apply_path))?;
        let mut undo_paths = store.managed_undo_journal_paths(&run.id)?;
        if let Some(path) = run.undo_path.as_ref()
            && !undo_paths.contains(path)
        {
            undo_paths.push(path.clone());
        }
        let mut undo_outcomes = HashMap::new();
        for path in undo_paths {
            for movement in UndoSession::load(Path::new(&path))?.moves {
                undo_outcomes.insert(movement.file_id, movement.outcome);
            }
        }
        for movement in apply.moves {
            let undo_outcome = undo_outcomes.get(&movement.file_id).cloned();
            let undone = matches!(
                undo_outcome,
                Some(UndoMoveOutcome::Restored | UndoMoveOutcome::AlreadyRestored)
            );
            moves.push(ManagedMoveView {
                run_id: run.id.clone(),
                kind: run.kind,
                file_id: movement.file_id,
                source_path: movement.source_path,
                destination_path: movement.destination_path,
                undone,
                undo_outcome,
                finished_unix_ms: run.finished_unix_ms,
            });
            if moves.len() == limit as usize {
                return Ok(moves);
            }
        }
    }
    Ok(moves)
}

fn undo_run(cli: &Cli, run_id: &str, out: &Path, files: &[String], yes: bool) -> Result<()> {
    confirm(cli, yes, "Undo this managed run? [y/N] ")?;
    let out = resolved_target(out)?;
    let context = ManagedContext::new(cli)?;
    let mut store = context.store()?;
    let run = store
        .managed_run(run_id)?
        .ok_or_else(|| anyhow::anyhow!("unknown managed run {run_id:?}"))?;
    if run.state != RunState::Completed {
        bail!("managed run must be completed before Undo");
    }
    if run.kind == ManagedRunKind::Setup {
        bail!("managed setup runs must be undone with managed undo-setup");
    }
    if run.kind == ManagedRunKind::Adopt {
        if !files.is_empty() {
            bail!("directory adoption Undo restores the complete adoption session");
        }
        drop(store);
        ManagedService::new(&context.state).undo_adoption_run(run_id, &out)?;
        return print_output_result(cli, &out);
    }
    let apply_path = run
        .apply_path
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("managed run has no Apply session"))?;
    let apply = ApplySession::load(Path::new(apply_path))?;
    let selected_file_ids = resolve_undo_file_ids(&apply, files)?;
    let workspace = require_workspace(&store, &run.workspace_id)?;
    let lock = SourceLock::acquire(Path::new(&workspace.source))?;
    let undo = if selected_file_ids.is_empty() {
        undo_session_with_lock(&apply, &out, &lock)?
    } else {
        undo_session_files_with_lock(&apply, &selected_file_ids, &out, &lock)?
    };
    let undo_path = path_text(&out, "managed Undo session")?;
    let restored = undo
        .moves
        .iter()
        .filter(|movement| {
            matches!(
                movement.outcome,
                UndoMoveOutcome::Restored | UndoMoveOutcome::AlreadyRestored
            )
        })
        .map(|movement| movement.file_id.as_str())
        .collect::<HashSet<_>>();
    let restored_identities = apply
        .moves
        .iter()
        .filter(|movement| restored.contains(movement.file_id.as_str()))
        .map(|movement| movement.fingerprint.identity.clone())
        .collect::<Vec<_>>();
    store.finalize_managed_undo(&run.id, &undo_path, &restored_identities, unix_ms()?)?;
    if undo.state != UndoState::Completed {
        bail!(
            "managed Undo finished with {:?}; inspect {}",
            undo.state,
            out.display()
        );
    }
    print_output_result(cli, &out)
}

fn resolve_undo_file_ids(apply: &ApplySession, selectors: &[String]) -> Result<Vec<String>> {
    let mut resolved = Vec::with_capacity(selectors.len());
    let mut seen = HashSet::new();
    for selector in selectors {
        let matches = apply
            .moves
            .iter()
            .filter(|movement| movement.file_id == *selector || movement.source_path == *selector)
            .collect::<Vec<_>>();
        let movement = match matches.as_slice() {
            [] => bail!(
                "managed Undo selector {selector:?} is neither a file ID nor an original source path"
            ),
            [movement] => movement,
            _ => bail!(
                "managed Undo selector {selector:?} is ambiguous; use the file ID shown by managed history"
            ),
        };
        if !seen.insert(movement.file_id.as_str()) {
            bail!(
                "managed Undo selects file {:?} more than once",
                movement.file_id
            );
        }
        resolved.push(movement.file_id.clone());
    }
    Ok(resolved)
}

fn undo_setup(cli: &Cli, session: &Path, out: &Path, yes: bool) -> Result<()> {
    confirm(cli, yes, "Undo this managed setup? [y/N] ")?;
    let setup = ManagedSetupSession::load(session)?;
    let undo = undo_managed_setup(&setup, out)?;
    if undo.state != temari_core::ManagedSetupUndoState::Completed {
        bail!(
            "managed setup Undo finished with {:?}; inspect {}",
            undo.state,
            out.display()
        );
    }
    deactivate_undone_setup(cli, session)?;
    print_output_result(cli, out)
}

fn resume_setup(cli: &Cli, session: &Path, yes: bool) -> Result<()> {
    confirm(cli, yes, "Resume this managed setup? [y/N] ")?;
    let resumed = resume_managed_setup(session)?;
    if resumed.state != ManagedSetupState::Completed {
        bail!(
            "managed setup resume finished with {:?}; inspect {}",
            resumed.state,
            session.display()
        );
    }
    print_output_result(cli, session)
}

fn reconcile_inbox(
    store: &mut StateStore,
    workspace: &ManagedWorkspace,
    now: i64,
) -> Result<InboxReconcileSummary> {
    let previously_moved = store
        .inbox_items(&workspace.id)?
        .into_iter()
        .filter(|item| item.state == InboxState::Moved)
        .map(|item| (item.file_identity.device, item.file_identity.inode))
        .collect::<HashSet<_>>();
    let mut observed = Vec::new();
    for candidate in inbox_file_candidates(Path::new(&workspace.source))? {
        let fingerprint = fingerprint_candidate(Path::new(&workspace.source), &candidate)?;
        observed.push(fingerprint.identity.clone());
        store.upsert_observation(&workspace.id, &fingerprint, &candidate.source_path, now)?;
    }
    let summary = store.reconcile_inbox_index(&workspace.id, &observed)?;
    for identity in observed
        .into_iter()
        .filter(|identity| previously_moved.contains(&(identity.device, identity.inode)))
    {
        store.forget_processed_file(&workspace.monitor_id, identity)?;
    }
    Ok(summary)
}

fn require_workspace(store: &StateStore, id: &str) -> Result<ManagedWorkspace> {
    store
        .managed_workspace(id)?
        .ok_or_else(|| anyhow::anyhow!("unknown managed workspace {id:?}"))
}

fn validate_workspace(
    context: &ManagedContext,
    store: &StateStore,
    workspace: &ManagedWorkspace,
) -> Result<()> {
    if !workspace.enabled {
        bail!("managed workspace is disabled");
    }
    validate_workspace_binding(context, store, workspace)
}

fn validate_workspace_binding(
    context: &ManagedContext,
    store: &StateStore,
    workspace: &ManagedWorkspace,
) -> Result<()> {
    let (source, identity) = canonical_source_identity(Path::new(&workspace.source))?;
    if source != Path::new(&workspace.source) || identity != workspace.source_identity {
        bail!("managed workspace source identity changed");
    }
    ensure_outside(&context.state, &source, "state database")?;
    let folders = FolderSet::load(Path::new(&workspace.folder_set_path))?;
    if folders.source != workspace.source || folders.sha256()? != workspace.folder_set_sha256 {
        bail!("managed workspace folder set changed");
    }
    let monitor = store
        .monitor(&workspace.monitor_id)?
        .filter(|monitor| monitor.deleted_unix_ms.is_none())
        .ok_or_else(|| anyhow::anyhow!("managed monitor is missing"))?;
    if monitor.source != workspace.source
        || monitor.source_identity != workspace.source_identity
        || monitor.folder_set_sha256 != workspace.folder_set_sha256
        || monitor.enabled != workspace.enabled
    {
        bail!("managed monitor no longer matches its workspace");
    }
    for area in ["Kept", "Inbox", "Library"] {
        let path = source.join(area);
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("failed to inspect managed area {}", path.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("managed area is not a real directory: {}", path.display());
        }
    }
    Ok(())
}

fn validate_activation_durations(retention_seconds: u64, settle_seconds: u64) -> Result<()> {
    if retention_seconds == 0 {
        bail!("--retention-seconds must be greater than zero");
    }
    if settle_seconds == 0 {
        bail!("--settle-seconds must be greater than zero");
    }
    Ok(())
}

fn deactivate_undone_setup(cli: &Cli, session: &Path) -> Result<()> {
    let session = fs::canonicalize(session)
        .with_context(|| format!("failed to resolve {}", session.display()))?;
    let context = ManagedContext::new(cli)?;
    let mut store = context.store()?;
    let Some(workspace) = store.managed_workspaces()?.into_iter().find(|workspace| {
        workspace
            .setup_session_path
            .as_deref()
            .is_some_and(|path| Path::new(path) == session)
    }) else {
        return Ok(());
    };
    store.set_managed_workspace_enabled(&workspace.id, false, unix_ms()?)?;
    Ok(())
}

struct ManagedContext {
    state: PathBuf,
}

impl ManagedContext {
    fn new(cli: &Cli) -> Result<Self> {
        let state = match &cli.state {
            Some(path) => path.clone(),
            None => {
                let directories = ProjectDirs::from("dev", "yutakobayashidev", "temari")
                    .ok_or_else(|| {
                        anyhow::anyhow!("could not determine the user state directory")
                    })?;
                directories
                    .state_dir()
                    .unwrap_or_else(|| directories.data_local_dir())
                    .join("state.sqlite3")
            }
        };
        Ok(Self {
            state: resolved_target(&state)?,
        })
    }

    fn store(&self) -> Result<StateStore> {
        StateStore::open(&self.state)
            .with_context(|| format!("failed to open managed state {}", self.state.display()))
    }
}

fn confirm(cli: &Cli, yes: bool, prompt: &str) -> Result<()> {
    let mode = approval_mode(
        io::stdin().is_terminal(),
        io::stderr().is_terminal(),
        cli.no_input,
        yes,
    );
    confirm_mutation(mode, prompt)
}

fn ensure_outside(path: &Path, source: &Path, label: &str) -> Result<()> {
    let target = resolved_target(path)?;
    if target.starts_with(source) {
        bail!("{label} must be outside the managed source");
    }
    Ok(())
}

fn resolved_target(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let name = absolute
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("path must include a final component"))?;
    let parent = absolute
        .parent()
        .ok_or_else(|| anyhow::anyhow!("path must have a parent"))?;
    let mut existing = parent;
    while !existing.exists() {
        existing = existing
            .parent()
            .ok_or_else(|| anyhow::anyhow!("path has no existing ancestor"))?;
    }
    let canonical = fs::canonicalize(existing)?;
    Ok(canonical.join(parent.strip_prefix(existing)?).join(name))
}

fn path_text(path: &Path, label: &str) -> Result<String> {
    let value = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("{label} path must be valid UTF-8"))?;
    if value.chars().any(char::is_control) {
        bail!("{label} path must contain no control characters");
    }
    Ok(value.into())
}

fn new_id(prefix: &str) -> Result<String> {
    Ok(format!(
        "{prefix}-{}-{}-{}",
        unix_ms()?,
        std::process::id(),
        ID_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ))
}

fn unix_ms() -> Result<i64> {
    let value = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    i64::try_from(value).context("system time exceeds the supported range")
}

fn print_json<T: Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}

fn print_value<T: Serialize>(cli: &Cli, value: &T, text: &str) -> Result<()> {
    if cli.json {
        print_json(value)
    } else {
        println!("{text}");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn generated_ids_are_unique_safe_components() {
        let first = new_id("managed").unwrap();
        let second = new_id("managed").unwrap();
        assert_ne!(first, second);
        assert!(!first.contains('/'));
    }

    #[test]
    fn managed_rule_json_uses_workspace_ids_only() {
        let rule = LocalRule {
            id: "rule-1".into(),
            monitor_id: "internal-monitor-1".into(),
            name_glob: "*.pdf".into(),
            destination_id: "destination-1".into(),
            priority: 50,
            enabled: true,
        };
        let value = serde_json::to_value(ManagedRuleView::new(&rule, "workspace-1")).unwrap();
        assert_eq!(value["workspace_id"], "workspace-1");
        assert!(value.get("monitor_id").is_none());
    }

    #[test]
    fn apply_and_yes_are_a_pair() {
        assert!(matches!(
            (false, false),
            (apply, yes) if apply == yes
        ));
        assert!(!matches!(
            (true, false),
            (apply, yes) if apply == yes
        ));
    }

    #[test]
    fn parses_managed_command_tree_and_defaults() {
        let cli = Cli::try_parse_from([
            "temari",
            "managed",
            "apply",
            "setup.json",
            "--folders",
            "folders.json",
            "--out",
            "run",
            "--yes",
        ])
        .unwrap();
        let crate::Command::Managed(ManagedCommand::Apply {
            retention_seconds,
            settle_seconds,
            yes,
            ..
        }) = cli.command
        else {
            panic!("expected managed apply");
        };
        assert_eq!(retention_seconds, 86_400);
        assert_eq!(settle_seconds, 30);
        assert!(yes);

        let cli = Cli::try_parse_from([
            "temari",
            "managed",
            "resume-run",
            "managed-classify-1",
            "--yes",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            crate::Command::Managed(ManagedCommand::ResumeRun { yes: true, .. })
        ));

        let cli = Cli::try_parse_from([
            "temari",
            "managed",
            "rule",
            "add",
            "workspace-1",
            "--name-glob",
            "*.pdf",
            "--destination",
            "destination-1",
        ])
        .unwrap();
        let crate::Command::Managed(ManagedCommand::Rule {
            command:
                RuleCommand::Add {
                    workspace_id,
                    priority,
                    disabled,
                    ..
                },
        }) = cli.command
        else {
            panic!("expected managed rule add");
        };
        assert_eq!(workspace_id, "workspace-1");
        assert_eq!(priority, 50);
        assert!(!disabled);

        let cli = Cli::try_parse_from(["temari", "managed", "rule", "remove", "rule-1", "--yes"])
            .unwrap();
        assert!(matches!(
            cli.command,
            crate::Command::Managed(ManagedCommand::Rule {
                command: RuleCommand::Remove { yes: true, .. }
            })
        ));

        let cli = Cli::try_parse_from([
            "temari",
            "managed",
            "library",
            "plan",
            "workspace-1",
            "--out",
            "library-plan.json",
            "add",
            "--path",
            "Research",
            "--description",
            "Research material",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            crate::Command::Managed(ManagedCommand::Library {
                command: LibraryCommand::Plan {
                    operation: LibraryPlanCommand::Add { .. },
                    ..
                }
            })
        ));

        for arguments in [
            vec!["library", "show", "workspace-1", "--out", "-"],
            vec!["library", "apply", "plan.json", "--yes"],
            vec!["library", "undo", "workspace-1", "run-1", "--yes"],
            vec!["library", "resume", "workspace-1", "run-1"],
        ] {
            let mut command = vec!["temari", "managed"];
            command.extend(arguments);
            assert!(Cli::try_parse_from(command).is_ok());
        }
    }

    #[test]
    fn rejects_zero_activation_durations() {
        assert!(validate_activation_durations(0, 30).is_err());
        assert!(validate_activation_durations(60, 0).is_err());
        assert!(validate_activation_durations(60, 30).is_ok());
    }

    #[test]
    fn parses_managed_lifecycle_reprocess_and_schedule_commands() {
        let cli = Cli::try_parse_from([
            "temari",
            "managed",
            "reprocess",
            "workspace-1",
            "--from",
            "library",
            "--all",
            "--apply",
            "--yes",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            crate::Command::Managed(ManagedCommand::Reprocess {
                from: ReprocessArea::Library,
                all: true,
                out: None,
                ..
            })
        ));

        let cli = Cli::try_parse_from([
            "temari",
            "managed",
            "schedule",
            "print",
            "workspace-1",
            "--platform",
            "systemd",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            crate::Command::Managed(ManagedCommand::Schedule {
                command: ScheduleCommand::Print {
                    every_seconds: 300,
                    platform: SchedulerPlatform::Systemd,
                    ..
                }
            })
        ));

        assert!(
            Cli::try_parse_from([
                "temari",
                "managed",
                "reprocess",
                "workspace-1",
                "--from",
                "kept",
                "--all",
                "--path",
                "Projects",
            ])
            .is_err()
        );
    }

    #[test]
    fn undo_selectors_accept_file_ids_or_source_paths_and_reject_ambiguity() {
        let root = tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("a.txt"), b"a").unwrap();
        fs::write(source.join("b.txt"), b"b").unwrap();
        fs::write(source.join("f000001"), b"ambiguous").unwrap();
        fs::create_dir(source.join("Inbox")).unwrap();
        let plan =
            build_stage_to_inbox_plan(&source, &root_file_candidates(&source).unwrap()).unwrap();
        let apply = apply_plan(&plan, &root.path().join("apply.json")).unwrap();

        assert_eq!(
            resolve_undo_file_ids(&apply, &["b.txt".into()]).unwrap(),
            ["f000002"]
        );
        assert_eq!(
            resolve_undo_file_ids(&apply, &["f000002".into()]).unwrap(),
            ["f000002"]
        );
        assert!(resolve_undo_file_ids(&apply, &["b.txt".into(), "f000002".into()]).is_err());
        assert!(resolve_undo_file_ids(&apply, &["missing.txt".into()]).is_err());
        assert!(resolve_undo_file_ids(&apply, &["f000001".into()]).is_err());
    }

    #[test]
    fn library_commands_show_plan_apply_undo_and_resume_run_owned_artifacts() {
        let root = tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        let source = fs::canonicalize(source).unwrap();
        let state_path = root.path().join("state.sqlite3");
        let config_path = root.path().join("temari.toml");
        fs::write(
            &config_path,
            include_str!("../../../examples/temari.example.toml"),
        )
        .unwrap();
        let raw_folders = temari_core::Proposal {
            version: 2,
            source: source.display().to_string(),
            scope: temari_core::ScanScope::default(),
            files_considered: 0,
            folders: vec![temari_core::FolderProposal {
                path: "Documents".into(),
                description: "Documents".into(),
            }],
        }
        .approve()
        .unwrap();
        let setup = build_managed_setup_plan(&source).unwrap();
        let service = ManagedService::new(&state_path);
        let activation = service
            .activate_workspace(&setup, &raw_folders, &config_path, 60, 1)
            .unwrap();
        let workspace_id = activation.workspace.id;
        StateStore::open(&state_path)
            .unwrap()
            .set_managed_workspace_enabled(&workspace_id, false, unix_ms().unwrap())
            .unwrap();
        let cli = Cli {
            config: config_path,
            state: Some(state_path.clone()),
            json: false,
            no_input: true,
            no_color: true,
            verbose: 0,
            command: crate::Command::Managed(ManagedCommand::List),
        };

        let shown_path = root.path().join("shown-folders.json");
        library_show(&cli, &workspace_id, &shown_path).unwrap();
        let shown = FolderSet::load(&shown_path).unwrap();
        assert_eq!(shown.source, source.display().to_string());

        let plan_path = root.path().join("library-plan.json");
        library_plan(
            &cli,
            &workspace_id,
            &plan_path,
            &LibraryPlanCommand::Add {
                path: "Research".into(),
                description: "Research material".into(),
            },
        )
        .unwrap();
        let plan = ManagedLibraryEditPlan::load(&plan_path).unwrap();
        assert!(
            plan.after_folders
                .folders
                .iter()
                .any(|folder| folder.path == "Library/Research")
        );

        library_apply(&cli, &plan_path, true).unwrap();
        let mut run = StateStore::open(&state_path)
            .unwrap()
            .managed_runs(&workspace_id)
            .unwrap()
            .into_iter()
            .find(|run| run.kind == ManagedRunKind::Configure)
            .unwrap();
        let run_id = run.id.clone();
        run.state = RunState::Applying;
        run.finished_unix_ms = None;
        run.error = None;
        StateStore::open(&state_path)
            .unwrap()
            .update_managed_run(&run)
            .unwrap();
        library_resume(&cli, &workspace_id, &run_id).unwrap();

        library_undo(&cli, &workspace_id, &run_id, true).unwrap();
        let mut run = StateStore::open(&state_path)
            .unwrap()
            .managed_run(&run_id)
            .unwrap()
            .unwrap();
        let undo_path = PathBuf::from(run.undo_path.as_deref().unwrap());
        let mut undo: ManagedLibraryEditUndoSession =
            serde_json::from_slice(&fs::read(&undo_path).unwrap()).unwrap();
        undo.state = ManagedLibraryEditState::Running;
        undo.finished_unix_ms = None;
        write_artifact(&undo_path, &undo).unwrap();
        run.state = RunState::NeedsResume;
        run.error = Some("Library edit Undo finalization is pending".into());
        StateStore::open(&state_path)
            .unwrap()
            .update_managed_run(&run)
            .unwrap();
        library_resume(&cli, &workspace_id, &run_id).unwrap();

        let workspace = StateStore::open(&state_path)
            .unwrap()
            .managed_workspace(&workspace_id)
            .unwrap()
            .unwrap();
        let restored = FolderSet::load(Path::new(&workspace.folder_set_path)).unwrap();
        assert!(
            restored
                .folders
                .iter()
                .all(|folder| folder.path != "Library/Research")
        );
    }

    #[test]
    fn activates_stages_and_undoes_a_managed_workspace() {
        let root = tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("baseline.txt"), b"baseline").unwrap();
        fs::create_dir(source.join("ExistingDirectory")).unwrap();
        fs::write(source.join("ExistingDirectory/kept.txt"), b"kept").unwrap();
        let source = fs::canonicalize(source).unwrap();
        let plan_path = root.path().join("setup-plan.json");
        let raw_folders_path = root.path().join("raw-folders.json");
        let activation_root = root.path().join("activation");
        let state_path = root.path().join("state.sqlite3");
        let config_path = root.path().join("temari.toml");
        fs::write(
            &config_path,
            include_str!("../../../examples/temari.example.toml"),
        )
        .unwrap();
        let plan = build_managed_setup_plan(&source).unwrap();
        write_artifact(&plan_path, &plan).unwrap();
        let raw_folders = temari_core::Proposal {
            version: 2,
            source: source.display().to_string(),
            scope: temari_core::ScanScope::default(),
            files_considered: 1,
            folders: vec![temari_core::FolderProposal {
                path: "Documents".into(),
                description: "Documents".into(),
            }],
        }
        .approve()
        .unwrap();
        write_artifact(&raw_folders_path, &raw_folders).unwrap();
        let cli = Cli {
            config: config_path,
            state: Some(state_path.clone()),
            json: false,
            no_input: true,
            no_color: true,
            verbose: 0,
            command: crate::Command::Managed(ManagedCommand::List),
        };

        activate(
            &cli,
            &plan_path,
            &raw_folders_path,
            &activation_root,
            1,
            1,
            true,
        )
        .unwrap();
        assert!(source.join("Inbox/baseline.txt").is_file());
        assert!(source.join("Kept/ExistingDirectory/kept.txt").is_file());

        let store = StateStore::open(&state_path).unwrap();
        let workspace = store.managed_workspaces().unwrap().pop().unwrap();
        let setup_run = store
            .managed_runs(&workspace.id)
            .unwrap()
            .into_iter()
            .find(|run| run.kind == ManagedRunKind::Setup)
            .unwrap();
        assert!(
            store
                .recent_managed_moves(&workspace.id, 20)
                .unwrap()
                .is_empty()
        );
        let managed_folders = FolderSet::load(Path::new(&workspace.folder_set_path)).unwrap();
        assert!(
            managed_folders
                .folders
                .iter()
                .all(|folder| folder.path.starts_with("Library/"))
        );
        let destination_id = managed_folders
            .folders
            .iter()
            .find(|folder| folder.path == "Library/Documents")
            .unwrap()
            .id
            .clone();
        drop(store);

        let setup_undo_error = undo_run(
            &cli,
            &setup_run.id,
            &root.path().join("invalid-setup-undo.json"),
            &[],
            true,
        )
        .unwrap_err();
        assert!(setup_undo_error.to_string().contains("managed undo-setup"));

        add_rule(&cli, &workspace.id, "*.txt", &destination_id, 100, true).unwrap();
        let store = StateStore::open(&state_path).unwrap();
        let rule = store
            .active_rules(&workspace.monitor_id)
            .unwrap()
            .pop()
            .unwrap();
        assert!(!rule.enabled);
        drop(store);
        set_rule_enabled(&cli, &rule.id, true).unwrap();

        fs::write(source.join("fresh.txt"), b"fresh").unwrap();
        fs::write(source.join("other.txt"), b"other").unwrap();
        let cycle_root = root.path().join("cycle");
        run_cycle(&cli, &workspace.id, Some(&cycle_root), true, true).unwrap();
        assert!(source.join("Inbox/fresh.txt").is_file());
        assert!(source.join("Inbox/other.txt").is_file());
        let mut store = StateStore::open(&state_path).unwrap();
        let mut stage = store
            .managed_runs(&workspace.id)
            .unwrap()
            .into_iter()
            .find(|run| run.kind == ManagedRunKind::Stage)
            .unwrap();
        assert_eq!(stage.state, RunState::Completed);
        stage.state = RunState::NeedsResume;
        stage.error = Some("apply completed; state finalization is pending".into());
        store.update_managed_run(&stage).unwrap();
        drop(store);
        resume_run(&cli, &stage.id, true).unwrap();
        let store = StateStore::open(&state_path).unwrap();
        stage = store.managed_run(&stage.id).unwrap().unwrap();
        assert_eq!(stage.state, RunState::Completed);
        assert_eq!(stage.error, None);
        let moves = managed_move_history(&store, &workspace.id, 20).unwrap();
        assert_eq!(moves.len(), 2);
        assert!(moves.iter().all(|movement| movement.run_id == stage.id));
        let fresh_move = moves
            .iter()
            .find(|movement| movement.source_path == "fresh.txt")
            .unwrap();
        assert_eq!(fresh_move.destination_path, "Inbox/fresh.txt");
        assert!(!fresh_move.undone);
        let other_file_id = moves
            .iter()
            .find(|movement| movement.source_path == "other.txt")
            .unwrap()
            .file_id
            .clone();
        drop(store);

        let undo_path = root.path().join("stage-undo.json");
        undo_run(&cli, &stage.id, &undo_path, &["fresh.txt".into()], true).unwrap();
        assert!(source.join("fresh.txt").is_file());
        let store = StateStore::open(&state_path).unwrap();
        assert!(
            store
                .inbox_items(&workspace.id)
                .unwrap()
                .iter()
                .all(|item| item.relative_path != "Inbox/fresh.txt")
        );
        let moves = managed_move_history(&store, &workspace.id, 20).unwrap();
        assert_eq!(moves.len(), 2);
        let fresh_move = moves
            .iter()
            .find(|movement| movement.source_path == "fresh.txt")
            .unwrap();
        assert!(fresh_move.undone);
        assert!(matches!(
            fresh_move.undo_outcome,
            Some(UndoMoveOutcome::Restored)
        ));
        assert!(
            !moves
                .iter()
                .find(|movement| movement.source_path == "other.txt")
                .unwrap()
                .undone
        );
        drop(store);

        let second_undo_path = root.path().join("stage-undo-other.json");
        undo_run(&cli, &stage.id, &second_undo_path, &[other_file_id], true).unwrap();
        assert!(source.join("other.txt").is_file());
        let store = StateStore::open(&state_path).unwrap();
        let moves = managed_move_history(&store, &workspace.id, 20).unwrap();
        assert!(moves.iter().all(|movement| movement.undone));
        let undo_paths = store.managed_undo_journal_paths(&stage.id).unwrap();
        assert_eq!(undo_paths.len(), 2);
        assert!(undo_paths.contains(&undo_path.display().to_string()));
        assert!(undo_paths.contains(&second_undo_path.display().to_string()));
        drop(store);

        std::thread::sleep(std::time::Duration::from_millis(1_100));
        let classify_root = root.path().join("classify-cycle");
        run_cycle(&cli, &workspace.id, Some(&classify_root), true, true).unwrap();
        assert!(source.join("Library/Documents/baseline.txt").is_file());
        let store = StateStore::open(&state_path).unwrap();
        let classify = store
            .managed_runs(&workspace.id)
            .unwrap()
            .into_iter()
            .find(|run| {
                run.kind == ManagedRunKind::Classify
                    && run.state == RunState::Completed
                    && run.move_count == 1
            })
            .unwrap();
        drop(store);

        let classify_undo_path = root.path().join("classify-undo.json");
        undo_run(&cli, &classify.id, &classify_undo_path, &[], true).unwrap();
        assert!(source.join("Inbox/baseline.txt").is_file());
        let store = StateStore::open(&state_path).unwrap();
        assert_eq!(
            store
                .inbox_items(&workspace.id)
                .unwrap()
                .into_iter()
                .find(|item| item.relative_path == "Inbox/baseline.txt")
                .unwrap()
                .state,
            InboxState::Pending
        );
        drop(store);

        let reclassify_root = root.path().join("reclassify-cycle");
        run_cycle(&cli, &workspace.id, Some(&reclassify_root), true, true).unwrap();
        assert!(source.join("Library/Documents/baseline.txt").is_file());

        fs::rename(
            source.join("Library/Documents/baseline.txt"),
            source.join("baseline.txt"),
        )
        .unwrap();
        fs::create_dir(source.join("NewManualDirectory")).unwrap();
        fs::write(source.join("NewManualDirectory/note.txt"), b"manual").unwrap();
        let intent_cycle_root = root.path().join("intent-cycle");
        run_cycle(&cli, &workspace.id, Some(&intent_cycle_root), true, true).unwrap();
        assert!(source.join("baseline.txt").is_file());
        assert!(source.join("Kept/NewManualDirectory/note.txt").is_file());
        assert!(
            intent_cycle_root
                .join("directory-adoption-plan.json")
                .is_file()
        );
        assert!(
            intent_cycle_root
                .join("directory-adoption-apply.json")
                .is_file()
        );
        fs::rename(
            source.join("baseline.txt"),
            source.join("Library/Documents/baseline.txt"),
        )
        .unwrap();

        let reprocess_root = root.path().join("reprocess-cycle");
        reprocess(
            &cli,
            &workspace.id,
            ReprocessArea::Library,
            &["Documents/baseline.txt".into()],
            false,
            Some(&reprocess_root),
            true,
            true,
        )
        .unwrap();
        assert!(source.join("Inbox/baseline.txt").is_file());
        let store = StateStore::open(&state_path).unwrap();
        let reprocess_run = store
            .managed_runs(&workspace.id)
            .unwrap()
            .into_iter()
            .find(|run| {
                run.plan_path
                    .as_deref()
                    .is_some_and(|path| path.ends_with("reprocess-plan.json"))
            })
            .unwrap();
        drop(store);
        undo_run(
            &cli,
            &reprocess_run.id,
            &root.path().join("reprocess-undo.json"),
            &[],
            true,
        )
        .unwrap();
        assert!(source.join("Library/Documents/baseline.txt").is_file());

        set_workspace_enabled(&cli, &workspace.id, false).unwrap();
        assert!(run_cycle(&cli, &workspace.id, None, false, false).is_err());
        reconcile_workspace(&cli, &workspace.id).unwrap();
        edit_workspace(&cli, &workspace.id, Some(2), Some(1)).unwrap();
        set_workspace_enabled(&cli, &workspace.id, true).unwrap();
        run_cycle(&cli, &workspace.id, None, true, true).unwrap();
        let automatic_runs = state_path
            .parent()
            .unwrap()
            .join("managed-runs")
            .join(&workspace.id);
        assert!(automatic_runs.read_dir().unwrap().next().is_some());

        set_rule_enabled(&cli, &rule.id, false).unwrap();
        let store = StateStore::open(&state_path).unwrap();
        assert!(!store.rule(&rule.id).unwrap().unwrap().enabled);
        drop(store);
        assert!(remove_rule(&cli, &rule.id, false).is_err());
        assert!(
            StateStore::open(&state_path)
                .unwrap()
                .rule(&rule.id)
                .unwrap()
                .is_some()
        );
        remove_rule(&cli, &rule.id, true).unwrap();
        assert!(
            StateStore::open(&state_path)
                .unwrap()
                .rule(&rule.id)
                .unwrap()
                .is_none()
        );

        let mut changed_folders = managed_folders;
        changed_folders.folders[0].description.push_str(" changed");
        fs::write(
            &workspace.folder_set_path,
            serde_json::to_vec_pretty(&changed_folders).unwrap(),
        )
        .unwrap();
        assert!(list_rules(&cli, &workspace.id).is_err());
        set_workspace_enabled(&cli, &workspace.id, false).unwrap();
        remove_workspace(&cli, &workspace.id, true).unwrap();
        assert!(
            StateStore::open(&state_path)
                .unwrap()
                .managed_workspace(&workspace.id)
                .unwrap()
                .is_none()
        );
        assert!(source.join("Library/Documents/baseline.txt").is_file());
    }
}
