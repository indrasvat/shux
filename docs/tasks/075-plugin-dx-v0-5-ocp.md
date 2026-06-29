# Task 075: Plugin DX v0.5 and OCP Extraction

**Status:** Done
**Priority:** High
**Milestone:** M2
**Depends On:** 044a
**Touches:** `crates/shux/src/cli.rs`, `crates/shux/src/main.rs`, `crates/shux/src/features/plugin/`, `crates/shux-plugin/`

---

## Problem

Process plugins work, but the authoring and lifecycle surface is still too
centralized and too manual. Adding plugin UX currently tends to touch large
central files such as `cli.rs` and `main.rs`, which makes future plugin
features less additive than they should be.

This task creates the first local-only plugin DX foundation without building a
remote registry or marketplace.

## Scope

Implement a narrow first pass:

- extract plugin-facing CLI/RPC dispatch into a focused plugin feature module,
- add a manifest validator for `shux-plugin.toml` local directory packages,
- preserve current direct executable `plugin install` behavior,
- add local scaffold authoring commands:
  - `shux plugin scaffold <path> --runtime sh`,
  - `shux plugin create <path> --runtime sh` as an alias,
  - `shux plugin init --runtime sh` for the current directory,
- define lifecycle wording so `plugin stop` is the durable UX name for current
  graceful shutdown/unregister behavior while `plugin kill` remains compatible,
- keep permission, grant, audit, runtime UUID, and plugin process supervision
  on the existing plugin manager path.

## Non-Goals

- No remote plugin registry.
- No Cloudflare package index.
- No archive package install.
- No plugin signing or trust store.
- No top-level `shux <plugin>` command fallback.
- No `plugin uninstall`; managed package metadata is deferred to a later
  registry/package milestone.
- No broad multi-runtime scaffold set beyond `sh`.
- No Sightline plugin implementation.
- No rewrite of command routing beyond the plugin feature boundary needed for
  this task.

## Lifecycle State Model

The implementation must not create a parallel runtime truth beside the existing
plugin manager.

| State | Meaning |
|---|---|
| scaffolded | Local source directory exists; Shux has not installed or started it. |
| installed | Existing daemon plugin manager spawned, handshook, and registered a runtime UUID. |
| running | Existing daemon plugin manager spawned, handshook, and registered a runtime UUID. |
| stopped | The runtime plugin instance has been gracefully shutdown and unregistered. |

Transition rules:

- Existing `plugin install <direct-executable>` remains spawn + handshake +
  register.
- Manifest-directory install resolves and validates package metadata, then uses
  the existing spawn + handshake + register path without forking permission or
  audit behavior.
- `plugin stop <name>` is a user-facing alias for the existing graceful
  shutdown/unregister path.
- `plugin kill <name>` remains compatible and byte-for-byte equivalent at the
  daemon RPC level for this milestone.

## Mandatory Process

- DootSabha design review must be saved before coding.
- Start on a feature branch.
- Update this task and `docs/PROGRESS.md` before implementation.
- Use red-green TDD for every new behavior.
- Run implementation-diff DootSabha review before push.
- Dogfood the lifecycle with a real Shux binary and leak guard, but do not
  commit visual screenshot evidence unless this task changes terminal rendering.

## Testing Matrix

| Layer | Required Evidence |
|---|---|
| Level 1 unit | Manifest validation rejects missing name/id/entrypoint, textual path escapes, symlink escapes, unsupported platform, built-in command aliases, and daemon-incompatible dotted runtime names. |
| Level 1 unit | Scaffold rendering creates the expected `shux-plugin.toml`, README, license, and executable `sh` entrypoint. |
| Level 1 unit | Scaffold refuses to overwrite non-empty directories without an explicit force path if that path exists. |
| Level 1 unit | Plugin command parsing proves `scaffold`, `create`, and `init` do not require daemon startup. |
| Level 2 integration | Existing direct executable `plugin install/list/kill` remains compatible. |
| Level 2 integration | `plugin stop` behaves like the graceful shutdown/unregister path and `plugin kill` remains compatible. |
| Level 2 integration | Local manifest-directory install validates package metadata and reports actionable errors. |
| Level 2 integration | Manifest-directory install canonicalizes entrypoints, defaults cwd to the package root, and rejects process handshake name/version drift. |
| Level 2 security | Existing default-deny permissions, grants, and audit invariants remain green. |
| Level 3 dogfood | Release-like Shux binary scaffolds, validates, installs, starts/stops, and captures evidence from a real Shux session through `.shux/scripts/no_leak_guard.sh`. |
| Level 3 QA | Leak-guarded dogfood proves scaffold/install/list/stop behavior against a real Shux binary without committing screenshot artifacts for this non-rendering task. |

## Acceptance Criteria

- [x] Plugin feature code is less centralized and new plugin subcommands are
  primarily additive inside the plugin feature module.
- [x] `plugin scaffold`, `plugin create`, and `plugin init` generate a runnable
  `sh` process plugin.
- [x] Generated plugin directories include a valid `shux-plugin.toml`.
- [x] Current direct executable plugin install/list/kill behavior is preserved.
- [x] `plugin stop` exists as lifecycle naming over the graceful shutdown path.
- [x] Manifest-directory validation is strict and path-safe.
- [x] Permission, grant, audit, and runtime UUID behavior are not forked.
- [x] Process cleanup is proven by leak-guarded smoke testing.
- [x] Top-level plugin command fallback is not implemented in this task.

## Definition of Done

- [x] DootSabha design review findings are incorporated.
- [x] Red tests are captured before implementation.
- [x] Level 1, Level 2, and Level 3 tests pass.
- [x] `make check` passes.
- [x] `make check-tui-qa` passes with no plugin-DX screenshot manifest required.
- [x] Implementation-diff DootSabha review is clean or all findings are
  addressed.
- [x] `docs/PROGRESS.md` and this task are updated.
- [x] Relevant learnings are appended to `docs/agents/learnings.md`.
