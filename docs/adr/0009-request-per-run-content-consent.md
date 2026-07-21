# ADR 0009: Request Per-Run Content Consent

## Status

- Accepted

## Decision Drivers

- Name-only classification must remain useful without granting access to file contents.
- Consent must occur after ambiguity is known but before local extraction or a content request.
- Interactive and automated commands need predictable, distinct behavior.
- Consent and extracted text must not become durable workflow state.

## Context

The classifier first sends file metadata. It reads and sends bounded extracted text only when the name result requests content and the configured policy permits it. A single classification function cannot ask for informed consent at the correct boundary because it performs the name pass and content pass together.

The primitive `plan` command is an automation interface and must never prompt. The interactive `organize` command may prompt, but only when at least one validated name result needs content.

## Options Considered

- Always require metadata-only classification: safest, but discards useful content classification.
- Treat `on_demand` as the default: convenient, but grants content access without per-run confirmation.
- Retry classification after a consent-required error: repeats model calls and may produce a different ambiguous set.
- Split classification into a name pass and a completion pass: preserves one validated ambiguous set and creates an enforceable consent boundary.

## Decision

- Add `ask` as the default content policy. Keep `metadata_only` and `on_demand` as explicit unattended policies.
- Split classification into an ephemeral validated name pass and a completion pass. The name pass returns resolved classifications and the files that need content. The completion pass receives an explicit decision to extract content or use local fallbacks.
- Under `ask`, `organize` prompts once only when the name pass contains ambiguous files. Approval applies to the current run only. Declining or submitting an empty answer uses approved local fallbacks and continues planning.
- Under `ask`, primitive `plan` never prompts. If ambiguity exists, it stops before extraction with guidance to select `metadata_only`, select `on_demand`, or use interactive `organize`. If no ambiguity exists, planning completes normally.
- `on_demand` performs bounded extraction without another prompt because configuration is explicit consent. `metadata_only` never invokes an extractor.
- The disclosure shows the sanitized model origin, ambiguous file count and relative paths, input-byte and output-character limits, and whether local OCR may run. It states that raw files are not uploaded, extraction failures use local fallbacks, and extracted text is not logged or persisted.
- Display only the model origin. Never display credentials, secret values, URL paths or queries, OCR executable paths, extracted text, or child-process output.
- Do not serialize consent, the ephemeral name pass, extraction outcomes, or extracted text into Proposal, FolderSet, Plan, apply, undo, monitoring, or history state.

## Consequences

- Positive: the user sees the exact ambiguous set before content access.
- Positive: automation remains non-interactive and fails before unintended content extraction.
- Positive: GUI and CLI adapters can reuse the same core consent boundary.
- Negative: classification becomes a two-stage application service.
- Negative: unattended use with the default policy requires an explicit policy choice when ambiguity occurs.

## Adoption and Exceptions

- Configuration validation must recognize all three policies and default omitted content policy to `ask`.
- Core tests must prove that the name pass performs no extraction, fallback completion never invokes an extractor, and extraction completion touches only the ambiguous set.
- CLI tests must cover zero-ambiguity `ask`, interactive approval and refusal, primitive-plan refusal, safe disclosure formatting, and absence of content requests before approval.
- Artifact tests must prove that consent state, extracted text, OCR output, and endpoint details are absent from persisted artifacts.
- Any new command that can trigger content extraction must resolve the policy through this same boundary. Exceptions require a new ADR with equivalent disclosure, persistence, and non-interactive safeguards.
