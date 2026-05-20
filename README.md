# lazyagents

Manage reusable Agent profiles across various coding agents.

`lazyagents` lets you save a configured agent setup once, then apply it to one or more local coding harnesses. A profile can contain shared instructions, skills, saved prompts, MCP servers, model preferences, and permission preferences. Harnesses apply the parts they natively support.

## Install

Install the latest release:

```sh
curl -fsSL https://raw.githubusercontent.com/delegence/lazyagents/main/install.sh | sh
```

Or inspect the installer first:

```sh
curl -fsSLO https://raw.githubusercontent.com/delegence/lazyagents/main/install.sh
sh install.sh
```

Install a specific version:

```sh
curl -fsSL https://raw.githubusercontent.com/delegence/lazyagents/main/install.sh | LAZYAGENTS_VERSION=0.1.0 sh
```

Build from source:

```sh
cargo build --release
```

Run directly during development:

```sh
cargo run -- <command>
```

For example:

```sh
cargo run -- doctor
```

## Releasing

Release automation lives in `.github/workflows/release.yml` and runs when a `v*` tag is pushed. The workflow file must already be committed before the tag is created.

For the first release, commit the release infrastructure, then run:

```sh
make release VERSION=0.1.0
```

For later releases, choose the next version and run:

```sh
make release VERSION=0.1.1
```

The release target updates `Cargo.toml`, runs `cargo test`, commits `Cargo.toml` and `Cargo.lock` if they changed, creates an annotated tag, pushes the current branch, and pushes the tag.

## Commands

```sh
lazyagents doctor
lazyagents create <name>
lazyagents create <name> --harness <harness-id>
lazyagents create <name> -H <harness-id>
lazyagents show <name>
lazyagents edit <name>
lazyagents delete <name> [--yes]
lazyagents settings reset [--yes]
lazyagents use <name> --harness <harness-id>
lazyagents use <name> -H <harness-id>
lazyagents use <name> --all
```

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

If the file is missing, `lazyagents` creates it with the default Claude, Codex, Gemini, OpenCode, and Pi instances. Each instance has an id, a harness `type`, an optional `displayName`, an optional `binary`, and a `configDir`. Instance ids use lowercase ASCII letters, numbers, and dashes. `configDir` must be absolute, `~`, or begin with `~/`.

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

Doctor reports shared config directories using the same normalized type and `configDir` identity used for active-profile aliasing.

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
4. Writes profile instructions into the harness config and symlinks supported skills and commands.
5. Renders neutral sub-agent definitions into native harness agent files when supported.
6. Patches native model, permission, and MCP settings when supported by the harness.
7. Verifies the result.
8. Updates `~/.lazyagents/state.json` only after success.

If apply, verification, or state saving fails, the harness is rolled back from the latest backup.

## Importing Existing Setup

Create a profile from an existing harness:

```sh
lazyagents create work -H codex
```

Import copies the current managed harness state into a self-contained profile. Symlinks are dereferenced. Valid shared skills from `~/.agents/skills` are also imported unless a harness-native skill with the same name already exists.

Imported shared skills are removed from `~/.agents/skills` after a successful import. Invalid entries and hidden files are left alone.

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

Saving drift imports the same shared skills as `create --harness`: valid skills from `~/.agents/skills` are copied into the active profile unless a harness-native skill with the same name already exists, then the imported shared skill is removed from `~/.agents/skills`.

Or discard it:

```sh
lazyagents use home --harness claude --discard-changes
```

For `--all`, drift can only be discarded or cancelled.

Hidden files and directories starting with `.` inside managed folders are ignored. They do not trigger drift, are not backed up, and are not cleared during profile use.

## Sub-agent Format

`agents/*.md` uses neutral Markdown with YAML frontmatter. The Markdown body is the sub-agent prompt/instructions. Empty bodies are allowed for harness-native agents that intentionally carry all behavior in frontmatter:

```md
---
name: reviewer
description: Reviews changes for correctness, security, regressions, and missing tests.
model:
  default: inherit
  codex: gpt-5.4
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
      "TOKEN": "$TOKEN"
    }
  }
]
```

Supported transports:

- `stdio`
- `http`

Disabled MCP entries are validated and emitted to harness configs as disabled entries for harnesses with native MCP support. Harnesses without native MCP support ignore `mcps.json` and preserve it during imports/save-changes.

HTTP MCP `url` values must start with `http://` or `https://`. Environment variable references are preserved; for Codex HTTP headers, values like `"$TOKEN"` are rendered into Codex `env_http_headers`.

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
---

# Shared instructions

Work carefully and explain important tradeoffs.
```

Missing model or permission entries behave like `"default"`, which means `lazyagents` leaves that native harness setting unchanged.

Some harnesses can only serialize string model or permission preferences. Pi model preferences may be written as `provider/model`; lazyagents maps that to Pi's provider and model settings while a plain model value updates only the model.

## Operational Notes

Mutating commands take an exclusive lock in `~/.lazyagents/.lock`, so a second mutating command fails while another one is running.

`edit` opens the profile directory with `$EDITOR` when it is set; otherwise it prints the path.

`delete` refuses to remove an active profile, including profiles still referenced by lazyagents state or by symlinks in managed harness config.

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
cargo test
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
