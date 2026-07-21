# Desktop proof of concept

## Scope

The Tauri 2 application under `apps/temari-desktop` is a non-mutating proof of concept for Linux and macOS. Its purpose is to validate the guided review experience while keeping the CLI workflow authoritative.

The native flow is:

1. Select a local source directory with the operating-system picker.
2. Scan file-name metadata through `temari-core` and summarize the result locally.
3. Request a compact hierarchy from the model configured by the selected TOML path.
4. Edit destination paths and descriptions in the review panel.
5. Approve the edited destinations locally, preserving the source and scan scope recorded by the backend.
6. Build and inspect a real, read-only Plan containing every exact source and collision-safe destination path.

There is no desktop command for applying moves. The disabled Apply control communicates that boundary. Filesystem mutation remains available only through the reviewed CLI artifacts until the desktop can present exact confirmation, journal, recovery, and undo guarantees.

## Trust boundary

- The frontend cannot replace proposal provenance during approval. The backend retains the latest real `Proposal` and accepts only edited `FolderProposal` entries.
- Plan preview accepts no source, configuration, folder set, classification, or destination path from the frontend. It uses the backend-held configuration and approved `FolderSet`.
- Scanning, proposal generation, approval, name classification, bounded content classification, fallback routing, and Plan construction use existing `temari-core` types and validation.
- Proposal generation receives file-name metadata only. During planning, explicit `on_demand` permits bounded extraction for ambiguous files. `ask` behaves as declined consent and uses local extension fallbacks because this POC does not yet include a content-consent screen.
- The resulting Plan contains local fingerprints and collision-safe destination paths, but remains in memory and cannot be applied through desktop IPC.
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

Outside Tauri, the API adapter uses fixed sample data. This browser mode is visually useful but does not prove native filesystem or model integration.

Run the independent checks with:

```console
$ corepack pnpm --dir apps/temari-desktop build
$ cargo check --manifest-path apps/temari-desktop/src-tauri/Cargo.toml
$ cargo test --manifest-path apps/temari-desktop/src-tauri/Cargo.toml --lib
```

Linux requires the normal Tauri WebKitGTK, GTK, GLib, Cairo, Pango, ATK, and D-Bus development libraries. macOS uses the system WebKit framework.
