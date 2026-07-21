use std::{fs, path::Path, process::Command};

use temari_core::{
    ApplyState, Classification, ClassificationBasis, FileCandidate, FolderProposal, MoveOutcome,
    Plan, Proposal, RunState, apply_plan, build_plan,
};
use tempfile::tempdir;

fn write_plan(source: &Path, path: &Path) -> Plan {
    fs::write(source.join("report.txt"), b"report").unwrap();
    let folders = Proposal {
        version: 2,
        source: source.display().to_string(),
        scope: temari_core::ScanScope::default(),
        files_considered: 1,
        folders: vec![FolderProposal {
            path: "Documents".into(),
            description: "Documents".into(),
        }],
    }
    .approve()
    .unwrap()
    .folders;
    let plan = build_plan(
        source,
        &temari_core::ScanScope::default(),
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
    .unwrap();
    fs::write(path, serde_json::to_vec_pretty(&plan).unwrap()).unwrap();
    plan
}

#[test]
fn non_interactive_apply_requires_yes_then_undo_restores_the_file() {
    let source = tempdir().unwrap();
    let artifacts = tempdir().unwrap();
    let plan_path = artifacts.path().join("plan.json");
    let apply_path = artifacts.path().join("apply.json");
    let undo_path = artifacts.path().join("undo.json");
    write_plan(source.path(), &plan_path);

    let refused = Command::new(env!("CARGO_BIN_EXE_temari"))
        .args(["--no-input", "apply"])
        .arg(&plan_path)
        .args(["--out"])
        .arg(&apply_path)
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(!apply_path.exists());
    assert!(source.path().join("report.txt").exists());

    let applied = Command::new(env!("CARGO_BIN_EXE_temari"))
        .args(["--no-input", "apply"])
        .arg(&plan_path)
        .args(["--yes", "--out"])
        .arg(&apply_path)
        .output()
        .unwrap();
    assert!(
        applied.status.success(),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    assert!(source.path().join("Documents/report.txt").exists());

    let undone = Command::new(env!("CARGO_BIN_EXE_temari"))
        .args(["--no-input", "undo"])
        .arg(&apply_path)
        .args(["--yes", "--out"])
        .arg(&undo_path)
        .output()
        .unwrap();
    assert!(
        undone.status.success(),
        "{}",
        String::from_utf8_lossy(&undone.stderr)
    );
    assert!(source.path().join("report.txt").exists());
    assert!(!source.path().join("Documents").exists());
}

#[test]
fn resume_command_reconciles_and_continues_a_running_session() {
    let source = tempdir().unwrap();
    let artifacts = tempdir().unwrap();
    let plan_path = artifacts.path().join("plan.json");
    let apply_path = artifacts.path().join("apply.json");
    let plan = write_plan(source.path(), &plan_path);
    let mut session = apply_plan(&plan, &apply_path).unwrap();
    fs::rename(
        source.path().join("Documents/report.txt"),
        source.path().join("report.txt"),
    )
    .unwrap();
    session.state = ApplyState::Running;
    session.finished_unix_ms = None;
    session.moves[0].outcome = MoveOutcome::Moving;
    fs::write(&apply_path, serde_json::to_vec_pretty(&session).unwrap()).unwrap();

    let resumed = Command::new(env!("CARGO_BIN_EXE_temari"))
        .args(["--no-input", "resume"])
        .arg(&apply_path)
        .arg("--yes")
        .output()
        .unwrap();

    assert!(
        resumed.status.success(),
        "{}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    assert!(source.path().join("Documents/report.txt").exists());
    let session: temari_core::ApplySession =
        serde_json::from_slice(&fs::read(&apply_path).unwrap()).unwrap();
    assert_eq!(session.state, ApplyState::Completed);
}

#[test]
fn organize_rejects_non_interactive_use_before_creating_run_directory() {
    let source = tempdir().unwrap();
    let artifacts = tempdir().unwrap();
    let run_directory = artifacts.path().join("run");
    fs::write(source.path().join("report.txt"), b"report").unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_temari"))
        .args(["--no-input", "organize"])
        .arg(source.path())
        .arg("--out")
        .arg(&run_directory)
        .output()
        .unwrap();

    assert!(!result.status.success());
    assert!(!run_directory.exists());
}

#[test]
fn monitoring_cli_applies_a_local_rule_and_records_history() {
    let source = tempdir().unwrap();
    let artifacts = tempdir().unwrap();
    let source_path = fs::canonicalize(source.path()).unwrap();
    fs::write(source_path.join("invoice.pdf"), b"invoice").unwrap();
    let folder_set = Proposal {
        version: 2,
        source: source_path.display().to_string(),
        scope: temari_core::ScanScope::default(),
        files_considered: 1,
        folders: vec![FolderProposal {
            path: "Documents".into(),
            description: "Documents".into(),
        }],
    }
    .approve()
    .unwrap();
    let folders_path = artifacts.path().join("folders.json");
    let config_path = artifacts.path().join("temari.toml");
    let state_path = artifacts.path().join("state.sqlite3");
    let runs_path = artifacts.path().join("runs");
    fs::write(
        &folders_path,
        serde_json::to_vec_pretty(&folder_set).unwrap(),
    )
    .unwrap();
    let config = include_str!("../../../examples/temari.example.toml").replace(
        "# api_key_env = \"TEMARI_MODEL_API_KEY\"",
        "api_key_env = \"TEMARI_TEST_KEY_THAT_IS_NOT_SET\"",
    );
    fs::write(&config_path, config).unwrap();

    let added = Command::new(env!("CARGO_BIN_EXE_temari"))
        .args(["--state"])
        .arg(&state_path)
        .args(["monitor", "add"])
        .arg(&source_path)
        .args(["--folders"])
        .arg(&folders_path)
        .output()
        .unwrap();
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    let monitor_id = String::from_utf8(added.stdout).unwrap().trim().to_owned();

    let rule = Command::new(env!("CARGO_BIN_EXE_temari"))
        .args(["--state"])
        .arg(&state_path)
        .args(["rule", "add", "--monitor", &monitor_id])
        .args(["--name-glob", "*.pdf", "--destination", "d000001"])
        .output()
        .unwrap();
    assert!(
        rule.status.success(),
        "{}",
        String::from_utf8_lossy(&rule.stderr)
    );

    let run = Command::new(env!("CARGO_BIN_EXE_temari"))
        .args(["--config"])
        .arg(&config_path)
        .args(["--state"])
        .arg(&state_path)
        .args(["monitor", "run", "--monitor", &monitor_id, "--out"])
        .arg(&runs_path)
        .arg("--once")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(source_path.join("invoice.pdf").exists());

    let planned_history = Command::new(env!("CARGO_BIN_EXE_temari"))
        .args(["--json", "--state"])
        .arg(&state_path)
        .args(["history", "list", "--monitor", &monitor_id])
        .output()
        .unwrap();
    let planned_runs: Vec<temari_core::MonitoringRun> =
        serde_json::from_slice(&planned_history.stdout).unwrap();
    assert_eq!(planned_runs[0].state, RunState::Planned);

    let applied = Command::new(env!("CARGO_BIN_EXE_temari"))
        .args(["--state"])
        .arg(&state_path)
        .args(["monitor", "apply", &planned_runs[0].id, "--yes"])
        .output()
        .unwrap();
    assert!(
        applied.status.success(),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    assert!(source_path.join("Documents/invoice.pdf").exists());

    let history = Command::new(env!("CARGO_BIN_EXE_temari"))
        .args(["--json", "--state"])
        .arg(&state_path)
        .args(["history", "list", "--monitor", &monitor_id])
        .output()
        .unwrap();
    assert!(history.status.success());
    let runs: Vec<temari_core::MonitoringRun> = serde_json::from_slice(&history.stdout).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].state, RunState::Completed);
    assert_eq!(runs[0].rule_matches, 1);
}
