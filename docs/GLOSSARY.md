# Ubiquitous Language

## Profile domain

| Term | Definition | Aliases to avoid |
| --- | --- | --- |
| **Profile** | A named, reusable bundle of agent-facing configuration stored under the lazyagents home. | Agent, workspace, state |
| **Profile Name** | The CLI-safe folder name that identifies a **Profile**, restricted to letters, numbers, and dash. | Name, profile id, folder name |
| **Display Name** | Optional human-friendly profile metadata stored in profile config. | Name, profile name |
| **Profile Skeleton** | The standard files and directories created for a new profile. | Template, scaffold |
| **Lazyagents Home** | The global root directory where lazyagents stores profiles, state, and backups. | Global config, home, root |
| **Profile Artifact** | A file or directory inside a **Profile** that can be applied to a **Harness**. | Asset, resource, config item |
| **Instruction Source** | The profile's universal instruction file used as the source for harness-specific instruction files. | AGENTS.md, system prompt, instructions |
| **Skill** | A valid profile skill directory containing a `SKILL.md` file. | Plugin, capability, tool |
| **Profile Command** | A Markdown saved prompt file under the profile commands directory. | Prompt, saved prompt, slash command, command |
| **MCP Definitions File** | The profile's neutral MCP file, `mcps.json`, containing zero or more **MCP Server Definitions**. | MCP config, MCP list |
| **MCP Server Definition** | One neutral MCP server entry inside the **MCP Definitions File**. | MCP server, server config |
| **Profile Config** | The required profile configuration file containing metadata, model preferences, and permission preferences. | Config, settings |

## Harness domain

| Term | Definition | Aliases to avoid |
| --- | --- | --- |
| **Harness** | A supported coding agent runtime whose global configuration can be managed by lazyagents. | Agent, coding agent, tool |
| **Supported Harness** | A harness for which lazyagents has a **Harness Integration**, such as Codex, Claude Code, or opencode. | Known harness |
| **Detected Harness** | A supported harness whose binary is found on PATH by its **Harness Integration**. | Installed harness, available harness |
| **Harness Config** | The native global configuration files and directories used by a **Harness**. | Global config, harness state |
| **Harness Integration** | The harness-specific boundary that detects, imports, applies, patches, and checks drift for one **Harness**. | Implementation, harness file, bridge |
| **Managed Surface** | A harness config area that a **Harness Integration** declares lazyagents owns during profile use. | Managed files, target files, surface |
| **Instruction Target** | The harness-specific instruction file linked to the profile **Instruction Source**. | Instruction file, AGENTS.md, CLAUDE.md |
| **Native Config File** | A harness-specific settings file patched by a **Harness Integration**. | Config file, settings file, harness config |
| **MCP List** | The complete native set of MCP servers owned by lazyagents for a harness. | MCP section, MCP servers |
| **Harness Registry** | The app-layer catalog of supported built-in harness integrations used to resolve one or all harnesses for workflows. | Catalog, integration list |
| **Integration Test Suite** | Shared test-only behavior checks that each concrete harness integration must pass. | Test template, integration tests |

## Profile use lifecycle

| Term | Definition | Aliases to avoid |
| --- | --- | --- |
| **Profile Use** | The operation that applies a **Profile** to one or more detected harnesses. | Apply, switch, use |
| **Active Profile** | The profile recorded in lazyagents state as last successfully used for a harness. | Current profile, selected profile |
| **Import** | The operation that copies current harness managed state into a profile without mutating the harness. | Capture, sync from, save |
| **Save Changes** | Drift handling that imports the current harness managed state into the **Active Profile** before using another profile. | Save drift, sync changes |
| **Discard Changes** | Drift handling that ignores current harness drift and proceeds with profile use. | Proceed without saving, ignore changes |
| **Cancel** | Drift handling that aborts the pending use operation before mutation. | Abort |
| **Normalize** | The operation that creates missing optional profile artifacts before use. | Repair, initialize |
| **Status** | A read-only summary of detected harnesses, last active profiles, and lightweight drift state. | State, current state |
| **Drift** | A difference between a harness managed surface and what its **Active Profile** expects. | Unsaved changes, mismatch |
| **Backup** | The latest per-harness copy of managed surfaces captured immediately before use. | Snapshot, restore point |
| **Rollback** | The internal restoration of a harness from its **Backup** after a failed use. | Restore, revert |
| **State** | Lazyagents metadata recording the last successful profile use per harness. | state.json, active record |
| **App Workflow** | A UI-independent product operation, such as create profile, delete profile, inspect profile, doctor, or profile use. | CLI command logic, controller |

## Source layers

| Layer | Responsibility |
| --- | --- |
| `profile/` | Profile names, profile config, neutral MCP parsing, validation, inspection, skeleton creation, and profile filesystem storage. |
| `harness/` | Generic harness primitives and mechanics: harness identity, integration trait, managed surfaces, drift report types, artifact helpers, transactional apply, backup/rollback, symlink helpers, and atomic writes. |
| `integrations/` | Concrete Codex, Claude Code, and OpenCode implementations plus shared test-only integration behavior checks. |
| `app/` | UI-independent product workflows, active state persistence, doctor report assembly, profile use decisions, and built-in harness registry composition. |
| `cli/` | Terminal UI adapter: argument parsing, prompts, rendering, and launching `$EDITOR`. |

Dependency direction:

- `profile/` and production `harness/` are lower-level modules and do not depend on `app/`, `cli/`, or concrete `integrations/`.
- `integrations/` implement the harness integration contract using `profile/` and `harness/`.
- `app/` composes profile, harness, and integrations into product workflows.
- `cli/` renders app results and handles terminal-specific input/output.

## Configuration concepts

| Term | Definition | Aliases to avoid |
| --- | --- | --- |
| **Model Preference** | An opaque profile config value that a **Harness Integration** can apply to a harness model setting. Current v1 harnesses use strings. | Model, model choice |
| **Permission Preference** | An opaque profile config value that a **Harness Integration** can apply to a harness permission setting. Values may be strings or structured JSON when the harness supports it. | Permissions, permission level |
| **Default Value** | The string `"default"`, meaning lazyagents leaves that model or permission setting untouched. | Default sentinel, fallback |
| **MCP Server** | One named MCP endpoint or process after profile definitions are parsed or imported. | Server, tool server |
| **MCP Name** | The stable machine identity of an **MCP Server**, restricted to letters, numbers, dash, and underscore. | Server name, display name |
| **Stdio MCP** | An MCP server launched as a local command with optional args and env. | Local MCP, process MCP |
| **HTTP MCP** | An MCP server reached through a URL with optional headers. | Remote MCP |
| **Disabled MCP** | An MCP server retained in the profile but not emitted into harness config. | Inactive MCP |
| **Environment Variable Reference** | A placeholder string for a secret or value that the harness resolves from its environment. | Secret, env var, environment reference |

## Relationships

- A **Profile** has exactly one **Profile Name**.
- A **Profile** has exactly one **Profile Config**.
- A **Profile** has zero or one **Display Name**.
- A **Profile** has exactly one **Instruction Source** after normalization.
- A **Profile** has zero or more **Skills**.
- A **Profile** has zero or more **Profile Commands**.
- A **Profile** has zero or more **MCP Servers**.
- A **Lazyagents Home** contains zero or more **Profiles**.
- A **Harness** has exactly one **Harness Integration**.
- A **Harness Integration** declares one or more **Managed Surfaces** for its **Harness**.
- A **Profile Use** targets either exactly one **Harness** or all **Detected Harnesses**.
- A **Harness** has zero or one **Active Profile** in **State**.
- A **Backup** belongs to exactly one **Harness**.
- A **Rollback** uses exactly one **Backup**.
- A **Save Changes** operation imports from exactly one **Harness** into that harness's **Active Profile**.
- A **Default Value** applies only to **Model Preference** and **Permission Preference** values.
- An **MCP Definitions File** produces the full native **MCP List** for a harness.

## Example dialogue

> **Dev:** "When the user runs `lazyagents use work --harness codex`, are we switching an **Agent**?"
> **Domain expert:** "No. The canonical term is **Profile**. Codex is the **Harness**, and `work` is the **Profile** being used."
>
> **Dev:** "So Codex's `AGENTS.md` becomes the **Instruction Source**?"
> **Domain expert:** "No. The profile owns the **Instruction Source**. Codex gets an **Instruction Target** symlinked to that source."
>
> **Dev:** "If Codex has a new skill that is not in the **Active Profile**, is that **Drift**?"
> **Domain expert:** "Yes. On single-harness **Profile Use**, the user can **Save Changes**, **Discard Changes**, or **Cancel**."
>
> **Dev:** "Does **Save Changes** update every harness entry in the **Profile Config**?"
> **Domain expert:** "No. It imports from one **Harness** and updates only that harness's model and permission preferences."
>
> **Dev:** "If applying the new **Profile** fails, do we expose a restore command?"
> **Domain expert:** "No. **Rollback** is internal and restores the harness from its latest **Backup** automatically."

## Flagged ambiguities

- "agent" was used to mean both a saved configuration bundle and an external coding runtime. Use **Profile** for the saved bundle and **Harness** for Codex, Claude Code, opencode, or similar runtimes.
- "name" was used for both a profile folder identifier and human-friendly metadata. Use **Profile Name** for the folder-derived identity and **Display Name** for optional metadata.
- "apply" and "switch" were discussed as command names. Use **Profile Use** for the canonical operation because the CLI command is `lazyagents use`.
- "prompt", "saved prompt", "slash command", and "command" referred to the same profile artifact. Use **Profile Command** for Markdown saved prompt files.
- "config" was used for both profile-level settings and harness-native files. Use **Profile Config** for `config.json`, **Native Config File** for a patched harness settings file, and **Harness Config** for the broader harness-owned file tree.
- "MCP config", "MCP list", and "MCP servers" were used interchangeably. Use **MCP Definitions File** for `mcps.json`, **MCP Server Definition** for one profile entry, and **MCP List** for the harness-integration-owned native output.
- "default" could mean a fallback value or preserving the current harness setting. Use **Default Value** specifically for the string `"default"`, which means lazyagents leaves the setting untouched and does not create the native key during apply.
- "backup", "rollback", "restore", and "revert" overlapped. Use **Backup** for the saved managed state and **Rollback** for the internal failed-use recovery operation.
