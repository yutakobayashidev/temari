# Temari

`Temari` is a private, personal-use tool that plans and applies file organization with a local model or a model hosted on an explicitly trusted internal network.

Proposal, approval, and planning are read-only. Filesystem changes require a separate confirmed `apply` command and produce a durable JSON journal that can be inspected or passed to `undo`. The default `ask` privacy policy requests per-run consent only when names are ambiguous; only bounded extracted text is eligible to be sent. The application does not send telemetry.

## Trust boundaries

- During classification, the model selects only user-approved, model-visible opaque destination IDs. Model-proposed folder names remain untrusted data until local approval validates them and assigns those IDs.
- Approval adds deterministic `Others/*` fallbacks as opaque destinations. Automatically added fallbacks are local-only; an identically named user destination is reused.
- The entire response is rejected if it contains an unknown file ID, unknown destination ID, duplicate result, or missing result.
- Symlinks, directories, and paths outside the source root are excluded from classification.
- Root-level files are always included. Recursive traversal is opt-in through artifact-bound source-relative subtrees, and approved destination subtrees are excluded to prevent reprocessing.
- Plans contain local SHA-256, size, device, and inode fingerprints. These values and raw files are never sent to the model. Extracted text is neither logged nor persisted.
- Apply revalidates the source root, fingerprints, real directory components, and unoccupied destinations before mutation. It never overwrites.
- Apply, resume, and undo hold an exclusive advisory lock on the canonical source directory so competing writers fail before mutation.
- Undo tolerates a consistent filesystem device-number change across reboot, while still requiring the same source inode and matching file hashes and inodes.
- Undo restores only recorded moves whose identity and content still match, and removes only session-created directories that remain empty.
- Every model endpoint hostname must appear in an explicit allowlist.
- API keys may be stored in an owner-only private config or read from a configured environment variable. They are never serialized into workflow artifacts.
- No external service, analytics SDK, or crash-reporting SDK is included.

## Requirements

- Rust 1.85 or later
- An OpenAI Chat Completions-compatible API, such as Ollama, llama.cpp, or vLLM
- Linux or macOS

## Quick start

For the guided human workflow:

```console
$ cp examples/temari.example.toml .temari.toml
$ cargo run -p temari-cli -- organize ~/Downloads --out downloads-run
```

`organize` is TTY-only. It preserves the raw proposal, opens an optional `$VISUAL` or `$EDITOR` review path, asks separately whether to approve destinations and apply exact moves, then prints an undo command.

The equivalent primitive workflow for agents and scripts is:

```console
$ cargo run -p temari-cli -- propose ~/Downloads --out downloads.proposal.json
$ $EDITOR downloads.proposal.json
$ cargo run -p temari-cli -- approve downloads.proposal.json --out downloads.folders.json
$ cargo run -p temari-cli -- plan ~/Downloads --folders downloads.folders.json --out downloads.plan.json
$ cargo run -p temari-cli -- apply downloads.plan.json --out downloads.apply.json
$ cargo run -p temari-cli -- undo downloads.apply.json --out downloads.undo.json
```

To include selected existing directories recursively, repeat `--include-subtree`. Use `.` only when the entire source tree should be included:

```console
$ cargo run -p temari-cli -- propose ~/Downloads \
    --include-subtree Receipts --include-subtree Work --out downloads.proposal.json
$ cargo run -p temari-cli -- organize ~/Downloads \
    --include-subtree . --out downloads-run
```

The selected scope is stored in the proposal, approved folder set, and plan. `plan` does not accept a replacement scope.

If an apply journal remains `running` after a crash, reconcile and continue it explicitly:

```console
$ cargo run -p temari-cli -- resume downloads.apply.json
```

## Desktop proof of concept

`apps/temari-desktop` contains a Tauri 2 proof of concept for Linux and macOS. The native app uses `temari-core` to select and scan a source, request a folder proposal from the configured model, edit and approve it locally, then preview every exact source-to-destination move in a read-only Plan. Apply requires a final confirmation for that exact Plan digest, persists the Plan before mutation, and writes a durable Apply journal. The same session can be undone through a separate confirmation and Undo journal.

```console
$ nix develop
$ corepack pnpm --dir apps/temari-desktop install
$ corepack pnpm --dir apps/temari-desktop tauri dev
```

Desktop automatically loads `config.toml` from the platform application-config directory (`$XDG_CONFIG_HOME/temari` on Linux and Application Support on macOS). The native file picker can override it for the current session. Desktop accepts only absolute regular-file paths and never searches the source folder or process working directory. Plan preview repeats the scan against the backend-held approved destinations. Under `privacy.content = "ask"`, ambiguous files use local fallbacks because this POC has no content-consent screen; explicit `on_demand` retains bounded extraction. Desktop workflow artifacts are private files under the platform application-state directory and remain usable by the CLI for explicit recovery after the app restarts. Running `pnpm dev` without Tauri opens an explicitly simulated browser preview for UI development; simulated Apply and Undo do not access the filesystem or model. See [the desktop POC notes](docs/desktop-poc.md) for its command boundary and verification steps.

## Foreground monitoring

Register an approved folder set, optionally add deterministic basename rules, then run a read-only check:

```console
$ temari monitor add ~/Downloads --folders /absolute/path/downloads.folders.json
$ temari monitor list
$ temari rule add --monitor <MONITOR_ID> --name-glob 'invoice-*.pdf' \
    --destination d000001 --priority 100
$ temari monitor run --out ~/.local/state/temari/runs --once
$ temari history list
$ temari monitor apply <RUN_ID> --yes
```

`monitor run --once` writes an immutable Plan but does not move files. After reviewing it, `monitor apply <RUN_ID>` uses the same recorded Plan, writes an ApplySession beside it, and updates processed state only after completion. Continuous foreground polling is deliberately more explicit:

```console
$ temari monitor run --out ~/.local/state/temari/runs --apply --yes
```

No daemon or login service is installed. Stop the foreground process with the normal process signal. Every mutation cycle writes its Plan before applying it and uses the normal ApplySession journal. Only a completed journal marks a fingerprint as processed; startup reconciliation can finish the SQLite index after a crash, while a running journal still requires explicit `temari resume`.

Monitoring stores definitions, schedules, rules, processed signatures, and run indexes in SQLite. Authoritative workflow artifacts remain JSON files under `--out`. The default database uses the platform user-state directory; override it with global `--state PATH`. Both the database and run artifacts must remain outside monitored sources.

Rules match file basenames case-insensitively in descending priority and stable ID order. They select reviewed opaque destination IDs locally before any model call. Rule changes alter the processing signature. In unattended monitoring, `privacy.content = "ask"` behaves as no consent and uses approved local fallbacks; choose `on_demand` explicitly to permit bounded extracted text.

The approval command previews every destination and asks for confirmation when stdin and stderr are terminals. For a proposal that has already been reviewed by an agent or script, make acceptance explicit:

```console
$ cargo run -p temari-cli -- approve downloads.proposal.json \
    --accept-all --no-input --out downloads.folders.json
```

To use an internal model, change `model.base_url` and add its hostname to `model.allowed_hosts`. The allowlist is enforced immediately before the request; it is not documentation-only configuration.

The default interactive privacy policy is:

```toml
[privacy]
content = "ask"
max_content_chars = 20000
max_content_file_bytes = 10485760
```

The name pass runs once for every file. Only `needs_content` results reach the consent boundary. In interactive `organize`, `ask` displays the model origin, exact ambiguous paths, extraction limits, and OCR status; declining uses local fallbacks and continues. Primitive `plan` never prompts and requires `metadata_only` or `on_demand` if ambiguity occurs. Temari reads bounded UTF-8, PDF, DOCX, PPTX, XLSX, ODT, ODP, and ODS text on both platforms. Optional OCR supports common raster images through one explicitly configured local executable. Raw files are never uploaded, and extracted text and consent are never persisted.

Document containers are parsed in memory without unpacking files. Archive expansion, entry count, XML events, XML depth, output bytes, output characters, and OCR runtime are bounded. OCR is disabled unless `[privacy.extraction.ocr]` is present; Temari invokes that executable directly with fixed arguments and never through a shell.

## CLI

```text
temari [--config PATH] [--state PATH] [--json] [--no-input] [--no-color] [-v] <COMMAND>

Commands:
  propose <SOURCE> --out <PROPOSAL> [--include-subtree <PATH>]...
  approve <PROPOSAL> --out <FOLDER_SET>             Validate and approve it
  plan <SOURCE> --folders <FOLDER_SET> --out <PLAN> Classify files without changing them
  apply <PLAN> --out <APPLY_SESSION> [--yes]         Create directories and move files
  undo <APPLY_SESSION> --out <UNDO_SESSION> [--yes] Restore safely recorded moves
  resume <APPLY_SESSION> [--yes]                     Reconcile and continue a running apply
  organize <SOURCE> --out <RUN_DIR> [--include-subtree <PATH>]...
  monitor add|list|enable|disable|remove|run|apply ...
  rule add|list|enable|disable|remove ...
  history list|show ...
```

- Artifact paths go to stdout. Read-only commands accept `--out -` to emit artifact JSON. Apply and undo require persistent journal paths outside the organized source.
- Diagnostics, progress, prompts, and errors go to stderr.
- With a file output, `--json` emits a machine-readable result containing the output path.
- `--no-input` prevents prompts. Approval additionally requires `--accept-all`; apply, undo, and resume require `--yes`. Primitive `plan` never prompts under any policy.
- `resume` updates only an unfinished `running` journal; terminal sessions remain immutable. `organize` rejects non-interactive execution.
- Exit code `0` means success, `1` means a runtime failure, and `2` means invalid arguments.

## Product flow

The workflow follows five explicit trust boundaries:

1. `propose`: the model suggests a compact folder hierarchy from file-name metadata. Generated paths default to at most two components, and `--max-folders` bounds all physical directories including implicit parents.
2. `approve`: the user edits and approves the proposal; local code assigns opaque destination IDs, validates every relative path, and adds visible local-only extension fallbacks.
3. `plan`: local rules may select approved IDs before the model classifies remaining names; ambiguous files optionally use bounded extracted text, and unresolved files use approved local fallback IDs. The command writes a validated, read-only plan with its classification basis and optional rule ID per move.
4. `apply`: after confirmation, local code creates only required approved directories and performs validated moves while atomically updating an audit journal.
5. `undo`: local code conservatively reverses recorded moves and removes only unchanged, empty directories created by that apply session.

All five primitive stages, explicit crash resume, the interactive `organize` orchestrator, and foreground monitoring are implemented. See [ADR 0002](docs/adr/0002-propose-and-create-approved-folders.md) for the filesystem safety policy, [ADR 0004](docs/adr/0004-use-json-journals-before-a-state-database.md) for artifact persistence, [ADR 0005](docs/adr/0005-adopt-on-demand-content-classification-and-local-fallbacks.md) for two-pass classification, [ADR 0006](docs/adr/0006-bind-explicit-recursive-scope-to-workflow-artifacts.md) for recursive scope, [ADR 0007](docs/adr/0007-adopt-bounded-document-and-ocr-extraction.md) for extraction limits, [ADR 0008](docs/adr/0008-use-sqlite-for-monitoring-state.md) for monitoring state, [ADR 0009](docs/adr/0009-request-per-run-content-consent.md) for consent, [ADR 0010](docs/adr/0010-load-desktop-config-from-platform-directory.md) for desktop configuration and private credentials, and [ADR 0011](docs/adr/0011-apply-backend-held-desktop-plans.md) for confirmed desktop Apply and Undo.

Example schemas are available for a model-created [proposal](examples/proposal.example.json) and a locally approved [folder set](examples/folders.example.json). Their source paths are illustrative and must match the canonical source used by `plan`.

## Development

```console
$ nix develop
$ cargo fmt --all -- --check
$ cargo clippy --workspace --all-targets -- -D warnings
$ cargo test --workspace
```

See [the CLI specification](docs/cli-spec.md), [ADR 0001](docs/adr/0001-adopt-rust-core-and-read-only-cli.md), and [ADR 0003](docs/adr/0003-redesign-cli-around-versioned-workflow-artifacts.md).

## Implementation boundary

This repository is an independent implementation. Product behavior and safety requirements are documented and tested within this repository. It does not contain third-party source code, branding, UI assets, internal identifiers, prompt text, database schemas, or log strings. This repository is intended for private use and is not planned for publication.
