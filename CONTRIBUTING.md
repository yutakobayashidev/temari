# Contributing

Bug reports and focused feature proposals are welcome. Temari's license is still TBD, so external code and documentation contributions are not yet accepted. This avoids ambiguous rights for contributors and users.

Before opening an issue:

- Search existing issues and keep one problem or proposal per issue.
- Use synthetic file names and redact logs and configuration.
- Never post API keys, private file names, extracted content, model payloads, or workflow artifacts.
- Report suspected vulnerabilities through the process in [SECURITY.md](SECURITY.md), not through a public issue.

For local verification, run:

```console
$ cargo fmt --all -- --check
$ cargo clippy --workspace --all-targets -- -D warnings
$ cargo test --workspace
$ corepack pnpm --dir apps/temari-desktop install --frozen-lockfile
$ corepack pnpm --dir apps/temari-desktop build
```

Changes maintained in this repository must preserve the trust boundaries in [AGENTS.md](AGENTS.md), add behavior-focused tests where a boundary changes, and check `README.md`, `AGENTS.md`, `CLAUDE.md`, and related `docs/` files for documentation impact.
