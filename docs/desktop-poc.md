# Desktop proof of concept

## Scope

The Tauri 2 application under `apps/temari-desktop` is a non-mutating proof of concept for Linux and macOS. Its purpose is to validate the guided review experience while keeping the CLI workflow authoritative.

The native flow is:

1. Select a local source directory with the operating-system picker.
2. Scan file-name metadata through `temari-core` and summarize the result locally.
3. Request a compact hierarchy from the model configured by the selected TOML path.
4. Edit destination paths and descriptions in the review panel.
5. Approve the edited destinations locally, preserving the source and scan scope recorded by the backend.

There is no desktop command for planning or applying moves. The disabled Apply control communicates that boundary. Filesystem mutation remains available only through the reviewed CLI artifacts until the desktop can present the exact Plan, confirmation, journal, recovery, and undo guarantees.

## Trust boundary

- The frontend cannot replace proposal provenance during approval. The backend retains the latest real `Proposal` and accepts only edited `FolderProposal` entries.
- Scanning, proposal generation, and approval use existing `temari-core` types and validation.
- Proposal generation receives file-name metadata only. Content extraction is outside this POC.
- Tauri capabilities grant the main window only core defaults and access to the native open dialog.
- The content security policy allows packaged assets and Tauri IPC; no arbitrary remote page or script is enabled.
- No telemetry, updater, browser shell, or direct filesystem API is included.

## Development

Install the pinned frontend dependencies and start the native application:

```console
$ corepack pnpm --dir apps/temari-desktop install --frozen-lockfile
$ corepack pnpm --dir apps/temari-desktop tauri dev
```

The config field accepts an absolute path when the process working directory is uncertain. The file follows the same format as `examples/temari.example.toml`.

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
