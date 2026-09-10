# lazyagents

Turn any folder into an AI agent.
Describe its role, choose a harness, and start chatting.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/delegence/lazyagents/main/install.sh | sh
```

### Supported harnesses

- Codex
- Claude Code
- OpenCode
- Pi

Requires an installed harness on `PATH`.
Codex, Claude Code, and Pi also require Node.js 22.19 or newer and npm for their ACP adapters. OpenCode has built-in ACP support.

## Create an agent

```sh
mkdir researcher
cd researcher
lazyagents init
lazyagents chat
```

Setup asks for a harness, name, description, model, and reasoning level. It installs the required adapter and generates `.agents/SOUL.md` automatically.

The agent name determines the launcher name. Setup can also add a launcher symlink in `~/.local/bin`. Launchers work from any directory and use the `lazyagents` binary on `PATH`.

Edit `.agents/SOUL.md` to change the agent's instructions. The agent uses `workspace/` as its working directory.

## Chat

```sh
lazyagents chat             # start a new session
lazyagents chat -r          # choose a previous session
lazyagents chat -r <id>     # resume a specific session
```

The agent launcher accepts the same options. The session picker shows the latest message and its local date and time, with recent sessions first.

Type `/` to see commands. Keep typing to filter and use the arrow keys to select. Press Tab to complete the selected command, or Enter to run it.

| Command | Action |
| --- | --- |
| `/model` | Select model and reasoning level; save them for new sessions |
| `/permissions` | Select and save `ask for approval` or `full access` |
| `/new` | Start an empty conversation with the current settings |
| `/status` | Show session settings and usage reported by the harness |
| `/quit` | Exit chat |

Unsent input is saved per agent and restored on the next launch. Tool output is shortened in chat; complete events are saved in `.agents/sessions/`.

## Configuration

Settings live in `.agents/agent.json`. Setup writes the initial configuration, and chat commands save accepted changes.

| Field | Purpose |
| --- | --- |
| `name`, `slug`, `description` | Agent identity and launcher name |
| `harness` | `codex`, `claude`, `opencode`, or `pi` |
| `model`, `thinking` | Default model and reasoning level for new sessions |
| `permission_mode` | `ask` or `full_access` |
| `permissions` | Permanent tool approvals |
| `idle_timeout` | Seconds without harness activity before cancelling a turn; defaults to `300` |
| `color` | Header name color, chosen during setup; defaults to cyan if absent |
| `draft` | Unsent input, managed by chat |

`idle_timeout` must be a positive integer. It applies to an active turn, not time spent at the input prompt.

Available colors: `red`, `green`, `yellow`, `blue`, `magenta`, `cyan`, `orange`, `coral`, `pink`, `rose`, `violet`, `lavender`, `purple`, `teal`, `turquoise`, `mint`, `lime`, `gold`, `amber`, `sky`.

### Permissions

Ask mode prompts when a request has no saved approval. Permanent approvals are stored per agent and harness in `agent.json`. Session-only approvals stay with the harness.

Full access automatically approves tool permission requests. It does not change the harness sandbox.

## Agent files

```text
researcher                  launcher
workspace/                  agent working directory
.agents/
  SOUL.md                   editable system prompt
  agent.json                configuration
  mcps.json                 MCP servers
  skills/<name>/SKILL.md     agent skills
  runtime/                  private harness files and ACP adapters
  sessions/*.jsonl           chat event logs
```

The generated `.agents/.gitignore` excludes configuration, the system prompt, MCP settings, runtime files, and session logs. Skills remain available for version control.

## MCP servers and skills

Add MCP servers to `.agents/mcps.json`, which contains an empty array by default:

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
    "headers": { "Authorization": "Bearer token" }
  }
]
```

Set `"enabled": false` to disable a server without removing it. Transport support depends on the harness adapter.

Put each skill at `.agents/skills/<name>/SKILL.md`. The selected harness handles skill discovery and loading.

## Troubleshooting

Run `lazyagents repair` in the agent folder to reinstall its pinned ACP adapter. Interactive chat offers to install a missing adapter; for scripts, run repair first.

## Pipes

Piped chat sends assistant output to stdout and diagnostics to stderr. It does not save drafts or load input history. Failed turns and inactivity timeouts return a nonzero exit code. Permission requests that require an interactive choice are cancelled. Ctrl-C stops the turn.

## Development

```sh
cargo test --locked
cargo build --release --locked
```

Use `cargo run -- <command>` to run from source.

Harness implementations live in `src/harness/`. To add one, implement the `Harness` trait and register it in `HARNESSES` in `src/harness/mod.rs`.
