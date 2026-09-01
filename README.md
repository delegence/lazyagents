# lazyagents

Manage reusable Agent profiles across various coding agents.

`lazyagents` lets you save a configured agent setup once, then apply it to one or more local coding harnesses. A profile can contain shared instructions, skills, saved prompts, MCP servers, model preferences, and permission preferences. Harnesses apply the parts they natively support.

## Install

Install the latest release:

```sh
curl -fsSL https://raw.githubusercontent.com/delegence/lazyagents/main/install.sh | sh
```

Install a specific version:

```sh
curl -fsSL https://raw.githubusercontent.com/delegence/lazyagents/main/install.sh | LAZYAGENTS_VERSION=0.1.0 sh
```

Build from source:

```sh
cargo build --release
```

Run from source:
```sh
cargo run -- <command>
```

## Commands

```sh
lazyagents doctor
lazyagents new <name>
lazyagents new <name> --harness <harness-id>
lazyagents new <name> -H <harness-id>
lazyagents show <name>
lazyagents edit <name>
lazyagents delete <name> [--yes]
lazyagents settings edit
lazyagents settings reset [--yes]
lazyagents use <name> --harness <harness-id>
lazyagents use <name> -H <harness-id>
lazyagents use <name> --all
lazyagents unset --harness <harness-id>
lazyagents unset -H <harness-id>
lazyagents unset --all
```

`unset` deactivates the profile for one harness or all harnesses. It only updates lazyagents state: harness files are left unchanged, no drift check runs, and the harness binary does not need to be installed. Harness instances that share the same type and `configDir` are unset together.

`settings edit` opens `settings.json` with `$EDITOR`. If `$EDITOR` is not set, it prints the settings path.

Drift handling:

```sh
lazyagents use <name> --harness codex --save-changes
lazyagents use <name> --harness codex --discard-changes
lazyagents use <name> --all --discard-changes
```

`use` always requires an explicit target: either `--harness <harness-id>` (`-H <harness-id>`) or `--all`. Harness ids come from `settings.json`, so they can be built-in ids such as `codex` or custom instance ids such as `codex-max`.

## What Is A Profile?

A profile is stored under:

```text
~/.lazyagents/profiles/<name>/
```

Profile layout:

```text
PROFILE.md     metadata/preferences in YAML frontmatter and instructions in Markdown body
skills/        skill directories containing SKILL.md
commands/      Markdown saved prompts
agents/        neutral Markdown sub-agent definitions
mcps.json      neutral MCP server definitions
```

Profile names are CLI-safe identifiers: ASCII letters, numbers, and dashes only. Names may be up to 64 characters, cannot start or end with a dash, and cannot contain consecutive dashes.

## Harness Settings

Harness instances are configured in:

```text
~/.lazyagents/settings.json
```

If the file is missing, `lazyagents` uses default Claude, Codex, Gemini, OpenCode, and Pi instances in memory. `settings edit` materializes those defaults before opening the file, and `settings reset` always writes them. Each instance has an id, a harness `type`, an optional `displayName`, an optional `binary`, and a `configDir`. Instance ids use lowercase ASCII letters, numbers, and dashes. `configDir` must be absolute, `~`, or begin with `~/`.

Example:

```json
{
  "harnesses": {
    "codex": {
      "type": "codex",
      "displayName": "Codex",
      "binary": "codex",
      "configDir": "~/.codex"
    },
    "codex-max": {
      "type": "codex",
      "displayName": "Codex Max",
      "binary": "codex",
      "configDir": "~/.codex-max"
    }
  }
}
```

Use instance ids with `--harness` or `-H`. Model and permission preferences in profile `PROFILE.md` frontmatter are keyed by instance id. If two instances have the same type and `configDir`, applying a profile to one marks both active because they represent the same native harness state.

Doctor reports shared config directories using the same physical type and `configDir` identity used for active-profile aliasing. Existing symlinked ancestors are resolved, including paths with missing final components.

`use --all` applies a shared native configuration only once. Other instance ids in the same alias group are marked active without rewriting the same files again.

Reset `settings.json` to the built-in defaults:

```sh
lazyagents settings reset
lazyagents settings reset --yes
```

Reset asks for confirmation when the file already exists. `--yes` skips the prompt.

## How It Works

When you apply a profile, `lazyagents`:

1. Checks the selected harness is available on `PATH`.
2. Checks whether the currently active profile has unsaved drift.
3. Creates a backup of the harness-managed files.
4. Writes profile instructions, skills, and commands into the harness config as independent copies.
5. Renders neutral sub-agent definitions into native harness agent files when supported.
6. Patches native model, permission, and MCP settings when supported by the harness.
7. Verifies the result.
8. Updates `~/.lazyagents/state.json` only after success.

If apply, verification, or state saving fails, the harness is rolled back from the latest backup.

## Importing Existing Setup

Create a profile from an existing harness:

```sh
lazyagents new work -H codex
```

Import copies the current managed harness state into a self-contained profile. Symlinks are dereferenced. Valid shared skills from `~/.agents/skills` are also imported unless a harness-native skill with the same name already exists.

Imported shared skills are removed from `~/.agents/skills` only after the profile transaction commits. Hidden files, empty directories, and Unix file and directory modes are preserved. A skill that contains a symlink or unsupported filesystem entry is rejected and left in place. If the process stops during post-commit cleanup, the durable profile copy remains and a marked `.lazyagents-import-*` copy can remain beside the shared skills. The next locked mutating command finishes this cleanup. Unmarked directories are left for manual inspection.

## Drift

Drift means the current harness-managed files no longer match the active profile. This can happen if you edit a harness config directly after using a profile.

Drift checks include:

- instruction content
- skills
- commands
- sub-agents, for harnesses with native sub-agent support
- MCP definitions, for harnesses with native MCP support
- managed directory damage

Model and permission differences do not block switching.

For one harness, you can save drift back into the active profile:

```sh
lazyagents use home --harness claude --save-changes
```

Saving drift imports the same shared skills as `new --harness`. Source removal waits until target profile use, verification, and state update commit. A cleanup error is a warning after the successful profile switch.

Or discard it:

```sh
lazyagents use home --harness claude --discard-changes
```

For `--all`, drift can only be discarded or cancelled.

Hidden files and directories starting with `.` inside managed folders are ignored. They do not trigger drift, are not backed up, and are not cleared during profile use.

## Sub-agent Format

`agents/*.md` uses neutral Markdown with YAML frontmatter. The Markdown body is the sub-agent prompt/instructions. Empty bodies are allowed for harnesses that support them. Codex rejects blank agent instructions during preflight:

```md
---
name: reviewer
description: Reviews changes for correctness, security, regressions, and missing tests.
model:
  default: inherit
  codex: gpt-5.6-sol
  claude: sonnet
tools:
  read: allow
  write: deny
permission:
  codex: on-request
maxTurns: 10
harness:
  claude:
    isolation: worktree
---

You are a careful code reviewer.
```

Supported harnesses render this neutral format into native sub-agent files. Pi core does not natively support sub-agents, so Pi ignores profile sub-agents unless a future integration explicitly targets a Pi sub-agent extension.

Native field mappings follow the current harness schemas:

- Codex writes `name`, `description`, and `developer_instructions` TOML. Codex has no documented per-agent turn-limit field, so neutral `maxTurns` is not emitted there.
- Claude writes YAML `permissionMode` and `maxTurns`.
- Gemini writes YAML `max_turns`; its agent format has no permission field.
- OpenCode gets the agent name from the filename and writes `permission` and `steps`. The deprecated `tools` and `maxSteps` fields are not emitted.

Codex custom prompts and Claude custom commands still work, but their maintainers now recommend skills for new reusable workflows. lazyagents keeps command support for existing profiles.

## MCP Format

`mcps.json` uses a neutral list of MCP definitions:

```json
[
  {
    "name": "local-server",
    "enabled": true,
    "transport": "stdio",
    "command": "lazy-mcp",
    "args": ["--flag"],
    "env": {
      "TOKEN": {"env": "TOKEN"}
    }
  }
]
```

Supported transports:

- `stdio`
- `http`

Disabled MCP entries are validated and emitted where the native global schema supports them. Claude rejects disabled entries because Claude disablement is project-specific. Harnesses without native MCP support ignore `mcps.json` and preserve it during imports/save-changes.

HTTP MCP `url` values must start with `http://` or `https://`. Plain environment and header strings are literal. Use `{"env":"TOKEN"}` for an environment reference. Claude renders `${TOKEN}`, OpenCode renders `{env:TOKEN}`, and Codex HTTP uses `env_http_headers`. Codex stdio references use `env_vars` and require the destination key to match the source name.

Native MCP import rejects fields outside the neutral core. This prevents profile use from dropping native authentication, tool restrictions, working directories, or timeouts.

The neutral format does not represent Gemini SSE, mixed native transports, OpenCode tool permission rules, Codex duplicate literal and inherited environment sources, or compound native substitution expressions. Import and profile preflight reject these forms before backup. Claude and Codex also reject malformed native `args` values.

## Profile File

`PROFILE.md` stores opaque per-harness preferences in YAML frontmatter. Its Markdown body stores the shared agent instructions:

```md
---
name: Work
description: ""
models:
  codex: gpt-5.2
  codex-max: gpt-5.2-high
permissions:
  codex: on-request
  opencode:
    edit: ask
    bash: deny
---

# Shared instructions

Work carefully and explain important tradeoffs.
```

Missing model or permission entries behave like `"default"`, which means `lazyagents` leaves that native harness setting unchanged.

Preferences stay opaque in the profile but must match the target harness shape. Codex approval policies accept either a string or a granular object; OpenCode permissions use an object. Pi model preferences may be written as `provider/model`; lazyagents maps that to Pi's provider and model settings while a plain model value updates only the model.

## Operational Notes

Commands that change LazyAgents data, and commands that recover an interrupted profile transaction, take an exclusive lock in `~/.lazyagents/.lock`. `show` and `doctor` can recover one valid profile rollback directory when the canonical profile is missing, so they also take this lock. Registry reads do not create `settings.json`.

`edit` opens the profile directory with `$EDITOR` when it is set; otherwise it prints the path.

`delete` refuses to remove an active profile, including profiles still referenced by lazyagents state or by legacy symlinks in managed harness config.

In non-interactive shells, drift prompts fail with instructions to pass `--save-changes` or `--discard-changes`. For `--all`, drift can only be discarded with `--discard-changes`.

## Supported harnesses:

- Claude Code
- Codex
- Gemini
- OpenCode
- Pi

## Development

Run tests:

```sh
mise run test
```

Format Rust code:

```sh
cargo fmt
```

Main source layout:

```text
src/profile/       profile schema, validation, inspection, storage
src/harness/       shared harness mechanics, backup, rollback, drift
src/integrations/  Coding Agents' integrations
src/app/           UI-independent workflows
src/cli/           terminal parsing, prompts, and rendering
docs/              documentation
```

Read `docs/ARCHITECTURE.md` and `docs/INTEGRATION.md` before changing behavior.
