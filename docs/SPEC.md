# Specifications

Status: draft for recovery, with owner confirmations from 2026-04-28

## Product Goal

lazyagents manages reusable coding-agent profiles and applies them to supported local harnesses.

A profile is a named bundle of:

- universal instructions
- skills
- commands
- model preferences
- permission preferences
- MCP server definitions

A harness is an external coding-agent runtime whose global configuration lazyagents can manage. Current supported harnesses are:

- Claude Code
- Codex
- OpenCode

lazyagents is intentionally local-first. It manages files under the user's home directory and does not install harnesses, discover marketplaces, or fetch remote skills.

## Non-Goals

- No harness installation, uninstall, marketplace, plugin, GitHub, or package discovery features.
- No migration or backwards compatibility machinery while the project is still early.
- No multi-crate architecture unless the codebase grows enough to justify it.
- No separate validate command for now. Validation should surface through `show` and `doctor`.

## Command Surface

The CLI binary is `lazyagents`.

Supported top-level commands:

```text
lazyagents help
lazyagents doctor
lazyagents create <name> [--from <harness>]
lazyagents show <name>
lazyagents edit <name>
lazyagents delete <name> [--yes]
lazyagents use <profile> --harness <harness> [--save-changes | --discard-changes]
lazyagents use <profile> --all [--discard-changes]
```

Behavior:

- `use` requires either `--harness <name>` or `--all`.
- `--save-changes` and `--discard-changes` are mutually exclusive.
- `--save-changes` is invalid with `--all`.
- `edit` opens the profile directory with `$EDITOR`, or prints the path when `$EDITOR` is not set.

## Filesystem Layout

The lazyagents home is:

```text
$LAZYAGENTS_HOME
```

or, if unset:

```text
~/.lazyagents
```

Expected structure:

```text
~/.lazyagents/
  profiles/
    <profile>/
      AGENTS.md
      config.json
      mcps.json
      skills/
      commands/
  state.json
  backups/
    <harness>/
```

Profile skeleton created by `create <name>`:

```text
<profile>/
  AGENTS.md
  config.json
  mcps.json
  skills/
  commands/
```

`create` writes a default `AGENTS.md`, empty `mcps.json`, and default `config.json`.
New skeleton `config.json` includes metadata plus empty `models` and `permissions` objects. It does not need explicit `"default"` entries for each supported harness.

`use` normalizes optional profile artifacts before applying:

- create missing `skills/`
- create missing `commands/`
- create missing `AGENTS.md`
- create missing `mcps.json`

`config.json` is required. Missing `config.json` is an error.
`AGENTS.md` is auto-created during `use`.
`mcps.json` is auto-created during `use`.

## Profile Names

Profile names are validated by `ProfileName`.

Rules:

- non-empty
- max length 64
- ASCII alphanumeric plus dash only
- no leading dash
- no trailing dash
- no consecutive dashes

Examples:

- valid: `work`, `work-1`, `a`
- invalid: ``, `work 1`, `work.1`, `work_1`, `-work`, `work-`, `work--1`

Profile names are folder names under `profiles/`.
Implementations must use a typed `ProfileName` value at command and domain boundaries instead of passing unvalidated strings through profile workflows. Profile names are preserved exactly after validation; lazyagents must not silently normalize or lowercase user input.

## Profile Config

Profile config file:

```text
config.json
```

Shape:

```json
{
  "name": "Work",
  "description": "Optional human-readable description",
  "models": {
    "codex": "gpt-5.2",
    "claude": "opus",
    "opencode": "default"
  },
  "permissions": {
    "codex": "on-request",
    "claude": "acceptEdits",
    "opencode": {
      "*": "ask",
      "bash": "allow"
    }
  }
}
```

Semantics:

- `name` is optional display metadata.
- `description` is optional display metadata.
- Generated profile skeletons include `name` with the profile name and `description` as an empty string.
- `models` and `permissions` are required objects.
- `models` maps harness id to an opaque native model value. String values are expected for current v1 harnesses.
- `permissions` maps harness id to an opaque native permission value. String values are common, but JSON values are allowed when a harness uses structured permission configuration, such as Claude Code or opencode.
- For Claude Code, a string permission value is a lazyagents shorthand for the native `permissions.defaultMode` setting. For example, `"acceptEdits"` writes `{ "permissions": { "defaultMode": "acceptEdits" } }` while preserving other existing Claude permission rules. A JSON object is written as the full native `permissions` object.
- Missing model/permission means `"default"` inside lazyagents.
- The `"default"` sentinel means lazyagents does not mutate that native setting. Applying `"default"` must not create the native model or permission key.
- Generated profile skeletons intentionally omit per-harness model and permission entries. Empty `models` and `permissions` objects are the canonical compact skeleton form, because missing harness keys already mean `"default"`.
- Importing from a harness copies the native model/permission value when the native key exists. If the native key does not exist, the profile stores `"default"` for that harness entry to preserve the observed harness behavior explicitly.
- lazyagents does not validate model or permission values. Harnesses may add models, permission modes, or structured permission forms independently of lazyagents releases.
- Unknown config keys are ignored by v1.

## Profile Artifacts

### Instructions

Profile instruction source:

```text
AGENTS.md
```

During `use`, this is symlinked into the harness-specific instruction target.

### Skills

Profile skills live under:

```text
skills/
```

A valid skill is a directory containing:

```text
SKILL.md
```

Invalid skill entries are ignored by apply and should be reported by `show` validation.

### Commands

Profile commands live under:

```text
commands/
```

All Markdown files under this tree are command files.

Nested commands:

- Claude: supported
- OpenCode: supported
- Codex: not supported

When applying to a harness that does not support nested commands, nested command files should fail before partial native writes.

Non-Markdown command files are ignored by apply and should be reported by `show` validation.
Empty command directories are ignored.
Empty Markdown command files are valid profile artifacts. lazyagents applies them as-is and lets the target harness decide whether an empty prompt is useful.

## MCP Definitions

Profile MCP file:

```text
mcps.json
```

Accepted top-level shape:

```json
[
  {
    "name": "local-server",
    "enabled": true,
    "transport": "stdio",
    "command": "lazy-mcp",
    "args": ["--flag"],
    "env": {
      "TOKEN": "$TOKEN"
    }
  }
]
```

Current neutral transports:

- `stdio`
- `http`

No other neutral transports are supported in v1.

Disabled MCP behavior:

- disabled entries remain in the profile
- disabled entries require fully valid transport fields, exactly like enabled entries
- disabled entries are not emitted into native harness config
- duplicate names fail even if one or both entries are disabled

MCP name rules:

- non-empty
- ASCII alphanumeric, dash, underscore

MCP field rules:

- `transport` is required for enabled servers.
- `stdio` servers require `command`; `args` and `env` default empty.
- `http` servers require `url`; `headers` is optional.
- Environment variable references are passed through.
- Secrets are not materialized.
- Unknown MCP keys are ignored by v1.
- There are no per-harness MCP overrides in v1.
- If a harness cannot represent a profile MCP, that harness apply fails and rolls back.
- Disabled MCP entries are retained in the profile and must pass the exact same structural and logical validation as enabled entries.

## Harness Integration Contract

Each harness has one implementation file:

```text
src/integrations/claude.rs
src/integrations/codex.rs
src/integrations/opencode.rs
```

Each harness integration owns:

- stable harness id
- binary name for PATH detection
- native paths
- settings patching
- MCP import/export
- native config validation
- apply verification

The intended architecture is:

- one harness = one harness integration file
- common harness mechanics live in `src/harness/`
- harness-specific behavior does not move into a giant enum match
- a `HarnessKind` enum may centralize identity and display metadata only
- adding a new harness should usually mean adding one `src/integrations/<harness>.rs` file, adding it to the built-in registry in `src/app/harness_registry.rs`, and making it pass the shared integration test suite in `src/integrations/test_suite.rs`

## Architecture Layers

lazyagents uses a small layered architecture:

```text
src/profile/
  Profile domain and storage primitives: profile names, config schema, neutral MCP parsing,
  validation, inspection, skeleton creation, profile filesystem writes.

src/harness/
  Generic harness primitives and mechanics: harness identity, integration trait, config paths,
  managed surfaces, drift report type, artifact comparison helpers, transactional apply,
  backup/rollback, atomic filesystem helpers.

src/integrations/
  Concrete harness implementations for Codex, Claude Code, and OpenCode. Each integration owns
  native paths, detection, import/export, native config patching, native MCP mapping, drift checks,
  apply, preflight, and verification. `test_suite.rs` contains shared test-only contract tests for
  integration implementations.

src/app/
  UI-independent product workflows and composition: profile creation/import, deletion safety checks,
  edit path resolution, profile inspection, doctor report assembly, profile use/drift decisions,
  active state persistence, and the built-in harness registry.

src/cli/
  Terminal UI only: clap argument parsing, terminal prompts, rendering, and launching `$EDITOR`.
```

Dependency direction:

- `profile/` and production `harness/` must not depend on `app/`, `cli/`, or concrete `integrations/`.
- `integrations/` may depend on `profile/` and `harness/`, but not on `app/` or `cli/` in production code.
- `app/` may compose `profile/`, `harness/`, and concrete `integrations/` through `app/harness_registry.rs`.
- `cli/` depends on `app/` and renders typed app results. CLI must not own workflow policy such as drift handling, active-state updates, delete safety checks, or doctor status assembly.
- Test-only modules may cross these boundaries when they intentionally exercise full workflows.

Important files:

- `src/app/harness_registry.rs` defines `HarnessRegistry` and `BuiltInHarnessRegistry`.
- `src/app/state.rs` is the single owner of lazyagents active profile state serialization.
- `src/app/use_profile.rs` owns product-level use/drift decision flow.
- `src/harness/apply.rs` owns lower-level rollback-protected mutation of one already-approved harness.
- `src/harness/drift.rs` owns the shared `DriftReport`/`DriftItem` contract returned by harness integrations.
- `src/integrations/test_suite.rs` owns reusable test-only behavior checks for concrete integrations.

## Harness Paths

### Claude Code

```text
config dir: ~/.claude
instruction target: ~/.claude/CLAUDE.md
skills dir: ~/.claude/skills
commands dir: ~/.claude/commands
settings file: ~/.claude/settings.json
global MCP file: ~/.claude.json
nested commands: yes
```

Claude Code native settings mapping:

- source/rationale: current official Claude Code settings and permissions docs define `~/.claude/settings.json` as the user settings file, list `permissions.defaultMode` as the setting for the default permission mode, and describe `permissions.allow`, `permissions.ask`, and `permissions.deny` as permission rule arrays
- model preference: top-level `primaryModel` in `~/.claude/settings.json`
- permission preference: top-level `permissions` object in `~/.claude/settings.json`
- string profile permission values map to `permissions.defaultMode`; Claude Code documents valid current default modes as `default`, `acceptEdits`, `plan`, `auto`, `dontAsk`, and `bypassPermissions`
- object profile permission values replace the native `permissions` object, enabling full `allow`, `ask`, `deny`, `additionalDirectories`, `defaultMode`, and related permission settings
- `theme` is a UI setting and is not used for lazyagents permission preference import or apply

### Codex

```text
config dir: ~/.codex
instruction target: ~/.codex/AGENTS.md
skills dir: ~/.codex/skills
commands dir: ~/.codex/prompts
settings file: ~/.codex/config.toml
global MCP file: ~/.codex/config.toml
nested commands: no
```

### OpenCode

Current intended paths:

```text
config dir: ~/.config/opencode
instruction target: ~/.config/opencode/AGENTS.md
skills dir: ~/.config/opencode/skills
commands dir: ~/.config/opencode/commands
settings file: ~/.config/opencode/opencode.json
global MCP file: ~/.config/opencode/opencode.json
nested commands: yes
```

Important: OpenCode is using plural `skills` and `commands` instead of old singular folder names. 

Confirmed:

- Claude instruction target is `~/.claude/CLAUDE.md`.
- Codex commands dir is `~/.codex/prompts`.
- OpenCode settings file is `~/.config/opencode/opencode.json`.

## Native MCP Mapping

lazyagents owns the native MCP list for each harness and translates enabled neutral MCP entries into the harness-native shape.

Claude Code:

- native file: `~/.claude.json` for user-scoped cross-project MCP servers
- native shape: `mcpServers` object keyed by server name for user scope; project-scoped `.mcp.json` uses the same `mcpServers` object shape
- `stdio` mapping: `{ "type": "stdio", "command": "...", "args": [...], "env": {...} }`
- `http` mapping: `{ "type": "http", "url": "...", "headers": {...} }`
- lazyagents does not use project-scoped `.mcp.json` in v1 because profiles are global-only

Codex:

- native file: `~/.codex/config.toml`
- native shape: TOML tables under `[mcp_servers.<name>]`
- `stdio` mapping: `command`, optional `args`, optional `[mcp_servers.<name>.env]`, and `enabled = true`
- `http` mapping: `url`, optional `[mcp_servers.<name>.http_headers]`, optional `[mcp_servers.<name>.env_http_headers]`, and `enabled = true`
- profile HTTP header values that are literal strings map to `http_headers`; values that are environment references map to `env_http_headers` where the harness supports that native shape

OpenCode:

- native file: `~/.config/opencode/opencode.json`
- native shape: top-level `mcp` object keyed by server name
- `stdio` mapping: `{ "type": "local", "command": ["cmd", "arg"], "environment": {...}, "enabled": true }`
- `http` mapping: `{ "type": "remote", "url": "...", "headers": {...}, "enabled": true }`

## Apply / Use Workflow

Single-harness use:

1. Verify target harness is detected on PATH.
2. Check drift against the harness active profile.
3. Handle drift according to `Prompt`, `Save`, or `Discard`.
4. Normalize profile optional artifacts.
5. Load profile config and MCP definitions.
6. Resolve harness paths.
7. Capture backup of managed harness surfaces.
8. Apply profile:
   - ensure harness config dir exists
   - symlink instruction source to instruction target
   - clear and repopulate skills dir with symlinks
   - clear and repopulate commands dir with symlinks
   - patch native settings
   - patch native MCP list
9. Verify applied state.
10. Save active profile in state.
11. On apply or verification failure, rollback from backup.

All-harness use:

1. Detect all supported harnesses.
2. Fail if none are detected.
3. Preflight drift for all detected harnesses.
4. If any drift exists, name the affected harnesses.
5. If drift exists and `--discard-changes` is not set, prompt to proceed without saving or cancel the whole operation in interactive mode; fail clearly in non-interactive mode.
6. Normalize profile.
7. Apply profile to each detected harness.
8. Continue after individual harness failures.
9. Update state independently for each successful harness apply.
10. Return a per-harness success/failure summary.

`--all --discard-changes` discards drift independently per affected harness. A state-save failure for one harness is reported as that harness's failure and does not prevent other harnesses from being attempted.

## Drift

Drift means the harness managed surface differs from what the active profile expects.

Managed drift areas:

- instruction target
- valid skill symlinks
- command symlinks
- native MCP list
- malformed native config

Model and permission differences do not trigger drift prompts.
Saving drift still imports current opaque model and permission values for the relevant harness into the active profile. Missing native keys import as `"default"`.

Single-harness drift handling:

- `--discard-changes`: ignore drift and proceed
- `--save-changes`: save any changes in profile-related harness surfaces back into the active profile, then proceed
- default interactive prompt: ask user whether to save, discard, or cancel
- default non-interactive behavior: fail with a clear message

Confirmed owner intent:

- Save changes is for the case where the user is switching away from one active profile to another profile and the current active profile has drift.
- The user should be able to proceed while saving changes, proceed without saving changes, or cancel switching.
- Drift detection is limited to profile-related files/surfaces managed by lazyagents.

Current recovered code does not implement real save/prompt behavior. That must be restored.

Save changes should import all drifted profile-related surfaces from the current harness into the active profile:

- instruction target into profile `AGENTS.md`
- harness skills into profile `skills/`
- harness commands into profile `commands/`
- native MCP list into profile `mcps.json`
- native model/permission values into profile `config.json`, or `"default"` when native keys are absent

Harness-integration-owned managed directory contents are replaced exactly during `use`, so extra harness files inside managed skills/commands directories count as drift and are saved when `--save-changes` is selected for a single harness.

## Backup And Rollback

Before mutating a harness, lazyagents captures a backup of managed surfaces:

- instruction target
- skills dir
- commands dir
- settings file
- separate MCP file, if not the same as settings file

Rollback is internal and automatic after apply/verification failure.

Rollback behavior:

- restore surfaces that existed before apply
- remove surfaces that did not exist before apply
- include original error and rollback error in failure message if rollback fails
- store only the latest backup per harness
- include metadata sufficient to restore existing paths and remove originally absent paths
- copy file and directory contents into the backup rather than storing symlinks
- dereference symlinks while backing up, so rollback remains valid even if the previously active profile is manually removed
- represent originally missing paths in the backup manifest instead of trying to link them

Rollback is an internal safety mechanism. There is no public rollback or restore command in v1.

## Import Workflow

`create <name> --from <harness>` creates a profile from current harness state.

Expected behavior:

- create profile skeleton
- import harness instruction target into profile `AGENTS.md`
- import native MCPs into profile `mcps.json`
- import native model/permission into profile `config.json`, using `"default"` when native keys are absent
- import skills and commands from harness managed dirs where possible
- do not mutate harness config
- fail if the profile already exists
- require the harness binary to be detected
- dereference imported symlinks so the profile is self-contained
- limit imported skills to valid skill folders
- preserve nested Markdown command paths
- fail on malformed native config needed for import

Confirmed owner intent:

- Any MCP present in the harness should be represented in the profile after import/save, whether disabled or enabled in profile semantics.

Native harness configs generally do not expose a lazyagents disabled marker, so imported harness MCPs become `enabled: true`.

## Status

`doctor` is read-only.

It should show:

- detected supported harnesses only
- all validly named profiles
- profile usage across harnesses
- drift status for active profiles
- invalid profiles and malformed native config if present for detected harnesses

It should not:

- mutate profiles
- normalize profile files
- install anything
- show undetected harnesses solely because they are present in state

Line format:

```text
[✓] Harnesses (2 available: codex, opencode)
[!] Profiles (1 drifted, 1 error):
  - Personal (used by codex, opencode)
  - Work (drifted by claude)
  - LegacyProject (invalid: missing config.json)
  - Playground (unused)
```

Profile state values may include `used by <harness>`, `drifted by <harness>`, `error: <harness>`, `invalid: <reason>`, or `unused`.

## Show

`show <name>` is read-only.

It should show:

- profile name
- path
- display name
- description
- instruction status
- valid skills
- ignored skills
- command files
- ignored command files
- MCP summaries
- model preferences
- permission preferences
- validation warnings/errors once validation is restored

It should not mutate the profile.

## Validation

Validation should use collected issues, not only fail-fast parsing.

Issue model should include at least:

- severity: error/warning
- code or category
- path/context
- message

Validation should surface through:

- `show`
- `doctor` for active profile/harness issues only

No separate `validate` command for now.

Checks to restore:

- invalid/ignored skill directories
- missing `SKILL.md`
- ignored command files
- nested commands incompatible with target harness
- malformed `config.json`
- malformed `mcps.json`
- empty MCP command
- invalid MCP URL
- unsupported MCP transport/fields for harness
- missing env vars or suspicious env references if supported

## Architecture Requirements

Keep the codebase simple and Rust-idiomatic:

- prefer typed domain values over raw strings where useful
- keep filesystem effects explicit
- use clear user-facing error messages
- keep harness-specific behavior in harness integration files, but extract shared domain parsing (such as neutral `mcps.json` deserialization and `"default"` config sentinel handling) into the `profile` module to avoid duplication across integrations.
- keep CLI, TUI, GUI, and other presentation layers separate from storage and core domain logic
- app workflows should return typed results or summaries that any UI can render
- avoid async
- avoid new external dependencies unless clearly useful

Preferred module shape:

```text
src/
  app/
  cli/
  harness/
  integrations/
  profile/
  main.rs
```

Future cleanup tickets already capture:

- split harness trait into focused modules
- improve validation through show/doctor
- keep CLI rendering thin as app workflows grow

## Recovery Acceptance Criteria

The source restoration is complete when:

- all CLI commands listed in this spec work
- harness paths match confirmed native tool expectations
- create/show/edit/delete/doctor behavior matches spec
- profile import works without mutating harness state
- single-harness use applies profile and rolls back on failure
- all-harness use reports per-harness outcomes
- drift detection and `--save-changes` behavior are restored
- backup/rollback is covered by tests
- MCP import/export is covered by tests for Claude, Codex, OpenCode
- `ProfileName` validation is covered by tests, including empty names, length, invalid characters, leading/trailing dash, and consecutive dash
- profile switching isolation is covered by tests:
  - switching from a profile with skills to one without skills removes old managed skills
  - switching from a profile with commands to one without commands removes old managed commands
  - switching from a profile with MCP servers to one without MCP servers clears the native MCP list
  - switching with `--discard-changes` does not write harness drift into the old active profile
  - switching with `--save-changes` imports only the relevant harness's managed surfaces into the old active profile
  - failed apply restores previous managed surfaces and does not update state
- doctor and show are deterministic and read-only
- `cargo fmt` and `cargo test` pass
- test coverage covers high-risk workflows, not just parser smoke tests

## Open Questions To Resolve

None currently.
