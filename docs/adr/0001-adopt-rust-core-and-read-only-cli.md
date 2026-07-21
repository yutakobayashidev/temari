# ADR 0001: Adopt a Rust core and a read-only CLI first

## Status

- Accepted

## Decision Drivers

- Linux and macOS must share classification and filesystem safety behavior.
- A future GUI must reuse the same trust-boundary validation as the CLI.
- Private file names or contents must not reach an unapproved endpoint.
- The first usable slice must be testable without risking file movement.
- Distribution should eventually support a small number of self-contained artifacts.

## Context

- The repository starts without an existing implementation or compatibility obligations.
- The product is for private use and is not planned for publication.
- Local models and models on an explicitly trusted internal network are required.
- Product requirements must be expressed as independent, testable behavior and safety constraints.
- Assumption: OpenAI Chat Completions compatibility covers the initial Ollama, llama.cpp, and vLLM deployments.

## Options Considered

- Rust core plus CLI: one cross-platform core, strong typed boundaries, and a path to a later Tauri or native GUI.
- TypeScript CLI: fastest initial scripting, but adds a runtime/distribution decision and does not remove the need for a native GUI core.
- Separate native macOS and Linux applications: best platform integration, but duplicates the most security-sensitive logic.

## Decision

- Build reusable behavior in `temari-core` and expose it first through `temari-cli`.
- The initial command is read-only: it scans one directory level, sends only file name and extension, validates the complete model response, and emits a plan.
- Represent files and approved destinations with opaque IDs. The model never returns a filesystem path.
- Require the configured model hostname to appear in an explicit allowlist. API credentials are read from an environment variable named by the configuration, never stored in the configuration itself.
- Keep the initial provider to the OpenAI-compatible Chat Completions protocol.
- Do not implement content extraction, move/apply, watch mode, persistence, or GUI in the first slice.

## Consequences

- Positive: CLI and future GUI share the same validation and provider boundary.
- Positive: early use cannot modify the filesystem, and model hallucinations fail closed.
- Positive: internal endpoints work without adding provider-specific code.
- Negative: Rust and HTTP dependencies make the initial scaffold larger than a shell or Python prototype.
- Negative: models without an OpenAI-compatible endpoint require a future adapter.
- Follow-up: add a signed or hashed plan artifact, explicit apply confirmation, durable audit history, collision handling, and undo before enabling file movement.

## Adoption and Exceptions

- Tests must cover unknown file IDs, unknown destination IDs, duplicate classifications, unsafe destination paths, and disallowed endpoint hosts.
- Code review must reject any flow that sends content without an explicit content policy or executes a model-produced path.
- A change to provider protocol, filesystem mutation policy, or supported platforms requires a new ADR.
- As the sole private operator, the repository owner approves exceptions and records them in an ADR with tests demonstrating the revised trust boundary.
