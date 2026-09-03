# lazyagents

Turn any folder into an AI agent. Describe its role, choose a harness, and start chatting.

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
cargo build --release --locked
```

Run from source:

```sh
cargo run -- <command>
```

## Supported harnesses

- Codex
- Claude Code
- OpenCode
- Pi

## Requirements

- One supported harness installed and available on `PATH`
- Node.js 22.19 or newer and npm for Codex, Claude Code, and Pi
- Rust when building from source

OpenCode has a built-in ACP server and does not need Node.js or an npm adapter.

## Build

```sh
cargo build --release --locked
cp target/release/lazyagents ~/.local/bin/lazyagents
```

## Create an agent

```sh
mkdir researcher
cd researcher
lazyagents init
```

The interactive wizard selects an installed harness, asks for a name and description, and then selects the model and thinking level exposed by that harness. It generates `.agents/SOUL.md` and creates a launcher from the normalized name. Use the arrow keys and Enter to select an option. Escape cancels setup. For example, `Research Agent` creates `./research-agent`.

An existing harness login is reused. If it is missing or expired, the wizard asks before it starts the native login command. Pi has no general login command, so sign in with Pi before you create the agent.

The wizard can also create `~/.local/bin/research-agent` as a symbolic link. The launcher in the agent folder stays the source of truth.

Running `lazyagents` without a command shows help.

## Files

```text
research-agent              shell shortcut for `lazyagents chat`
workspace/                  agent working directory
.agents/
  .gitignore                ignores private and generated agent state
  SOUL.md                   editable system prompt
  agent.json                agent configuration
  mcps.json                 agent-specific MCP servers
  skills/<name>/SKILL.md    agent-specific native skills
  runtime/
    <harness>/              isolated harness files, when required
    acp-<harness>/          pinned ACP adapter, when required
  sessions/*.jsonl          local chat event logs
```

The generated `.agents/.gitignore` ignores `agent.json`, `SOUL.md`, `mcps.json`, `runtime/`, and `sessions/`. Agent skills remain available for version control.

Each launch reads the current `SOUL.md` and adds the `workspace/` instruction and current date and time. A normal chat starts a new ACP session. The `--resume` option resumes the latest session. The tool has no special logic for `AGENTS.md` or `CLAUDE.md`, but a harness can still read its native project files.

## MCP servers

`.agents/mcps.json` is a JSON array. It is empty by default.

```json
[
  {
    "name": "local-tools",
    "transport": "stdio",
    "command": "local-tools",
    "args": ["serve"],
    "env": { "MODE": "agent" }
  },
  {
    "name": "remote-tools",
    "transport": "http",
    "url": "https://example.com/mcp",
    "headers": { "Authorization": "Bearer token" },
    "enabled": true
  }
]
```

Set `"enabled": false` to keep a server in the file without loading it. ACP adapter support determines which transports are available.

## Skills

Put each skill at `.agents/skills/<skill-name>/SKILL.md`. Codex, Claude Code, OpenCode, and Pi recognize this project skill location. Skill discovery and loading stay native to the selected harness.

## Add a harness

Harness support lives behind the `Harness` trait in `src/harness/mod.rs`. Shared code handles detection, ACP adapter installation, registry lookup, and process startup.

Each harness owns one file:

```text
src/harness/codex.rs
src/harness/claude.rs
src/harness/opencode.rs
src/harness/pi.rs
```

To add another harness:

1. Create `src/harness/<id>.rs` and implement `Harness`.
2. Declare its runtime command, pinned ACP adapter, authentication commands, and launch options in that file.
3. Add the implementation to `HARNESSES` in `src/harness/mod.rs`.

No config or session-log changes are required. `agent.json` stores the harness registry ID as a string.

## Repair

If an ACP adapter is missing or damaged, run:

```sh
lazyagents repair
```

This checks that the selected harness is on `PATH` and reinstalls its pinned ACP adapter when required.

## Chat

Chat with the agent in the current folder:

```sh
lazyagents chat
```

Or run its local launcher:

```sh
./research-agent
```

Both use the same agent configuration. Without `--resume`, each command starts a new ACP session. The local launcher changes to its agent folder and runs the `lazyagents` binary found on `PATH`. A global launcher symlink works from any current directory.

Resume the most recent session with either form:

```sh
lazyagents chat -r
./research-agent -r
```

Lazyagents shows the permission choices sent by the harness. It does not keep its own permission history. The harness decides whether an "always allow" choice persists. Use `/exit`, `/quit`, Control-C, or Control-D to leave. Reasoning, tool calls, tool results, permissions, and messages are saved in session JSONL and shown again when you resume.
