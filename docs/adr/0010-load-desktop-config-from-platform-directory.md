# ADR 0010: Load Desktop Config from the Platform Directory

## Status

- Accepted

## Decision Drivers

- A desktop process has no stable user-facing working directory.
- Linux and macOS need predictable native configuration locations.
- This private application may use an owner-only plaintext API key without requiring shell environment setup.
- Credentials must stay out of workflow artifacts.

## Context

The CLI originally used a relative `.temari.toml` and environment-only API keys. Tauri development runs from its Rust project directory, while a packaged application may start elsewhere, so the same relative path resolves inconsistently. Requiring an environment variable also makes launching the private desktop application unnecessarily dependent on the parent shell.

This decision supersedes only the environment-only credential statement in ADR 0001.

## Options Considered

- Keep a relative desktop config: simple in one shell, but dependent on an unstable working directory.
- Require explicit file selection every session: predictable, but repetitive and awkward for a primary local configuration.
- Load the platform config automatically and allow a session override: predictable by default while preserving explicit control.

## Decision

- Desktop loads `config.toml` from `ProjectDirs::config_dir` by default: `$XDG_CONFIG_HOME/temari` on Linux and Application Support on macOS.
- A native file picker may select a different absolute regular file for the current session.
- Desktop never searches the selected source directory or process working directory for configuration.
- `model.api_key` may contain a plaintext key in an owner-only private config. `model.api_key_env` remains supported, and configuration validation rejects setting both.
- Inline API keys are deserializable for connection setup but omitted when configuration is serialized. Workflow artifacts never contain model configuration or credentials.

## Consequences

- Positive: desktop launch works independently of shell working directory and environment setup.
- Positive: Linux and macOS follow their native per-user configuration conventions.
- Positive: users can still keep credentials in an environment variable when preferred.
- Negative: a plaintext key is exposed to any process or user that can read the config file.
- Negative: CLI and desktop may use different default config locations until CLI discovery is unified separately.

## Adoption and Exceptions

- Document the standard desktop path and keep private configs owner-readable only.
- Tests must cover absolute config validation, platform-path discovery, mutually exclusive credential sources, bearer authentication, and omission of inline keys during serialization.
- Reviews must reject credentials in examples, repository files, logs, and workflow artifacts.
- Any additional credential store or automatic config search path requires a new ADR or an explicit amendment to this one.
