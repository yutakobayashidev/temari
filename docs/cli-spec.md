# CLI Specification

## Decision

The implementation will evolve, but the experimental CLI contract will be redesigned before release. `temari-core` remains the shared implementation; the CLI becomes an explicit workflow over versioned artifacts.

## Command tree

```text
temari [global options] managed init <SOURCE> --out <SETUP_PLAN>
temari [global options] managed apply <SETUP_PLAN> --folders <FOLDER_SET> --out <RUN_DIR> [--yes]
temari [global options] managed list|status|enable|disable|edit|remove|reconcile ...
temari [global options] managed run|reprocess|schedule|apply-run|resume-run|history|undo ...
temari [global options] managed rule add|list|enable|disable|remove ...
temari [global options] managed undo-setup|resume-setup ...
temari [global options] organize <SOURCE> --out <RUN_DIR> [--include-subtree <PATH>]...
temari [global options] propose <SOURCE> --out <PROPOSAL> [--include-subtree <PATH>]...
temari [global options] approve <PROPOSAL> --out <FOLDER_SET>
temari [global options] plan <SOURCE> --folders <FOLDER_SET> --out <PLAN>
temari [global options] apply <PLAN> --out <APPLY_SESSION> [--yes]
temari [global options] undo <APPLY_SESSION> --out <UNDO_SESSION> [--yes]
temari [global options] resume <APPLY_SESSION> [--yes]
```

`managed` is the normal recurring workflow. `organize` is an interactive one-time cleanup. The six primitive commands are the stable advanced interface for agents, scripts, inspection, and recovery.

## Command semantics

### `propose`

- Read-only.
- Always scans regular files directly below the source. Each repeated `--include-subtree` adds one source-relative directory recursively; `.` selects the complete tree.
- Never follows symlink files or directories. Rejects overlapping recursive roots and non-portable paths.
- Sends a representative sample of at most 100 relative paths, extensions, and opaque file IDs.
- Treats `--max-folders` as a ceiling for all generated physical directories, including implicit parent path prefixes. Generated paths have at most two components.
- Prefers broad, reusable destinations and asks the model to group date, version, sequence, and half-year variants. A structurally invalid response receives at most one corrective retry.
- Produces a version 2 `Proposal` containing the canonical source path, immutable `ScanScope`, sample count, relative hierarchy suggestions, and descriptions.
- Does not create folders or assign executable destination paths.

### `approve`

- Read-only with respect to the organized source.
- Previews the proposed hierarchy and asks for confirmation when stdin and stderr are terminals. Edit the proposal JSON before approval when hierarchy changes are needed.
- Validates normalized relative paths and assigns opaque destination IDs. Generation limits do not constrain a hierarchy explicitly edited and approved by the user.
- Adds deterministic `Others/*` destinations for PDF, spreadsheet, image, video, audio, archive, code, presentation, and miscellaneous fallbacks. Automatically added fallbacks are visible during approval but local-only during classification; an identically named user proposal is reused and remains model-visible.
- Produces a version 3 `FolderSet` that preserves the approved scope and identifies model-visible and fallback destinations.
- In non-interactive mode, fails unless `--accept-all` is supplied.

### `plan`

- Read-only.
- Classifies file names in batches of 50 into model-visible IDs from the supplied `FolderSet`.
- A name result may request content. The default `ask` policy pauses interactive `organize` after the validated name pass and before extraction. Approval is per run; refusal uses local fallbacks and continues. Primitive `plan` never prompts and stops before extraction when `ask` encounters ambiguity.
- With `privacy.content = "on_demand"`, the core extracts bounded UTF-8, PDF, supported ZIP/XML document, or optional local OCR text and classifies those files in batches of 20. With `metadata_only`, no content is read or sent.
- Document extraction supports DOCX, PPTX, XLSX, ODT, ODP, and ODS with archive, expansion, XML, byte, and character limits. Optional OCR supports common raster images through a fixed, direct, time-limited local executable invocation.
- Uses deterministic approved fallback IDs when content is disabled, unsupported, oversized, empty, or cannot be extracted. Model and endpoint failures remain errors rather than silently falling back.
- Rejects a `FolderSet` created for a different canonical source path.
- Uses the scope stored in the `FolderSet`; callers cannot replace it at planning time. Approved destination subtrees are excluded from scanning.
- Produces a version 4 `Plan` containing scope, relative source paths, approved folders, local SHA-256 and filesystem identities, required directories, collision-resolved destinations, classification basis, optional local rule ID, and optional reasoning.
- Hashes are computed locally and are never sent to the model.
- Extracted text and model connectivity are never written to the Plan.

### `apply`

- Mutates the filesystem.
- Shows a summary and asks for confirmation on a TTY.
- Requires `--yes` when no TTY is available.
- Revalidates root containment, symlinks, fingerprints, destination types, permissions, and collisions immediately before each operation.
- Revalidates every existing source-parent component before nested moves. Empty source directories are never removed.
- Acquires an exclusive advisory lock on the canonical source before preflight and holds it through journal finalization.
- Requires a new persistent `--out` path outside the organized source; `--out -` and existing paths are rejected.
- Creates missing directories lazily, never overwrites, and atomically checkpoints `pending`, in-progress, and completed outcomes.
- Finalizes an immutable `ApplySession`; partial failure remains honestly recorded and returns a failure exit.

### `undo`

- Mutates the filesystem from a recorded `ApplySession` only.
- Restores moves in reverse order when current state still matches the session.
- Accepts a consistent source-filesystem device renumber after reboot, while still requiring matching source and entry inodes plus file sizes and hashes.
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
- Under `ask`, discloses the model origin, exact sanitized ambiguous paths, extraction limits, and local OCR status, then asks once. It never displays credentials, endpoint paths or queries, executable paths, or extracted text.

### `managed`

- `init` is read-only. It inventories the complete source root and writes a `ManagedSetupPlan` that creates `Kept`, `Inbox`, and `Library`, moves existing real directories to Kept, and moves existing regular files to Inbox. It rejects reserved-name collisions, special entries, non-portable names, stale source identity, and cross-filesystem directory entries.
- `apply` confirms that exact setup Plan, writes a durable setup journal before mutation, then registers the completed workspace with a Library-prefixed FolderSet. Retention and stability windows are fixed in the workspace definition.
- `run` is finite. It first writes a local Plan for new root files to Inbox, reconciles Inbox fingerprints in SQLite, and writes a classification Plan only for files past both deadlines. Without `--apply --yes`, Plans remain reviewable and no file moves. Omitted `--out` creates a private unique artifact directory below the state directory.
- `enable` and `disable` atomically update the workspace and its internal monitor. Disabled workspaces reject new runs and Apply but still permit recovery. `edit` changes retention and stability windows and recalculates only pending deadlines. `remove` requires a disabled idle workspace and deletes only registration and mutable indexes; files and JSON artifacts remain.
- `reconcile` removes stale pending Inbox rows and recognizes manually returned files without treating SQLite as filesystem proof. `status` reports health, physical and indexed Inbox counts, eligibility, and actionable runs without contacting the model.
- `reprocess` creates a model-free Plan that stages explicitly selected Kept or Library files back to Inbox. Kept always requires area-relative `--path` selectors; Library additionally accepts explicit `--all`. Normal retention, rules, model privacy, Plan, Apply, and Undo behavior then applies.
- `schedule print|install|status|uninstall` manages explicit per-user systemd timers or launchd agents. Installed definitions call a stable absolute executable and absolute config/state paths without a shell. `--executable` preserves a user-facing launcher path, while garbage-collectable Nix store paths are rejected. Installation validates before its separate confirmation and rejects environment-backed API keys. Uninstall keeps definitions when the scheduler cannot be stopped.
- `apply-run` applies a previously recorded stage or classification Plan by run ID and exact digest.
- `resume-run` conservatively reconciles a running or already-completed Apply journal, then synchronizes the Inbox index before marking the managed run completed. A completed filesystem Apply with interrupted index finalization remains resumable. Other terminal runs are immutable.
- `history` lists each recent indexed move with file ID, original path, destination, and accumulated Undo state. `undo` resolves a completed run to its ApplySession and restores either the complete session or repeated `--file <FILE_ID_OR_SOURCE_PATH>` selections. It keeps the source lock through atomic journal and Inbox reconciliation. Every individual Undo journal is indexed without replacing earlier Undo state, and selected-file Undo never removes shared directories.
- `rule` manages case-insensitive basename globs for one workspace. Rules select reviewed opaque destination IDs, run before model classification, use descending priority and stable rule-ID ordering, and never store executable paths.
- `undo-setup` and `resume-setup` operate only from their versioned setup journals. Setup Undo refuses a changed kept directory, occupied original path, or changed area identity.
- SQLite stores mutable retention and history indexes only. Setup Plans, normal Plans, Apply sessions, and Undo sessions remain authoritative JSON artifacts outside the managed source.
- Internal monitoring records, processed signatures, and reconciliation services are implementation details. They do not create a second public workflow, expose internal monitor IDs, or install a daemon.

## Global options and output

```text
--config <PATH>   Override model and privacy configuration
--state <PATH>    Override the local managed-workspace SQLite database
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

- Configuration version 4 requires `[privacy]` and `[privacy.extraction]` sections. Omitted `content` defaults to `ask`; `metadata_only` disables extraction and `on_demand` explicitly permits bounded extraction for unattended planning.
- `.temari.toml` contains model connectivity, endpoint allowlists, bounded extraction limits, optional OCR executable settings, and privacy policy only.
- OCR is disabled when `[privacy.extraction.ocr]` is absent. Its executable and optional data directory must be absolute paths; language identifiers are validated tokens.
- Approved destinations live in a `FolderSet`, not in application configuration.
- The current implementation reads `.temari.toml` or the path supplied with `--config`.
- API keys may be read from `model.api_key` in an owner-only private config or from the environment-variable name in `model.api_key_env`; the fields are mutually exclusive and secret values never appear in flags or workflow artifacts.

## Common flows

```console
$ temari propose ~/Downloads --out downloads.proposal.json
$ temari approve downloads.proposal.json --out downloads.folders.json
$ temari plan ~/Downloads --folders downloads.folders.json --out downloads.plan.json
$ temari apply downloads.plan.json --out downloads.apply.json
$ temari undo downloads.apply.json --out downloads.undo.json
$ temari resume interrupted.apply.json
$ temari organize ~/Downloads --out downloads-run
$ temari organize ~/Downloads --include-subtree Receipts --include-subtree Work --out downloads-run
$ temari managed init ~/Downloads --out downloads.setup-plan.json
$ temari managed apply downloads.setup-plan.json --folders downloads.folders.json --out downloads-setup
$ temari managed run <WORKSPACE_ID> --out downloads-cycle
$ temari managed run <WORKSPACE_ID> --apply --yes
$ temari managed reprocess <WORKSPACE_ID> --from kept --path Projects --apply --yes
$ temari managed schedule install <WORKSPACE_ID> --every-seconds 300 --executable ~/.local/bin/temari --yes
$ temari managed history <WORKSPACE_ID>
$ temari managed rule add <WORKSPACE_ID> --name-glob 'invoice-*.pdf' --destination d000001
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
6. Reuse the same services internally for rules, processed-file tracking, reconciliation, and run history. Completed.
7. Add automatic two-pass name/content classification and approved deterministic extension fallbacks. Completed.
8. Add bounded cross-platform document extraction and optional local OCR. Completed.
9. Add ambiguity-aware per-run content consent without making primitive commands interactive. Completed.
10. Make managed workspaces the only public recurring workflow. Completed.
11. Add a GUI adapter over the same application services. In progress as a proof of concept.
12. Add explicit finite-run scheduling, workspace lifecycle management, detailed move history, and reprocessing through Inbox. Completed.
