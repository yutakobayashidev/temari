# CLI Specification

## Decision

The implementation will evolve, but the experimental CLI contract will be redesigned before release. `temari-core` remains the shared implementation; the CLI becomes an explicit workflow over versioned artifacts.

## Command tree

```text
temari [global options] propose <SOURCE> --out <PROPOSAL>
temari [global options] approve <PROPOSAL> --out <FOLDER_SET>
temari [global options] plan <SOURCE> --folders <FOLDER_SET> --out <PLAN>
temari [global options] apply <PLAN> --out <APPLY_SESSION> [--yes]
temari [global options] undo <APPLY_SESSION> --out <UNDO_SESSION> [--yes]
temari [global options] resume <APPLY_SESSION> [--yes]
temari [global options] organize <SOURCE> --out <RUN_DIR>
```

`organize` is an interactive convenience command. The five primitive commands remain the stable interface for agents and scripts.

## Command semantics

### `propose`

- Read-only.
- Scans regular files directly below the selected source and sends a representative sample of at most 100 file names, extensions, and opaque file IDs.
- Produces a versioned `Proposal` containing the canonical source path, sample count, relative hierarchy suggestions, and descriptions.
- Does not create folders or assign executable destination paths.

### `approve`

- Read-only with respect to the organized source.
- Previews the proposed hierarchy and asks for confirmation when stdin and stderr are terminals. Edit the proposal JSON before approval when hierarchy changes are needed.
- Validates normalized relative paths and assigns opaque destination IDs.
- Adds deterministic `Others/*` destinations for PDF, spreadsheet, image, video, audio, archive, code, presentation, and miscellaneous fallbacks. Automatically added fallbacks are visible during approval but local-only during classification; an identically named user proposal is reused and remains model-visible.
- Produces a version 2 `FolderSet` that identifies model-visible and fallback destinations.
- In non-interactive mode, fails unless `--accept-all` is supplied.

### `plan`

- Read-only.
- Classifies file names in batches of 50 into model-visible IDs from the supplied `FolderSet`.
- A name result may request content. With `privacy.content = "on_demand"`, the core extracts bounded UTF-8 or PDF text locally and classifies those files in batches of 20. With `metadata_only`, no content is read or sent.
- Uses deterministic approved fallback IDs when content is disabled, unsupported, oversized, empty, or cannot be extracted. Model and endpoint failures remain errors rather than silently falling back.
- Rejects a `FolderSet` created for a different canonical source path.
- Produces a version 2 `Plan` containing approved folders, local SHA-256 and filesystem identities, required directories, collision-resolved destinations, classification basis, and optional reasoning.
- Hashes are computed locally and are never sent to the model.
- Extracted text and model connectivity are never written to the Plan.

### `apply`

- Mutates the filesystem.
- Shows a summary and asks for confirmation on a TTY.
- Requires `--yes` when no TTY is available.
- Revalidates root containment, symlinks, fingerprints, destination types, permissions, and collisions immediately before each operation.
- Requires a new persistent `--out` path outside the organized source; `--out -` and existing paths are rejected.
- Creates missing directories lazily, never overwrites, and atomically checkpoints `pending`, in-progress, and completed outcomes.
- Finalizes an immutable `ApplySession`; partial failure remains honestly recorded and returns a failure exit.

### `undo`

- Mutates the filesystem from a recorded `ApplySession` only.
- Restores moves in reverse order when current state still matches the session.
- Removes only directories created by that session and only when they are empty.
- Records success, skips, and failures instead of pretending rollback is atomic.
- Writes a separate `UndoSession`; it never modifies the original apply journal.

### `resume`

- Accepts only an unfinished `running` ApplySession and updates that active journal in place.
- Reconciles each in-progress operation from source and destination fingerprints before any new mutation.
- Converts a clearly unperformed move back to pending and a clearly completed move to moved.
- Finalizes ambiguous filesystem state as a recorded conflict without retrying or overwriting.
- Requires `--yes` outside a TTY. Finalized completed, failed, and partial-failure sessions remain immutable.

### `organize`

- TTY-only convenience orchestration over the same propose, approve, plan, and apply services.
- Requires a new `--out <RUN_DIR>` outside the organized source and preserves every completed artifact on cancellation or failure.
- Writes `proposal.json`, editable `proposal-review.json`, `folders.json`, `plan.json`, and `apply-session.json` as stages complete.
- Offers approve, edit through `$VISUAL` or `$EDITOR`, or quit. Editing may change only folders and descriptions, not the source or sampling context.
- Shows exact mkdir and collision-resolved move operations before a separate apply confirmation.
- Keeps name, content, and fallback processing inside Stage 3 and reports their counts without adding another command boundary.

## Global options and output

```text
--config <PATH>   Override model and privacy configuration
--json            Emit a machine-readable command result
--no-input        Never prompt; fail when explicit input is missing
--no-color        Disable color
-v, --verbose     Add diagnostics to stderr
-h, --help        Show help
--version         Show version
```

- stdout contains the primary result or artifact when a read-only command uses `--out -`.
- stderr contains progress, prompts, warnings, and errors.
- `--out <PATH>` writes artifacts atomically and prints the resulting path to stdout.
- `--quiet` is not supported because it could suppress the only result.
- Exit code `0` means success, `1` means a runtime or configuration failure, and `2` means invalid command-line arguments. Additional stable codes will be added only when apply automation needs them.

## Configuration

- Configuration version 2 requires a `[privacy]` section. `content = "metadata_only"` disables content extraction; `content = "on_demand"` enables it only for ambiguous files.
- `.temari.toml` contains model connectivity, endpoint allowlists, extraction limits, and privacy policy only.
- Approved destinations live in a `FolderSet`, not in application configuration.
- The current implementation reads `.temari.toml` or the path supplied with `--config`.
- API keys are loaded only from the environment-variable name configured by `model.api_key_env`; secret values never appear in flags or artifacts.

## Common flows

```console
$ temari propose ~/Downloads --out downloads.proposal.json
$ temari approve downloads.proposal.json --out downloads.folders.json
$ temari plan ~/Downloads --folders downloads.folders.json --out downloads.plan.json
$ temari apply downloads.plan.json --out downloads.apply.json
$ temari undo downloads.apply.json --out downloads.undo.json
$ temari resume interrupted.apply.json
$ temari organize ~/Downloads --out downloads-run
```

For an agent-driven approval after the proposal has already been reviewed:

```console
$ temari approve downloads.proposal.json --accept-all --no-input --out downloads.folders.json
```

## Implementation order

1. Split model configuration from `FolderSet` and introduce artifact schemas.
2. Implement read-only `propose` and `approve`.
3. Migrate `plan` to consume a `FolderSet` and emit a durable plan.
4. Implement apply with audit sessions, then undo. Completed.
5. Add interactive `organize` orchestration and explicit crash resume. Completed.
6. Reuse the same services from the GUI and add a state database only when monitoring requires it.
7. Add automatic two-pass name/content classification and approved deterministic extension fallbacks. Completed.
