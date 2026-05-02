# lazyagents

Manage reusable Agent profiles across various coding agents.

`lazyagents` lets you save a configured agent setup once, then apply it to one or more local coding harnesses. A profile can contain shared instructions, skills, saved prompts, MCP servers, model preferences, and permission preferences. Harnesses apply the parts they natively support.

The tool is local-first. It manages files on your machine, does not install harnesses, and does not fetch remote skills or plugins.

## Install

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

## Commands

```sh
lazyagents help
lazyagents doctor
lazyagents create <name>
lazyagents create <name> --from <claude|codex|gemini|opencode|pi>
lazyagents show <name>
lazyagents edit <name>
lazyagents delete <name> [--yes]
lazyagents use <name> --harness <claude|codex|gemini|opencode|pi>
lazyagents use <name> --all
```

Drift handling:

```sh
lazyagents use <name> --harness codex --save-changes
lazyagents use <name> --harness codex --discard-changes
lazyagents use <name> --all --discard-changes
```

`use` always requires an explicit target: either `--harness <id>` or `--all`.

## What Is A Profile?

A profile is stored under:

```text
~/.lazyagents/profiles/<name>/
```

Profile layout:

```text
AGENTS.md      shared agent instructions
skills/        skill directories containing SKILL.md
commands/      Markdown saved prompts
mcps.json      neutral MCP server definitions
config.json    model and permission preferences
```

Profile names are CLI-safe identifiers: ASCII letters, numbers, and dashes only.

## How It Works

When you apply a profile, `lazyagents`:

1. Checks the selected harness is available on `PATH`.
2. Checks whether the currently active profile has unsaved drift.
3. Creates a backup of the harness-managed files.
4. Symlinks profile instructions, skills, and commands into the harness config.
5. Patches native model, permission, and MCP settings when supported by the harness.
6. Verifies the result.
7. Updates `~/.lazyagents/state.json` only after success.

If apply, verification, or state saving fails, the harness is rolled back from the latest backup.

## Importing Existing Setup

Create a profile from an existing harness:

```sh
lazyagents create work --from codex
```

Import copies the current managed harness state into a self-contained profile. Symlinks are dereferenced. Valid shared skills from `~/.agents/skills` are also imported unless a harness-native skill with the same name already exists.

Imported shared skills are removed from `~/.agents/skills` after a successful import. Invalid entries and hidden files are left alone.

## Drift

Drift means the current harness-managed files no longer match the active profile. This can happen if you edit a harness config directly after using a profile.

Drift checks include:

- instruction links
- skills
- commands
- MCP definitions, for harnesses with native MCP support
- managed directory damage

Model and permission differences do not block switching.

For one harness, you can save drift back into the active profile:

```sh
lazyagents use home --harness claude --save-changes
```

Or discard it:

```sh
lazyagents use home --harness claude --discard-changes
```

For `--all`, drift can only be discarded or cancelled.

Hidden files and directories starting with `.` inside managed folders are ignored. They do not trigger drift, are not backed up, and are not cleared during profile use.

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

## Configuration

`config.json` stores opaque per-harness preferences:

```json
{
  "name": "Work",
  "description": "",
  "models": {
    "codex": "gpt-5.2"
  },
  "permissions": {
    "codex": "on-request"
  }
}
```

Missing model or permission entries behave like `"default"`, which means `lazyagents` leaves that native harness setting unchanged.

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
