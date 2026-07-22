# Lessons

- Keep product requirements self-contained, independently written, and testable within this repository.
- Distinguish AI folder-structure proposal from classification into approved folders and from the later filesystem creation step. Verify all three flows before narrowing the MVP roadmap.
- Keep repository language policies explicit: product implementation documentation is written in English.
- When an early vertical-slice CLI models only the middle of a discovered user journey, preserve the tested core but redesign the unreleased command and artifact boundaries before adding mutation features.
- When asked for a coined product name, avoid transparent English compounds; prefer short, pronounceable forms whose meaning can grow with the brand.
- For an AI product name, a completely opaque coinage may feel generic; anchor the sound and imagery in the product's desired category signals, such as luminosity, transformation, or infrastructure workflows, without copying an existing brand.
- When the user prefers a cute Japanese product name, drop forced AI and futuristic signals; favor familiar Japanese sounds or gentle derivations with a concrete organizing metaphor.
- When presenting Japanese name candidates, vary consonants and sound textures; repeated hard `k` sounds across the shortlist can make otherwise cute names feel monotonous.
- A literal diminutive such as "small box" can have the right product metaphor but still feel cheap or childish; preserve the image while moving to a more evocative Japanese word with room for interpretation.
- When the desired brand personality is cute and Japanese, prefer short native words, mimetic expressions, or Japanese-derived coinages over cold, globally generic AI-style names.
- When reverse-engineering a trust boundary, distinguish limiting the model's prompt candidates from enforcing the returned value locally. Verify the lookup operands and the final call path before claiming an allowlist check exists.
- Treat an AI folder limit as a budget for every physical directory, including implicit parents. Keep generated hierarchies shallow, ask for broad reusable categories, and preserve deeper structures only when a person explicitly edits and approves them.
- On NixOS, verify that GTK 3 GSettings schemas are present in `XDG_DATA_DIRS` before diagnosing a Tauri file chooser as an application hang; a missing `org.gtk.Settings.FileChooser` schema terminates the native process when the dialog opens.
- Never present a relative configuration path as a desktop default: automatically load from the platform application-config directory, offer a native picker for session overrides, and validate the absolute path at the backend boundary.
- For a private local application, do not force environment-only secrets after the user explicitly accepts an owner-only plaintext config; support one clear credential source at a time and keep it out of artifacts and logs.
- When the product's current experience has shifted from a one-shot wizard to protected, waiting, and classified areas, prioritize that filesystem state machine in Core and CLI before polishing GUI adapters. Keep directory initialization separate from regular-file Plans, and derive retention from first local observation rather than mtime.
- Once a stateful product workflow becomes the primary experience, remove overlapping implementation-oriented public commands instead of asking users to choose between equivalent mechanisms. Keep reusable engines internal and move rules, history, and recovery under the product-facing workspace identity.
- When the user narrows a parity phase, implement the named workflow gaps completely and keep explicitly deferred product areas out of the patch, documentation, and navigation.
- After completing a roadmap item, re-audit both the implementation and current analysis evidence before reporting parity; update stale scope labels and distinguish organizer gaps from separate product epics.
- When the user narrows an E2E request to one adapter, stop setting up the excluded adapter immediately and spend the remaining validation budget on the requested path.
# Library structure editing

- Treat post-setup AI Library edits as logical FolderSet CRUD only. Add, rename, description edit, delete, and Undo change immutable FolderSet revisions and bindings; they do not rename or delete physical directories or move existing files.
- Keep existing-file movement in the explicit reprocessing/reorganization workflow. Do not infer filesystem movement from a logical destination rename.
- Keep physical area names consistent across Core, CLI, desktop, help, and documentation: support only `Manual Library`, `Recents`, and `AI Library`. In an unreleased private project, remove obsolete layouts and reject their artifacts instead of carrying migration or compatibility paths.

# Execution discipline

- When the user authorizes a concrete implementation, do not repeatedly stop at plans or partial status reports. Continue through code, verification, documentation, and commit unless a genuine external blocker requires user input.
- Browser demo state must preserve the selected workspace identity across preview and Apply, and should model the resulting recovery metadata; otherwise UI E2E can pass against a different workspace or cannot exercise Undo.
