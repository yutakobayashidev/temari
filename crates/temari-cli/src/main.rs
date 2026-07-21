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
    ApplySession, ApplyState, Classifier, Config, FolderProposer, FolderSet, OpenAiCompatibleModel,
    Plan, Proposal, UndoState, apply_plan, build_plan, preflight_apply, preflight_resume,
    preflight_undo, resume_apply_session, scan_directory, select_representative_files,
    undo_session,
};
use tempfile::NamedTempFile;

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

        /// Maximum number of folders the model may propose.
        #[arg(long, default_value_t = 12)]
        max_folders: usize,
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

        /// Maximum number of folders the model may propose.
        #[arg(long, default_value_t = 12)]
        max_folders: usize,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ApprovalMode {
    Accept,
    Prompt,
    Refuse,
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
        } => propose(&cli, source, out, *max_folders),
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
        } => organize(&cli, source, out, *max_folders),
    }
}

fn propose(cli: &Cli, source: &Path, out: &Path, max_folders: usize) -> Result<()> {
    let (_, proposal, _) = generate_proposal(cli, source, max_folders)?;
    write_artifact(out, &proposal)?;
    print_output_result(cli, out)
}

fn generate_proposal(
    cli: &Cli,
    source: &Path,
    max_folders: usize,
) -> Result<(PathBuf, Proposal, usize)> {
    if max_folders == 0 {
        bail!("--max-folders must be greater than zero");
    }
    let config = load_config(cli)?;
    let source = canonical_source(source)?;
    let files =
        scan_directory(&source).with_context(|| format!("failed to scan {}", source.display()))?;
    if files.is_empty() {
        bail!(
            "no regular files found directly below {}; add files or choose another source",
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
        version: 1,
        source: portable_source_text(&source)?.to_owned(),
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
            eprintln!("Approve these folders for {}?", folder_set.source);
            for folder in &folder_set.folders {
                eprintln!(
                    "  {}/ — {}",
                    terminal_text(&folder.path),
                    terminal_text(&folder.description)
                );
            }
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

    let plan = generate_plan(cli, &source, &folder_set)?;
    write_artifact(out, &plan)?;
    print_output_result(cli, out)
}

fn generate_plan(cli: &Cli, source: &Path, folder_set: &FolderSet) -> Result<Plan> {
    let config = load_config(cli)?;
    let files =
        scan_directory(source).with_context(|| format!("failed to scan {}", source.display()))?;
    if cli.verbose > 0 {
        eprintln!("Classifying {} file name(s)...", files.len());
    }
    let classifications = if files.is_empty() {
        Vec::new()
    } else {
        let model = OpenAiCompatibleModel::new(&config.model)?;
        model.classify_names(&files, &folder_set.folders)?
    };
    Ok(build_plan(
        source,
        &files,
        &folder_set.folders,
        classifications,
    )?)
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

fn organize(cli: &Cli, source: &Path, run_directory: &Path, max_folders: usize) -> Result<()> {
    if cli.no_input || !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        bail!(
            "organize requires an interactive terminal; use propose, approve, plan, and apply for automation"
        );
    }
    if max_folders == 0 {
        bail!("--max-folders must be greater than zero");
    }
    let source = canonical_source(source)?;
    create_run_directory(run_directory, &source)?;
    eprintln!("Source: {}", source.display());
    eprintln!("Scope: regular files directly below the source");
    eprintln!("Existing subdirectories and symlinks are excluded");

    let proposal_path = run_directory.join("proposal.json");
    let review_path = run_directory.join("proposal-review.json");
    let folders_path = run_directory.join("folders.json");
    let plan_path = run_directory.join("plan.json");
    let apply_path = run_directory.join("apply-session.json");

    eprintln!("Stage 1/4: proposing a folder hierarchy...");
    let (_, raw_proposal, total_files) = generate_proposal(cli, &source, max_folders)
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
    let plan = generate_plan(cli, &source, &folder_set).with_context(|| {
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
        render_proposal(&review);
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

fn render_plan(plan: &Plan, apply_path: &Path) -> Result<()> {
    eprintln!("Plan for {}:", plan.source);
    eprintln!("  SHA-256: {}", plan.sha256()?);
    for directory in &plan.directories {
        eprintln!("  mkdir {directory}/");
    }
    for entry in &plan.entries {
        eprintln!("  move {} -> {}", entry.file_name, entry.destination_path);
    }
    eprintln!(
        "Summary: {} move(s), up to {} new directorie(s), no overwrites",
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
    use tempfile::tempdir;

    use super::*;

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
            version: 1,
            source: "/tmp/inbox".into(),
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
}
