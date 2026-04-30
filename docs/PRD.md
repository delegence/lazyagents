# lazyagents Profile Management PRD

## Problem Statement

Developers use multiple coding agent harnesses such as Codex, Claude Code, and opencode. Each harness has its own global configuration, instruction files, skills, commands, MCP settings, model choices, and permission settings. Switching between work contexts currently requires manually editing or copying files across those harness-specific configuration directories.

From the user's perspective, a configured coding harness is effectively an agent: instructions plus skills plus MCPs plus saved prompts plus model and permission configuration. The user wants to save these agent states as reusable profiles, then quickly use a chosen profile across one harness or all installed supported harnesses without manually copying files or losing changes.

The user also wants profile switching to be safe. Applying a profile mutates global harness configuration, so lazyagents must back up the current managed harness state, roll back automatically if an apply fails, detect unsaved drift before switching away from an active profile, and keep profile artifacts centralized so edits are preserved.

## Solution

Build a Rust terminal CLI for managing global lazyagents profiles stored under the lazyagents home directory. A profile is a named bundle of instructions, skills, commands, MCP definitions, and harness-specific model and permission preferences.

Profiles live globally under the lazyagents home, defaulting to the user's home directory. Project-local profiles are intentionally unsupported. A profile can be used with one explicitly selected harness or all detected supported harnesses. Harness support is implemented through harness integration modules, allowing Codex, Claude Code, and opencode to translate the same profile into their native config locations and formats.

When a profile is used, lazyagents creates or normalizes the profile skeleton, checks for drift from the currently active profile, optionally saves or discards those changes according to CLI flags or an interactive prompt, creates a latest working backup for the target harness, clears harness-integration-owned managed surfaces, symlinks profile-owned filesystem artifacts, patches native MCP and config sections, and updates state only after success. Backups are real copies of managed file and directory contents, with symlinks dereferenced, so rollback does not depend on profiles that may later be manually removed. If any apply step fails, lazyagents automatically restores the harness from the backup and reports the failure.

The v1 CLI focuses on profile lifecycle, use, and health inspection:

- `lazyagents doctor`
- `lazyagents show <name>`
- `lazyagents create <name> [--from codex|claude|opencode]`
- `lazyagents edit <name>`
- `lazyagents delete <name> [--yes]`
- `lazyagents use <profile> --harness codex|claude|opencode`
- `lazyagents use <profile> --all`

The TUI is explicitly out of scope for this PRD.

## User Stories

1. As a developer, I want to create a named profile, so that I can save a reusable coding agent configuration.
2. As a developer, I want profiles to live in one global lazyagents home, so that profiles are available from any repository.
3. As a developer, I want profile names to be simple CLI-safe identifiers, so that I can use them reliably in commands and scripts.
4. As a developer, I want profile creation to generate the standard profile skeleton, so that I immediately know where to put instructions, skills, commands, MCPs, and config.
5. As a developer, I want each profile to contain an `AGENTS.md`, so that I have one universal instruction source.
6. As a developer, I want missing optional profile artifacts to be created during use, so that old or partially created profiles can still be normalized safely.
7. As a developer, I want `config.json` to be required, so that each profile has an explicit configuration contract.
8. As a developer, I want `mcps.json` to support empty content as no MCPs, so that I can quickly define profiles without MCP servers.
9. As a developer, I want invalid non-empty MCP files to fail, so that syntax mistakes do not silently remove my MCP setup.
10. As a developer, I want profile metadata like name and description, so that profile summaries are easier to understand.
11. As a developer, I want to store model values per harness without lazyagents validating them, so that new harness model releases do not require lazyagents changes.
12. As a developer, I want to store permission values per harness without lazyagents validating them, so that each harness can keep its own evolving permission vocabulary and shape.
13. As a developer, I want `"default"` model and permission values to leave that harness setting untouched, so that profiles do not have to override everything.
14. As a developer, I want missing harness-specific model and permission keys to behave like `"default"`, so that I can omit harnesses I do not use.
15. As a developer, I want unknown config keys to be ignored, so that profile files can evolve without breaking older lazyagents versions.
16. As a developer, I want to inspect profiles and harnesses together, so that I can see system health in one command.
17. As a developer, I want doctor output to show which harnesses last used each profile, so that I can understand current usage at a glance.
18. As a developer, I want invalid profile directories to be marked in doctor output, so that broken profiles are discoverable.
19. As a developer, I want invalidly named profile directories ignored, so that unrelated files under the profile root do not clutter the CLI.
20. As a developer, I want to show a profile summary, so that I can inspect skills, commands, MCPs, models, and permissions without opening files manually.
21. As a developer, I want ignored skills and command files shown in profile summaries, so that I can diagnose why something will not be applied.
22. As a developer, I want show to be read-only, so that inspection does not mutate my profile.
23. As a developer, I want to edit a profile directory with my editor, so that I can modify all profile artifacts in one place.
24. As a developer, I want `edit` to work even for invalid profiles, so that I can fix broken profile files.
25. As a developer, I want `edit` to simply open the directory, so that it does not unexpectedly rewrite anything.
26. As a developer, I want to delete an unused profile, so that I can remove obsolete configurations.
27. As a developer, I want delete to be blocked while a profile is active, so that active symlinks are not broken.
28. As a developer, I want delete to scan all supported harness config paths, so that stale state does not allow deleting a still-linked profile.
29. As a developer, I want delete confirmation, so that I do not accidentally remove a profile.
30. As a scripting user, I want `delete --yes`, so that I can remove inactive profiles non-interactively.
31. As a developer, I want profile deletion to leave backups alone, so that harness safety state is not tied to profile lifecycle.
32. As a developer, I want to create a profile from an existing harness, so that I can bootstrap lazyagents from my current setup.
33. As a developer, I want `create --from <harness>` to copy from the harness without mutating it, so that import is safe.
34. As a developer, I want imported symlinks dereferenced, so that the resulting profile is self-contained.
35. As a developer, I want imported skills limited to valid skill folders, so that junk files are not promoted into profiles.
36. As a developer, I want imported commands to preserve nested Markdown command paths, so that command organization survives import.
37. As a developer, I want missing imported surfaces to become empty/default profile artifacts, so that every created profile has the standard skeleton.
38. As a developer, I want malformed native harness config to fail import, so that lazyagents does not build a profile from an unsafe partial parse.
39. As a developer, I want `create --from` to require the harness binary to be detected, so that imports target real installed harnesses.
40. As a developer, I want to import from an active harness, so that I can branch or clone the current real state into a new profile.
41. As a developer, I want to use a profile with one harness, so that I can switch only Codex, Claude Code, or opencode.
42. As a developer, I want `use` to require `--harness` or `--all`, so that global config mutation is always explicit.
43. As a developer, I want explicit harness use to fail if the harness is not detected, so that lazyagents does not write configs for unavailable tools.
44. As a developer, I want `--all` to apply only to detected supported harnesses, so that unavailable harnesses are silently ignored.
45. As a developer, I want `--all` with no detected supported harnesses to fail, so that I know nothing was applied.
46. As a developer, I want `--all` to continue after one harness fails, so that other detected harnesses can still be updated.
47. As a developer, I want a final `--all` summary, so that I know which attempted harnesses succeeded and which failed.
48. As a developer, I want each harness apply to be transactional, so that a failed apply does not leave partial managed state.
49. As a developer, I want automatic rollback on failed apply, so that the last working harness state is restored without a manual command.
50. As a developer, I want no public rollback command in v1, so that backup behavior remains an internal safety mechanism.
51. As a developer, I want the latest backup stored per harness, so that lazyagents avoids accumulating old harness versions.
52. As a developer, I want backups to include only managed surfaces, so that caches, logs, auth, and unrelated harness data are not copied.
53. As a developer, I want backups to store copied file contents rather than symlinks, so that rollback does not depend on a profile directory still existing.
54. As a developer, I want backups to dereference symlinks, so that a previously active profile can still be restored after manual profile removal.
55. As a developer, I want backups replaced atomically, so that backup state is not corrupted by interruptions.
56. As a developer, I want rollback to restore deleted paths and previously missing paths correctly, so that apply failure recovery is accurate.
57. As a developer, I want state updated only after successful apply, so that doctor does not claim a failed profile switch.
58. As a developer, I want `--all` to update state per successful harness, so that partial success is represented accurately.
59. As a developer, I want state to store profile names rather than absolute profile paths, so that the current lazyagents home remains the source of truth.
60. As a developer, I want doctor to show detected harnesses, so that I can see which supported tools are available.
61. As a developer, I want doctor to omit undetected harnesses, so that the output stays focused on harnesses lazyagents can inspect right now.
62. As a developer, I want undetected harness drift to be omitted rather than guessed, so that lazyagents does not inspect unusable harnesses.
63. As a developer, I want doctor to perform a lightweight drift check, so that I can see whether active profiles still match harness managed surfaces.
64. As a developer, I want doctor output as a line-based health summary, so that it is readable in a terminal.
65. As a developer, I want no JSON output in v1, so that the CLI remains focused.
66. As a developer, I want no dry-run in v1, so that the implementation focuses on actual profile use.
67. As a developer, I want CLI use to run without confirmation when there is no drift, so that intentional switching is fast.
68. As a developer, I want drift detected before switching away from an active profile, so that harness-local changes are not accidentally discarded.
69. As a developer, I want single-harness drift prompts to offer save, discard, or cancel, so that I control how unsaved changes are handled.
70. As a scripting user, I want `--save-changes`, so that drift can be saved non-interactively for single-harness use.
71. As a scripting user, I want `--discard-changes`, so that drift can be discarded non-interactively.
72. As a developer, I want `--save-changes` and `--discard-changes` to be mutually exclusive, so that drift handling is unambiguous.
73. As a developer, I want drift flags ignored when no drift exists, so that scripts can be simple.
74. As a developer, I want `--save-changes --all` rejected, so that ambiguous multi-harness saves cannot overwrite profile state incorrectly.
75. As a developer, I want `--all` drift handling to offer only discard or cancel, so that multiple harnesses cannot race to save conflicting profile artifacts.
76. As a developer, I want `--all` drift messages to name the affected harnesses, so that I know where unsaved changes exist.
77. As a developer, I want cancel during `--all` drift handling to cancel the entire operation, so that no harness changes happen after I decline.
78. As a developer, I want saving drift to reuse import logic, so that captured harness state is consistent with `create --from`.
79. As a developer, I want saving drift to update only the relevant harness's model and permission entries, so that one harness does not change unrelated harness config.
80. As a developer, I want model and permission differences excluded from drift prompts, so that routine config changes do not block switching.
81. As a developer, I want MCP differences to count as drift, so that harness-specific MCP edits can be saved or intentionally discarded.
82. As a developer, I want instruction link drift detected, so that deleted or retargeted instruction symlinks are not missed.
83. As a developer, I want skill set drift detected, so that harness-local skill additions or removals are handled before switching.
84. As a developer, I want command set drift detected, so that harness-local command additions or removals are handled before switching.
85. As a developer, I want malformed harness config during drift checking to fail, so that lazyagents does not proceed from an unreadable state.
86. As a developer, I want missing active profiles or profile source files to fail clearly, so that broken state is not silently ignored.
87. As a developer, I want instructions symlinked from the profile into each harness, so that edits happen in one shared profile source.
88. As a developer, I want Codex and Claude Code to map their native instruction filenames to the same profile `AGENTS.md`, so that one instruction file can serve different harnesses.
89. As a developer, I want valid skills symlinked as whole directories, so that skill scripts, references, and assets stay together.
90. As a developer, I want commands symlinked as Markdown files, so that saved prompts remain centralized.
91. As a developer, I want nested command directories supported in profiles, so that harnesses with command namespacing can preserve it.
92. As a developer, I want harness integrations to fail when a harness cannot represent nested command structure, so that unsupported command layouts are visible.
93. As a developer, I want non-Markdown command files ignored, so that notes or temporary files do not affect apply.
94. As a developer, I want empty Markdown command files applied as valid command files, so that lazyagents does not invent stricter rules than the harnesses document.
95. As a developer, I want invalid skills ignored, so that only folders containing `SKILL.md` are applied.
96. As a developer, I want harness-integration-owned managed directory contents replaced exactly, so that profile switching truly changes context.
97. As a developer, I want existing collisions backed up and replaced during use, so that profile artifacts take precedence while recovery remains possible.
98. As a developer, I want symlinks to be absolute, so that they are robust from any working directory.
99. As a developer, I want managed surfaces rewritten each use, so that apply behavior is deterministic.
100. As a developer, I want MCP profile definitions translated by harness integrations, so that one neutral file can configure different harness native formats.
101. As a developer, I want lazyagents to fully own the harness MCP list, so that the active profile's MCP context is exact.
102. As a developer, I want missing MCP files to mean no MCPs, so that profiles can intentionally clear MCPs.
103. As a developer, I want MCP server names to be stable machine identities, so that harness integrations can map them to native keyed config formats.
104. As a developer, I want MCP names to allow letters, numbers, dash, and underscore, so that common server identifiers work.
105. As a developer, I want duplicate MCP names rejected, so that native config generation is unambiguous.
106. As a developer, I want disabled duplicate MCP names rejected too, so that identity remains clear inside the profile.
107. As a developer, I want disabled MCP entries to be fully validated just like enabled entries, so that my disabled drafts are structurally sound.
108. As a developer, I want MCP `stdio` transport, so that local process servers are supported.
109. As a developer, I want MCP `http` transport, so that remote MCP servers are supported.
110. As a developer, I want MCP `transport` required, so that server definitions are explicit.
111. As a developer, I want `stdio` MCPs to require a command, so that launch definitions are complete.
112. As a developer, I want `stdio` MCP args and env to default empty, so that simple local servers are concise.
113. As a developer, I want `http` MCPs to require a URL, so that remote servers are complete.
114. As a developer, I want HTTP headers optional, so that unauthenticated and authenticated remote servers both work.
115. As a developer, I want MCP `enabled` to default true, so that entries apply unless explicitly disabled.
116. As a developer, I want disabled MCP servers not emitted to harness configs, so that I can keep them in profiles without activating them.
117. As a developer, I want lazyagents not to materialize secret values, so that profile files do not store actual secrets.
118. As a developer, I want environment variable references passed through, so that harnesses can resolve secrets at runtime.
119. As a developer, I want no per-harness MCP overrides in v1, so that MCP configuration remains a neutral standard profile input.
120. As a developer, I want harness applies to fail and roll back if an MCP cannot be represented, so that unsupported configuration does not partially apply.
121. As a developer, I want harness integrations to patch native config files instead of replacing them wholesale, so that unrelated harness settings are preserved.
122. As a developer, I want lazyagents to own only selected config keys and MCP sections, so that auth, cache, logs, plugins, and unrelated settings are untouched.
123. As a developer, I want native config patch writes to be atomic, so that config files are not corrupted by interruption.
124. As a developer, I want state writes to be atomic, so that lazyagents doctor remains trustworthy.
125. As a developer, I want Rust-native PATH lookup for harness detection, so that detection is testable and does not depend on shell behavior.
126. As a developer, I want harness detection logic inside harness integrations, so that each harness can define what installed means.
127. As a developer, I want detected harnesses with missing config directories to have those directories created during apply, so that first-time setup works.
128. As a developer, I want Unix-like systems supported in v1, so that symlink and home-directory behavior is predictable.
129. As a developer, I want `LAZYAGENTS_HOME` supported, so that tests and advanced usage can isolate profile state.
130. As a developer, I do not want public harness config path overrides, so that v1 behavior stays simple.
131. As a maintainer, I want internal harness path injection for tests, so that tests do not touch real harness directories.
132. As a maintainer, I want deep modules with small interfaces, so that profile schema, harness integration behavior, apply transactions, backups, drift, and CLI orchestration can be tested independently.
133. As a maintainer, I want detailed errors with paths and parse messages, so that users can fix broken profiles and configs quickly.
134. As a maintainer, I want profile names represented by a validated `ProfileName` domain type, so that invalid strings cannot leak into profile workflows.
135. As a maintainer, I want profile switching isolation covered by tests, so that stale managed skills, commands, and MCPs cannot leak between profiles.

## Implementation Decisions

- Implement profiles as global-only entities under the lazyagents home directory. The default home is the user's home-based lazyagents directory, with `LAZYAGENTS_HOME` as the only public override.
- Do not support project-local profiles or project-local lazyagents state.
- Represent each profile with a fixed skeleton: universal instruction file, skills directory, commands directory, MCP definition file, and required config file.
- Profile names are derived from directory names and must contain only letters, numbers, and dash.
- Profile names must be represented by a typed `ProfileName` value at command and domain boundaries. The implementation should reject invalid names before profile workflows run and must not silently normalize user input.
- Config metadata fields `name` and `description` are optional, but generated profile skeletons include them with the profile name and an empty description.
- `models` and `permissions` are required config objects. Individual harness keys are optional and missing values behave as `"default"`.
- Model and permission values are opaque harness-owned values. String values are common, but permission values may be JSON values when a harness uses structured permission configuration, such as Claude Code or opencode.
- `"default"` is the lazyagents sentinel and means leave that native setting untouched during apply. New profile skeletons intentionally omit per-harness model and permission entries; missing harness keys are the canonical compact representation of `"default"`.
- Imported profiles and saved drift copy the actual native model and permission values from the source harness when those native keys exist. If the native key is absent, lazyagents stores `"default"` for that harness entry to preserve the observed harness behavior explicitly.
- lazyagents does not validate model or permission values. Harnesses may add models, permission modes, or structured permission forms independently of lazyagents releases.
- Unknown keys in profile config and MCP definitions are allowed and ignored by v1.
- MCP definitions are stored as a neutral list of server objects. Server `name` is the stable machine identity and must allow letters, numbers, dash, and underscore.
- MCP `transport` is required for enabled servers. V1 supports `stdio` and `http`.
- `stdio` MCP servers require `command`; `args` and `env` default empty.
- `http` MCP servers require `url`; `headers` is optional.
- MCP `enabled` defaults to true. Disabled servers are retained in the profile but not emitted.
- Duplicate MCP names fail validation, including disabled entries.
- Disabled MCP entries must be fully valid just like enabled entries, including all transport-specific fields (e.g., `stdio` must have a command). No validation is skipped for disabled entries.
- Empty MCP files mean no MCPs. Invalid non-empty MCP files fail validation.
- Secrets are not materialized. Environment variable references are passed through and translated only where a harness requires syntax adaptation.
- No per-harness MCP overrides are supported in v1. If a harness cannot represent a profile MCP, that harness apply fails and rolls back.
- Use a harness integration contract as a deep module boundary under `src/harness/integration.rs`. Each concrete harness implementation under `src/integrations/` owns detection, target path resolution, profile-to-harness application, harness-to-profile import, drift detection, config patching, and native format translation.
- Implement initial harness integrations for Codex, Claude Code, and opencode.
- Adding a new harness should usually require adding one `src/integrations/<harness>.rs` file that implements the harness integration contract, adding it to `src/app/harness_registry.rs`, and making it pass the shared test suite in `src/integrations/test_suite.rs`. Shared orchestration should not need harness-specific branches beyond identity/registration.
- Harness detection uses Rust-native PATH lookup. Explicit harness use and profile import require detection. `--all` applies only to detected supported harnesses and silently ignores undetected ones.
- If no supported harness is detected during `--all`, the command fails.
- The harness integration declares managed surfaces: instruction file, skills directory contents, commands directory contents, MCP native section/list, and selected config keys.
- Filesystem artifacts that are universal and profile-owned are symlinked into harness config: instructions, valid skill directories, and Markdown command files.
- Instruction file names are harness-integration-specific. Different harness instruction filenames may point to the same profile instruction source.
- Valid skills are direct child directories containing `SKILL.md`. Invalid skill entries are ignored.
- Valid skill directories are symlinked wholesale.
- Commands support nested directories. Only `.md` files are applied; non-Markdown files and empty directories are ignored.
- Harness integrations preserve nested command structure where supported and fail if a harness cannot represent a profile's command layout.
- Managed directory contents are fully replaced during use after backup. The directory itself is kept.
- Symlinks are absolute.
- Use always rewrites managed surfaces deterministically.
- MCPs are not symlinked. Each harness integration translates the neutral MCP profile file into the harness-native MCP format and fully owns the harness MCP list/section.
- Native config files are patched, not replaced wholesale. Unrelated native config outside owned keys and sections is preserved.
- Native config patch writes are atomic.
- The backup module is a deep module responsible for capturing and restoring managed harness surfaces. It stores only the latest backup per harness.
- Backups include metadata sufficient to restore existing paths and remove paths that were originally absent.
- Backups copy file and directory contents and dereference symlinks. They never store symlinks to profile-owned artifacts.
- Backup replacement uses a temporary directory and atomic rename where possible.
- Rollback is internal only. There is no public rollback or restore command in v1.
- Apply is transactional per harness. Backup is captured immediately before apply, after any drift handling. On failure, rollback restores the backup.
- State is stored in one lazyagents state file, created lazily on first write and treated as empty when absent.
- State stores profile names, not absolute profile paths.
- State writes are atomic.
- State updates only after successful harness apply. In `--all`, state updates independently for each successful harness.
- Drift detection is performed before switching away from a known active profile.
- Drift includes instruction symlink drift, skill set drift, command set drift, MCP differences, and managed directory damage.
- Model and permission differences do not trigger drift prompts, but saving drift imports current opaque model and permission values for the relevant harness into the active profile. Missing native keys import as `"default"`.
- If single-harness use detects drift, interactive mode prompts for save changes, proceed without saving, or cancel. Non-interactive usage requires `--save-changes` or `--discard-changes`.
- `--save-changes` and `--discard-changes` are mutually exclusive.
- Drift flags are accepted and ignored when no drift exists.
- `--save-changes` is invalid with `--all`.
- If `--all` detects drift, it names affected harnesses and offers only proceed without saving or cancel entire operation. Non-interactive `--all` with drift requires `--discard-changes`.
- Saving drift reuses harness integration import logic and writes current harness state into the currently active profile. It updates only the relevant harness's model and permission entries, preserving opaque JSON values where present.
- `create` creates a skeleton and fails if the profile already exists.
- `create --from <harness>` copies supported current harness state into a new self-contained profile, dereferencing symlinks and leaving the harness unchanged.
- `create --from` fails on malformed native config needed for import.
- `doctor` shows detected harnesses plus validly named profile directories, marks invalid or missing config, and reports used, drifted, error, or unused profile states. Invalidly named directories are ignored.
- `show` summarizes a profile and is read-only.
- `edit` opens the profile directory with the user's editor or prints the path if no editor is configured. It does not normalize or validate.
- `delete` deletes inactive profiles, supports confirmation and `--yes`, allows invalid profiles, leaves backups untouched, and blocks deletion if state or harness symlinks indicate the profile is active.
- `delete` scans all supported harness config paths, not only detected harnesses.
- `doctor` shows detected harnesses only and performs lightweight drift checks for active profiles on detected harnesses.
- `doctor` uses a line-based health summary with `[✓]`, `[!]`, and `[x]` markers instead of tables.
- No global verbose mode, JSON output, dry-run, public rollback, TUI, or project-local profile support is included in v1.
- CLI, TUI, GUI, and other presentation layers are separate from domain logic. App workflows return typed results, summaries, and errors that any UI can render; profile, harness, integrations, state, backup, drift, and transaction modules must not print directly.
- Profile switching isolation is a required test theme: switching from a profile with skills, commands, or MCP servers to a profile without them must remove stale managed surfaces, and failed apply must restore previous managed surfaces without updating state.
- Current source layers:
  - `src/profile/`: profile names, profile config schema, neutral MCP parsing, profile validation, profile summaries, skeleton creation, profile filesystem storage, and import writes.
  - `src/harness/`: generic harness primitives and mechanics: `HarnessKind`, the `HarnessIntegration` trait, managed surfaces, artifact comparison helpers, drift report types, transactional apply, backup/rollback, symlink helpers, and atomic filesystem helpers.
  - `src/integrations/`: one concrete implementation file per supported harness, plus `test_suite.rs` for shared test-only contract checks.
  - `src/app/`: UI-independent product workflows and composition: create/import profile, delete safety checks, edit path lookup, inspect profile, doctor report assembly, active state persistence, profile use/drift decisions, and the built-in harness registry.
  - `src/cli/`: terminal-only adapter for clap parsing, prompts, rendering, and `$EDITOR` process execution.
- Dependency direction:
  - Production `profile/` and `harness/` do not depend on `app/`, `cli/`, or concrete `integrations/`.
  - Production `integrations/` depend on `profile/` and `harness/`.
  - `app/` composes `profile/`, `harness/`, and concrete `integrations/` through `src/app/harness_registry.rs`.
  - `cli/` depends on `app/` and should stay presentation-focused.
