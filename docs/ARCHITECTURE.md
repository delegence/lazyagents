# ARCHITECTURE

This document captures the stable design decisions in `lazyagents`. It is intentionally shorter than the original product/spec notes and should stay focused on choices contributors need to preserve.

## Product Shape

`lazyagents` manages reusable profiles for local coding-agent harnesses.

A profile is a named bundle of:

- shared instructions
- skills
- saved prompt commands
- MCP server definitions, for harnesses with native MCP support
- model preferences
- permission preferences

A harness is an external coding-agent runtime whose global configuration can be managed by `lazyagents`. Current built-in harnesses are Claude Code, Codex, Gemini, OpenCode, and Pi.

The tool is local-first:

- profiles live under the lazyagents home
- harness configs are read and patched on the local filesystem
- no harness installation or marketplace discovery
- no project-local profile state
- no network behavior in core workflows

## Core Terms

- **Profile**: reusable saved configuration stored under `~/.lazyagents/profiles/<name>`.
- **Harness Type**: supported external runtime behavior such as Codex, Claude Code, Gemini CLI, OpenCode, or Pi.
- **Harness Instance**: configured target in `settings.json` with an id, type, binary, and config directory.
- **Profile Use**: applying a profile to one harness or all detected harnesses.
- **Active Profile**: profile recorded in `state.json` as last successfully used for a harness.
- **Managed Surface**: harness config path that `lazyagents` owns during profile use.
- **Drift**: difference between harness managed surfaces and the active profile.
- **Backup**: latest copied snapshot of managed surfaces for one harness.
- **Rollback**: internal restore from backup after failed profile use.

Avoid using "agent" for both sides of the system. Use **Profile** for the saved bundle and **Harness** for the external tool. Use **Harness Type** for behavior and **Harness Instance** for configured targets.

## Source Layers

```text
src/profile/       profile names, config, MCP parsing, validation, inspection, storage
src/harness/       generic harness mechanics, drift, backup, rollback, symlinks
src/integrations/  concrete Claude Code, Codex, Gemini CLI, OpenCode, Pi implementations
src/app/           UI-independent workflows and composition
src/cli/           terminal parsing, prompts, rendering, editor launch
```

Dependency direction:

- `profile/` and production `harness/` do not depend on `app/`, `cli/`, or concrete integrations.
- `integrations/` depend on `profile/` and `harness/`.
- `app/` composes `profile/`, `harness/`, and built-in integrations through the harness registry.
- `cli/` depends on `app/` and should stay presentation-focused.

Workflow policy belongs in `app/`, not `cli/`. Filesystem mechanics that are not harness-specific belong in `harness/`. Native format translation belongs in the relevant integration file.

## Profile Storage

Default lazyagents home:

```text
~/.lazyagents
```

Override for tests and advanced usage:

```text
LAZYAGENTS_HOME=/path/to/home
```

Profile skeleton:

```text
profiles/<name>/
  PROFILE.md
  mcps.json
  skills/
  commands/
  agents/
```

`PROFILE.md` is required. Missing optional artifacts are normalized during profile use:

- `mcps.json`
- `skills/`
- `commands/`
- `agents/`

Profile names are represented by `ProfileName` and must be validated at boundaries. They may contain only ASCII letters, numbers, and dashes; may be up to 64 characters; cannot start or end with a dash; and cannot contain consecutive dashes. Do not silently normalize profile names.

## Harness Settings

`~/.lazyagents/settings.json` is created automatically when missing and is the source of truth for harness instances:

```json
{
  "harnesses": {
    "codex": {
      "type": "codex",
      "displayName": "Codex",
      "binary": "codex",
      "configDir": "~/.codex"
    }
  }
}
```

`type` selects integration behavior. `configDir` selects the native harness home and must be absolute, `~`, or begin with `~/`. Integrations derive all managed paths from `configDir`; per-surface path overrides are intentionally unsupported. `displayName` and `binary` are optional and default from the harness type. Harness instance ids may contain only lowercase ASCII letters, numbers, and dashes.

State and profile preferences are keyed by harness instance id. If two instances have the same type and lexically normalized `configDir`, they are aliases for the same native state; profile use updates active state for every alias in that group. Doctor reports shared config directories using the same normalized alias identity.

## Profile File

`PROFILE.md` stores optional display metadata and opaque per-harness preferences in YAML frontmatter. The Markdown body stores shared profile instructions:

```md
---
name: Work
description: ""
models:
  codex: gpt-5.2
permissions:
  codex: on-request
---

# Shared instructions
```

Rules:

- `models` and `permissions` are required objects.
- Missing harness entries behave like `"default"`.
- The string `"default"` means leave that native harness setting unchanged.
- Model values are opaque harness-owned values.
- Permission values are opaque and may be strings or structured JSON.
- Unknown config keys are ignored.

Do not validate model names or permission vocabularies unless a harness cannot serialize the value shape. Codex, Gemini, and Pi currently require string preferences for the native settings they patch. Pi model strings may use `provider/model`; otherwise Pi preserves any existing provider and updates only the model.

## MCP Definitions

Profiles use one neutral MCP file:

```text
mcps.json
```

Supported neutral transports:

- `stdio`
- `http`

Rules:

- MCP names allow ASCII letters, numbers, dash, and underscore.
- Duplicate names are rejected, including disabled entries.
- `enabled` defaults to `true`.
- Disabled entries are fully validated and emitted to native configs as disabled.
- `stdio` requires `command`; `args` and `env` default empty.
- `http` requires `url`; `headers` defaults empty.
- `http` URLs must start with `http://` or `https://`.
- Unknown MCP keys are ignored.
- Environment variable references are passed through.

Codex represents HTTP headers split across literal `http_headers` and environment-backed `env_http_headers`; neutral header values that start with `$` are rendered to Codex env header entries.

Integrations with native MCP support translate the neutral list into native harness config. MCP support is optional per harness; integrations without native MCP support ignore profile MCP definitions, do not drift-check them, and preserve existing profile MCPs on import/save by returning no MCP import data. If a harness supports MCP but cannot represent a valid neutral MCP definition, apply must fail and roll back.

## Sub-agent Definitions

Profiles use one neutral sub-agent directory:

```text
agents/*.md
```

Each file is Markdown with YAML frontmatter. Required frontmatter fields:

- `name`
- `description`

Optional neutral fields:

- `model`: scalar value or per-harness/default map
- `tools`: neutral allow/deny map or native-compatible value
- `permission`: scalar value or per-harness/default map
- `maxTurns`
- `harness`: per-harness native override maps

The Markdown body becomes the native sub-agent prompt/instructions. Empty bodies are allowed for harness-native agents that intentionally carry all behavior in frontmatter. There is no `prompt` frontmatter field in the neutral contract.

Integrations with native sub-agent support render neutral sub-agents into native files. Codex renders TOML files under `agents/`; Claude, Gemini, and OpenCode render Markdown files with native frontmatter. Integrations without native sub-agent support return `supports_subagents() == false`, ignore profile sub-agents during apply/drift, and preserve existing profile sub-agents on import/save by returning no sub-agent import data. Pi currently falls into this category because Pi core does not ship sub-agents.

## Profile Use Workflow

Single-harness profile use:

1. Resolve the target harness through the registry.
2. Require the harness binary to be detected on `PATH`.
3. Check drift against the harness active profile.
4. Save, discard, prompt, or cancel according to drift decision.
5. Normalize target profile optional artifacts.
6. Run harness preflight.
7. Capture backup of managed surfaces.
8. Clear managed surfaces.
9. Apply profile artifacts and native config patches.
10. Verify applied state.
11. Save active profile state only after success.
12. Roll back on apply, verify, or state-save failure.

All-harness profile use:

- detects all supported harnesses
- fails if none are detected
- prompts only once for drift
- does not support `--save-changes`
- continues after individual harness failures
- updates state per successful harness

Mutating workflows acquire an exclusive lazyagents home lock before changing profiles, settings, harness config, or state. A second mutating command fails rather than racing the first.

In non-interactive shells, drift prompts are not attempted. The CLI reports the required explicit flag instead: `--save-changes` or `--discard-changes` for one harness, and only `--discard-changes` for `--all`.

## Drift

Drift is checked before switching away from an active profile.

Drift includes:

- instruction content mismatch
- skill set mismatch
- command set mismatch
- MCP differences for harnesses with native MCP support
- managed directory damage
- malformed native config needed for drift comparison

Model and permission differences do not trigger drift prompts.

Saving drift imports current harness managed state into the active profile:

- instruction target into the Markdown body of `PROFILE.md`
- valid skills into `skills/`
- Markdown commands into `commands/`
- native sub-agents into neutral `agents/`, when the harness supports native sub-agents
- native MCP list into `mcps.json`, when the harness supports native MCP
- native model/permission values into `PROFILE.md` frontmatter

Saving drift updates only the relevant harness model and permission entries.

Saving drift also imports valid shared skills from `~/.agents/skills`, using the same merge rules as `create --harness`: harness-native skills win on name collision, imported shared skills are removed from the shared skills directory, and invalid or hidden shared entries are left alone.

Hidden files and directories starting with `.` inside managed folders are ignored for drift, backup, import, and clearing.

## Backup And Rollback

Before mutating a harness, `lazyagents` captures a latest backup under:

```text
~/.lazyagents/backups/<harness>/
```

Backups cover only managed surfaces:

- instruction target
- skills directory contents
- commands directory contents
- native settings file
- native MCP file, if separate and supported by the harness

Backups copy real file contents and dereference symlinks. Rollback must not depend on profile files still existing.

Native config files are usually `ManagedSurface::preserved_file`: they are backed up, then patched rather than deleted wholesale.

Rollback is internal. There is no public restore command in v1.

## Harness Integrations

Each harness integration implements `HarnessIntegration` in one file under `src/integrations/`.

Responsibilities:

- identify the harness kind
- declare whether skills, commands, and native MCP are supported
- declare whether native sub-agents are supported
- detect the binary from `AppEnvironment.path_entries`
- define native config paths from `AppEnvironment.user_home`
- declare managed surfaces
- preflight unsupported profile shapes before mutation
- detect drift
- import current harness state into `ProfileImport`
- apply profile artifacts and native config patches
- verify resulting state

Adding a harness should usually touch:

```text
src/harness/kind.rs
src/integrations/<harness>.rs
src/integrations/mod.rs
src/app/harness_registry.rs
docs/ARCHITECTURE.md
docs/INTEGRATION.md
README.md
```

Do not add a second CLI-only harness enum. CLI accepts harness ids as strings and resolves them through `HarnessRegistry`.

## Native Harness Paths

Claude Code:

```text
config dir: ~/.claude
instruction target: ~/.claude/CLAUDE.md
skills dir: ~/.claude/skills
commands dir: ~/.claude/commands
agents dir: ~/.claude/agents
settings file: ~/.claude/settings.json
MCP file: ~/.claude.json
nested commands: yes
```

Codex:

```text
config dir: ~/.codex
instruction target: ~/.codex/AGENTS.md
skills dir: ~/.codex/skills
commands dir: ~/.codex/prompts
agents dir: ~/.codex/agents
settings file: ~/.codex/config.toml
MCP file: ~/.codex/config.toml
nested commands: no
```

OpenCode:

```text
config dir: ~/.config/opencode
instruction target: ~/.config/opencode/AGENTS.md
skills dir: ~/.config/opencode/skills
commands dir: ~/.config/opencode/commands
agents dir: ~/.config/opencode/agents
settings file: ~/.config/opencode/opencode.json
MCP file: ~/.config/opencode/opencode.json
nested commands: yes
```

Gemini CLI:

```text
config dir: ~/.gemini
instruction target: ~/.gemini/GEMINI.md
skills dir: ~/.gemini/skills
commands dir: ~/.gemini/commands
agents dir: ~/.gemini/agents
settings file: ~/.gemini/settings.json
MCP file: ~/.gemini/settings.json
nested commands: yes
```

Pi:

```text
config dir: ~/.pi/agent
instruction target: ~/.pi/agent/AGENTS.md
skills dir: ~/.pi/agent/skills
commands dir: ~/.pi/agent/prompts
agents dir: unsupported
settings file: ~/.pi/agent/settings.json
MCP file: unsupported
nested commands: no
```

## Testing Strategy

The shared integration test suite is the behavioral contract for harnesses. Every concrete integration should use it.

It covers:

- optional profile artifact normalization
- stale managed surface clearing for supported artifact types
- default preference behavior
- optional skill/command/MCP behavior, including invalid MCP failure for harnesses with native MCP support
- rollback with symlink dereferencing
- import behavior
- malformed native config failures
- save/discard drift behavior
- supported artifact, preference, MCP, and state application
- nested command support

Use focused harness-specific tests for native format quirks, config preservation, MCP mapping edge cases, and unsupported shapes.

## Coding Choices

- Keep changes simple and explicit.
- Prefer typed domain values at boundaries, especially `ProfileName`, harness type, and harness instance id.
- Prefer standard library plus well-known crates.
- Use structured parsers for JSON and TOML rather than string manipulation.
- Patch native config files; preserve unrelated settings.
- Use direct file writes for profile instructions and absolute symlinks for profile-owned skills and commands.
- Keep CLI output user-facing and clear.
- Keep validation accumulated where possible so `show` and `doctor` can report multiple issues.
- Avoid async until there is a clear need.
- Avoid new dependencies unless they remove real complexity.
