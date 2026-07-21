use std::{
    collections::HashSet,
    fs,
    io::{self, IsTerminal},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use directories::ProjectDirs;
use serde::Serialize;
use temari_core::{
    ApplySession, ApplyState, Config, FolderSet, InboxState, LocalContentExtractor, LocalRule,
    ManagedRun, ManagedRunKind, ManagedSetupPlan, ManagedSetupSession, ManagedSetupState,
    ManagedWorkspace, MonitorRecord, MonitoringOptions, OpenAiCompatibleModel, Plan, RuleSet,
    RunState, StateStore, UndoMoveOutcome, UndoState, apply_managed_setup, apply_monitoring_plan,
    apply_plan, build_managed_setup_plan, build_stage_to_inbox_plan, canonical_source_identity,
    filter_inbox_candidates, fingerprint_candidate, inbox_file_candidates, library_folder_set,
    persist_monitoring_plan, plan_monitor_candidates, resume_managed_setup, root_file_candidates,
    undo_managed_setup, undo_session, undo_session_files,
};

use crate::{
    Cli, approval_mode, confirm_mutation, create_run_directory, print_output_result, write_artifact,
};

static ID_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
    /// Configure deterministic local routing rules for managed workspaces.
    Rule {
        #[command(subcommand)]
        command: RuleCommand,
    },
    /// Run one staging and classification cycle.
    Run {
        /// Managed workspace ID.
        id: String,
        /// New directory for cycle artifacts and journals.
        #[arg(long)]
        out: PathBuf,
        /// Apply each generated plan after writing it.
        #[arg(long)]
        apply: bool,
        /// Confirm filesystem mutations without prompting.
        #[arg(long)]
        yes: bool,
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
        /// Original source-relative file to undo; repeat for multiple files.
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
        ManagedCommand::Rule { command } => run_rule(cli, command),
        ManagedCommand::Run {
            id,
            out,
            apply,
            yes,
        } => run_cycle(cli, id, out, *apply, *yes),
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
    let managed_folders = library_folder_set(&raw_folders)?;
    let context = ManagedContext::new(cli)?;
    ensure_outside(&context.state, source, "state database")?;
    let mut store = context.store()?;
    ensure_no_monitor_overlap(&store, source)?;
    confirm(cli, yes, "Apply this managed setup plan? [y/N] ")?;
    create_run_directory(run_directory, source)?;
    let run_directory = fs::canonicalize(run_directory)?;
    let folders_path = run_directory.join("folders.json");
    let setup_path = run_directory.join("setup-session.json");
    write_artifact(&folders_path, &managed_folders)?;

    let setup = match apply_managed_setup(&plan, &setup_path) {
        Ok(session) => session,
        Err(error) => {
            if setup_path.exists() {
                eprintln!("managed setup recovery journal: {}", setup_path.display());
            }
            return Err(error.into());
        }
    };
    if setup.state != ManagedSetupState::Completed {
        eprintln!("managed setup recovery journal: {}", setup_path.display());
        bail!("managed setup finished with {:?}", setup.state);
    }

    let now = unix_ms()?;
    let workspace_id = new_id("workspace")?;
    let monitor_id = new_id("managed-monitor")?;
    let folder_digest = managed_folders.sha256()?;
    let monitor = MonitorRecord {
        id: monitor_id.clone(),
        source: plan.source.clone(),
        source_identity: plan.source_identity.clone(),
        folder_set_path: path_text(&folders_path, "managed folder set")?,
        folder_set_sha256: folder_digest.clone(),
        interval_seconds: retention_seconds.max(10),
        enabled: true,
        last_checked_unix_ms: None,
        created_unix_ms: now,
        updated_unix_ms: now,
        deleted_unix_ms: None,
    };
    let workspace = ManagedWorkspace {
        id: workspace_id.clone(),
        monitor_id: monitor_id.clone(),
        source: plan.source.clone(),
        source_identity: plan.source_identity.clone(),
        folder_set_path: path_text(&folders_path, "managed folder set")?,
        folder_set_sha256: folder_digest,
        retention_seconds,
        settle_seconds,
        enabled: true,
        setup_session_path: Some(path_text(&setup_path, "managed setup session")?),
        created_unix_ms: now,
        updated_unix_ms: now,
    };
    let activation = (|| -> Result<()> {
        store.insert_monitor(&monitor)?;
        if let Err(error) = store.insert_managed_workspace(&workspace) {
            let _ = store.remove_monitor(&monitor_id, unix_ms()?);
            return Err(error.into());
        }
        let setup_run = ManagedRun {
            id: new_id("managed-setup")?,
            workspace_id: workspace_id.clone(),
            kind: ManagedRunKind::Setup,
            state: RunState::Completed,
            plan_path: Some(path_text(&plan_path, "managed setup Plan")?),
            apply_path: Some(path_text(&setup_path, "managed setup session")?),
            undo_path: None,
            started_unix_ms: i64::try_from(setup.started_unix_ms)
                .context("managed setup start time is too large")?,
            finished_unix_ms: setup
                .finished_unix_ms
                .map(i64::try_from)
                .transpose()
                .context("managed setup finish time is too large")?,
            move_count: setup.moves.len() as u64,
            error: None,
        };
        if let Err(error) = store.insert_managed_run(&setup_run) {
            let _ = store.delete_managed_workspace(&workspace_id);
            let _ = store.remove_monitor(&monitor_id, unix_ms()?);
            return Err(error.into());
        }
        Ok(())
    })();
    if let Err(error) = activation {
        eprintln!("managed setup completed but activation failed");
        eprintln!("managed setup recovery journal: {}", setup_path.display());
        return Err(error);
    }
    print_value(
        cli,
        &serde_json::json!({
            "workspace_id": workspace_id,
            "setup_session": setup_path,
            "folder_set": folders_path,
        }),
        &workspace_id,
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
    let store = ManagedContext::new(cli)?.store()?;
    let workspace = require_workspace(&store, id)?;
    let inbox = store.inbox_items(id)?;
    let runs = store.managed_runs(id)?;
    if cli.json {
        print_json(&serde_json::json!({
            "workspace": ManagedWorkspaceView::from(&workspace),
            "inbox": inbox,
            "runs": runs,
        }))
    } else {
        println!("Workspace: {}", workspace.id);
        println!("Source: {}", workspace.source);
        println!("Inbox items: {}", inbox.len());
        println!("Runs: {}", runs.len());
        for item in inbox {
            println!(
                "  {:?}\t{}\teligible {}",
                item.state, item.relative_path, item.eligible_unix_ms
            );
        }
        Ok(())
    }
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

fn run_cycle(cli: &Cli, id: &str, out: &Path, apply: bool, yes: bool) -> Result<()> {
    if apply != yes {
        bail!("--apply and --yes must be supplied together");
    }
    let context = ManagedContext::new(cli)?;
    let mut store = context.store()?;
    let workspace = require_workspace(&store, id)?;
    validate_workspace(&context, &store, &workspace)?;
    let source = Path::new(&workspace.source);
    create_run_directory(out, source)?;
    let out = fs::canonicalize(out)?;

    let mut results = Vec::new();
    let root_candidates = root_file_candidates(source)?;
    if !root_candidates.is_empty() {
        results.push(run_stage(
            &mut store,
            &workspace,
            &out,
            &root_candidates,
            apply,
        )?);
    }

    observe_inbox(&mut store, &workspace, unix_ms()?)?;
    let inbox_candidates = inbox_file_candidates(source)?;
    let eligible_paths = store
        .eligible_items(id, unix_ms()?)?
        .into_iter()
        .map(|item| item.relative_path)
        .collect::<HashSet<_>>();
    let eligible = filter_inbox_candidates(&inbox_candidates, &HashSet::new(), &eligible_paths)?;
    results.push(run_classify(
        cli, &mut store, &workspace, &out, &eligible, apply,
    )?);

    if cli.json {
        print_json(&results)
    } else {
        for result in results {
            println!(
                "{}\t{:?}\t{:?}\t{} moves{}",
                result.id,
                result.kind,
                result.state,
                result.move_count,
                result
                    .plan_path
                    .as_deref()
                    .map(|path| format!("\t{path}"))
                    .unwrap_or_default()
            );
        }
        Ok(())
    }
}

fn run_stage(
    store: &mut StateStore,
    workspace: &ManagedWorkspace,
    out: &Path,
    candidates: &[temari_core::FileCandidate],
    apply: bool,
) -> Result<ManagedRun> {
    let id = new_id("managed-stage")?;
    let plan = build_stage_to_inbox_plan(Path::new(&workspace.source), candidates)?;
    let plan_path = out.join("stage-plan.json");
    write_artifact(&plan_path, &plan)?;
    let mut run = planned_run(&id, &workspace.id, ManagedRunKind::Stage, &plan_path, &plan)?;
    store.insert_managed_run(&run)?;
    if apply {
        apply_indexed_run(store, workspace, &mut run)?;
    }
    Ok(run)
}

fn run_classify(
    cli: &Cli,
    store: &mut StateStore,
    workspace: &ManagedWorkspace,
    out: &Path,
    candidates: &[temari_core::FileCandidate],
    apply: bool,
) -> Result<ManagedRun> {
    let id = new_id("managed-classify")?;
    let started = unix_ms()?;
    if candidates.is_empty() {
        let run = ManagedRun {
            id,
            workspace_id: workspace.id.clone(),
            kind: ManagedRunKind::Classify,
            state: RunState::Noop,
            plan_path: None,
            apply_path: None,
            undo_path: None,
            started_unix_ms: started,
            finished_unix_ms: Some(unix_ms()?),
            move_count: 0,
            error: None,
        };
        store.insert_managed_run(&run)?;
        return Ok(run);
    }

    let monitor = store
        .monitor(&workspace.monitor_id)?
        .filter(|monitor| monitor.deleted_unix_ms.is_none())
        .ok_or_else(|| anyhow::anyhow!("managed monitor is missing"))?;
    let folders = FolderSet::load(Path::new(&workspace.folder_set_path))?;
    let rules = store.active_rules(&monitor.id)?;
    let config = Config::load(&cli.config)
        .with_context(|| format!("failed to load {}", cli.config.display()))?;
    let model = OpenAiCompatibleModel::new(&config.model)?;
    let extractor = LocalContentExtractor::new(config.privacy.extraction.clone());
    let monitoring = plan_monitor_candidates(
        store,
        &monitor,
        &folders,
        &rules,
        candidates,
        &model,
        &extractor,
        MonitoringOptions::from_config(&config),
    )?;
    store.start_run(&id, &monitor.id, started)?;
    if monitoring.plan.entries.is_empty() {
        store.finish_noop(&id, monitoring.stats.total_files as u64, unix_ms()?)?;
        let run = ManagedRun {
            id,
            workspace_id: workspace.id.clone(),
            kind: ManagedRunKind::Classify,
            state: RunState::Noop,
            plan_path: None,
            apply_path: None,
            undo_path: None,
            started_unix_ms: started,
            finished_unix_ms: Some(unix_ms()?),
            move_count: 0,
            error: None,
        };
        store.insert_managed_run(&run)?;
        return Ok(run);
    }
    let plan_path = out.join("classify-plan.json");
    persist_monitoring_plan(store, &id, &plan_path, &monitoring)?;
    let mut run = planned_run(
        &id,
        &workspace.id,
        ManagedRunKind::Classify,
        &plan_path,
        &monitoring.plan,
    )?;
    store.insert_managed_run(&run)?;
    mark_inbox_entries(store, workspace, &monitoring.plan, InboxState::Planned, &id)?;
    if apply {
        apply_indexed_run(store, workspace, &mut run)?;
    }
    Ok(run)
}

fn apply_saved_run(cli: &Cli, run_id: &str, yes: bool) -> Result<()> {
    confirm(cli, yes, "Apply this managed run? [y/N] ")?;
    let context = ManagedContext::new(cli)?;
    let mut store = context.store()?;
    let mut run = store
        .managed_run(run_id)?
        .ok_or_else(|| anyhow::anyhow!("unknown managed run {run_id:?}"))?;
    if run.state != RunState::Planned {
        bail!("managed run {run_id:?} is not waiting for apply");
    }
    let workspace = require_workspace(&store, &run.workspace_id)?;
    validate_workspace(&context, &store, &workspace)?;
    apply_indexed_run(&mut store, &workspace, &mut run)?;
    print_value(cli, &run, &run.id)
}

fn resume_run(cli: &Cli, run_id: &str, yes: bool) -> Result<()> {
    confirm(cli, yes, "Resume this managed run? [y/N] ")?;
    let context = ManagedContext::new(cli)?;
    let mut store = context.store()?;
    let mut run = store
        .managed_run(run_id)?
        .ok_or_else(|| anyhow::anyhow!("unknown managed run {run_id:?}"))?;
    if run.state != RunState::NeedsResume {
        bail!("managed run {run_id:?} does not need resume");
    }
    let workspace = require_workspace(&store, &run.workspace_id)?;
    validate_workspace(&context, &store, &workspace)?;
    let apply_path = run
        .apply_path
        .clone()
        .ok_or_else(|| anyhow::anyhow!("managed run has no Apply session"))?;
    let current = ApplySession::load(Path::new(&apply_path))?;
    if current.state != ApplyState::Running {
        bail!(
            "managed Apply session is not running; found {:?}",
            current.state
        );
    }
    let session = match temari_core::resume_apply_session(Path::new(&apply_path)) {
        Ok(session) => session,
        Err(error) => {
            finalize_apply_error(
                &mut store,
                &mut run,
                Path::new(&apply_path),
                &error.to_string(),
            )?;
            return Err(error.into());
        }
    };
    let plan_path = run
        .plan_path
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("managed run has no Plan path"))?;
    let plan = Plan::load(Path::new(plan_path))?;
    if run.kind == ManagedRunKind::Classify {
        store.reconcile_applying_runs(Some(&workspace.monitor_id), unix_ms()?)?;
    }
    if session.state == ApplyState::Completed {
        run.state = RunState::Completed;
        run.finished_unix_ms = Some(unix_ms()?);
        run.error = None;
        store.update_managed_run(&run)?;
        match run.kind {
            ManagedRunKind::Stage => observe_inbox(&mut store, &workspace, unix_ms()?)?,
            ManagedRunKind::Classify => {
                mark_inbox_entries(&mut store, &workspace, &plan, InboxState::Moved, &run.id)?
            }
            ManagedRunKind::Setup => bail!("setup runs use managed resume-setup"),
        }
    } else {
        run.state = if session.state == ApplyState::Running {
            RunState::NeedsResume
        } else {
            RunState::Failed
        };
        run.finished_unix_ms = Some(unix_ms()?);
        run.error = Some(format!("apply resume finished with {:?}", session.state));
        store.update_managed_run(&run)?;
        bail!("managed Apply resume finished with {:?}", session.state);
    }
    print_value(cli, &run, &run.id)
}

fn apply_indexed_run(
    store: &mut StateStore,
    workspace: &ManagedWorkspace,
    run: &mut ManagedRun,
) -> Result<()> {
    let plan_path = run
        .plan_path
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("managed run has no Plan path"))?;
    let plan = Plan::load(Path::new(plan_path))?;
    let parent = Path::new(plan_path)
        .parent()
        .ok_or_else(|| anyhow::anyhow!("managed Plan has no parent directory"))?;
    let apply_path = parent.join(match run.kind {
        ManagedRunKind::Stage => "stage-apply.json",
        ManagedRunKind::Classify => "classify-apply.json",
        ManagedRunKind::Setup => bail!("setup runs cannot be applied through apply-run"),
    });
    let apply_time = unix_ms()?;
    run.state = RunState::Applying;
    run.apply_path = Some(path_text(&apply_path, "managed Apply session")?);
    store.update_managed_run(run)?;
    let applied = match run.kind {
        ManagedRunKind::Stage => apply_plan(&plan, &apply_path),
        ManagedRunKind::Classify => (|| {
            let lock = temari_core::SourceLock::acquire(Path::new(&workspace.source))?;
            apply_monitoring_plan(store, &run.id, &plan, &apply_path, &lock, apply_time)
        })(),
        ManagedRunKind::Setup => unreachable!(),
    };
    let session = match applied {
        Ok(session) => session,
        Err(error) => {
            finalize_apply_error(store, run, &apply_path, &error.to_string())?;
            return Err(error.into());
        }
    };
    if session.state != ApplyState::Completed {
        run.state = RunState::Failed;
        run.finished_unix_ms = Some(unix_ms()?);
        run.error = Some(format!("apply session finished with {:?}", session.state));
        store.update_managed_run(run)?;
        bail!("managed apply finished with {:?}", session.state);
    }
    run.state = RunState::Completed;
    run.finished_unix_ms = Some(unix_ms()?);
    store.update_managed_run(run)?;
    match run.kind {
        ManagedRunKind::Stage => observe_inbox(store, workspace, unix_ms()?)?,
        ManagedRunKind::Classify => {
            mark_inbox_entries(store, workspace, &plan, InboxState::Moved, &run.id)?
        }
        ManagedRunKind::Setup => unreachable!(),
    }
    Ok(())
}

fn history(cli: &Cli, id: &str, limit: u32) -> Result<()> {
    let store = ManagedContext::new(cli)?.store()?;
    require_workspace(&store, id)?;
    let runs = store.recent_managed_moves(id, limit)?;
    if cli.json {
        print_json(&runs)
    } else {
        for run in runs {
            println!(
                "{}\t{:?}\t{:?}\t{} moves\t{}",
                run.id, run.kind, run.state, run.move_count, run.started_unix_ms
            );
        }
        Ok(())
    }
}

fn undo_run(cli: &Cli, run_id: &str, out: &Path, files: &[String], yes: bool) -> Result<()> {
    confirm(cli, yes, "Undo this managed run? [y/N] ")?;
    let context = ManagedContext::new(cli)?;
    let mut store = context.store()?;
    let mut run = store
        .managed_run(run_id)?
        .ok_or_else(|| anyhow::anyhow!("unknown managed run {run_id:?}"))?;
    if run.state != RunState::Completed {
        bail!("managed run must be completed before Undo");
    }
    let apply_path = run
        .apply_path
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("managed run has no Apply session"))?;
    let apply = ApplySession::load(Path::new(apply_path))?;
    let undo = if files.is_empty() {
        undo_session(&apply, out)?
    } else {
        undo_session_files(&apply, files, out)?
    };
    run.undo_path = Some(path_text(out, "managed Undo session")?);
    store.update_managed_run(&run)?;

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
    let classify_monitor_id = if run.kind == ManagedRunKind::Classify {
        Some(require_workspace(&store, &run.workspace_id)?.monitor_id)
    } else {
        None
    };
    for movement in apply
        .moves
        .iter()
        .filter(|movement| restored.contains(movement.file_id.as_str()))
    {
        match run.kind {
            ManagedRunKind::Stage => {
                if store
                    .inbox_item(&run.workspace_id, movement.fingerprint.identity.clone())?
                    .is_some()
                {
                    store.delete_inbox_item(
                        &run.workspace_id,
                        movement.fingerprint.identity.clone(),
                    )?;
                }
            }
            ManagedRunKind::Classify => {
                store.set_inbox_item_state(
                    &run.workspace_id,
                    movement.fingerprint.identity.clone(),
                    InboxState::Pending,
                    None,
                )?;
                store.forget_processed_file(
                    classify_monitor_id
                        .as_deref()
                        .ok_or_else(|| anyhow::anyhow!("classify run has no monitor ID"))?,
                    movement.fingerprint.identity.clone(),
                )?;
            }
            ManagedRunKind::Setup => {}
        }
    }
    if undo.state != UndoState::Completed {
        bail!(
            "managed Undo finished with {:?}; inspect {}",
            undo.state,
            out.display()
        );
    }
    print_output_result(cli, out)
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

fn observe_inbox(store: &mut StateStore, workspace: &ManagedWorkspace, now: i64) -> Result<()> {
    for candidate in inbox_file_candidates(Path::new(&workspace.source))? {
        let fingerprint = fingerprint_candidate(Path::new(&workspace.source), &candidate)?;
        store.upsert_observation(&workspace.id, &fingerprint, &candidate.source_path, now)?;
    }
    Ok(())
}

fn mark_inbox_entries(
    store: &mut StateStore,
    workspace: &ManagedWorkspace,
    plan: &Plan,
    state: InboxState,
    run_id: &str,
) -> Result<()> {
    for entry in &plan.entries {
        if store
            .inbox_item(&workspace.id, entry.source_fingerprint.identity.clone())?
            .is_some()
        {
            store.set_inbox_item_state(
                &workspace.id,
                entry.source_fingerprint.identity.clone(),
                state,
                Some(run_id),
            )?;
        }
    }
    Ok(())
}

fn planned_run(
    id: &str,
    workspace_id: &str,
    kind: ManagedRunKind,
    plan_path: &Path,
    plan: &Plan,
) -> Result<ManagedRun> {
    Ok(ManagedRun {
        id: id.into(),
        workspace_id: workspace_id.into(),
        kind,
        state: RunState::Planned,
        plan_path: Some(plan_path.display().to_string()),
        apply_path: None,
        undo_path: None,
        started_unix_ms: unix_ms()?,
        finished_unix_ms: None,
        move_count: plan.entries.len() as u64,
        error: None,
    })
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
    {
        bail!("managed monitor no longer matches its workspace");
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

fn ensure_no_monitor_overlap(store: &StateStore, source: &Path) -> Result<()> {
    for monitor in store.active_monitors()? {
        let existing = Path::new(&monitor.source);
        if source.starts_with(existing) || existing.starts_with(source) {
            bail!(
                "managed source overlaps active monitor {} at {}",
                monitor.id,
                existing.display()
            );
        }
    }
    Ok(())
}

fn finalize_apply_error(
    store: &mut StateStore,
    run: &mut ManagedRun,
    apply_path: &Path,
    message: &str,
) -> Result<()> {
    let running =
        ApplySession::load(apply_path).is_ok_and(|session| session.state == ApplyState::Running);
    run.state = if running {
        RunState::NeedsResume
    } else {
        RunState::Failed
    };
    run.finished_unix_ms = Some(unix_ms()?);
    run.error = Some(sanitize_error(message));
    store.update_managed_run(run)?;
    Ok(())
}

fn sanitize_error(message: &str) -> String {
    let value = message
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    let value = value.trim();
    if value.is_empty() {
        "managed apply failed".into()
    } else {
        value.into()
    }
}

fn deactivate_undone_setup(cli: &Cli, session: &Path) -> Result<()> {
    let session = fs::canonicalize(session)
        .with_context(|| format!("failed to resolve {}", session.display()))?;
    let context = ManagedContext::new(cli)?;
    let mut store = context.store()?;
    let Some(mut workspace) = store.managed_workspaces()?.into_iter().find(|workspace| {
        workspace
            .setup_session_path
            .as_deref()
            .is_some_and(|path| Path::new(path) == session)
    }) else {
        return Ok(());
    };
    store.set_monitor_enabled(&workspace.monitor_id, false, unix_ms()?)?;
    workspace.enabled = false;
    workspace.updated_unix_ms = unix_ms()?;
    store.update_managed_workspace(&workspace)?;
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
    }

    #[test]
    fn rejects_zero_activation_durations() {
        assert!(validate_activation_durations(0, 30).is_err());
        assert!(validate_activation_durations(60, 0).is_err());
        assert!(validate_activation_durations(60, 30).is_ok());
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
        let cycle_root = root.path().join("cycle");
        run_cycle(&cli, &workspace.id, &cycle_root, true, true).unwrap();
        assert!(source.join("Inbox/fresh.txt").is_file());
        let store = StateStore::open(&state_path).unwrap();
        let stage = store
            .managed_runs(&workspace.id)
            .unwrap()
            .into_iter()
            .find(|run| run.kind == ManagedRunKind::Stage)
            .unwrap();
        assert_eq!(stage.state, RunState::Completed);
        drop(store);

        let undo_path = root.path().join("stage-undo.json");
        undo_run(&cli, &stage.id, &undo_path, &[], true).unwrap();
        assert!(source.join("fresh.txt").is_file());
        let store = StateStore::open(&state_path).unwrap();
        assert!(
            store
                .inbox_items(&workspace.id)
                .unwrap()
                .iter()
                .all(|item| item.relative_path != "Inbox/fresh.txt")
        );
        drop(store);

        std::thread::sleep(std::time::Duration::from_millis(1_100));
        let classify_root = root.path().join("classify-cycle");
        run_cycle(&cli, &workspace.id, &classify_root, true, true).unwrap();
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
        run_cycle(&cli, &workspace.id, &reclassify_root, true, true).unwrap();
        assert!(source.join("Library/Documents/baseline.txt").is_file());

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
    }
}
