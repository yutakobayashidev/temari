# Temari

`Temari` is a privacy-focused, personal-use tool that plans and applies file organization with a local model or a model hosted on an explicitly trusted internal network.

> [!WARNING]
> Temari is pre-release software that moves files. Start with a backed-up test directory, inspect every generated Plan, and verify Undo before trusting it with important data. The desktop application is still a proof of concept.

Proposal, approval, and planning are read-only. Filesystem changes require a separate confirmed `apply` command and produce a durable JSON journal that can be inspected or passed to `undo`. The default `ask` privacy policy requests per-run consent only when names are ambiguous; only bounded extracted text is eligible to be sent. The application does not send telemetry.

For a long-lived folder, the managed workflow creates three physical areas: `Manual Library` protects existing directories, `Recents` holds new files through a configurable retention and stability window, and `AI Library` contains classified files under the reviewed folder hierarchy. Initial setup and every later move remain separately planned, journaled, and undoable.

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

- Rust 1.88 or later
- An OpenAI Chat Completions-compatible API, such as Ollama, llama.cpp, or vLLM
- Linux or macOS

## Install from source

Clone the repository and build the CLI with the pinned Rust toolchain:

```console
$ cargo build --release -p temari-cli
$ install target/release/temari ~/.local/bin/temari
```

Copy `examples/temari.example.toml` to the platform configuration directory or pass its absolute path with `--config`. Review `model.base_url`, `model.name`, and `model.allowed_hosts` before the first run. File names and extensions are sent to that configured model; bounded extracted text is sent only according to `privacy.content`. Raw files are never uploaded by Temari.

## Quick start

For the normal long-lived workflow, first review a folder hierarchy, then initialize the managed source:

```console
$ cp examples/temari.example.toml .temari.toml
$ cargo run -p temari-cli -- propose ~/Downloads --out downloads.proposal.json
$ cargo run -p temari-cli -- approve downloads.proposal.json --out downloads.folders.json
$ cargo run -p temari-cli -- managed init ~/Downloads --out downloads.setup-plan.json
$ cargo run -p temari-cli -- managed apply downloads.setup-plan.json \
    --folders downloads.folders.json \
    --out ~/.local/state/temari/downloads-setup
```

This creates `Manual Library`, `Recents`, and `AI Library`. Run one reviewed cycle with `temari managed run <WORKSPACE_ID>`; Temari allocates a private artifact directory below its state directory. Add `--apply --yes` when the generated Plans should be applied, or keep `--out <RUN_DIR>` when a caller needs an explicit location.

For a one-time folder cleanup instead of ongoing management, use the TTY-only `organize` command:

```console
$ cargo run -p temari-cli -- organize /path/to/one-time-folder --out cleanup-run
```

The top-level `propose`, `approve`, `plan`, `apply`, `undo`, and `resume` commands are advanced primitives for agents, scripts, inspection, and recovery. They expose the same versioned artifacts used by managed workspaces.

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

`apps/temari-desktop` contains a Tauri 2 proof of concept for Linux and macOS. Its primary surface is the same managed three-area workflow as the CLI: propose an AI Library hierarchy, review the exact setup preview, apply the backend-held preview, run finite organization cycles, inspect status and move history, reprocess selected files through Recents, edit the logical Library, separately review physical reorganization moves, and undo either a complete run or one move. The CLI and desktop both call the shared `ManagedService` in `temari-core`; neither shells out to the other. OS schedule rendering and installation are shared through the separate `temari-schedule` crate.

```console
$ nix develop
$ corepack pnpm --dir apps/temari-desktop install
$ corepack pnpm --dir apps/temari-desktop tauri dev
```

Desktop automatically loads `config.toml` from the platform application-config directory (`$XDG_CONFIG_HOME/temari` on Linux and Application Support on macOS). The native file picker can override it while proposing a workspace, and the canonical configuration path is then bound to that workspace so manual and scheduled runs use the same model and privacy policy. Setup proposal and preview tokens refer only to backend-held artifacts, are single-use, and cannot be replaced with a frontend-submitted Plan or journal path. Managed state uses the same platform SQLite database and private JSON artifact directories as the CLI. Enabling an OS schedule additionally requires selecting a stable Temari CLI executable because the desktop binary does not pretend to implement CLI subcommands. Running `pnpm dev` without Tauri opens an explicitly simulated browser preview for UI development; simulated Apply and Undo do not access the filesystem or model. See [the desktop POC notes](docs/desktop-poc.md) for its command boundary and verification steps.

## Managed workspaces

Managed setup is deliberately split into read-only planning and confirmed apply. Existing directories move to `Manual Library`, existing loose files move to `Recents`, and the reviewed folder set is namespaced below `AI Library` without changing its opaque destination IDs.

```console
$ temari managed init ~/Downloads --out downloads.setup-plan.json
$ temari managed apply downloads.setup-plan.json \
    --folders /absolute/path/downloads.folders.json \
    --out ~/.local/state/temari/downloads-setup --yes
$ temari managed list
$ temari managed status <WORKSPACE_ID>
$ temari managed run <WORKSPACE_ID>
$ temari managed run <WORKSPACE_ID> --apply --yes
$ temari managed reprocess <WORKSPACE_ID> \
    --from ai-library --path Documents/old-report.pdf
$ temari managed rule add <WORKSPACE_ID> \
    --name-glob 'invoice-*.pdf' --destination d000001 --priority 100
$ temari managed disable <WORKSPACE_ID>
$ temari managed library show <WORKSPACE_ID> --out library.folders.json
$ temari managed library plan <WORKSPACE_ID> --out library-edit.plan.json \
    add --path Research --description "Research material"
$ temari managed library apply library-edit.plan.json --yes
$ temari managed library reorganize plan <WORKSPACE_ID> <CONFIGURE_RUN_ID> \
    --out library-reorganization.plan.json
$ temari managed library reorganize apply library-reorganization.plan.json --yes
$ temari managed library reorganize undo <WORKSPACE_ID> <REORGANIZE_RUN_ID> --yes
$ temari managed library undo <WORKSPACE_ID> <CONFIGURE_RUN_ID> --yes
$ temari managed library redo <WORKSPACE_ID> <CONFIGURE_RUN_ID> --yes
$ temari managed schedule install <WORKSPACE_ID> \
    --every-seconds 300 --executable ~/.local/bin/temari --yes
```

Setup Apply records the first local observation for every file it stages into `Recents`, so retention begins at setup rather than at the first later run. The first `managed run` is read-only unless `--apply --yes` is supplied. A run adopts newly created root directories into `Manual Library`, stages new root files into `Recents`, then considers only direct Recents files whose retention and stability deadlines have passed. A file manually returned from `AI Library` to the root keeps its processed identity and is not staged again; explicit `managed reprocess` remains the way to request reclassification. A directory manually returned from `Manual Library`, including by Undo, is likewise left at the root; Temari derives this intent from its authoritative setup and adoption journals, while a newly created directory with the same name but a different filesystem identity is still adopted. Classification writes a normal Plan before Apply. `managed history` lists file moves and root-directory adoptions with their Undo state. `managed undo` accepts a displayed file ID or original source-relative path for file runs; directory adoption is undone as one complete session without removing the managed areas. Every Undo remains an authoritative JSON journal.

Arrival time is the first local observation stored in SQLite, not file modification time. Editing or replacing a pending file resets its stability deadline. `Manual Library` is never recursively scanned, and `AI Library` is always excluded as an approved destination subtree.

`managed reprocess` creates a model-free reviewed Plan from explicitly selected `Manual Library` or `AI Library` files back to `Recents`; normal retention and classification then apply. AI Library supports explicit `--all`, while Manual Library always requires `--path`. Workspace registration can be enabled, disabled, edited, reconciled, and removed without deleting the three physical areas or JSON recovery artifacts.

`managed library` edits the approved AI Library structure while a workspace is disabled. `show` exports the current FolderSet; `plan add|rename|describe|delete` writes a single-operation Plan, and `plan batch --operations <JSON>` writes one ordered multi-operation Plan. Parent Rename and Delete require a reviewed `reject`, `cascade`, or `reparent` descendant policy. `apply` changes the workspace's FolderSet binding only: it does not move files or rename or delete physical directories. Each completed Configure run owns fixed Apply, Undo, and Redo artifacts, so callers never supply a journal path and `resume` dispatches interrupted recovery from the recorded run state.

After a logical Rename or Delete, `managed library reorganize` offers a separate model-free Plan/Apply/Resume/Undo workflow for files already in AI Library. Core matches files to completed classification records by filesystem identity, size, and content hash. Stable destination IDs map renamed paths to the new approved subtree; removed destinations return files directly to Recents. Untracked, changed, or manually relocated files are reported as attention items and left untouched. The Plan shows collision-resolved destinations, Apply never overwrites, old directories are not removed, and the run owns fixed Apply and Undo journals.

Temari supports only the current `Manual Library`, `Recents`, and `AI Library` layout. Experimental artifacts and state databases from earlier layouts are rejected instead of being upgraded or interpreted through compatibility paths. After an incompatible schema change, preserve the old database if recovery is still needed, restore or move the existing managed contents manually, then create a fresh database and initialize the workspace again. Temari never deletes or rewrites those physical files automatically.

The portable monitoring engine, processed signatures, local rules, and run reconciliation remain internal Core services used by `managed`. There is no separate public monitor workflow or resident Temari daemon. Explicit `managed schedule` commands render, install, inspect, or uninstall a systemd user timer on Linux or a per-user launchd agent on macOS. Scheduled definitions run the same finite `managed run --apply --yes` service with absolute paths and no shell; installation never happens during workspace setup.

Schedule installation requires an owner-only configuration and state database. It rejects `model.api_key_env` because user schedulers do not reliably inherit an interactive shell environment; use an owner-only inline `model.api_key`, or print the definition and install a separately reviewed environment configuration yourself. A Nix store executable is also rejected because garbage collection can invalidate a persistent schedule; pass `--executable` pointing to a stable launcher such as `~/.local/bin/temari`. Uninstall verifies that the timer or agent stopped before removing its owned definition. Uninstall a workspace schedule before removing its registration.

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
  managed init|apply|list|status|enable|disable|edit|remove|reconcile|run|reprocess|schedule|apply-run|resume-run|history|rule|library|undo|undo-setup|resume-setup ...
  organize <SOURCE> --out <RUN_DIR> [--include-subtree <PATH>]...  One-time cleanup
  propose <SOURCE> --out <PROPOSAL> [--include-subtree <PATH>]...
  approve <PROPOSAL> --out <FOLDER_SET>             Validate and approve it
  plan <SOURCE> --folders <FOLDER_SET> --out <PLAN> Classify files without changing them
  apply <PLAN> --out <APPLY_SESSION> [--yes]         Create directories and move files
  undo <APPLY_SESSION> --out <UNDO_SESSION> [--yes] Restore safely recorded moves
  resume <APPLY_SESSION> [--yes]                     Reconcile and continue a running apply
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

The managed workflow is the primary product surface. `organize` remains a one-time convenience, and the six primitive commands remain available for agents and recovery. See [ADR 0002](docs/adr/0002-propose-and-create-approved-folders.md) for the filesystem safety policy, [ADR 0004](docs/adr/0004-use-json-journals-before-a-state-database.md) for artifact persistence, [ADR 0005](docs/adr/0005-adopt-on-demand-content-classification-and-local-fallbacks.md) for two-pass classification, [ADR 0006](docs/adr/0006-bind-explicit-recursive-scope-to-workflow-artifacts.md) for recursive scope, [ADR 0007](docs/adr/0007-adopt-bounded-document-and-ocr-extraction.md) for extraction limits, [ADR 0008](docs/adr/0008-use-sqlite-for-monitoring-state.md) for internal monitoring state, [ADR 0009](docs/adr/0009-request-per-run-content-consent.md) for consent, [ADR 0010](docs/adr/0010-load-desktop-config-from-platform-directory.md) for desktop configuration and private credentials, [ADR 0011](docs/adr/0011-apply-backend-held-desktop-plans.md) for confirmed desktop Apply and Undo, [ADR 0012](docs/adr/0012-adopt-managed-three-area-workspaces.md) for protected, staged, and classified areas, [ADR 0013](docs/adr/0013-make-managed-the-primary-cli.md) for the public CLI boundary, [ADR 0014](docs/adr/0014-schedule-finite-managed-runs.md) for explicit OS scheduling, [ADR 0015](docs/adr/0015-reprocess-managed-files-through-recents.md) for protected and classified file reprocessing, and [ADR 0016](docs/adr/0016-share-managed-services-across-cli-and-desktop.md) for shared CLI and desktop orchestration.

The detailed backlog is maintained in [the product roadmap](docs/roadmap.md). It separates current workflow completion from optional cleanup capabilities and records areas that need more product research.

Example schemas are available for a model-created [proposal](examples/proposal.example.json) and a locally approved [folder set](examples/folders.example.json). Their source paths are illustrative and must match the canonical source used by `plan`.

## Development

```console
$ nix develop
$ cargo fmt --all -- --check
$ cargo clippy --workspace --all-targets -- -D warnings
$ cargo test --workspace
```

Entering `nix develop` also installs the pinned Emil Kowalski design and animation skills into the supported local agent skill directories through `agent-skills-nix`.

See [the CLI specification](docs/cli-spec.md), [ADR 0001](docs/adr/0001-adopt-rust-core-and-read-only-cli.md), and [ADR 0003](docs/adr/0003-redesign-cli-around-versioned-workflow-artifacts.md).

## Project policy

- Read [CONTRIBUTING.md](CONTRIBUTING.md) before opening an issue or proposing a change.
- Report vulnerabilities privately as described in [SECURITY.md](SECURITY.md).
- Review [the public repository checklist](docs/public-release-checklist.md) before changing repository visibility or distributing binaries.
- The project license is TBD. Do not make the repository public or redistribute the source until the license and contribution terms are selected.

## Implementation boundary

This repository is an independent implementation. Product behavior and safety requirements are documented and tested within this repository. It does not contain third-party source code, branding, UI assets, internal identifiers, prompt text, database schemas, or log strings.
