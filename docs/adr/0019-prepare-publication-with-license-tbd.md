# ADR 0019: Prepare public publication while the license is TBD

## Status

- Accepted

## Context

ADR 0001 assumed that the repository would remain private. The project is now being prepared for public visibility, but its license and external contribution terms have not been selected.

## Decision

- Prepare the independently implemented source and its safety documentation for publication.
- Keep the repository private until the copyright holder explicitly selects a license and the repository contains the corresponding `LICENSE` file.
- Prevent accidental crates.io and npm publication while licensing, distribution metadata, and third-party notices remain incomplete.
- Accept bug reports and feature proposals after publication, but do not accept external code or documentation contributions until their licensing terms are defined.
- Treat secret scanning, contributor privacy, application identifiers, asset provenance, repository security settings, and binary notices as explicit public-release checks.

## Consequences

- The absence of a selected license is a release blocker, not an implicit proprietary-license decision.
- The repository must not describe itself as open source before an open-source license is selected.
- Selecting a license requires one coordinated update to the license file, package metadata, README, and contribution policy.
- ADR 0001's private-publication assumption is superseded; its technical architecture decisions remain in force.
