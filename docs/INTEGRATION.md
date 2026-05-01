# Adding a Harness Integration

This document is for maintainers and coding agents adding support for another coding-agent harness.

The goal is that a new harness is added as one concrete integration file plus small registration updates. Product workflows should not gain harness-specific branches beyond identity and registration.

## Layer Rules

Keep the dependency direction intact:

- `src/profile/` owns profile names, config, neutral MCP parsing, validation, summaries, and profile filesystem storage.
- `src/harness/` owns generic harness primitives and mechanics: `HarnessKind`, `HarnessIntegration`, config paths, managed surfaces, drift report types, artifact helpers, transactional apply, backup/rollback, symlink helpers, and atomic writes.
- `src/integrations/` owns concrete harness implementations. Put one harness per file.
- `src/app/` owns product workflows and composition, including the built-in harness registry.
- `src/cli/` owns terminal-specific parsing, prompts, rendering, and `$EDITOR` execution.

Production `profile/` and `harness/` code must not depend on `app/`, `cli/`, or concrete integrations. Production integration code should depend on `profile/` and `harness/`, not on `app/` or `cli/`.

## Files To Touch

For a new harness named `Example`, expect to update:

```text
src/harness/kind.rs
src/integrations/example.rs
src/integrations/mod.rs
src/app/harness_registry.rs
docs/ARCHITECTURE.md
README.md
```

If the harness needs reusable filesystem or artifact logic, add it to `src/harness/artifacts.rs` or `src/harness/fs.rs` only when it is genuinely shared.

## Implementation Steps

1. Add a `HarnessKind` variant.

Update `src/harness/kind.rs`:

- add the enum variant
- add `id()`
- add `display_name()`
- add `binary_name()`

The `id()` value is the stable lazyagents harness id used in profile config and state serialization. Keep it lowercase and CLI-friendly.

2. Do not add a CLI harness enum.

The CLI accepts harness ids as strings and resolves them through `HarnessRegistry`. Do not add a second CLI-only harness enum, and do not add harness-specific CLI parsing branches.

3. Create `src/integrations/<harness>.rs`.

Implement:

```rust
pub struct ExampleIntegration;

impl HarnessIntegration for ExampleIntegration {
    fn kind(&self) -> HarnessKind { ... }
    fn supports_skills(&self) -> bool { ... } // optional; defaults to true
    fn supports_commands(&self) -> bool { ... } // optional; defaults to true
    fn supports_mcp(&self) -> bool { ... } // optional; defaults to true
    fn detect(&self, env: &AppEnvironment) -> Result<HarnessDetection> { ... }
    fn paths(&self, env: &AppEnvironment) -> Result<HarnessConfigPaths> { ... }
    fn managed_surfaces(&self, paths: &HarnessConfigPaths) -> Vec<ManagedSurface> { ... }
    fn preflight(&self, profile: &ProfileRef) -> Result<()> { ... }
    fn detect_drift(&self, active: &ProfileRef, paths: &HarnessConfigPaths) -> Result<DriftReport> { ... }
    fn import_from_harness(&self, paths: &HarnessConfigPaths) -> Result<ProfileImport> { ... }
    fn apply(&self, profile: &ProfileRef, paths: &HarnessConfigPaths) -> Result<()> { ... }
    fn verify(&self, profile: &ProfileRef, paths: &HarnessConfigPaths) -> Result<()> { ... }
}
```

Use existing integrations as models:

- `src/integrations/codex.rs` for TOML config and flat command handling
- `src/integrations/claude.rs` for JSON config and global MCP file handling
- `src/integrations/opencode.rs` for JSON config and nested command handling

4. Register the module.

Update `src/integrations/mod.rs`:

```rust
pub mod example;
```

5. Register the built-in integration.

Update `src/app/harness_registry.rs`:

- import `example::ExampleIntegration`
- add `Box::new(ExampleIntegration)` to `BuiltInHarnessRegistry::all()`
- update the registry test expected order

The built-in harness list lives in `app/` because it is product composition, not low-level harness mechanics.

6. Add shared integration test coverage.

In the new integration file, add a test adapter that implements:

```rust
crate::integrations::test_suite::template::HarnessTestAdapter
```

Then invoke:

```rust
crate::define_standard_harness_tests!(ExampleAdapter);
```

The shared suite checks normalization, stale surface clearing, default preference behavior, optional skill/command/MCP behavior, rollback, import, save/discard drift, apply/state updates, and nested command behavior.

The adapter must define harness-specific test setup and assertions for:

- whether skills, commands, and native MCP are supported, and clear behavior when they are
- malformed native config
- whether nested commands are supported
- native settings preservation
- import config extraction
- saved drift preferences
- applied native config

7. Add focused harness-specific tests when needed.

Use extra tests for behavior not covered by the shared suite, especially:

- native MCP shape quirks
- config patch preservation
- env var or secret syntax translation
- model/permission mapping edge cases
- unsupported native feature errors

8. Update docs.

Update:

- `docs/ARCHITECTURE.md` harness paths, native mappings, and architectural notes
- `README.md` supported harness list
- this file if the integration workflow changes

Do not leave docs saying only Codex, Claude Code, and OpenCode are supported if the new harness is registered.

## Method Responsibilities

### `kind`

Return the new `HarnessKind` variant.

### `supports_skills`, `supports_commands`, and `supports_mcp`

Return `false` for any profile artifact type the harness cannot represent. Unsupported artifact types are ignored by the integration: do not apply, verify, drift-check, import, or clear them. The defaults are `true` for existing harnesses.

For MCP specifically, returning `false` also tells app-layer drift setup not to validate the active profile's `mcps.json` for this harness.

### `detect`

Use `harness::fs::detect_binary(env, self.kind().binary_name())` unless the harness has a stronger detection rule.

Detection must use `AppEnvironment.path_entries`, not shell commands.

### `paths`

Return all native paths derived from `env.user_home`. Do not read public path override env vars in v1.

Use:

- `config_dir` for the harness config root
- `instruction_target` for the harness-native instruction file
- `skills_dir` for managed skill directory contents, when supported
- `commands_dir` for managed command file contents, when supported
- `settings_file` for native model/permission config
- `mcp_file` for the native MCP config location, when the harness has one

`settings_file` and `mcp_file` may be the same path. If the harness does not support skills, commands, or MCP, set the corresponding path to a harmless config path such as `config_dir`, do not include it as a managed surface for that artifact type, and ignore that artifact type in the integration methods.

### `managed_surfaces`

Declare every file or directory lazyagents owns during profile use.

Use:

- `ManagedSurface::file` for instruction targets and fully-owned files
- `ManagedSurface::directory` for managed directory contents
- `ManagedSurface::preserved_file` for native config files that must be backed up but patched rather than deleted

Do not include auth, logs, caches, plugins, or unrelated harness data.

### `preflight`

Fail before mutation when the profile cannot be represented by the harness.

Common example: if the harness only supports flat commands, call `flat_profile_commands(&profile.path)` so nested commands fail before backup/apply.

### `detect_drift`

Compare the active profile to current harness managed surfaces and return `DriftReport`.

Drift should include:

- instruction target not linked to active profile instruction source
- skill set mismatch, when skills are supported
- command set mismatch, when commands are supported
- MCP differences, when native MCP is supported
- managed surface damage

Model and permission differences should not trigger drift prompts.

Use shared helpers from `src/harness/artifacts.rs` when possible.

### `import_from_harness`

Read current harness managed state into a `ProfileImport` without mutating the harness.

Rules:

- dereference imported symlinks
- import only valid skill directories, when skills are supported
- import Markdown command files, when commands are supported
- preserve nested command paths when the harness supports them
- parse malformed native config as an error
- import native model/permission values when present
- use `ImportedPreference::default_value()` when the native key is absent
- produce neutral `mcps.json` text for `mcp_definitions` when the harness supports native MCP; otherwise return `None` so existing profile MCP definitions are preserved

### `apply`

Apply a profile to the harness after shared transaction code has captured backup and cleared managed surfaces.

Rules:

- create missing config directories
- symlink profile-owned instructions and supported valid skills/command files with absolute symlinks
- patch native config files, preserving unrelated keys
- translate neutral MCP definitions into native format, when the harness supports native MCP
- honor `"default"` model/permission values by not mutating those native keys
- write native config atomically with `write_text_atomic`

Do not update lazyagents state here. State is app-layer behavior.

### `verify`

Check that the managed harness state now matches the profile.

Verification failures trigger rollback. Keep errors clear and path-specific.

## Artifact Support And MCP Rules

Skill, command, and MCP support are optional per harness. If the harness has no native support for one of these artifact types, the integration should not apply, verify, drift-check, import, or clear it. Returning `mcp_definitions: None` from `import_from_harness` preserves existing profile MCPs during `--save-changes` and `create --from`.

For harnesses with native MCP support, use `crate::profile::mcp::read_mcp_definitions` to parse profile MCP definitions.

Current neutral transports:

- `stdio`
- `http`

Disabled MCP entries are validated and emitted to native config as disabled entries. If a harness supports native MCP but cannot represent a specific MCP definition, apply must fail so rollback can restore the previous harness state.

## Native Config Rules

Patch native config files instead of replacing them wholesale. Preserve unrelated settings.

Model and permission values are opaque profile values. Do not validate model names or permission modes unless the harness cannot serialize the value shape at all.

The string `"default"` means lazyagents leaves that setting untouched and must not create the native key.

## Transaction And State Rules

Do not implement backup, rollback, or active state updates inside integrations.

The flow is:

```text
app/use_profile.rs
  handles drift decisions and active state

harness/apply.rs
  captures backup
  clears managed surfaces
  calls integration.apply
  calls integration.verify
  commits app-provided state update
  rolls back on apply, verify, or state-save failure
```

If apply, verify, or state save fails, lazyagents must restore the previous managed harness state and leave active state unchanged.

## Testing Checklist

Run:

```sh
cargo fmt
cargo test
```

Before considering the integration done, confirm:

- explicit `use --harness <new>` fails when the binary is not detected
- `use --all` applies only when the harness is detected
- `create --from <new>` imports without mutating harness state
- applying an empty profile clears stale skills, commands, and supported MCPs
- applying invalid MCP config fails without updating state for harnesses that support MCP, and is ignored by harnesses that do not
- apply failure rolls back managed files and native config
- state is updated only after successful apply and verify
- `--save-changes` imports drift into the previous active profile
- `--discard-changes` does not modify the previous active profile
- `doctor` reports detected active harness state correctly
- docs list the new harness and native paths

## Common Mistakes

- Adding harness-specific branches to `app/use_profile.rs` or CLI instead of implementing the trait.
- Updating state from an integration.
- Replacing entire native config files and losing unrelated settings.
- Treating `"default"` as a literal native model or permission value.
- Letting nested commands partially apply on a harness that cannot represent them.
- Forgetting to register the integration in `BuiltInHarnessRegistry`.
- Adding a duplicate CLI harness enum instead of resolving harness strings through `HarnessRegistry`.
- Adding broad `#[allow(dead_code)]` instead of deleting stale code or gating test helpers with `#[cfg(test)]`.
