# Desktop proof of concept

## Scope

The Tauri 2 application under `apps/temari-desktop` is a private proof of concept for Linux and macOS. It exposes the managed three-area workflow while keeping filesystem safety and orchestration in the shared `ManagedService` in `temari-core`.

The native flow is:

1. Select a local source directory and model configuration. Existing Desktop, Downloads, and Documents directories are offered as shortcuts, while the operating-system picker accepts another root. Shortcuts only fill the reviewed setup form; they never register a workspace implicitly.
2. Request and edit a compact AI Library hierarchy from file-name metadata.
3. Review the backend-held setup preview: existing directories go to `Manual Library`, loose files go to `Recents`, and approved destinations are created below `AI Library`.
4. Confirm and apply that exact preview, creating durable setup artifacts and registering the workspace in the shared SQLite state.
5. Run explicit finite cycles that adopt new root directories, stage new files, and classify eligible Recents files.
6. Inspect workspace health, per-file Recents waiting reasons, next eligibility, actionable runs, and detailed move history. Visible windows refresh finite read-only snapshots every 30 seconds; hidden windows and open editors do not poll.
7. Reprocess selected `Manual Library` or `AI Library` entries through Recents when classification should run again.
8. Undo a completed run or one selected move with a separate durable Undo journal.
9. Pause a workspace to review an ordered batch of logical AI Library edits, including explicit subtree behavior, then Apply, Undo, or Redo that Configure session.

## Trust boundary

- The frontend cannot replace proposal provenance. Revisioned, process-unique proposal and preview tokens resolve only to the latest backend-held `Proposal`, approved `FolderSet`, and `ManagedSetupPlan`.
- Setup preview is read-only and Apply atomically consumes its opaque token before mutation. Replays, concurrent duplicate Apply requests, and stale tokens are rejected without discarding a newer preview. The frontend cannot submit a Plan, source identity, destination path, or journal path.
- The canonical model configuration path is stored with the workspace and revalidated before runs. Manual runs and generated schedules therefore use the same model and privacy policy selected during setup.
- `ManagedService` owns workspace activation, binding validation, private artifact directories, Recents reconciliation, stage/classify persistence, Apply finalization, directory adoption, and explicit reprocessing for both CLI and desktop.
- Managed filesystem and model operations run on blocking workers instead of the Tauri event loop.
- Workspace status is computed by Core from physical Recents, indexed deadlines, and actionable runs. IPC responses use explicit camel-case view types and omit internal artifact paths rather than exposing database records.
- History merges immutable file Apply/Undo journals and directory-adoption setup/Undo journals. File moves support session or individual Undo; directory adoption supports session Undo only so managed areas remain intact. Undo allocates the journal path in the backend and reconciles SQLite only after filesystem outcomes are known.
- AI Library structure editing exposes only model-visible destinations. An ordered batch of Add, Rename, Description, and Delete operations produces one immutable `FolderSet` revision while preserving existing opaque IDs. Parent edits require an explicit reject, cascade, or reparent policy. The confirmation shows the exact backend-produced before/after delta, including affected descendants, before Apply atomically consumes the preview token and updates both bindings.
- AI Library edits are logical configuration changes: they do not rename directories or move existing files. The editor links back to Reprocess when existing files should pass through Recents under the revised structure.
- Configure Apply, Undo, and Redo require a disabled workspace. System fallbacks, the final visible destination, active-rule targets anywhere in a removed subtree, stale previews, newer revisions, and workspaces with unfinished runs are rejected.
- New root directories are moved to `Manual Library`. A classified file manually returned to the root retains its processed identity and is left in place until the user explicitly requests reprocessing.
- Setup rejects a managed area or any directory below one before requesting a folder proposal. The same activation boundary remains enforced in `temari-core`; a native picker never grants a selected path authority by itself.
- OS scheduling uses `temari-schedule`, creates shell-free systemd or launchd definitions, and requires an explicitly selected stable Temari CLI executable. Setup never installs a schedule implicitly.
- Managed artifacts live below `ProjectDirs::state_dir()/managed-runs`, falling back to `data_local_dir()` where needed. Directories are mode `0700` and artifacts are mode `0600` on Linux and macOS.
- Tauri capabilities grant the main window only core defaults and access to the native open dialog.
- The content security policy allows packaged assets and Tauri IPC; no arbitrary remote page or script is enabled.
- No telemetry, updater, browser shell, or direct filesystem API is included.

## Development

Install the pinned frontend dependencies and start the native application:

```console
$ nix develop
$ corepack pnpm --dir apps/temari-desktop install --frozen-lockfile
$ corepack pnpm --dir apps/temari-desktop tauri dev
```

The repository dev shell supplies Rust, pnpm, the Linux Tauri libraries, and the GTK 3 file-chooser schema. On macOS it omits Linux-only libraries and uses the system WebKit framework.

The desktop automatically loads `config.toml` from the directory returned by `ProjectDirs::config_dir` (`$XDG_CONFIG_HOME/temari` on Linux and Application Support on macOS). Use the native file picker to override it for the current session. The backend accepts only absolute regular-file paths and never searches relative to the process working directory or selected source. Keep the config outside the folder being organized. The file follows the same format as `examples/temari.example.toml`.

For frontend-only work, start the Vite preview:

```console
$ corepack pnpm --dir apps/temari-desktop dev
```

Outside Tauri, the API adapter uses fixed sample data and simulates Apply, Undo, and Redo. This browser mode is visually useful but does not access the filesystem, write journals, or prove native model integration.

Run the independent checks with:

```console
$ corepack pnpm --dir apps/temari-desktop build
$ cargo check --manifest-path apps/temari-desktop/src-tauri/Cargo.toml
$ cargo test --manifest-path apps/temari-desktop/src-tauri/Cargo.toml --lib
```

Linux requires the normal Tauri WebKitGTK, GTK, GLib, Cairo, Pango, ATK, and D-Bus development libraries. macOS uses the system WebKit framework.
