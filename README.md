# lazyagents

CLI to manage Agent Profiles across Claude Code, Codex, OpenCode and other harnesses.

## Usage

```sh
lazyagents help
lazyagents doctor
lazyagents create <name>
lazyagents create <name> --from <claude|codex|opencode>
lazyagents show <name>
lazyagents edit <name>
lazyagents delete <name>
lazyagents use <name> --harness <claude|codex|opencode>
lazyagents use <name> --all
```

## How it works?

A configured coding harness is effectively an Agent Profile: instructions + skills + MCPs + saved prompts + other configurations. lazyagents enables to save these agent states as reusable profiles, then quickly use a chosen profile across one harness or all installed supported harnesses without manually copying files or losing changes.

A profile is stored under `~/.lazyagents/profiles/<name>` and contains:

- `AGENTS.md` agent instructions
- `skills/` agent skills
- `commands/` saved prompts
- `mcps.json` MCP servers
- `config.json` per-harness model and permission settings

Applying a profile symlinks instructions, valid skills, and markdown commands into the target harness, then patches native settings and MCP config. The active profile per harness is tracked in `state.json`.

Supported agent harnesses:

- Claude
- Codex
- OpenCode

Before replacing a harness config, `lazyagents` creates a backup under `backups/<harness>` and rolls back if apply, verification, or state saving fails.

When switching away from a previously active profile, `lazyagents` checks for drift in instruction links, skills, commands, MCP definitions, and native config. Hidden files or directories (starting with `.`) inside managed folders like `skills/` or `commands/` are completely ignored (they do not trigger drift, they are not backed up, and they are not cleared when applying a profile). Use `--save-changes` to import drift back into the active profile, or `--discard-changes` to overwrite it.
