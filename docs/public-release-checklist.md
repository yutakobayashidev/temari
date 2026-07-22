# Public Repository Checklist

This repository is being prepared for public visibility, but its license is still TBD. Complete the blocking decisions and GitHub settings below before changing repository visibility.

## Before publication

- [ ] Select Temari's license with legal advice as needed. Add `LICENSE` and update every Cargo manifest, `README.md`, and the contribution policy together. This blocks changing repository visibility.
- [ ] Confirm that the commit-history address `hi@yutakobayashi.com` is intended to be public. If not, rewrite all commits and change local Git author settings before the first push.
- [ ] Confirm that `dev.yutakobayashi.temari` is the intended permanent application, configuration, and scheduler namespace.
- [ ] Confirm the provenance and redistribution rights for every file under `apps/temari-desktop/src-tauri/icons/`.
- [ ] Run a dedicated full-history secret scanner such as gitleaks. Rotate and remove any discovered credential before publication; deleting it only from the latest commit is insufficient.
- [ ] Review the complete tracked file list with `git ls-files`, including `z-ai/lessons.md`, agent instructions, ADRs, examples, and lockfiles.
- [ ] Run the commands in the verification section below from a clean checkout.

## GitHub settings

- [ ] Enable private vulnerability reporting and verify that the Security tab flow matches `SECURITY.md`.
- [ ] Enable secret scanning and push protection where the repository plan supports them.
- [ ] Add a ruleset for `main`: require pull requests, the CI checks, resolved review conversations, and no force pushes or deletions.
- [ ] Keep default workflow permissions read-only and do not allow untrusted pull-request workflows to write repository contents or secrets.
- [ ] Set the repository description, topics, website, social preview, and issue/discussion settings intentionally.
- [ ] Add the Git remote and verify the exact owner and repository name before the first push.

## Binary distribution

Public source visibility does not by itself make the project release-ready. Before publishing CLI or desktop binaries:

- [ ] Generate and review third-party dependency notices for Rust, npm, Tauri, and linked system libraries; bundle all required notices.
- [ ] Add reproducible Linux and macOS release jobs, checksums, an SBOM, and signed provenance.
- [ ] Decide macOS signing and notarization and Linux package formats.
- [ ] Document supported versions, upgrade behavior, release notes, and artifact verification.
- [x] Resolve the Tauri path to vulnerable `quick-xml`: `plist` is locked to 1.10.0 and shares `quick-xml` 0.41.0, removing RUSTSEC-2026-0194 and RUSTSEC-2026-0195 from the desktop dependency graph.
- [ ] Reassess RustSec, npm, and transitive Tauri advisories at release time and document any other time-limited, reachability-based exceptions.

## Verification

```console
$ cargo fmt --all -- --check
$ cargo clippy --workspace --all-targets -- -D warnings
$ cargo test --workspace
$ corepack pnpm --dir apps/temari-desktop install --frozen-lockfile
$ corepack pnpm --dir apps/temari-desktop build
$ cargo check --manifest-path apps/temari-desktop/src-tauri/Cargo.toml
$ cargo test --manifest-path apps/temari-desktop/src-tauri/Cargo.toml --lib
$ nix flake check --all-systems --no-build
```
