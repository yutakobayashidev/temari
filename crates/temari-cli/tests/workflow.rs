use std::{fs, path::Path, process::Command};

use temari_core::{
    ApplyState, Classification, ClassificationBasis, FileCandidate, FolderProposal, MoveOutcome,
    Plan, Proposal, apply_plan, build_plan,
};
use tempfile::tempdir;

fn write_plan(source: &Path, path: &Path) -> Plan {
    fs::write(source.join("report.txt"), b"report").unwrap();
    let folders = Proposal {
        version: 1,
        source: source.display().to_string(),
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
        &[FileCandidate {
            id: "f000001".into(),
            name: "report.txt".into(),
            extension: "txt".into(),
        }],
        &folders,
        vec![Classification {
            file_id: "f000001".into(),
            destination_id: "d000001".into(),
            reasoning: None,
            basis: ClassificationBasis::Name,
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
