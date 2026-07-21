use std::{
    fs,
    io::Write,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Output},
};

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use directories::BaseDirs;
use serde::Serialize;
use tempfile::NamedTempFile;

const MINIMUM_INTERVAL_SECONDS: u32 = 60;
const SYSTEMD_PREFIX: &str = "temari-managed-";
const LAUNCHD_PREFIX: &str = "dev.yutakobayashi.temari.managed.";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum SchedulerPlatform {
    #[default]
    Auto,
    Systemd,
    Launchd,
}

impl SchedulerPlatform {
    pub fn resolve(self) -> Result<Self> {
        match self {
            Self::Auto if cfg!(target_os = "linux") => Ok(Self::Systemd),
            Self::Auto if cfg!(target_os = "macos") => Ok(Self::Launchd),
            Self::Auto => bail!("managed schedules are supported only on Linux and macOS"),
            platform => Ok(platform),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScheduleSpec {
    workspace_id: String,
    binary: PathBuf,
    config: PathBuf,
    state: PathBuf,
    source: PathBuf,
    interval_seconds: u32,
}

impl ScheduleSpec {
    pub fn new(
        workspace_id: &str,
        binary: &Path,
        config: &Path,
        state: &Path,
        source: &Path,
        interval_seconds: u32,
    ) -> Result<Self> {
        validate_workspace_id(workspace_id)?;
        validate_interval(interval_seconds)?;

        let binary = stable_executable(binary)?;
        if binary.starts_with("/nix/store") {
            bail!(
                "Temari executable is a garbage-collectable Nix store path; pass --executable with a stable user-facing launcher path"
            );
        }
        if binary.metadata()?.permissions().mode() & 0o111 == 0 {
            bail!("Temari executable is not executable: {}", binary.display());
        }
        let config = canonical_regular_file(config, "model configuration")?;
        ensure_owner_only(&config, "model configuration")?;
        let state = canonical_regular_file(state, "managed state database")?;
        ensure_owner_only(&state, "managed state database")?;
        let source = fs::canonicalize(source)
            .with_context(|| format!("failed to resolve managed source {}", source.display()))?;
        if !source.is_dir() {
            bail!("managed source is not a directory: {}", source.display());
        }
        for (path, label) in [
            (&binary, "Temari executable"),
            (&config, "model configuration"),
            (&state, "managed state database"),
            (&source, "managed source"),
        ] {
            validate_path_text(path, label)?;
        }

        Ok(Self {
            workspace_id: workspace_id.into(),
            binary,
            config,
            state,
            source,
            interval_seconds,
        })
    }

    pub fn source(&self) -> &Path {
        &self.source
    }

    pub fn interval_seconds(&self) -> u32 {
        self.interval_seconds
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ScheduleDefinition {
    pub path: PathBuf,
    pub contents: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ScheduleStatus {
    pub platform: SchedulerPlatform,
    pub installed: bool,
    pub enabled: bool,
    pub active: bool,
    pub definition_paths: Vec<PathBuf>,
}

pub fn render_schedule(
    spec: &ScheduleSpec,
    platform: SchedulerPlatform,
) -> Result<Vec<ScheduleDefinition>> {
    render_schedule_in(spec, platform.resolve()?, &ScheduleDirectories::detect()?)
}

pub fn install_schedule(
    spec: &ScheduleSpec,
    platform: SchedulerPlatform,
) -> Result<ScheduleStatus> {
    let platform = platform.resolve()?;
    let directories = ScheduleDirectories::detect()?;
    let runner = ProcessRunner;
    install_schedule_with(spec, platform, &directories, &runner)
}

pub fn schedule_status(workspace_id: &str, platform: SchedulerPlatform) -> Result<ScheduleStatus> {
    validate_workspace_id(workspace_id)?;
    let platform = platform.resolve()?;
    let directories = ScheduleDirectories::detect()?;
    let runner = ProcessRunner;
    schedule_status_with(workspace_id, platform, &directories, &runner)
}

pub fn uninstall_schedule(
    workspace_id: &str,
    platform: SchedulerPlatform,
) -> Result<ScheduleStatus> {
    validate_workspace_id(workspace_id)?;
    let platform = platform.resolve()?;
    let directories = ScheduleDirectories::detect()?;
    let runner = ProcessRunner;
    uninstall_schedule_with(workspace_id, platform, &directories, &runner)
}

#[derive(Clone, Debug)]
struct ScheduleDirectories {
    home: PathBuf,
    config: PathBuf,
}

impl ScheduleDirectories {
    fn detect() -> Result<Self> {
        let base = BaseDirs::new()
            .ok_or_else(|| anyhow::anyhow!("could not determine the user home directory"))?;
        Ok(Self {
            home: base.home_dir().into(),
            config: base.config_dir().into(),
        })
    }
}

trait CommandRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<Output>;
}

struct ProcessRunner;

impl CommandRunner for ProcessRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<Output> {
        Command::new(program)
            .args(args)
            .output()
            .with_context(|| format!("failed to run {program}"))
    }
}

fn render_schedule_in(
    spec: &ScheduleSpec,
    platform: SchedulerPlatform,
    directories: &ScheduleDirectories,
) -> Result<Vec<ScheduleDefinition>> {
    match platform {
        SchedulerPlatform::Systemd => render_systemd(spec, directories),
        SchedulerPlatform::Launchd => render_launchd(spec, directories),
        SchedulerPlatform::Auto => unreachable!("platform must be resolved before rendering"),
    }
}

fn render_systemd(
    spec: &ScheduleSpec,
    directories: &ScheduleDirectories,
) -> Result<Vec<ScheduleDefinition>> {
    let paths = definition_paths(&spec.workspace_id, SchedulerPlatform::Systemd, directories);
    let marker = ownership_marker(&spec.workspace_id);
    let description = format!("Temari managed workspace {}", spec.workspace_id);
    let arguments = managed_run_arguments(spec)?;
    let exec_start = arguments
        .iter()
        .map(|argument| systemd_word(argument))
        .collect::<Vec<_>>()
        .join(" ");
    let source = systemd_word(path_string(&spec.source, "managed source")?);
    let service = format!(
        "# {marker}\n[Unit]\nDescription={description}\nConditionPathIsDirectory={source}\n\n[Service]\nType=oneshot\nUMask=0077\nExecStart={exec_start}\n"
    );
    let timer = format!(
        "# {marker}\n[Unit]\nDescription=Run {description}\n\n[Timer]\nOnBootSec=2m\nOnUnitActiveSec={}s\nPersistent=true\nUnit={SYSTEMD_PREFIX}{}.service\n\n[Install]\nWantedBy=timers.target\n",
        spec.interval_seconds, spec.workspace_id
    );
    Ok(vec![
        ScheduleDefinition {
            path: paths[0].clone(),
            contents: service,
        },
        ScheduleDefinition {
            path: paths[1].clone(),
            contents: timer,
        },
    ])
}

fn render_launchd(
    spec: &ScheduleSpec,
    directories: &ScheduleDirectories,
) -> Result<Vec<ScheduleDefinition>> {
    let path =
        definition_paths(&spec.workspace_id, SchedulerPlatform::Launchd, directories).remove(0);
    let label = launchd_label(&spec.workspace_id);
    let log_dir = spec
        .state
        .parent()
        .ok_or_else(|| anyhow::anyhow!("managed state database has no parent directory"))?
        .join("logs");
    let stdout = log_dir.join(format!("{}.out.log", spec.workspace_id));
    let stderr = log_dir.join(format!("{}.err.log", spec.workspace_id));
    validate_path_text(&stdout, "launchd standard output")?;
    validate_path_text(&stderr, "launchd standard error")?;
    let arguments = managed_run_arguments(spec)?
        .iter()
        .map(|argument| format!("    <string>{}</string>", xml_escape(argument)))
        .collect::<Vec<_>>()
        .join("\n");
    let contents = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n  <key>Label</key>\n  <string>{}</string>\n  <key>TemariManagedWorkspace</key>\n  <string>{}</string>\n  <key>ProgramArguments</key>\n  <array>\n{}\n  </array>\n  <key>RunAtLoad</key>\n  <true/>\n  <key>StartInterval</key>\n  <integer>{}</integer>\n  <key>ProcessType</key>\n  <string>Background</string>\n  <key>StandardOutPath</key>\n  <string>{}</string>\n  <key>StandardErrorPath</key>\n  <string>{}</string>\n</dict>\n</plist>\n",
        xml_escape(&label),
        xml_escape(&spec.workspace_id),
        arguments,
        spec.interval_seconds,
        xml_escape(path_string(&stdout, "launchd standard output")?),
        xml_escape(path_string(&stderr, "launchd standard error")?),
    );
    Ok(vec![ScheduleDefinition { path, contents }])
}

fn managed_run_arguments(spec: &ScheduleSpec) -> Result<Vec<String>> {
    Ok(vec![
        path_string(&spec.binary, "Temari executable")?.into(),
        "--config".into(),
        path_string(&spec.config, "model configuration")?.into(),
        "--state".into(),
        path_string(&spec.state, "managed state database")?.into(),
        "managed".into(),
        "run".into(),
        spec.workspace_id.clone(),
        "--apply".into(),
        "--yes".into(),
    ])
}

fn install_schedule_with(
    spec: &ScheduleSpec,
    platform: SchedulerPlatform,
    directories: &ScheduleDirectories,
    runner: &dyn CommandRunner,
) -> Result<ScheduleStatus> {
    ensure_manager_available(platform, runner)?;
    let definitions = render_schedule_in(spec, platform, directories)?;
    for definition in &definitions {
        ensure_owned_or_missing(&definition.path, &spec.workspace_id)?;
    }
    if platform == SchedulerPlatform::Launchd {
        let log_dir = spec
            .state
            .parent()
            .ok_or_else(|| anyhow::anyhow!("managed state database has no parent directory"))?
            .join("logs");
        create_private_directory(&log_dir)?;
    }
    for definition in &definitions {
        atomic_write_private(&definition.path, definition.contents.as_bytes())?;
    }

    match platform {
        SchedulerPlatform::Systemd => {
            require_success(runner.run("systemctl", &strings(&["--user", "daemon-reload"]))?)?;
            let timer = systemd_timer_name(&spec.workspace_id);
            require_success(runner.run(
                "systemctl",
                &["--user".into(), "enable".into(), "--now".into(), timer],
            )?)?;
        }
        SchedulerPlatform::Launchd => {
            let domain = launchd_domain(runner)?;
            let label = launchd_label(&spec.workspace_id);
            let _ = runner.run(
                "launchctl",
                &["bootout".into(), format!("{domain}/{label}")],
            );
            require_success(runner.run(
                "launchctl",
                &[
                    "bootstrap".into(),
                    domain,
                    path_string(&definitions[0].path, "launchd definition")?.into(),
                ],
            )?)?;
        }
        SchedulerPlatform::Auto => unreachable!(),
    }
    schedule_status_with(&spec.workspace_id, platform, directories, runner)
}

fn schedule_status_with(
    workspace_id: &str,
    platform: SchedulerPlatform,
    directories: &ScheduleDirectories,
    runner: &dyn CommandRunner,
) -> Result<ScheduleStatus> {
    let paths = definition_paths(workspace_id, platform, directories);
    for path in &paths {
        ensure_owned_or_missing(path, workspace_id)?;
    }
    let installed = paths.iter().all(|path| path.is_file());
    let (enabled, active) = match platform {
        SchedulerPlatform::Systemd => {
            let timer = systemd_timer_name(workspace_id);
            (
                command_succeeds(
                    runner,
                    "systemctl",
                    &["--user".into(), "is-enabled".into(), timer.clone()],
                ),
                command_succeeds(
                    runner,
                    "systemctl",
                    &["--user".into(), "is-active".into(), timer],
                ),
            )
        }
        SchedulerPlatform::Launchd => {
            let active = launchd_domain(runner).ok().is_some_and(|domain| {
                command_succeeds(
                    runner,
                    "launchctl",
                    &[
                        "print".into(),
                        format!("{domain}/{}", launchd_label(workspace_id)),
                    ],
                )
            });
            (installed, active)
        }
        SchedulerPlatform::Auto => unreachable!(),
    };
    Ok(ScheduleStatus {
        platform,
        installed,
        enabled,
        active,
        definition_paths: paths,
    })
}

fn uninstall_schedule_with(
    workspace_id: &str,
    platform: SchedulerPlatform,
    directories: &ScheduleDirectories,
    runner: &dyn CommandRunner,
) -> Result<ScheduleStatus> {
    ensure_manager_available(platform, runner)?;
    let paths = definition_paths(workspace_id, platform, directories);
    for path in &paths {
        ensure_owned_or_missing(path, workspace_id)?;
    }
    let current = schedule_status_with(workspace_id, platform, directories, runner)?;
    match platform {
        SchedulerPlatform::Systemd => {
            if current.enabled || current.active {
                let timer = systemd_timer_name(workspace_id);
                let service = systemd_service_name(workspace_id);
                require_success(runner.run(
                    "systemctl",
                    &["--user".into(), "disable".into(), "--now".into(), timer],
                )?)?;
                require_success(
                    runner.run("systemctl", &["--user".into(), "stop".into(), service])?,
                )?;
            }
        }
        SchedulerPlatform::Launchd => {
            if current.active {
                let domain = launchd_domain(runner)?;
                let label = launchd_label(workspace_id);
                require_success(runner.run(
                    "launchctl",
                    &["bootout".into(), format!("{domain}/{label}")],
                )?)?;
            }
        }
        SchedulerPlatform::Auto => unreachable!(),
    }
    let stopped = schedule_status_with(workspace_id, platform, directories, runner)?;
    if stopped.active || (platform == SchedulerPlatform::Systemd && stopped.enabled) {
        bail!("scheduler is still active; definitions were not removed");
    }
    for path in &paths {
        if path.exists() {
            fs::remove_file(path).with_context(|| {
                format!("failed to remove schedule definition {}", path.display())
            })?;
        }
    }
    if platform == SchedulerPlatform::Systemd {
        require_success(runner.run("systemctl", &strings(&["--user", "daemon-reload"]))?)?;
    }
    schedule_status_with(workspace_id, platform, directories, runner)
}

fn definition_paths(
    workspace_id: &str,
    platform: SchedulerPlatform,
    directories: &ScheduleDirectories,
) -> Vec<PathBuf> {
    match platform {
        SchedulerPlatform::Systemd => {
            let root = directories.config.join("systemd/user");
            vec![
                root.join(systemd_service_name(workspace_id)),
                root.join(systemd_timer_name(workspace_id)),
            ]
        }
        SchedulerPlatform::Launchd => vec![
            directories
                .home
                .join("Library/LaunchAgents")
                .join(format!("{LAUNCHD_PREFIX}{workspace_id}.plist")),
        ],
        SchedulerPlatform::Auto => unreachable!(),
    }
}

fn ensure_manager_available(platform: SchedulerPlatform, runner: &dyn CommandRunner) -> Result<()> {
    let output = match platform {
        SchedulerPlatform::Systemd => {
            runner.run("systemctl", &strings(&["--user", "show-environment"]))?
        }
        SchedulerPlatform::Launchd => {
            let domain = launchd_domain(runner)?;
            runner.run("launchctl", &["print".into(), domain])?
        }
        SchedulerPlatform::Auto => unreachable!(),
    };
    if output.status.success() {
        Ok(())
    } else {
        bail!(
            "{} user manager is unavailable: {}",
            platform_name(platform),
            output_message(&output)
        )
    }
}

fn launchd_domain(runner: &dyn CommandRunner) -> Result<String> {
    let output = runner.run("id", &["-u".into()])?;
    let output = require_success(output)?;
    let uid = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u32>()
        .context("id -u returned an invalid user ID")?;
    Ok(format!("gui/{uid}"))
}

fn command_succeeds(runner: &dyn CommandRunner, program: &str, arguments: &[String]) -> bool {
    runner
        .run(program, arguments)
        .is_ok_and(|output| output.status.success())
}

fn require_success(output: Output) -> Result<Output> {
    if output.status.success() {
        Ok(output)
    } else {
        bail!("scheduler command failed: {}", output_message(&output))
    }
}

fn output_message(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let message = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    if message.is_empty() {
        format!("exit status {}", output.status)
    } else {
        message.replace(char::is_control, " ")
    }
}

fn validate_workspace_id(workspace_id: &str) -> Result<()> {
    if workspace_id.is_empty()
        || !workspace_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        bail!("workspace ID must contain only ASCII letters, digits, '-' or '_'");
    }
    Ok(())
}

fn validate_interval(interval_seconds: u32) -> Result<()> {
    if interval_seconds < MINIMUM_INTERVAL_SECONDS {
        bail!("schedule interval must be at least {MINIMUM_INTERVAL_SECONDS} seconds");
    }
    Ok(())
}

fn canonical_regular_file(path: &Path, label: &str) -> Result<PathBuf> {
    let path = fs::canonicalize(path)
        .with_context(|| format!("failed to resolve {label} {}", path.display()))?;
    if !path.is_file() {
        bail!("{label} is not a regular file: {}", path.display());
    }
    Ok(path)
}

fn ensure_owner_only(path: &Path, label: &str) -> Result<()> {
    let metadata = path.metadata()?;
    if metadata.permissions().mode() & 0o077 != 0 {
        bail!(
            "{label} must not be accessible by group or other users: {}",
            path.display()
        );
    }
    Ok(())
}

fn validate_path_text(path: &Path, label: &str) -> Result<()> {
    let value = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("{label} path must be valid UTF-8"))?;
    if value.chars().any(char::is_control) {
        bail!("{label} path must contain no control characters");
    }
    Ok(())
}

fn path_string<'a>(path: &'a Path, label: &str) -> Result<&'a str> {
    validate_path_text(path, label)?;
    Ok(path.to_str().expect("path text was validated"))
}

fn ownership_marker(workspace_id: &str) -> String {
    format!("Temari-Managed-Workspace: {workspace_id}")
}

fn ensure_owned_or_missing(path: &Path, workspace_id: &str) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.uid() != effective_user_id() {
        bail!(
            "refusing to replace schedule definition not owned by this user: {}",
            path.display()
        );
    }
    let contents = fs::read_to_string(path)
        .with_context(|| format!("failed to inspect schedule definition {}", path.display()))?;
    let systemd_marker = ownership_marker(workspace_id);
    let launchd_marker = format!(
        "<key>TemariManagedWorkspace</key>\n  <string>{}</string>",
        xml_escape(workspace_id)
    );
    if !(contents.contains(&systemd_marker) || contents.contains(&launchd_marker)) {
        bail!(
            "refusing to replace schedule definition not created by Temari: {}",
            path.display()
        );
    }
    Ok(())
}

// Rust's standard library does not expose geteuid. `id -u` is already required for launchd,
// while filesystem ownership checks need a syscall without invoking a shell.
#[cfg(unix)]
fn effective_user_id() -> u32 {
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    unsafe { geteuid() }
}

fn atomic_write_private(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("schedule definition has no parent directory"))?;
    create_private_directory(parent)?;
    let mut temporary = NamedTempFile::new_in(parent)
        .with_context(|| format!("failed to create temporary file in {}", parent.display()))?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    temporary.write_all(contents)?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to install schedule definition {}", path.display()))?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

fn create_private_directory(path: &Path) -> Result<()> {
    if path.exists() {
        if !path.is_dir() {
            bail!("schedule directory is not a directory: {}", path.display());
        }
        return Ok(());
    }
    fs::create_dir_all(path)
        .with_context(|| format!("failed to create schedule directory {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn systemd_word(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('%', "%%")
            .replace('$', "$$")
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    )
}

fn stable_executable(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        bail!(
            "Temari executable path must be absolute: {}",
            path.display()
        );
    }
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("Temari executable path has no file name"))?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Temari executable path has no parent directory"))?;
    let path = fs::canonicalize(parent)
        .with_context(|| {
            format!(
                "failed to resolve Temari executable parent {}",
                parent.display()
            )
        })?
        .join(file_name);
    let metadata = fs::metadata(&path)
        .with_context(|| format!("failed to inspect Temari executable {}", path.display()))?;
    if !metadata.is_file() {
        bail!(
            "Temari executable is not a regular file: {}",
            path.display()
        );
    }
    validate_path_text(&path, "Temari executable")?;
    Ok(path)
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).into()).collect()
}

fn systemd_service_name(workspace_id: &str) -> String {
    format!("{SYSTEMD_PREFIX}{workspace_id}.service")
}

fn systemd_timer_name(workspace_id: &str) -> String {
    format!("{SYSTEMD_PREFIX}{workspace_id}.timer")
}

fn launchd_label(workspace_id: &str) -> String {
    format!("{LAUNCHD_PREFIX}{workspace_id}")
}

fn platform_name(platform: SchedulerPlatform) -> &'static str {
    match platform {
        SchedulerPlatform::Systemd => "systemd",
        SchedulerPlatform::Launchd => "launchd",
        SchedulerPlatform::Auto => "automatic scheduler",
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, collections::VecDeque, os::unix::process::ExitStatusExt};

    use tempfile::tempdir;

    use super::*;

    struct FakeRunner {
        calls: RefCell<Vec<(String, Vec<String>)>>,
        outputs: RefCell<VecDeque<Output>>,
    }

    impl FakeRunner {
        fn successful(count: usize) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                outputs: RefCell::new(
                    (0..count)
                        .map(|_| output(0, "", ""))
                        .collect::<VecDeque<_>>(),
                ),
            }
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, program: &str, args: &[String]) -> Result<Output> {
            self.calls.borrow_mut().push((program.into(), args.into()));
            Ok(self
                .outputs
                .borrow_mut()
                .pop_front()
                .unwrap_or_else(|| output(1, "", "unexpected fake command")))
        }
    }

    fn output(code: i32, stdout: &str, stderr: &str) -> Output {
        Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.as_bytes().into(),
            stderr: stderr.as_bytes().into(),
        }
    }

    fn fixture() -> (tempfile::TempDir, ScheduleSpec, ScheduleDirectories) {
        let root = tempdir().unwrap();
        let binary = root.path().join("temari $archive % tool");
        let config = root.path().join("config & private.toml");
        let state = root.path().join("state.sqlite3");
        let source = root.path().join("source with spaces");
        fs::write(&binary, "binary").unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(&config, "model = 'test'").unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(&state, "state").unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o600)).unwrap();
        fs::create_dir(&source).unwrap();
        let spec =
            ScheduleSpec::new("managed-123", &binary, &config, &state, &source, 300).unwrap();
        let directories = ScheduleDirectories {
            home: root.path().join("home"),
            config: root.path().join("config-root"),
        };
        (root, spec, directories)
    }

    #[test]
    fn validates_workspace_and_interval() {
        assert!(validate_workspace_id("managed-123_ok").is_ok());
        assert!(validate_workspace_id("../escape").is_err());
        assert!(validate_workspace_id("").is_err());
        assert!(validate_interval(59).is_err());
        assert!(validate_interval(60).is_ok());
    }

    #[test]
    fn schedule_spec_requires_private_config() {
        let root = tempdir().unwrap();
        let binary = root.path().join("temari");
        let config = root.path().join("config.toml");
        let state = root.path().join("state.sqlite3");
        let source = root.path().join("source");
        fs::write(&binary, "binary").unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(&config, "secret").unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o644)).unwrap();
        fs::write(&state, "state").unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o600)).unwrap();
        fs::create_dir(&source).unwrap();
        assert!(ScheduleSpec::new("managed-1", &binary, &config, &state, &source, 60).is_err());
    }

    #[test]
    fn renders_shell_free_systemd_definitions_with_safe_escaping() {
        let (_root, spec, directories) = fixture();
        let definitions =
            render_schedule_in(&spec, SchedulerPlatform::Systemd, &directories).unwrap();
        assert_eq!(definitions.len(), 2);
        assert!(
            definitions[0]
                .path
                .ends_with("temari-managed-managed-123.service")
        );
        assert!(
            definitions[1]
                .path
                .ends_with("temari-managed-managed-123.timer")
        );
        assert!(
            definitions[0]
                .contents
                .contains("Temari-Managed-Workspace: managed-123")
        );
        assert!(definitions[0].contents.contains("temari $$archive %% tool"));
        assert!(
            definitions[0]
                .contents
                .contains("\"managed\" \"run\" \"managed-123\" \"--apply\" \"--yes\"")
        );
        assert!(!definitions[0].contents.contains("/bin/sh"));
        assert!(definitions[1].contents.contains("OnUnitActiveSec=300s"));
    }

    #[test]
    fn managed_run_argv_contains_the_workspace_once() {
        let (_root, spec, _directories) = fixture();
        let arguments = managed_run_arguments(&spec).unwrap();
        assert_eq!(
            &arguments[arguments.len() - 5..],
            ["managed", "run", "managed-123", "--apply", "--yes"]
        );
        assert_eq!(
            arguments
                .iter()
                .filter(|argument| argument.as_str() == "managed-123")
                .count(),
            1
        );
    }

    #[test]
    fn renders_launchd_program_arguments_and_escaped_paths() {
        let (_root, spec, directories) = fixture();
        let definitions =
            render_schedule_in(&spec, SchedulerPlatform::Launchd, &directories).unwrap();
        assert_eq!(definitions.len(), 1);
        assert!(
            definitions[0].path.ends_with(
                "Library/LaunchAgents/dev.yutakobayashi.temari.managed.managed-123.plist"
            )
        );
        let plist = &definitions[0].contents;
        assert!(plist.contains("<key>ProgramArguments</key>"));
        assert!(plist.contains("config &amp; private.toml"));
        assert!(plist.contains("<integer>300</integer>"));
        assert!(plist.contains("<key>TemariManagedWorkspace</key>"));
        assert!(!plist.contains("/bin/sh"));
    }

    #[test]
    fn install_writes_private_owned_systemd_files_and_enables_timer() {
        let (_root, spec, directories) = fixture();
        let runner = FakeRunner::successful(5);
        let status =
            install_schedule_with(&spec, SchedulerPlatform::Systemd, &directories, &runner)
                .unwrap();
        assert!(status.installed);
        for path in &status.definition_paths {
            assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        }
        let calls = runner.calls.borrow();
        assert!(
            calls
                .iter()
                .any(|(_, args)| args == &strings(&["--user", "daemon-reload"]))
        );
        assert!(
            calls
                .iter()
                .any(|(_, args)| args.contains(&"enable".into()) && args.contains(&"--now".into()))
        );
    }

    #[test]
    fn reinstall_replaces_only_its_owned_definitions() {
        let (_root, spec, directories) = fixture();
        install_schedule_with(
            &spec,
            SchedulerPlatform::Systemd,
            &directories,
            &FakeRunner::successful(5),
        )
        .unwrap();
        let status = install_schedule_with(
            &spec,
            SchedulerPlatform::Systemd,
            &directories,
            &FakeRunner::successful(5),
        )
        .unwrap();
        assert!(status.installed);
        assert!(status.definition_paths.iter().all(|path| path.is_file()));
    }

    #[test]
    fn install_refuses_to_replace_foreign_definition() {
        let (_root, spec, directories) = fixture();
        let path =
            definition_paths("managed-123", SchedulerPlatform::Systemd, &directories).remove(0);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "[Unit]\nDescription=not temari\n").unwrap();
        let runner = FakeRunner::successful(1);
        let error = install_schedule_with(&spec, SchedulerPlatform::Systemd, &directories, &runner)
            .unwrap_err();
        assert!(error.to_string().contains("not created by Temari"));
    }

    #[test]
    fn uninstall_removes_only_owned_definitions() {
        let (_root, spec, directories) = fixture();
        let install_runner = FakeRunner::successful(5);
        install_schedule_with(
            &spec,
            SchedulerPlatform::Systemd,
            &directories,
            &install_runner,
        )
        .unwrap();
        let unrelated = directories.config.join("systemd/user/unrelated.timer");
        fs::write(&unrelated, "keep").unwrap();

        let uninstall_runner = FakeRunner {
            calls: RefCell::new(Vec::new()),
            outputs: RefCell::new(VecDeque::from([
                output(0, "", ""),
                output(0, "", ""),
                output(0, "", ""),
                output(0, "", ""),
                output(0, "", ""),
                output(1, "", "disabled"),
                output(1, "", "inactive"),
                output(0, "", ""),
                output(1, "", "disabled"),
                output(1, "", "inactive"),
            ])),
        };
        let status = uninstall_schedule_with(
            &spec.workspace_id,
            SchedulerPlatform::Systemd,
            &directories,
            &uninstall_runner,
        )
        .unwrap();
        assert!(!status.installed);
        assert!(unrelated.is_file());
        assert!(status.definition_paths.iter().all(|path| !path.exists()));
    }

    #[test]
    fn uninstall_keeps_definitions_when_stop_fails() {
        let (_root, spec, directories) = fixture();
        install_schedule_with(
            &spec,
            SchedulerPlatform::Systemd,
            &directories,
            &FakeRunner::successful(5),
        )
        .unwrap();
        let runner = FakeRunner {
            calls: RefCell::new(Vec::new()),
            outputs: RefCell::new(VecDeque::from([
                output(0, "", ""),
                output(0, "", ""),
                output(0, "", ""),
                output(1, "", "could not disable"),
            ])),
        };

        assert!(
            uninstall_schedule_with(
                &spec.workspace_id,
                SchedulerPlatform::Systemd,
                &directories,
                &runner,
            )
            .is_err()
        );
        assert!(
            definition_paths(&spec.workspace_id, SchedulerPlatform::Systemd, &directories)
                .iter()
                .all(|path| path.is_file())
        );
    }
}
