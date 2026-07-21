# Temari

`Temari` is a private, personal-use tool that plans and applies file organization with a local model or a model hosted on an explicitly trusted internal network.

Proposal, approval, and planning are read-only. Filesystem changes require a separate confirmed `apply` command and produce a durable JSON journal that can be inspected or passed to `undo`. The application does not send file contents, watch directories, use a state database, or send telemetry.

## Trust boundaries

- During classification, the model selects only user-approved opaque destination IDs. Model-proposed folder names remain untrusted data until local approval validates them and assigns those IDs.
- The entire response is rejected if it contains an unknown file ID, unknown destination ID, duplicate result, or missing result.
- Symlinks, directories, and paths outside the source root are excluded from classification.
- Plans contain local SHA-256, size, device, and inode fingerprints. These values and file contents are never sent to the model.
- Apply revalidates the source root, fingerprints, real directory components, and unoccupied destinations before mutation. It never overwrites.
- Undo restores only recorded moves whose identity and content still match, and removes only session-created directories that remain empty.
- Every model endpoint hostname must appear in an explicit allowlist.
- API keys are read from a configured environment variable and are never stored in the configuration file.
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

If an apply journal remains `running` after a crash, reconcile and continue it explicitly:

```console
$ cargo run -p temari-cli -- resume downloads.apply.json
```

The approval command previews every destination and asks for confirmation when stdin and stderr are terminals. For a proposal that has already been reviewed by an agent or script, make acceptance explicit:

```console
$ cargo run -p temari-cli -- approve downloads.proposal.json \
    --accept-all --no-input --out downloads.folders.json
```

To use an internal model, change `model.base_url` and add its hostname to `model.allowed_hosts`. The allowlist is enforced immediately before the request; it is not documentation-only configuration.

## CLI

```text
temari [--config PATH] [--json] [--no-input] [--no-color] [-v] <COMMAND>

Commands:
  propose <SOURCE> --out <PROPOSAL>                 Propose a folder hierarchy
  approve <PROPOSAL> --out <FOLDER_SET>             Validate and approve it
  plan <SOURCE> --folders <FOLDER_SET> --out <PLAN> Classify files without changing them
  apply <PLAN> --out <APPLY_SESSION> [--yes]         Create directories and move files
  undo <APPLY_SESSION> --out <UNDO_SESSION> [--yes] Restore safely recorded moves
  resume <APPLY_SESSION> [--yes]                     Reconcile and continue a running apply
  organize <SOURCE> --out <RUN_DIR>                  Run the guided TTY workflow
```

- Artifact paths go to stdout. Read-only commands accept `--out -` to emit artifact JSON. Apply and undo require persistent journal paths outside the organized source.
- Diagnostics, progress, prompts, and errors go to stderr.
- With a file output, `--json` emits a machine-readable result containing the output path.
- `--no-input` prevents prompts. Approval additionally requires `--accept-all`; apply, undo, and resume require `--yes`.
- `resume` updates only an unfinished `running` journal; terminal sessions remain immutable. `organize` rejects non-interactive execution.
- Exit code `0` means success, `1` means a runtime failure, and `2` means invalid arguments.

## Product flow

The workflow follows five explicit trust boundaries:

1. `propose`: the model suggests a folder hierarchy from file-name metadata.
2. `approve`: the user edits and approves the proposal; local code assigns opaque destination IDs and validates every relative path.
3. `plan`: the model classifies files only into those approved IDs and writes a validated, read-only plan.
4. `apply`: after confirmation, local code creates only required approved directories and performs validated moves while atomically updating an audit journal.
5. `undo`: local code conservatively reverses recorded moves and removes only unchanged, empty directories created by that apply session.

All five primitive stages, explicit crash resume, and the interactive `organize` orchestrator are implemented. Background monitoring remains future work. See [ADR 0002](docs/adr/0002-propose-and-create-approved-folders.md) for the filesystem safety policy and [ADR 0004](docs/adr/0004-use-json-journals-before-a-state-database.md) for the persistence decision.

Example schemas are available for a model-created [proposal](examples/proposal.example.json) and a locally approved [folder set](examples/folders.example.json). Their source paths are illustrative and must match the canonical source used by `plan`.

## Development

```console
$ cargo fmt --all -- --check
$ cargo clippy --workspace --all-targets -- -D warnings
$ cargo test --workspace
```

See [the CLI specification](docs/cli-spec.md), [ADR 0001](docs/adr/0001-adopt-rust-core-and-read-only-cli.md), and [ADR 0003](docs/adr/0003-redesign-cli-around-versioned-workflow-artifacts.md).

## Implementation boundary

This repository is an independent implementation. Product behavior and safety requirements are documented and tested within this repository. It does not contain third-party source code, branding, UI assets, internal identifiers, prompt text, database schemas, or log strings. This repository is intended for private use and is not planned for publication.
