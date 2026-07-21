use std::{
    env, fs,
    io::{self, BufRead, IsTerminal, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
};

use anyhow::{Context, Result, bail};
use clap::{ArgAction, Parser, Subcommand};
use serde::Serialize;
use temari_core::{
    ApplySession, ApplyState, ClassificationBasis, ClassificationOptions, Config, ContentDecision,
    ContentPolicy, FileCandidate, FolderProposer, FolderSet, LocalContentExtractor,
    OpenAiCompatibleModel, Plan, Proposal, ScanScope, UndoState, apply_plan, build_plan,
    classify_file_names, complete_classification, preflight_apply, preflight_resume,
    preflight_undo, resume_apply_session, scan_directory, select_representative_files,
    undo_session,
};
use tempfile::NamedTempFile;

mod managed;
mod monitoring;

use managed::ManagedCommand;
use monitoring::{HistoryCommand, MonitorCommand, MonitoringContext, RuleCommand};

const PROPOSAL_SAMPLE_LIMIT: usize = 100;

#[derive(Debug, Parser)]
#[command(
    name = "temari",
    version,
    about = "Organize files through reviewable, privacy-conscious AI workflows"
)]
struct Cli {
    /// Path to the model configuration file.
    #[arg(long, default_value = ".temari.toml", global = true)]
    config: PathBuf,

    /// Path to the local monitoring state database.
    #[arg(long, global = true)]
    state: Option<PathBuf>,

    /// Print the command result as JSON when the artifact is written to a file.
    #[arg(long, global = true)]
    json: bool,

    /// Refuse any operation that would require interactive confirmation.
    #[arg(long, global = true)]
    no_input: bool,

    /// Disable colored output. Reserved for stable scripting behavior.
    #[arg(long, global = true)]
    no_color: bool,

    /// Show additional progress information on stderr.
    #[arg(short, long, global = true, action = ArgAction::Count)]
    verbose: u8,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Ask the configured model to propose a folder hierarchy.
    Propose {
        /// Directory whose direct child files should be considered.
        source: PathBuf,

        /// Write the proposal artifact to this path, or '-' for stdout.
        #[arg(long)]
        out: PathBuf,

        /// Maximum physical directories, including parent path prefixes.
        #[arg(long, default_value_t = 12)]
        max_folders: usize,

        /// Recursively include this source-relative directory; repeat as needed. Use '.' for all.
        #[arg(long = "include-subtree")]
        include_subtrees: Vec<String>,
    },

    /// Validate and explicitly approve a folder proposal.
    Approve {
        /// Proposal JSON created by `temari propose`.
        proposal: PathBuf,

        /// Write the approved folder set to this path, or '-' for stdout.
        #[arg(long)]
        out: PathBuf,

        /// Approve every proposed folder without prompting.
        #[arg(long)]
        accept_all: bool,
    },

    /// Classify files into an approved folder set without changing the filesystem.
    Plan {
        /// Directory whose direct child files should be classified.
        source: PathBuf,

        /// Approved folder-set JSON created by `temari approve`.
        #[arg(long)]
        folders: PathBuf,

        /// Write the plan artifact to this path, or '-' for stdout.
        #[arg(long)]
        out: PathBuf,
    },

    /// Create approved directories and move files exactly as recorded in a plan.
    Apply {
        /// Plan JSON created by `temari plan`.
        plan: PathBuf,

        /// Write the durable apply journal to this new path.
        #[arg(long)]
        out: PathBuf,

        /// Apply the reviewed plan without prompting.
        #[arg(long)]
        yes: bool,
    },

    /// Safely reverse the recorded moves from an apply session.
    Undo {
        /// Apply-session JSON created by `temari apply`.
        session: PathBuf,

        /// Write the separate undo journal to this new path.
        #[arg(long)]
        out: PathBuf,

        /// Undo the reviewed session without prompting.
        #[arg(long)]
        yes: bool,
    },

    /// Continue a running apply journal after conservative crash reconciliation.
    Resume {
        /// Running apply-session JSON to reconcile and continue in place.
        session: PathBuf,

        /// Resume without prompting.
        #[arg(long)]
        yes: bool,
    },

    /// Run the complete proposal, review, plan, and apply flow interactively.
    Organize {
        /// Directory whose direct child files should be organized.
        source: PathBuf,

        /// Create this new directory for all workflow artifacts.
        #[arg(long)]
        out: PathBuf,

        /// Maximum physical directories, including parent path prefixes.
        #[arg(long, default_value_t = 12)]
        max_folders: usize,

        /// Recursively include this source-relative directory; repeat as needed. Use '.' for all.
        #[arg(long = "include-subtree")]
        include_subtrees: Vec<String>,
    },

    /// Configure or run foreground directory monitoring.
    #[command(subcommand)]
    Monitor(MonitorCommand),

    /// Configure deterministic local routing rules.
    #[command(subcommand)]
    Rule(RuleCommand),

    /// Inspect durable monitoring run history.
    #[command(subcommand)]
    History(HistoryCommand),

    /// Manage a Kept, Inbox, and Library workspace.
    #[command(subcommand)]
    Managed(ManagedCommand),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ApprovalMode {
    Accept,
    Prompt,
    Refuse,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PlanningInteraction {
    Primitive,
    InteractiveOrganize,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let _ = cli.no_color;

    match &cli.command {
        Command::Propose {
            source,
            out,
            max_folders,
            include_subtrees,
        } => propose(&cli, source, out, *max_folders, include_subtrees),
        Command::Approve {
            proposal,
            out,
            accept_all,
        } => approve(&cli, proposal, out, *accept_all),
        Command::Plan {
            source,
            folders,
            out,
        } => plan(&cli, source, folders, out),
        Command::Apply { plan, out, yes } => apply(&cli, plan, out, *yes),
        Command::Undo { session, out, yes } => undo(&cli, session, out, *yes),
        Command::Resume { session, yes } => resume(&cli, session, *yes),
        Command::Organize {
            source,
            out,
            max_folders,
            include_subtrees,
        } => organize(&cli, source, out, *max_folders, include_subtrees),
        Command::Monitor(command) => monitoring::run_monitor(
            MonitoringContext::from_cli(
                &cli.config,
                cli.state.as_deref(),
                cli.json,
                cli.no_input,
                cli.verbose,
            )?,
            command,
        ),
        Command::Rule(command) => monitoring::run_rule(
            MonitoringContext::from_cli(
                &cli.config,
                cli.state.as_deref(),
                cli.json,
                cli.no_input,
                cli.verbose,
            )?,
            command,
        ),
        Command::History(command) => monitoring::run_history(
            MonitoringContext::from_cli(
                &cli.config,
                cli.state.as_deref(),
                cli.json,
                cli.no_input,
                cli.verbose,
            )?,
            command,
        ),
        Command::Managed(command) => managed::run_managed(&cli, command),
    }
}

fn propose(
    cli: &Cli,
    source: &Path,
    out: &Path,
    max_folders: usize,
    include_subtrees: &[String],
) -> Result<()> {
    let scope = ScanScope::new(include_subtrees.to_vec())?;
    let (_, proposal, _) = generate_proposal(cli, source, max_folders, &scope)?;
    write_artifact(out, &proposal)?;
    print_output_result(cli, out)
}

fn generate_proposal(
    cli: &Cli,
    source: &Path,
    max_folders: usize,
    scope: &ScanScope,
) -> Result<(PathBuf, Proposal, usize)> {
    if max_folders == 0 {
        bail!("--max-folders must be greater than zero");
    }
    let config = load_config(cli)?;
    let source = canonical_source(source)?;
    let files = scan_directory(&source, scope, &[])
        .with_context(|| format!("failed to scan {}", source.display()))?;
    if files.is_empty() {
        bail!(
            "no regular files found in the selected scope below {}; add files or choose another source",
            source.display()
        );
    }
    let sample = select_representative_files(&files, PROPOSAL_SAMPLE_LIMIT);
    if cli.verbose > 0 {
        eprintln!(
            "Proposing up to {max_folders} folders from {} of {} file name(s)...",
            sample.len(),
            files.len()
        );
    }
    let model = OpenAiCompatibleModel::new(&config.model)?;
    let folders = model.propose_folders(&sample, max_folders)?;
    let proposal = Proposal {
        version: 2,
        source: portable_source_text(&source)?.to_owned(),
        scope: scope.clone(),
        files_considered: sample.len(),
        folders,
    };
    Ok((source, proposal, files.len()))
}

fn approve(cli: &Cli, proposal_path: &Path, out: &Path, accept_all: bool) -> Result<()> {
    let proposal = Proposal::load(proposal_path)
        .with_context(|| format!("failed to load {}", proposal_path.display()))?;
    let folder_set = proposal.approve()?;
    let mode = approval_mode(
        io::stdin().is_terminal(),
        io::stderr().is_terminal(),
        cli.no_input,
        accept_all,
    );

    match mode {
        ApprovalMode::Accept => {}
        ApprovalMode::Prompt => {
            render_folder_set(&folder_set);
            eprint!("Approve all folders? [y/N] ");
            io::stderr().flush()?;
            let mut answer = String::new();
            io::stdin().lock().read_line(&mut answer)?;
            if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                bail!("approval declined; no artifact was written");
            }
        }
        ApprovalMode::Refuse => {
            bail!(
                "approval requires an interactive terminal or --accept-all; no artifact was written"
            );
        }
    }

    write_artifact(out, &folder_set)?;
    print_output_result(cli, out)
}

fn plan(cli: &Cli, source: &Path, folders_path: &Path, out: &Path) -> Result<()> {
    let source = canonical_source(source)?;
    let folder_set = FolderSet::load(folders_path)
        .with_context(|| format!("failed to load {}", folders_path.display()))?;
    let source_text = portable_source_text(&source)?.to_owned();
    if folder_set.source != source_text {
        bail!(
            "folder set belongs to {:?}, but the requested source is {:?}",
            folder_set.source,
            source_text
        );
    }

    let plan = generate_plan(
        cli,
        &source,
        &folder_set,
        cli.verbose > 0,
        PlanningInteraction::Primitive,
    )?;
    write_artifact(out, &plan)?;
    print_output_result(cli, out)
}

fn generate_plan(
    cli: &Cli,
    source: &Path,
    folder_set: &FolderSet,
    show_progress: bool,
    interaction: PlanningInteraction,
) -> Result<Plan> {
    let config = load_config(cli)?;
    let excluded: Vec<_> = folder_set
        .folders
        .iter()
        .map(|folder| folder.path.clone())
        .collect();
    let files = scan_directory(source, &folder_set.scope, &excluded)
        .with_context(|| format!("failed to scan {}", source.display()))?;
    if show_progress {
        eprintln!("Classifying {} file name(s)...", files.len());
    }
    let model = OpenAiCompatibleModel::new(&config.model)?;
    let name_pass = classify_file_names(
        &files,
        &folder_set.folders,
        &model,
        50,
        std::time::Duration::from_millis(500),
    )?;
    let decision = {
        let stdin = io::stdin();
        let stderr = io::stderr();
        resolve_content_decision(
            &config,
            name_pass.needs_content(),
            interaction,
            &mut stdin.lock(),
            &mut stderr.lock(),
        )?
    };
    let extractor = LocalContentExtractor::new(config.privacy.extraction.clone());
    let summary = complete_classification(
        source,
        &files,
        &folder_set.folders,
        &model,
        &extractor,
        ClassificationOptions {
            content_decision: decision,
            max_content_chars: config.privacy.max_content_chars,
            max_content_file_bytes: config.privacy.max_content_file_bytes,
            content_batch_size: 20,
            batch_delay: std::time::Duration::from_millis(500),
        },
        name_pass,
    )?;
    if show_progress {
        eprintln!(
            "Classification: {} by rule, {} by name, {} by content, {} by extension fallback",
            summary.by_rule, summary.by_name, summary.by_content, summary.by_fallback
        );
    }
    Ok(build_plan(
        source,
        &folder_set.scope,
        &files,
        &folder_set.folders,
        summary.classifications,
    )?)
}

fn resolve_content_decision<R: BufRead, W: Write>(
    config: &Config,
    ambiguous: &[FileCandidate],
    interaction: PlanningInteraction,
    input: &mut R,
    output: &mut W,
) -> Result<ContentDecision> {
    if ambiguous.is_empty() {
        return Ok(ContentDecision::Fallback);
    }
    match config.privacy.content {
        ContentPolicy::MetadataOnly => return Ok(ContentDecision::Fallback),
        ContentPolicy::OnDemand => return Ok(ContentDecision::Extract),
        ContentPolicy::Ask => {}
    }
    if interaction == PlanningInteraction::Primitive {
        bail!(
            "privacy.content = \"ask\" requires interactive organize when {} file(s) need content; set metadata_only to use local fallbacks or on_demand to permit bounded text",
            ambiguous.len()
        );
    }

    writeln!(output, "Content consent required for this run:")?;
    writeln!(output, "  Model: {}", config.model.endpoint_origin()?)?;
    writeln!(output, "  Ambiguous files ({}):", ambiguous.len())?;
    for file in ambiguous {
        writeln!(output, "    - {}", terminal_text(&file.source_path))?;
    }
    writeln!(
        output,
        "  Per file: read at most {} bytes; extract at most {} bytes; send at most {} characters",
        config.privacy.max_content_file_bytes,
        config.privacy.extraction.max_output_bytes,
        config.privacy.max_content_chars
    )?;
    writeln!(
        output,
        "  Local OCR: {}",
        if config.privacy.extraction.ocr.is_some() {
            "enabled"
        } else {
            "disabled"
        }
    )?;
    writeln!(
        output,
        "  Raw files are never uploaded. Only bounded extracted text is sent."
    )?;
    writeln!(
        output,
        "  Unsupported or failed extraction uses approved local fallbacks."
    )?;
    writeln!(
        output,
        "  Extracted text and consent are not logged or persisted."
    )?;
    write!(
        output,
        "Send bounded extracted text for these {} file(s) to {} for this run? [y/N] ",
        ambiguous.len(),
        config.model.endpoint_origin()?
    )?;
    output.flush()?;
    let mut answer = String::new();
    input.read_line(&mut answer)?;
    Ok(
        if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            ContentDecision::Extract
        } else {
            ContentDecision::Fallback
        },
    )
}

fn apply(cli: &Cli, plan_path: &Path, out: &Path, yes: bool) -> Result<()> {
    let plan =
        Plan::load(plan_path).with_context(|| format!("failed to load {}", plan_path.display()))?;
    preflight_apply(&plan, out)?;
    let mode = approval_mode(
        io::stdin().is_terminal(),
        io::stderr().is_terminal(),
        cli.no_input,
        yes,
    );
    if mode == ApprovalMode::Prompt || cli.verbose > 0 {
        eprintln!("Apply plan for {}?", plan.source);
        eprintln!("  Plan SHA-256: {}", plan.sha256()?);
        eprintln!("  Move {} file(s)", plan.entries.len());
        eprintln!(
            "  Create up to {} directorie(s) lazily",
            plan.directories.len()
        );
        eprintln!("  Never overwrite; fail if a planned destination is occupied");
        eprintln!("  Journal: {}", out.display());
    }
    confirm_mutation(mode, "Apply this plan? [y/N] ")?;

    let session = match apply_plan(&plan, out) {
        Ok(session) => session,
        Err(error) if out.exists() => {
            bail!(
                "apply stopped: {error}; inspect the durable session at {}",
                out.display()
            )
        }
        Err(error) => return Err(error.into()),
    };
    if session.state != ApplyState::Completed {
        bail!(
            "apply finished with {:?}; inspect the durable session at {}",
            session.state,
            out.display()
        );
    }
    print_output_result(cli, out)
}

fn undo(cli: &Cli, session_path: &Path, out: &Path, yes: bool) -> Result<()> {
    let apply_session = ApplySession::load(session_path)
        .with_context(|| format!("failed to load {}", session_path.display()))?;
    preflight_undo(&apply_session, out)?;
    let mode = approval_mode(
        io::stdin().is_terminal(),
        io::stderr().is_terminal(),
        cli.no_input,
        yes,
    );
    if mode == ApprovalMode::Prompt || cli.verbose > 0 {
        let moved = apply_session
            .moves
            .iter()
            .filter(|record| {
                matches!(
                    record.outcome,
                    temari_core::MoveOutcome::Moved | temari_core::MoveOutcome::Moving
                )
            })
            .count();
        eprintln!("Undo apply session {}?", apply_session.id);
        eprintln!("  Restore up to {moved} file(s)");
        eprintln!("  Remove only empty directories created by that session");
        eprintln!("  Never overwrite an occupied original path");
        eprintln!("  Journal: {}", out.display());
    }
    confirm_mutation(mode, "Undo this session? [y/N] ")?;

    let session = match undo_session(&apply_session, out) {
        Ok(session) => session,
        Err(error) if out.exists() => {
            bail!(
                "undo stopped: {error}; inspect the durable undo session at {}",
                out.display()
            )
        }
        Err(error) => return Err(error.into()),
    };
    if session.state != UndoState::Completed {
        bail!(
            "undo finished with {:?}; inspect the durable undo session at {}",
            session.state,
            out.display()
        );
    }
    print_output_result(cli, out)
}

fn resume(cli: &Cli, session_path: &Path, yes: bool) -> Result<()> {
    let session = ApplySession::load(session_path)
        .with_context(|| format!("failed to load {}", session_path.display()))?;
    preflight_resume(&session, session_path)?;
    let mode = approval_mode(
        io::stdin().is_terminal(),
        io::stderr().is_terminal(),
        cli.no_input,
        yes,
    );
    if mode == ApprovalMode::Prompt || cli.verbose > 0 {
        let remaining = session
            .moves
            .iter()
            .filter(|record| record.outcome != temari_core::MoveOutcome::Moved)
            .count();
        eprintln!("Resume apply session {}?", session.id);
        eprintln!("  Reconcile and continue up to {remaining} move(s)");
        eprintln!("  Keep every planned destination unchanged");
        eprintln!("  Journal: {}", session_path.display());
    }
    confirm_mutation(mode, "Resume this apply session? [y/N] ")?;

    let resumed = resume_apply_session(session_path).with_context(|| {
        format!(
            "resume stopped; inspect the durable session at {}",
            session_path.display()
        )
    })?;
    if resumed.state != ApplyState::Completed {
        bail!(
            "resume finished with {:?}; inspect conflicts at {}",
            resumed.state,
            session_path.display()
        );
    }
    print_output_result(cli, session_path)
}

fn organize(
    cli: &Cli,
    source: &Path,
    run_directory: &Path,
    max_folders: usize,
    include_subtrees: &[String],
) -> Result<()> {
    if cli.no_input || !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        bail!(
            "organize requires an interactive terminal; use propose, approve, plan, and apply for automation"
        );
    }
    if max_folders == 0 {
        bail!("--max-folders must be greater than zero");
    }
    let scope = ScanScope::new(include_subtrees.to_vec())?;
    let source = canonical_source(source)?;
    create_run_directory(run_directory, &source)?;
    eprintln!("Source: {}", source.display());
    if scope.recursive_roots.is_empty() {
        eprintln!("Scope: regular files directly below the source");
    } else {
        eprintln!(
            "Scope: root files plus recursive subtree(s): {}",
            scope.recursive_roots.join(", ")
        );
    }
    eprintln!("Unselected subdirectories and symlinks are excluded");

    let proposal_path = run_directory.join("proposal.json");
    let review_path = run_directory.join("proposal-review.json");
    let folders_path = run_directory.join("folders.json");
    let plan_path = run_directory.join("plan.json");
    let apply_path = run_directory.join("apply-session.json");

    eprintln!("Stage 1/4: proposing a folder hierarchy...");
    let (_, raw_proposal, total_files) = generate_proposal(cli, &source, max_folders, &scope)
        .with_context(|| {
            format!(
                "proposal failed; artifacts remain in {}",
                run_directory.display()
            )
        })?;
    write_artifact(&proposal_path, &raw_proposal)?;
    write_artifact(&review_path, &raw_proposal)?;
    eprintln!(
        "Scanned {total_files} file(s); sent {} representative name(s) and no file contents",
        raw_proposal.files_considered
    );

    eprintln!("Stage 2/4: reviewing approved destinations...");
    let folder_set = review_proposal(&raw_proposal, &review_path, run_directory)?;
    write_artifact(&folders_path, &folder_set)?;

    eprintln!("Stage 3/4: creating an exact read-only plan...");
    let plan = generate_plan(
        cli,
        &source,
        &folder_set,
        true,
        PlanningInteraction::InteractiveOrganize,
    )
    .with_context(|| {
        format!(
            "planning failed; artifacts remain in {}",
            run_directory.display()
        )
    })?;
    write_artifact(&plan_path, &plan)?;
    render_plan(&plan, &apply_path)?;
    confirm_mutation(ApprovalMode::Prompt, "Apply this exact plan? [y/N] ")
        .with_context(|| format!("plan preserved at {}", plan_path.display()))?;

    eprintln!("Stage 4/4: applying {} move(s)...", plan.entries.len());
    let session = apply_plan(&plan, &apply_path).with_context(|| {
        format!(
            "apply stopped; inspect artifacts in {}",
            run_directory.display()
        )
    })?;
    if session.state != ApplyState::Completed {
        bail!(
            "apply finished with {:?}; inspect {}",
            session.state,
            apply_path.display()
        );
    }
    let moved = session
        .moves
        .iter()
        .filter(|record| record.outcome == temari_core::MoveOutcome::Moved)
        .count();
    let created = session
        .directories
        .iter()
        .filter(|record| {
            matches!(
                record.outcome,
                temari_core::DirectoryOutcome::Created { .. }
            )
        })
        .count();
    eprintln!("Completed: moved {moved} file(s), created {created} directorie(s)");
    eprintln!("Session: {}", session.id);
    eprintln!(
        "Undo: temari undo {} --out {}",
        apply_path.display(),
        run_directory.join("undo-session.json").display()
    );
    print_output_result(cli, run_directory)
}

fn review_proposal(raw: &Proposal, review_path: &Path, run_directory: &Path) -> Result<FolderSet> {
    loop {
        let review = match Proposal::load(review_path) {
            Ok(review) => review,
            Err(error) => {
                eprintln!("Review artifact is invalid: {error}");
                eprint!("[e]dit again or [q]uit: ");
                io::stderr().flush()?;
                let mut choice = String::new();
                io::stdin().lock().read_line(&mut choice)?;
                if matches!(choice.trim().to_ascii_lowercase().as_str(), "e" | "edit") {
                    if let Err(error) = edit_proposal(review_path) {
                        eprintln!("Editor failed: {error}");
                    }
                } else {
                    bail!(
                        "organization cancelled; completed artifacts remain in {}",
                        run_directory.display()
                    );
                }
                continue;
            }
        };
        match review.clone().approve() {
            Ok(folder_set) => render_folder_set(&folder_set),
            Err(_) => render_proposal(&review),
        }
        eprint!("[a]pprove destinations, [e]dit in $VISUAL/$EDITOR, [q]uit: ");
        io::stderr().flush()?;
        let mut choice = String::new();
        io::stdin().lock().read_line(&mut choice)?;
        match choice.trim().to_ascii_lowercase().as_str() {
            "a" | "approve" => match approve_review(raw, review) {
                Ok(folder_set) => return Ok(folder_set),
                Err(error) => eprintln!("Cannot approve the edited proposal: {error}"),
            },
            "e" | "edit" => {
                if let Err(error) = edit_proposal(review_path) {
                    eprintln!("Editor failed: {error}");
                }
            }
            _ => bail!(
                "organization cancelled; completed artifacts remain in {}",
                run_directory.display()
            ),
        }
    }
}

fn approve_review(raw: &Proposal, review: Proposal) -> Result<FolderSet> {
    if raw.version != review.version
        || raw.source != review.source
        || raw.scope != review.scope
        || raw.files_considered != review.files_considered
    {
        bail!("editing may change folders and descriptions, but not proposal context");
    }
    Ok(review.approve()?)
}

fn render_proposal(proposal: &Proposal) {
    eprintln!(
        "Proposed destinations for {}:",
        terminal_text(&proposal.source)
    );
    for folder in &proposal.folders {
        eprintln!(
            "  {}/ — {}",
            terminal_text(&folder.path),
            terminal_text(&folder.description)
        );
    }
    eprintln!("No directories or files have been changed.");
}

fn render_folder_set(folder_set: &FolderSet) {
    eprintln!(
        "Destinations for {} (automatic fallbacks are marked):",
        terminal_text(&folder_set.source)
    );
    for folder in &folder_set.folders {
        let marker = match (folder.fallback, folder.model_visible) {
            (Some(_), true) => " [destination + fallback]",
            (Some(_), false) => " [local fallback]",
            (None, _) => "",
        };
        eprintln!(
            "  {}/{} — {}",
            terminal_text(&folder.path),
            marker,
            terminal_text(&folder.description)
        );
    }
    eprintln!("No directories or files have been changed.");
}

fn render_plan(plan: &Plan, apply_path: &Path) -> Result<()> {
    eprintln!("Plan for {}:", plan.source);
    eprintln!("  SHA-256: {}", plan.sha256()?);
    for directory in &plan.directories {
        eprintln!("  mkdir {}/", terminal_text(directory));
    }
    for entry in &plan.entries {
        let basis = match entry.classification_basis {
            ClassificationBasis::Name => "name",
            ClassificationBasis::Content => "content",
            ClassificationBasis::ExtensionFallback => "fallback",
            ClassificationBasis::Rule => "rule",
        };
        eprintln!(
            "  [{basis}] move {} -> {}",
            terminal_text(&entry.source_path),
            terminal_text(&entry.destination_path)
        );
    }
    let by_name = plan
        .entries
        .iter()
        .filter(|entry| entry.classification_basis == ClassificationBasis::Name)
        .count();
    let by_content = plan
        .entries
        .iter()
        .filter(|entry| entry.classification_basis == ClassificationBasis::Content)
        .count();
    let by_fallback = plan
        .entries
        .iter()
        .filter(|entry| entry.classification_basis == ClassificationBasis::ExtensionFallback)
        .count();
    let by_rule = plan
        .entries
        .iter()
        .filter(|entry| entry.classification_basis == ClassificationBasis::Rule)
        .count();
    eprintln!(
        "Summary: {} move(s) ({by_rule} rule, {by_name} name, {by_content} content, {by_fallback} fallback), up to {} new directorie(s), no overwrites",
        plan.entries.len(),
        plan.directories.len()
    );
    eprintln!("Apply journal: {}", apply_path.display());
    Ok(())
}

fn edit_proposal(path: &Path) -> Result<()> {
    let editor = env::var("VISUAL")
        .or_else(|_| env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());
    let command = shlex::split(&editor)
        .filter(|parts| !parts.is_empty())
        .ok_or_else(|| anyhow::anyhow!("$VISUAL/$EDITOR does not contain an executable"))?;
    let status = ProcessCommand::new(&command[0])
        .args(&command[1..])
        .arg(path)
        .status()
        .with_context(|| format!("failed to start editor {:?}", command[0]))?;
    if !status.success() {
        bail!("editor exited with {status}");
    }
    Ok(())
}

fn create_run_directory(path: &Path, source: &Path) -> Result<()> {
    if path == Path::new("-") {
        bail!("organize --out must be a persistent directory path");
    }
    match fs::symlink_metadata(path) {
        Ok(_) => bail!("run directory already exists: {}", path.display()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", path.display()));
        }
    }
    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let canonical_parent = fs::canonicalize(parent).with_context(|| {
        format!(
            "failed to resolve run-directory parent {}",
            parent.display()
        )
    })?;
    let name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("run directory must include a final component"))?;
    if canonical_parent.join(name).starts_with(source) {
        bail!("organize run directory must be outside the organized source");
    }
    fs::create_dir(path)
        .with_context(|| format!("failed to create run directory {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn load_config(cli: &Cli) -> Result<Config> {
    Config::load(&cli.config).with_context(|| format!("failed to load {}", cli.config.display()))
}

fn canonical_source(source: &Path) -> Result<PathBuf> {
    fs::canonicalize(source).with_context(|| format!("failed to resolve {}", source.display()))
}

fn portable_source_text(source: &Path) -> Result<&str> {
    let text = source
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("source path must be valid UTF-8 for portable artifacts"))?;
    if text.chars().any(char::is_control) {
        bail!("source path must not contain control characters");
    }
    Ok(text)
}

fn terminal_text(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() {
                '�'
            } else {
                character
            }
        })
        .collect()
}

fn approval_mode(
    stdin_is_terminal: bool,
    stderr_is_terminal: bool,
    no_input: bool,
    accept_all: bool,
) -> ApprovalMode {
    if accept_all {
        ApprovalMode::Accept
    } else if no_input || !stdin_is_terminal || !stderr_is_terminal {
        ApprovalMode::Refuse
    } else {
        ApprovalMode::Prompt
    }
}

fn confirm_mutation(mode: ApprovalMode, prompt: &str) -> Result<()> {
    match mode {
        ApprovalMode::Accept => Ok(()),
        ApprovalMode::Refuse => {
            bail!("operation requires an interactive terminal or --yes; nothing was changed")
        }
        ApprovalMode::Prompt => {
            eprint!("{prompt}");
            io::stderr().flush()?;
            let mut answer = String::new();
            io::stdin().lock().read_line(&mut answer)?;
            if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                Ok(())
            } else {
                bail!("operation declined; nothing was changed")
            }
        }
    }
}

fn write_artifact<T: Serialize>(out: &Path, artifact: &T) -> Result<()> {
    if out == Path::new("-") {
        let stdout = io::stdout();
        let mut writer = stdout.lock();
        serde_json::to_writer_pretty(&mut writer, artifact)?;
        writeln!(writer)?;
        return Ok(());
    }

    let parent = out
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create output directory {}", parent.display()))?;
    let mut temporary = NamedTempFile::new_in(parent)
        .with_context(|| format!("failed to create a temporary file in {}", parent.display()))?;
    serde_json::to_writer_pretty(temporary.as_file_mut(), artifact)?;
    writeln!(temporary.as_file_mut())?;
    temporary.as_file_mut().sync_all()?;
    temporary
        .persist(out)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to write {}", out.display()))?;
    Ok(())
}

fn print_output_result(cli: &Cli, out: &Path) -> Result<()> {
    if out == Path::new("-") {
        return Ok(());
    }
    if cli.json {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({ "output": out.display().to_string() }))?
        );
    } else {
        println!("{}", out.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use tempfile::tempdir;

    use super::*;

    fn consent_config(policy: ContentPolicy) -> Config {
        Config {
            version: 4,
            model: temari_core::ModelConfig {
                base_url: "https://model.example.test/private/v1?token=hidden".into(),
                name: "local".into(),
                allowed_hosts: vec!["model.example.test".into()],
                api_key: None,
                api_key_env: Some("PRIVATE_KEY".into()),
            },
            privacy: temari_core::PrivacyConfig {
                content: policy,
                max_content_chars: 20_000,
                max_content_file_bytes: 10_485_760,
                extraction: temari_core::ExtractionConfig {
                    max_output_bytes: 1_048_576,
                    max_archive_entries: 1024,
                    max_expanded_bytes: 67_108_864,
                    max_xml_events: 1_000_000,
                    max_xml_depth: 256,
                    timeout_seconds: 15,
                    ocr: None,
                },
            },
        }
    }

    fn ambiguous_file(path: &str) -> FileCandidate {
        FileCandidate {
            id: "f000001".into(),
            source_path: path.into(),
            extension: "txt".into(),
        }
    }

    #[test]
    fn approval_requires_both_terminals() {
        assert_eq!(
            approval_mode(true, true, false, false),
            ApprovalMode::Prompt
        );
        assert_eq!(
            approval_mode(false, true, false, false),
            ApprovalMode::Refuse
        );
        assert_eq!(
            approval_mode(true, false, false, false),
            ApprovalMode::Refuse
        );
    }

    #[test]
    fn explicit_flags_never_prompt() {
        assert_eq!(approval_mode(true, true, true, false), ApprovalMode::Refuse);
        assert_eq!(
            approval_mode(false, false, true, true),
            ApprovalMode::Accept
        );
    }

    #[test]
    fn proposal_review_may_change_folders_but_not_source_context() {
        let raw = Proposal {
            version: 2,
            source: "/tmp/inbox".into(),
            scope: ScanScope::default(),
            files_considered: 2,
            folders: vec![temari_core::FolderProposal {
                path: "Documents".into(),
                description: "Documents".into(),
            }],
        };
        let mut edited = raw.clone();
        edited.folders[0].path = "Work/Documents".into();
        assert!(approve_review(&raw, edited).is_ok());

        let mut changed_source = raw.clone();
        changed_source.source = "/tmp/other".into();
        assert!(approve_review(&raw, changed_source).is_err());
    }

    #[test]
    fn run_directory_is_private_and_outside_the_source() {
        let source = tempdir().unwrap();
        let artifacts = tempdir().unwrap();
        let run = artifacts.path().join("run");

        create_run_directory(&run, source.path()).unwrap();

        assert_eq!(
            fs::metadata(&run).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert!(create_run_directory(&source.path().join("run"), source.path()).is_err());
    }

    #[test]
    fn ask_discloses_only_the_origin_and_uses_the_current_run_answer() {
        let mut config = consent_config(ContentPolicy::Ask);
        config.privacy.extraction.ocr = Some(temari_core::OcrConfig {
            executable: "/private/bin/ocr".into(),
            languages: vec!["eng".into()],
            data_dir: None,
        });
        let mut output = Vec::new();

        let decision = resolve_content_decision(
            &config,
            &[ambiguous_file("Receipts/invoice.txt")],
            PlanningInteraction::InteractiveOrganize,
            &mut Cursor::new(b"yes\n"),
            &mut output,
        )
        .unwrap();
        let output = String::from_utf8(output).unwrap();

        assert_eq!(decision, ContentDecision::Extract);
        assert!(output.contains("https://model.example.test"));
        assert!(output.contains("Receipts/invoice.txt"));
        assert!(output.contains("Local OCR: enabled"));
        assert!(!output.contains("private/v1"));
        assert!(!output.contains("token=hidden"));
        assert!(!output.contains("PRIVATE_KEY"));
        assert!(!output.contains("/private/bin/ocr"));
    }

    #[test]
    fn ask_defaults_to_fallback_and_primitive_plan_never_prompts() {
        let config = consent_config(ContentPolicy::Ask);
        let files = [ambiguous_file("invoice.txt")];
        let mut interactive_output = Vec::new();
        let declined = resolve_content_decision(
            &config,
            &files,
            PlanningInteraction::InteractiveOrganize,
            &mut Cursor::new(b"\n"),
            &mut interactive_output,
        )
        .unwrap();
        assert_eq!(declined, ContentDecision::Fallback);

        let mut primitive_output = Vec::new();
        let error = resolve_content_decision(
            &config,
            &files,
            PlanningInteraction::Primitive,
            &mut Cursor::new(b"yes\n"),
            &mut primitive_output,
        )
        .unwrap_err();
        assert!(error.to_string().contains("requires interactive organize"));
        assert!(primitive_output.is_empty());
    }

    #[test]
    fn no_ambiguity_never_prompts_and_explicit_policies_are_unattended() {
        let mut output = Vec::new();
        assert_eq!(
            resolve_content_decision(
                &consent_config(ContentPolicy::Ask),
                &[],
                PlanningInteraction::InteractiveOrganize,
                &mut Cursor::new(b"yes\n"),
                &mut output,
            )
            .unwrap(),
            ContentDecision::Fallback
        );
        assert!(output.is_empty());

        assert_eq!(
            resolve_content_decision(
                &consent_config(ContentPolicy::OnDemand),
                &[ambiguous_file("invoice.txt")],
                PlanningInteraction::Primitive,
                &mut Cursor::new(Vec::<u8>::new()),
                &mut output,
            )
            .unwrap(),
            ContentDecision::Extract
        );
        assert_eq!(
            resolve_content_decision(
                &consent_config(ContentPolicy::MetadataOnly),
                &[ambiguous_file("invoice.txt")],
                PlanningInteraction::Primitive,
                &mut Cursor::new(Vec::<u8>::new()),
                &mut output,
            )
            .unwrap(),
            ContentDecision::Fallback
        );
        assert!(output.is_empty());
    }
}
