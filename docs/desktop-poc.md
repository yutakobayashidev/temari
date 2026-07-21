# Desktop proof of concept

## Scope

The Tauri 2 application under `apps/temari-desktop` is a private proof of concept for Linux and macOS. It validates the complete guided workflow while keeping filesystem safety in `temari-core`.

The native flow is:

1. Select a local source directory with the operating-system picker.
2. Scan file-name metadata through `temari-core` and summarize the result locally.
3. Request a compact hierarchy from the model configured by the selected TOML path.
4. Edit destination paths and descriptions in the review panel.
5. Approve the edited destinations locally, preserving the source and scan scope recorded by the backend.
6. Build and inspect a real, read-only Plan containing every exact source and collision-safe destination path.
7. Confirm the exact Plan digest, persist the Plan, and apply its moves with a durable journal.
8. Undo the active Apply session from a separate confirmation and journal.

## Trust boundary

- The frontend cannot replace proposal provenance during approval. The backend retains the latest real `Proposal` and accepts only edited `FolderProposal` entries.
- Plan preview accepts no source, configuration, folder set, classification, or destination path from the frontend. It uses the backend-held configuration and approved `FolderSet`.
- Scanning, proposal generation, approval, name classification, bounded content classification, fallback routing, and Plan construction use existing `temari-core` types and validation.
- Proposal generation receives file-name metadata only. During planning, explicit `on_demand` permits bounded extraction for ambiguous files. `ask` behaves as declined consent and uses local extension fallbacks because this POC does not yet include a content-consent screen.
- The frontend can request Apply only with the SHA-256 digest of the latest backend-held Plan. It cannot submit a Plan, source path, destination path, or journal path.
- Before the first mutation, the backend writes the exact Plan to a private workflow run directory. Apply rechecks the source identity, fingerprints, directory chain, and destination occupancy through `temari-core`; it never overwrites.
- The frontend can request Undo only for the backend-held Apply session ID. Undo uses the immutable Apply journal and writes a separate Undo journal.
- Workflow runs live under `ProjectDirs::state_dir()/workflows`, falling back to `data_local_dir()/workflows` where a platform state directory is unavailable. Run directories are mode `0700`; Plan, Apply, and Undo artifacts are mode `0600` on Linux and macOS.
- A desktop restart does not automatically resume or rediscover a run. If a crash leaves an Apply journal in `running` state, use the CLI `resume` command with the displayed journal path. Completed or partial Apply journals can be passed to the CLI `undo` command after a restart.
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

Outside Tauri, the API adapter uses fixed sample data and simulates Apply and Undo. This browser mode is visually useful but does not access the filesystem, write journals, or prove native model integration.

Run the independent checks with:

```console
$ corepack pnpm --dir apps/temari-desktop build
$ cargo check --manifest-path apps/temari-desktop/src-tauri/Cargo.toml
$ cargo test --manifest-path apps/temari-desktop/src-tauri/Cargo.toml --lib
```

Linux requires the normal Tauri WebKitGTK, GTK, GLib, Cairo, Pango, ATK, and D-Bus development libraries. macOS uses the system WebKit framework.
