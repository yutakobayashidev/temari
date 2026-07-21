use std::{
    cell::RefCell,
    collections::HashMap,
    env, fs,
    io::{self, IsTerminal, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use directories::ProjectDirs;
use serde::Serialize;
use temari_core::{
    ApprovedFolder, Classification, Classifier, Config, ContentCandidate, FileCandidate, FolderSet,
    LocalContentExtractor, LocalRule, ModelConfig, MonitorRecord, MonitoringOptions,
    NameClassification, OpenAiCompatibleModel, RuleSet, RunState, SourceLock, StateStore,
    apply_monitoring_plan, canonical_source_identity, persist_monitoring_plan, plan_monitor_cycle,
};

static ID_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct LazyModel {
    config: ModelConfig,
    model: RefCell<Option<OpenAiCompatibleModel>>,
}

impl LazyModel {
    fn new(config: ModelConfig) -> Self {
        Self {
            config,
            model: RefCell::new(None),
        }
    }

    fn call<T>(
        &self,
        operation: impl FnOnce(&OpenAiCompatibleModel) -> Result<T, temari_core::Error>,
    ) -> Result<T, temari_core::Error> {
        if self.model.borrow().is_none() {
            *self.model.borrow_mut() = Some(OpenAiCompatibleModel::new(&self.config)?);
        }
        operation(
            self.model
                .borrow()
                .as_ref()
                .expect("lazy model was initialized"),
        )
    }
}

impl Classifier for LazyModel {
    fn classify_names(
        &self,
        files: &[FileCandidate],
        folders: &[ApprovedFolder],
    ) -> Result<Vec<NameClassification>, temari_core::Error> {
        self.call(|model| model.classify_names(files, folders))
    }

    fn classify_contents(
        &self,
        files: &[ContentCandidate],
        folders: &[ApprovedFolder],
    ) -> Result<Vec<Classification>, temari_core::Error> {
        self.call(|model| model.classify_contents(files, folders))
    }
}

pub struct MonitoringContext {
    config: PathBuf,
    state: PathBuf,
    json: bool,
    no_input: bool,
    verbose: u8,
}

impl MonitoringContext {
    pub fn from_cli(
        config: &Path,
        state: Option<&Path>,
        json: bool,
        no_input: bool,
        verbose: u8,
    ) -> Result<Self> {
        let state = match state {
            Some(path) => path.to_path_buf(),
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
        let state = resolved_target(&state)?;
        Ok(Self {
            config: config.to_path_buf(),
            state,
            json,
            no_input,
            verbose,
        })
    }

    fn store(&self) -> Result<StateStore> {
        StateStore::open(&self.state)
            .with_context(|| format!("failed to open monitoring state {}", self.state.display()))
    }
}

#[derive(Debug, Subcommand)]
pub enum MonitorCommand {
    /// Register one source and its approved folder set.
    Add {
        source: PathBuf,
        #[arg(long)]
        folders: PathBuf,
        #[arg(long, default_value_t = 300)]
        interval: u64,
        #[arg(long)]
        disabled: bool,
    },
    /// List registered monitors.
    List,
    /// Enable a monitor.
    Enable { id: String },
    /// Disable a monitor.
    Disable { id: String },
    /// Remove a monitor and disable its rules.
    Remove {
        id: String,
        #[arg(long)]
        yes: bool,
    },
    /// Run one check or a foreground polling loop.
    Run {
        /// Limit execution to one monitor, including a disabled monitor.
        #[arg(long)]
        monitor: Option<String>,
        /// Parent directory for immutable run artifacts.
        #[arg(long)]
        out: PathBuf,
        /// Check once and exit. Without this flag, polling continues in the foreground.
        #[arg(long)]
        once: bool,
        /// Apply each durable plan after it is created.
        #[arg(long)]
        apply: bool,
        /// Confirm unattended filesystem changes.
        #[arg(long)]
        yes: bool,
    },
    /// Apply a previously reviewed monitoring Plan.
    Apply {
        run: String,
        /// Apply without prompting.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum RuleCommand {
    /// Add a basename glob rule to a monitor.
    Add {
        #[arg(long)]
        monitor: String,
        #[arg(long = "name-glob")]
        name_glob: String,
        #[arg(long)]
        destination: String,
        #[arg(long, default_value_t = 50)]
        priority: i32,
        #[arg(long)]
        disabled: bool,
    },
    /// List a monitor's rules in matching order.
    List {
        #[arg(long)]
        monitor: String,
    },
    /// Enable a rule.
    Enable { id: String },
    /// Disable a rule.
    Disable { id: String },
    /// Remove a rule.
    Remove {
        id: String,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum HistoryCommand {
    /// List recent monitoring runs.
    List {
        #[arg(long)]
        monitor: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Show one run and its staged file records.
    Show { id: String },
}

pub fn run_monitor(context: MonitoringContext, command: &MonitorCommand) -> Result<()> {
    match command {
        MonitorCommand::Add {
            source,
            folders,
            interval,
            disabled,
        } => add_monitor(&context, source, folders, *interval, *disabled),
        MonitorCommand::List => list_monitors(&context),
        MonitorCommand::Enable { id } => set_monitor_enabled(&context, id, true),
        MonitorCommand::Disable { id } => set_monitor_enabled(&context, id, false),
        MonitorCommand::Remove { id, yes } => remove_monitor(&context, id, *yes),
        MonitorCommand::Run {
            monitor,
            out,
            once,
            apply,
            yes,
        } => run_monitors(&context, monitor.as_deref(), out, *once, *apply, *yes),
        MonitorCommand::Apply { run, yes } => apply_saved_run(&context, run, *yes),
    }
}

pub fn run_rule(context: MonitoringContext, command: &RuleCommand) -> Result<()> {
    match command {
        RuleCommand::Add {
            monitor,
            name_glob,
            destination,
            priority,
            disabled,
        } => add_rule(
            &context,
            monitor,
            name_glob,
            destination,
            *priority,
            *disabled,
        ),
        RuleCommand::List { monitor } => list_rules(&context, monitor),
        RuleCommand::Enable { id } => set_rule_enabled(&context, id, true),
        RuleCommand::Disable { id } => set_rule_enabled(&context, id, false),
        RuleCommand::Remove { id, yes } => remove_rule(&context, id, *yes),
    }
}

pub fn run_history(context: MonitoringContext, command: &HistoryCommand) -> Result<()> {
    let store = context.store()?;
    match command {
        HistoryCommand::List { monitor, limit } => {
            let runs = store.recent_runs(monitor.as_deref(), *limit)?;
            if context.json {
                print_json(&runs)
            } else {
                for run in runs {
                    println!(
                        "{}\t{}\t{:?}\t{} files\t{}",
                        run.id, run.monitor_id, run.state, run.total_files, run.started_unix_ms
                    );
                }
                Ok(())
            }
        }
        HistoryCommand::Show { id } => {
            let run = store
                .run(id)?
                .ok_or_else(|| anyhow::anyhow!("unknown monitoring run {id:?}"))?;
            let files = store.staged_files(id)?;
            if context.json {
                print_json(&serde_json::json!({ "run": run, "files": files }))
            } else {
                println!("Run: {}", run.id);
                println!("Monitor: {}", run.monitor_id);
                println!("State: {:?}", run.state);
                println!("Started: {}", run.started_unix_ms);
                println!("Plan: {}", run.plan_path.as_deref().unwrap_or("-"));
                println!(
                    "Matches: {} rule, {} name, {} content, {} fallback",
                    run.rule_matches, run.name_matches, run.content_matches, run.fallback_matches
                );
                if let Some(error) = run.error {
                    println!("Error: {error}");
                }
                for file in files {
                    println!(
                        "  {} -> {} ({:?})",
                        file.relative_path, file.destination_id, file.classification_basis
                    );
                }
                Ok(())
            }
        }
    }
}

fn add_monitor(
    context: &MonitoringContext,
    source: &Path,
    folders: &Path,
    interval: u64,
    disabled: bool,
) -> Result<()> {
    let (source, source_identity) = canonical_source_identity(source)?;
    let folders = fs::canonicalize(folders)
        .with_context(|| format!("failed to resolve {}", folders.display()))?;
    ensure_outside(&folders, &source, "folder-set artifact")?;
    ensure_outside(&context.state, &source, "state database")?;
    let folder_set = FolderSet::load(&folders)?;
    let source_text = source_text(&source)?;
    if folder_set.source != source_text {
        bail!("folder set does not belong to source {}", source.display());
    }
    let now = unix_ms()?;
    let monitor = MonitorRecord {
        id: new_id("monitor")?,
        source: source_text,
        source_identity,
        folder_set_path: source_text_from_path(&folders, "folder-set path")?,
        folder_set_sha256: folder_set.sha256()?,
        interval_seconds: interval,
        enabled: !disabled,
        last_checked_unix_ms: None,
        created_unix_ms: now,
        updated_unix_ms: now,
        deleted_unix_ms: None,
    };
    let mut store = context.store()?;
    store.insert_monitor(&monitor)?;
    print_record(context, &monitor, &monitor.id)
}

fn list_monitors(context: &MonitoringContext) -> Result<()> {
    let records = context.store()?.active_monitors()?;
    if context.json {
        print_json(&records)
    } else {
        for monitor in records {
            println!(
                "{}\t{}\t{}\t{}s",
                monitor.id,
                if monitor.enabled {
                    "enabled"
                } else {
                    "disabled"
                },
                monitor.source,
                monitor.interval_seconds
            );
        }
        Ok(())
    }
}

fn set_monitor_enabled(context: &MonitoringContext, id: &str, enabled: bool) -> Result<()> {
    let mut store = context.store()?;
    if enabled {
        let monitor = active_monitor(&store, id)?;
        validate_monitor_definition(&monitor)?;
    }
    store.set_monitor_enabled(id, enabled, unix_ms()?)?;
    print_change(
        context,
        "monitor",
        id,
        if enabled { "enabled" } else { "disabled" },
    )
}

fn remove_monitor(context: &MonitoringContext, id: &str, yes: bool) -> Result<()> {
    confirm_removal(
        context,
        yes,
        &format!("Remove monitor {id:?} and disable its rules?"),
    )?;
    let mut store = context.store()?;
    store.remove_monitor(id, unix_ms()?)?;
    print_change(context, "monitor", id, "removed")
}

fn add_rule(
    context: &MonitoringContext,
    monitor_id: &str,
    name_glob: &str,
    destination: &str,
    priority: i32,
    disabled: bool,
) -> Result<()> {
    let mut store = context.store()?;
    let monitor = active_monitor(&store, monitor_id)?;
    let folders = FolderSet::load(Path::new(&monitor.folder_set_path))?;
    if folders.sha256()? != monitor.folder_set_sha256 {
        bail!("monitor folder set has changed; register a new monitor after reviewing it");
    }
    let rule = LocalRule {
        id: new_id("rule")?,
        monitor_id: monitor_id.to_owned(),
        name_glob: name_glob.to_owned(),
        destination_id: destination.to_owned(),
        priority,
        enabled: !disabled,
    };
    let mut candidate = store.active_rules(monitor_id)?;
    candidate.push(rule.clone());
    RuleSet::compile(&candidate, &folders.folders)?;
    store.insert_rule(&rule, unix_ms()?)?;
    print_record(context, &rule, &rule.id)
}

fn list_rules(context: &MonitoringContext, monitor: &str) -> Result<()> {
    let store = context.store()?;
    active_monitor(&store, monitor)?;
    let rules = store.active_rules(monitor)?;
    if context.json {
        print_json(&rules)
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

fn set_rule_enabled(context: &MonitoringContext, id: &str, enabled: bool) -> Result<()> {
    let mut store = context.store()?;
    if enabled {
        let mut rule = store
            .rule(id)?
            .ok_or_else(|| anyhow::anyhow!("unknown active rule {id:?}"))?;
        let monitor = active_monitor(&store, &rule.monitor_id)?;
        validate_monitor_definition(&monitor)?;
        let folders = FolderSet::load(Path::new(&monitor.folder_set_path))?;
        let mut rules = store.active_rules(&rule.monitor_id)?;
        rule.enabled = true;
        if let Some(existing) = rules.iter_mut().find(|candidate| candidate.id == id) {
            *existing = rule;
        }
        RuleSet::compile(&rules, &folders.folders)?;
    }
    store.set_rule_enabled(id, enabled, unix_ms()?)?;
    print_change(
        context,
        "rule",
        id,
        if enabled { "enabled" } else { "disabled" },
    )
}

fn remove_rule(context: &MonitoringContext, id: &str, yes: bool) -> Result<()> {
    confirm_removal(context, yes, &format!("Remove rule {id:?}?"))?;
    let mut store = context.store()?;
    store.remove_rule(id, unix_ms()?)?;
    print_change(context, "rule", id, "removed")
}

fn run_monitors(
    context: &MonitoringContext,
    selected: Option<&str>,
    out: &Path,
    once: bool,
    apply: bool,
    yes: bool,
) -> Result<()> {
    if apply != yes {
        bail!("--apply and --yes must be supplied together");
    }
    if !once && !apply {
        bail!("continuous monitoring requires --apply --yes; use --once for plan-only checks");
    }
    let out = resolved_target(out)?;
    let config = Config::load(&context.config)
        .with_context(|| format!("failed to load {}", context.config.display()))?;
    let model = LazyModel::new(config.model.clone());
    let extractor = LocalContentExtractor::new(config.privacy.extraction.clone());
    let options = MonitoringOptions::from_config(&config);
    let mut store = context.store()?;
    for monitor in selected_monitors(&store, selected)? {
        ensure_outside(&out, Path::new(&monitor.source), "monitor output")?;
    }
    let out = prepare_artifact_root(&out)?;
    let reconciliation = store.reconcile_applying_runs(selected, unix_ms()?)?;
    if context.verbose > 0
        && (reconciliation.completed + reconciliation.needs_resume + reconciliation.failed > 0)
    {
        eprintln!(
            "Reconciled runs: {} completed, {} need resume, {} failed",
            reconciliation.completed, reconciliation.needs_resume, reconciliation.failed
        );
    }

    let mut retry_after = HashMap::<String, i64>::new();
    loop {
        let monitors = selected_monitors(&store, selected)?;
        let now = unix_ms()?;
        let mut due = 0usize;
        let mut failures = 0usize;
        for monitor in monitors {
            if selected.is_none() && !monitor.enabled {
                continue;
            }
            if selected.is_none() && !is_due(&monitor, now) {
                continue;
            }
            if retry_after
                .get(&monitor.id)
                .is_some_and(|retry| now < *retry)
            {
                continue;
            }
            due += 1;
            if let Err(error) = run_monitor_cycle(
                context, &mut store, &monitor, &out, apply, &model, &extractor, options,
            ) {
                failures += 1;
                let retry_seconds = monitor.interval_seconds.min(30);
                retry_after.insert(
                    monitor.id.clone(),
                    now.saturating_add((retry_seconds as i64).saturating_mul(1000)),
                );
                eprintln!("monitor {} failed: {error:#}", monitor.id);
            } else {
                retry_after.remove(&monitor.id);
            }
        }
        if once {
            if failures > 0 {
                bail!("{failures} monitoring check(s) failed");
            }
            if context.verbose > 0 && due == 0 {
                eprintln!("No monitor was due");
            }
            return Ok(());
        }
        thread::sleep(next_poll_delay(&store, selected, unix_ms()?, &retry_after)?);
    }
}

fn apply_saved_run(context: &MonitoringContext, run_id: &str, yes: bool) -> Result<()> {
    let mut store = context.store()?;
    let run = store
        .run(run_id)?
        .ok_or_else(|| anyhow::anyhow!("unknown monitoring run {run_id:?}"))?;
    if run.state != RunState::Planned {
        bail!("monitoring run {run_id:?} is not waiting for apply");
    }
    let plan_path = run
        .plan_path
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("monitoring run has no Plan path"))?;
    let plan = temari_core::Plan::load(Path::new(plan_path))?;
    if !yes && !context.no_input && io::stdin().is_terminal() && io::stderr().is_terminal() {
        eprintln!("Apply monitoring run {run_id}?");
        eprintln!("  Plan: {plan_path}");
        eprintln!("  Plan SHA-256: {}", plan.sha256()?);
        eprintln!("  Move {} file(s)", plan.entries.len());
        eprintln!("  Never overwrite; record completion before marking files processed");
    }
    confirm_action(
        context,
        yes,
        &format!("Apply the reviewed Plan for monitoring run {run_id:?}?"),
    )?;
    let apply_path = Path::new(plan_path)
        .parent()
        .ok_or_else(|| anyhow::anyhow!("monitoring Plan has no artifact directory"))?
        .join("apply-session.json");
    let lock = SourceLock::acquire(Path::new(&active_monitor(&store, &run.monitor_id)?.source))?;
    let session = apply_monitoring_plan(&mut store, run_id, &plan, &apply_path, &lock, unix_ms()?)?;
    if session.state != temari_core::ApplyState::Completed {
        bail!("apply session finished with {:?}", session.state);
    }
    if context.json {
        print_json(&serde_json::json!({
            "run_id": run_id,
            "state": "completed",
            "apply_session": apply_path.display().to_string(),
        }))
    } else {
        println!("{}", apply_path.display());
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn run_monitor_cycle(
    context: &MonitoringContext,
    store: &mut StateStore,
    monitor: &MonitorRecord,
    out: &Path,
    apply: bool,
    model: &LazyModel,
    extractor: &LocalContentExtractor,
    options: MonitoringOptions,
) -> Result<()> {
    let started = unix_ms()?;
    let run_id = new_id("run")?;
    store.start_run(&run_id, &monitor.id, started)?;
    let result = (|| -> Result<()> {
        let folder_set = FolderSet::load(Path::new(&monitor.folder_set_path))?;
        let rules = store.active_rules(&monitor.id)?;
        let lock = apply
            .then(|| SourceLock::acquire(Path::new(&monitor.source)))
            .transpose()?;
        let monitoring = plan_monitor_cycle(
            store,
            monitor,
            &folder_set,
            &rules,
            model,
            extractor,
            options,
        )?;
        if monitoring.plan.entries.is_empty() {
            store.finish_noop(&run_id, monitoring.stats.total_files as u64, unix_ms()?)?;
            print_cycle(context, monitor, &run_id, &monitoring.stats, None, "noop")?;
            return Ok(());
        }
        let directory = create_run_directory(out, &monitor.id, &run_id, &monitor.source)?;
        let plan_path = directory.join("plan.json");
        persist_monitoring_plan(store, &run_id, &plan_path, &monitoring)?;
        let mut state = "planned";
        if let Some(lock) = lock.as_ref() {
            let apply_path = directory.join("apply-session.json");
            let session = apply_monitoring_plan(
                store,
                &run_id,
                &monitoring.plan,
                &apply_path,
                lock,
                unix_ms()?,
            )?;
            if session.state != temari_core::ApplyState::Completed {
                bail!("apply session finished with {:?}", session.state);
            }
            state = "completed";
        }
        print_cycle(
            context,
            monitor,
            &run_id,
            &monitoring.stats,
            Some(&plan_path),
            state,
        )
    })();
    if result.is_ok() {
        store.update_monitor_check(&monitor.id, unix_ms()?)?;
    } else if let Err(error) = &result {
        if store
            .run(&run_id)?
            .is_some_and(|run| matches!(run.state, RunState::Planning | RunState::Planned))
        {
            store.finish_run(
                &run_id,
                RunState::Failed,
                unix_ms()?,
                Some(&error.to_string()),
            )?;
        }
    }
    result
}

fn selected_monitors(store: &StateStore, selected: Option<&str>) -> Result<Vec<MonitorRecord>> {
    match selected {
        Some(id) => Ok(vec![active_monitor(store, id)?]),
        None => Ok(store.active_monitors()?),
    }
}

fn active_monitor(store: &StateStore, id: &str) -> Result<MonitorRecord> {
    store
        .monitor(id)?
        .filter(|monitor| monitor.deleted_unix_ms.is_none())
        .ok_or_else(|| anyhow::anyhow!("unknown active monitor {id:?}"))
}

fn validate_monitor_definition(monitor: &MonitorRecord) -> Result<()> {
    let folder_set = FolderSet::load(Path::new(&monitor.folder_set_path))?;
    if folder_set.source != monitor.source || folder_set.sha256()? != monitor.folder_set_sha256 {
        bail!("monitor no longer matches its approved folder set");
    }
    let (source, identity) = canonical_source_identity(Path::new(&monitor.source))?;
    if source_text(&source)? != monitor.source || identity != monitor.source_identity {
        bail!("monitor source identity has changed");
    }
    Ok(())
}

fn is_due(monitor: &MonitorRecord, now: i64) -> bool {
    monitor.last_checked_unix_ms.is_none_or(|last| {
        now.saturating_sub(last) >= (monitor.interval_seconds as i64).saturating_mul(1000)
    })
}

fn next_poll_delay(
    store: &StateStore,
    selected: Option<&str>,
    now: i64,
    retry_after: &HashMap<String, i64>,
) -> Result<Duration> {
    let seconds = selected_monitors(store, selected)?
        .into_iter()
        .filter(|monitor| selected.is_some() || monitor.enabled)
        .map(|monitor| {
            let scheduled = monitor.last_checked_unix_ms.map_or(0, |last| {
                let due =
                    last.saturating_add((monitor.interval_seconds as i64).saturating_mul(1000));
                due.saturating_sub(now)
            });
            let retry = retry_after
                .get(&monitor.id)
                .map_or(0, |retry| retry.saturating_sub(now));
            scheduled.max(retry)
        })
        .min()
        .unwrap_or(10_000)
        .clamp(250, 60_000) as u64;
    Ok(Duration::from_millis(seconds))
}

fn prepare_artifact_root(path: &Path) -> Result<PathBuf> {
    let path = absolute_path(path)?;
    if !path.exists() {
        fs::create_dir_all(&path)
            .with_context(|| format!("failed to create {}", path.display()))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    }
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        bail!("monitor output must be a real directory");
    }
    Ok(fs::canonicalize(path)?)
}

fn create_run_directory(root: &Path, monitor: &str, run: &str, source: &str) -> Result<PathBuf> {
    ensure_outside(root, Path::new(source), "monitor output")?;
    validate_path_component(monitor, "monitor ID")?;
    validate_path_component(run, "run ID")?;
    let monitor_root = root.join(monitor);
    if !monitor_root.exists() {
        fs::create_dir(&monitor_root)?;
        fs::set_permissions(&monitor_root, fs::Permissions::from_mode(0o700))?;
    }
    let run_root = monitor_root.join(run);
    fs::create_dir(&run_root)?;
    fs::set_permissions(&run_root, fs::Permissions::from_mode(0o700))?;
    Ok(run_root)
}

fn ensure_outside(path: &Path, source: &Path, label: &str) -> Result<()> {
    let resolved = resolved_target(path)?;
    if resolved.starts_with(source) {
        bail!("{label} must be outside the monitored source");
    }
    Ok(())
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(env::current_dir()?.join(path))
    }
}

fn resolved_target(path: &Path) -> Result<PathBuf> {
    let absolute = normalize_absolute(&absolute_path(path)?)?;
    let name = absolute
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("path must include a final component"))?;
    let parent = absolute
        .parent()
        .ok_or_else(|| anyhow::anyhow!("path must have a parent directory"))?;
    let mut existing = parent;
    while !existing.exists() {
        existing = existing
            .parent()
            .ok_or_else(|| anyhow::anyhow!("path has no existing ancestor"))?;
    }
    let canonical = fs::canonicalize(existing)
        .with_context(|| format!("failed to resolve {}", existing.display()))?;
    let missing = parent.strip_prefix(existing)?;
    Ok(canonical.join(missing).join(name))
}

fn normalize_absolute(path: &Path) -> Result<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::RootDir => normalized.push(Path::new("/")),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    bail!("path escapes the filesystem root");
                }
            }
            std::path::Component::Normal(value) => normalized.push(value),
            std::path::Component::Prefix(_) => {
                bail!("unsupported path prefix on this platform")
            }
        }
    }
    if !normalized.is_absolute() {
        bail!("resolved path must be absolute");
    }
    Ok(normalized)
}

fn validate_path_component(value: &str, label: &str) -> Result<()> {
    let path = Path::new(value);
    if value.is_empty()
        || value.chars().any(char::is_control)
        || path.is_absolute()
        || path.components().count() != 1
        || matches!(value, "." | "..")
    {
        bail!("{label} must be one safe path component");
    }
    Ok(())
}

fn source_text(path: &Path) -> Result<String> {
    source_text_from_path(path, "source path")
}

fn source_text_from_path(path: &Path, label: &str) -> Result<String> {
    let value = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("{label} must be valid UTF-8"))?;
    if value.chars().any(char::is_control) {
        bail!("{label} must not contain control characters");
    }
    Ok(value.to_owned())
}

fn confirm_removal(context: &MonitoringContext, yes: bool, prompt: &str) -> Result<()> {
    confirm_action(context, yes, prompt)
}

fn confirm_action(context: &MonitoringContext, yes: bool, prompt: &str) -> Result<()> {
    if yes {
        return Ok(());
    }
    if context.no_input || !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        bail!("operation requires an interactive terminal or --yes; nothing was changed");
    }
    eprint!("{prompt} [y/N] ");
    io::stderr().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        bail!("operation declined; nothing was changed")
    }
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
    let millis = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    i64::try_from(millis).context("system time exceeds the supported monitoring range")
}

fn print_record<T: Serialize>(context: &MonitoringContext, record: &T, id: &str) -> Result<()> {
    if context.json {
        print_json(record)
    } else {
        println!("{id}");
        Ok(())
    }
}

fn print_change(context: &MonitoringContext, kind: &str, id: &str, state: &str) -> Result<()> {
    if context.json {
        print_json(&serde_json::json!({ "kind": kind, "id": id, "state": state }))
    } else {
        println!("{kind} {id}: {state}");
        Ok(())
    }
}

fn print_cycle(
    context: &MonitoringContext,
    monitor: &MonitorRecord,
    run_id: &str,
    stats: &temari_core::MonitoringStats,
    plan: Option<&Path>,
    state: &str,
) -> Result<()> {
    if context.json {
        print_json(&serde_json::json!({
            "monitor_id": monitor.id,
            "run_id": run_id,
            "state": state,
            "plan": plan.map(|path| path.display().to_string()),
            "total_files": stats.total_files,
            "skipped_processed": stats.skipped_processed,
            "eligible_files": stats.eligible_files,
            "rule_matches": stats.rule_matches,
            "name_matches": stats.name_matches,
            "content_matches": stats.content_matches,
            "fallback_matches": stats.fallback_matches,
        }))
    } else {
        println!(
            "{} {}: {} ({} eligible, {} skipped; {} rule, {} name, {} content, {} fallback){}",
            monitor.id,
            run_id,
            state,
            stats.eligible_files,
            stats.skipped_processed,
            stats.rule_matches,
            stats.name_matches,
            stats.content_matches,
            stats.fallback_matches,
            plan.map(|path| format!("; plan {}", path.display()))
                .unwrap_or_default()
        );
        Ok(())
    }
}

fn print_json<T: Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn due_calculation_is_interval_based() {
        let monitor = MonitorRecord {
            id: "monitor-1".into(),
            source: "/tmp/source".into(),
            source_identity: temari_core::FsIdentity {
                device: 1,
                inode: 2,
            },
            folder_set_path: "/tmp/folders.json".into(),
            folder_set_sha256: "a".repeat(64),
            interval_seconds: 10,
            enabled: true,
            last_checked_unix_ms: Some(1_000),
            created_unix_ms: 0,
            updated_unix_ms: 0,
            deleted_unix_ms: None,
        };
        assert!(!is_due(&monitor, 10_999));
        assert!(is_due(&monitor, 11_000));
    }

    #[test]
    fn resolves_existing_parent_symlinks_before_source_containment_checks() {
        let root = tempdir().unwrap();
        let source = root.path().join("source");
        let alias = root.path().join("alias");
        fs::create_dir(&source).unwrap();
        symlink(&source, &alias).unwrap();

        let target = resolved_target(&alias.join("runs")).unwrap();
        assert!(target.starts_with(fs::canonicalize(&source).unwrap()));
        assert!(ensure_outside(&target, &source, "output").is_err());
        assert!(!target.exists());
    }

    #[test]
    fn normalizes_parent_components_before_source_containment_checks() {
        let root = tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        let requested = root.path().join("missing/../source/runs");

        let target = resolved_target(&requested).unwrap();
        assert_eq!(target, source.join("runs"));
        assert!(ensure_outside(&requested, &source, "output").is_err());
        assert!(!target.exists());
    }

    #[test]
    fn artifact_identifiers_are_single_path_components() {
        assert!(validate_path_component("monitor-1", "monitor ID").is_ok());
        assert!(validate_path_component("../escape", "monitor ID").is_err());
        assert!(validate_path_component("/absolute", "monitor ID").is_err());
    }
}
