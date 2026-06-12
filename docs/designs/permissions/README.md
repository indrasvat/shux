# shux plugin permissions — design doc

> **Status:** Locked after council review (2026-05-13) — implementing
> **Owner:** indrasvat
> **Phase:** v0.19+ (third plugin daemon FR; predecessors landed in
> [PR #31](https://github.com/indrasvat/shux/pull/31) `event.publish`
> and [PR #32](https://github.com/indrasvat/shux/pull/32)
> `plugin.state.*`)
> **Reviewers:** dootsabha council — claude + codex
> (gemini parked on [`indrasvat/dootsabha#14`](https://github.com/indrasvat/dootsabha/issues/14)).
> Full transcript: [`council-review.txt`](./council-review.txt).

## 1. Problem

shux ships an RPC surface of **39 methods** (see §3). A process
plugin installed via `shux plugin install ./plugin.sh` today gets
**unconditional access to every one of them**: it can `pane.send_keys`
arbitrary bytes (including `rm -rf ~/`), `state.apply` a batch that
destroys every session, `pane.snapshot` panes the plugin didn't
create (exfil), `plugin.install` a sibling plugin, `plugin.kill` the
permissions enforcer that ships next month, etc.

This was fine for v0.16–v0.18 — the only public plugin in the wild
is `examples/plugins/watcher/plugin.sh` which uses two methods
(`event.publish`, `plugin.state.set`). But the **conductor plugin**
([`feat/conductor-plugin-design`](../conductor/README.md)) and any
post-v0.5 ACP-bridged automation broker fan out to dozens of methods
including `pane.send_keys` to panes they didn't create. Without a
permission model, there is no honest answer to *"can I trust this
plugin?"* — the answer is always **no**, because plugins are
unsandboxed shell scripts with full daemon RPC.

This doc proposes the smallest model that lets a user install a
plugin, see exactly what it can do, and revoke individual capabilities
without uninstalling.

## 2. Threat model

We assume the user installs plugins they at least *intended* to run
(supply-chain attacks on the file itself are out of scope; that's a
checksum/signing problem). The threats this design addresses:

| # | Threat | Today | After this design |
|---|---|---|---|
| T1 | Malicious plugin destroys other plugins' state via `state.apply` mass mutation | unmitigated | default-denied; needs explicit grant |
| T2 | Plugin sends keystrokes (`rm -rf`, `:wq`, etc.) into a pane the user is actively typing in | unmitigated | denied for panes the plugin didn't create |
| T3 | Plugin exfils content of a pane it didn't create (passwords, dotfile contents shown by `cat`, agent prompts) | unmitigated | denied for panes the plugin didn't create |
| T4 | Buggy / over-eager plugin installs / kills sibling plugins | unmitigated | `plugin.install`/`plugin.kill`/`plugin.reload` always denied to plugins |
| T5 | User can't tell what a plugin actually did | no record | NDJSON audit log per plugin |
| T6 | Plugin author drift: a hot reload silently expands the manifest's claimed surface | enforcement is whatever's in the file | grants persisted out-of-band; new methods require explicit re-grant |

**Out of scope.** We are *not* trying to defend against:

- Plugins reading/writing arbitrary files on the local filesystem
  via direct shell commands (the plugin is a shell script — it can
  `cat ~/.ssh/id_rsa` regardless of what shux does).
- DoS via runaway resource use (own concern; rate-limits or quotas
  can be a later FR).
- Side-channel timing attacks via `pane.wait_for` / `events.watch`.
- The kernel boundary (we are not WASM-isolating; that's a separate
  multi-quarter effort).

This is a **least-authority RPC model**, not a sandbox.

## 3. Sensitive-method inventory

Auto-generated from `crates/shux/src/main.rs` (every `register(...)`
call). Categorised by what authority each grants:

### Tier 0 — informational reads (default-allow)

Listing entities the daemon already exposes; no exfil per se.
Plugins list these to discover what to act on.

`session.list`, `window.list`, `pane.list`, `plugin.list`

> Even these leak metadata (session names, pane cwds). Future cap
> for "no, also gate this" is open. v0 leaves them open.

### Tier 1 — content reads (default-deny when target ≠ plugin)

Read pane VT state. Major exfil risk if the plugin didn't create
the pane (the user could be typing a password into it).

`pane.capture`, `pane.snapshot`, `pane.output.watch`,
`pane.command_status`, `pane.wait_for`,
`session.snapshot`, `window.snapshot`

`pane.record.start` and `pane.record.stop` are stronger than ordinary
content reads because they create durable raw PTY transcripts. They are
registered as plugin-forbidden RPCs, not grantable Tier 1 reads.

### Tier 2 — owned-entity mutations (default-deny when target ≠ plugin)

Mutate entities, but the gate is per-target ownership not blanket.
A plugin that creates its own pane should be able to drive it
without ceremony.

`pane.split`, `pane.set_size`, `pane.resize`, `pane.zoom`,
`pane.swap`, `pane.kill`, `pane.set_title`, `pane.focus`,
`pane.focus_direction`, `pane.send_keys`, `pane.run_command`,
`pane.command_cancel`, `window.create`, `window.rename`,
`window.focus`, `window.reorder`, `window.kill`, `window.ensure`,
`session.create`, `session.rename`, `session.kill`, `session.ensure`

### Tier 3 — privileged (default-deny, no per-target distinction)

Affect the whole daemon or other plugins. No conceivable plugin
needs these by default.

`state.apply` — atomic batch over arbitrary entities; the most
dangerous single method on the surface.
`plugin.install`, `plugin.kill`, `plugin.reload` — privilege
escalation: a plugin that gets these can replace itself with a
fresh binary or kill the auditor.
`events.history`, `events.watch` (without filter) — full firehose
of every action across every entity.

`events.watch` *with a `plugin.<self>.` filter* is harmless and
should remain default-allow; the gate fires only on broader
filters or none at all.

## 4. Design space

Six options (per the parked RESUME). Each evaluated on (S)ecurity,
(E)rgonomics, protocol (P)rotocol surface, (R)eview burden.

### Option A — manifest allowlist

Plugin declares `requires: ["pane.send_keys", "state.apply"]` in its
manifest. Daemon enforces.

| Pros | Cons |
|---|---|
| Simple. Single source of truth. | User has zero say after install. |
| Discoverable: read the file. | Hot reload expands silently if author edits manifest. |
| No new state. | No revocation without uninstall. |

**S:** weak — author writes own permissions.
**E:** great — invisible to the user.
**P:** zero new surface.
**R:** none.

### Option B — capability tokens

Daemon issues per-method short-lived tokens (`mint_capability` →
opaque blob). Plugin presents on each call.

| Pros | Cons |
|---|---|
| Theoretically tight. | New protocol surface (token mint, refresh, revocation). |
| Tokens can carry expiry, scope. | Per-call latency cost. |
| Maps to OAuth-ish patterns developers know. | Plugin must store + refresh tokens — bash plugin overhead is real. |
| | Tokens don't help if plugin is malicious — it still asks for and gets them. |

**S:** medium — only really effective if grants are *interactively*
gated. If they auto-issue, this is option A with extra steps.
**E:** poor — bash plugins choke on JWT-style flows.
**P:** large — entire token lifecycle to spec.
**R:** large.

### Option C — prompt-on-first-use

First call to a sensitive method blocks the plugin and pops a
shux-side prompt: "watcher wants to call `pane.send_keys` on
pane abc123. Allow once / always / deny?" — like browser permissions.

| Pros | Cons |
|---|---|
| Beautiful UX for casual installs. | Needs a prompt channel into the attached client (we have one — the render loop). |
| Decisions land at the moment of consequence — high signal. | What if no client is attached? (cron job, headless install.) |
| Bypasses auditing the manifest. | Fatigue: a chatty plugin trains users to click "always". |
| | Plugins can't be deterministic — first-call latency unpredictable. |

**S:** medium — depends on user vigilance. Browser model shows this
degrades fast.
**E:** great for interactive use, terrible for headless / scripted.
**P:** medium — needs `permission.prompt` request frame, attached
client UI for it.
**R:** medium.

### Option D — default-deny + explicit grant via `shux plugin grant`

User runs `shux plugin grant watcher pane.send_keys`. Persisted to
`.shux/plugins/watcher/grants.toml`. Daemon checks on every call.

| Pros | Cons |
|---|---|
| Explicit. Auditable. Diffable in git. | Friction at install time. |
| Survives hot reload; survives daemon restart. | Plugin authors will document a `shux plugin grant ... && ...` one-liner; users will copy-paste it without reading. |
| `shux plugin grants <name>` lists everything. | Per-pane-target grants get noisy fast. |
| Maps cleanly to `chmod` / `setcap` mental model. | |

**S:** strong, *if* the user reads the grant. Same caveat as A but
the user is the one writing the policy, not the plugin author.
**E:** medium — one extra command per install.
**P:** small — three new RPC methods (`plugin.grant`, `plugin.revoke`,
`plugin.grants_list`).
**R:** small.

### Option E — audit log only (no enforcement)

Every plugin RPC call appended to NDJSON at
`.shux/plugins/<name>/audit.log`. Nothing is denied.

| Pros | Cons |
|---|---|
| Cheapest. Forensics-grade. | Doesn't prevent T1–T4. |
| Zero ergonomic cost. | Useless without someone reading the log. |
| Composes with everything. | |

**S:** zero (post-hoc visibility ≠ prevention).
**E:** great.
**P:** none.
**R:** none.

### Option F — combination (default-deny on Tier 2/3 + CLI grant + always-on audit log + ownership-based auto-grant for Tier 2)

The author's preferred starting point.

| Aspect | Behaviour |
|---|---|
| Tier 0 | Always allowed, audited. |
| Tier 1 | Denied unless target was created by the calling plugin. CLI grant `shux plugin grant <name> pane.snapshot[:<pane-id>]` upgrades. |
| Tier 2 | Same: ownership auto-allows; cross-target needs grant. |
| Tier 3 | Always denied to plugins. No grant exists. (`plugin.install` from a plugin = privilege escalation we never want; if a future workflow needs it, that's a separate principal model.) |
| Audit | Every call: `{ts, plugin, method, params_hash, target_ids[], decision: allow/deny, reason, error?}`. Append-only NDJSON. |

This composes options A (implicit allowlist via ownership), D
(explicit grant for the rest), E (audit always on). Skips B (no
token machinery — the calling plugin's identity is already
authenticated by the I/O loop, which knows which child wrote the
frame). Skips C for v0 (no prompt channel yet — can be layered on
later as a UX upgrade without changing the enforcement model).

## 5. Recommendation: option F

### 5.1 Enforcement point

Single chokepoint: `dispatch_plugin_frame` in
`crates/shux-plugin/src/lib.rs:858`. Every plugin RPC frame goes
through this function before reaching the router. The check happens
*after* method/params parse, *before* the `tokio::spawn` to the
router:

```rust
let decision = permissions::check(
    plugin,                  // captured from I/O loop, can't be spoofed
    &method,
    params.as_ref(),
    &graph_snapshot,         // for ownership lookups
    &grants,                 // .shux/plugins/<plugin>/grants.toml
);
audit::record(plugin, &method, &params, &decision);
match decision {
    Decision::Allow => { /* dispatch */ }
    Decision::Deny { reason } => {
        let err = serde_json::json!({
            "jsonrpc": "2.0",
            "error": {
                "code": -32004,
                "message": "permission denied",
                "data": {"plugin": plugin, "method": method, "reason": reason}
            },
            "id": id,
        });
        let _ = resp_tx.try_send(format!("{err}\n"));
        return;
    }
}
```

The router never sees a denied call.

### 5.2 Ownership tracking

Add `created_by_plugin: Option<String>` to `Pane`, `Window`,
`Session` in `crates/shux-core/src/model.rs`. Set when the entity
is created via a plugin RPC; `None` for user-created entities.
`state.apply` infers the principal per-op and stamps each newly
created entity.

Ownership is the cheap default that handles 90% of conductor's case
(it creates panes, drives the panes it created). Cross-pane grants
are the explicit-opt-in escape hatch.

### 5.3 Grant file

`.shux/plugins/<plugin-name>/grants.toml`:

```toml
# This file is consulted on every plugin RPC call. Edit by hand or
# via `shux plugin grant <plugin> <method>[:<target>]`.

[grants]
# blanket: this plugin can call this method on any target
"pane.snapshot"    = "*"
"events.watch"     = "*"

# scoped: this plugin can call this method only on these target IDs
"pane.send_keys"   = ["a1b2c3d4-...", "e5f6a7b8-..."]
```

The `*` form is the "yes, any target" upgrade from per-target.
Survives hot reload (the file is consulted on every call, not
cached). Survives daemon restart.

### 5.4 Audit log

`.shux/plugins/<plugin-name>/audit.log` — append-only NDJSON, one
line per call:

```json
{"ts":"2026-05-13T14:22:03.142Z","plugin":"conductor","method":"pane.snapshot","params_hash":"sha256:e3b0...","targets":{"pane_id":"a1b2..."},"owner":{"pane":"conductor"},"decision":"allow","reason":"owned_by_plugin"}
{"ts":"2026-05-13T14:22:03.910Z","plugin":"conductor","method":"pane.send_keys","params_hash":"sha256:c44b...","targets":{"pane_id":"f9d0..."},"owner":{"pane":"<user>"},"decision":"deny","reason":"not_owned_and_no_grant"}
```

Rotation: rotate at 1 MiB into `audit.log.1`, keep last 5
(`audit.log.{1..5}`). Cheap, no daemon-wide rotation infra needed.

### 5.5 New CLI surface

Three verbs:

```bash
shux plugin grant <plugin> <method>[:<target-id>]    # add a grant
shux plugin revoke <plugin> <method>[:<target-id>]   # drop a grant
shux plugin grants <plugin>                           # list grants
shux plugin audit <plugin> [--tail N] [--since DUR]   # tail audit log
```

All four backed by RPC methods of the same name (CLI/RPC parity per
[`feedback_agent_first_cli`](../../../.claude/projects/-Users-indrasvat-code-github-com-indrasvat-shux/memory/feedback_agent_first_cli.md)).

### 5.6 Backwards compatibility

The `watcher` plugin (currently the only public plugin) calls
`event.publish` and `plugin.state.*`. Both are **plugin-only**
methods (intercepted before the router) and remain unrestricted —
they only write to the plugin's own namespace.

No grants needed for `watcher` to keep working post-this-PR. ✅

Hypothetical conductor (designed but not built) would need:
```bash
shux plugin grant conductor pane.snapshot    # exfil settle PNGs
shux plugin grant conductor pane.send_keys   # auto-dismiss prompts
shux plugin grant conductor events.watch     # bus subscription
```
Three grants, one-time, documented in the conductor README.

## 6. Open questions for the council

1. **Tier 3 absolutism.** Option F flat-denies `plugin.install` /
   `plugin.kill` / `plugin.reload` / `state.apply` to plugins, no
   grant possible. Is that too rigid? Conductor v0.7 ("notes.md
   sync between agent panes") might want `state.apply` for an
   atomic multi-pane title update. Counter-argument: conductor can
   just call `pane.set_title` N times in a loop; not atomic but
   adequate.

2. **Hot-reload silent expansion.** If a hot reload changes the
   plugin's behaviour to call a new sensitive method, the existing
   grant file doesn't cover it → call denied → plugin breaks. Good
   security, possibly bad UX. Should we *prompt* on the first
   denied call (option C, scoped narrowly), or just emit a tracing
   warning and require manual `shux plugin grant`?

3. **Audit log default location.** Should it live in
   `.shux/plugins/<name>/audit.log` (current proposal — co-located
   with state and grants) or `.shux/audit.log` (single daemon-wide
   log, easier to grep across plugins)? Per-plugin is easier to
   reason about + rotate; daemon-wide is easier for "what did
   anything do in the last hour".

4. **Granularity of grants.** Method only, method + target-id,
   method + target-id + arg-pattern? Per-target is the bulk of
   the value. Arg-pattern (e.g. "this plugin can `pane.send_keys`
   *but only ASCII printable bytes*") is a rabbit hole — defer
   until a real use case lands.

5. **What about plugins consuming events from panes they don't
   own?** `events.watch` with a broad filter is the firehose
   problem. Today's design treats it as a Tier 3 sensitive read
   that needs an explicit grant. Conductor needs this. Watcher
   technically also subscribes to `pane.exited` from any pane —
   currently allowed because manifest `subscribes:` is honoured by
   the bus subscription, which is set at handshake time. Should
   we keep that *manifest-driven* (current behaviour) and only
   gate *post-handshake* `events.watch` calls? Probably yes —
   manifest subscribes are visible at install time and don't grow
   silently across hot reload.

6. **Sensitive-methods drift.** Adding a new RPC to the daemon
   that should be Tier 1/2 means updating a hardcoded match
   statement in this design. Is there a cleaner way — e.g.
   sensitivity tier as part of the `register()` call signature?
   Probably yes; revisit during implementation.

7. **Anything missing from the sensitive list?** RESUME flagged
   `pane.input.*` event subscription as a potential exfil vector
   (a plugin watching `pane.input` learns every keystroke).
   Currently no `pane.input.*` event exists; if/when it lands, it
   joins Tier 1 reads.

8. **Should grants survive `shux plugin uninstall`?** Argument
   for yes: reinstalling shouldn't lose your considered grants
   for an established plugin. Argument for no: uninstall = clean
   slate. Probably no by default, with `--keep-grants` opt-in.

## 7. Comparison with prior art

| System | Model | Lesson |
|---|---|---|
| **Browser permissions (Web)** | prompt-on-first-use + persistent grant | Fatigue is real; "always allow" gets clicked. We avoid by making CLI grant the *only* path — no prompt channel to fatigue out of. |
| **AppArmor / seccomp** | profile = `allow-list` of syscalls | Conceptually identical; ours is RPC-method-grained. Profiles are static; ours adds per-target dynamic. |
| **Wasmtime capability model** | linker-resolved imports per-component | Tightest model in the space, but requires WASM. shux plugins are unsandboxed processes; this model is what we'd graduate to in a hypothetical "WASM plugins" v2. |
| **OCI/Docker (`--cap-drop ALL --cap-add NET_BIND_SERVICE`)** | default-deny + explicit add | Same shape as our F. Validates the ergonomic choice. |
| **GitHub Actions `permissions:` block** | default-deny on all scopes once you opt in | Same shape. Our `grants.toml` ≈ a `permissions:` block on the manifest. |
| **macOS TCC (privacy.db)** | per-app, per-resource, prompt-on-first-use, persisted | The F + C combination. Worth pursuing if/when we ship a prompt channel — strict superset of F. |

## 8. Implementation sketch (out of scope for this doc)

Will land as a separate PR after council lock. Touchpoints:

- `crates/shux-plugin/src/permissions.rs` (new) — Decision enum,
  `check(plugin, method, params, snapshot, grants) -> Decision`,
  Tier 0/1/2/3 classification.
- `crates/shux-plugin/src/audit.rs` (new) — append-only NDJSON
  writer with rotation.
- `crates/shux-plugin/src/grants.rs` (new) — TOML parser, file
  watcher (re-read on save).
- `crates/shux-core/src/model.rs` — `created_by_plugin: Option<String>`
  on Pane, Window, Session.
- `crates/shux-plugin/src/lib.rs:858` (`dispatch_plugin_frame`) —
  insert `permissions::check` + `audit::record`.
- `crates/shux/src/main.rs` — register `plugin.grant`, `plugin.revoke`,
  `plugin.grants_list`, `plugin.audit` RPC methods + CLI subcommands.
- `skills/shux/references/plugins.md` — document the grant model.
- `pages/index.html` — security section addition.
- `docs/PRD.md` §plugin-host — update.
- Visual proof: `.shux/scripts/plugin_permissions_shoot.sh` →
  `pages/screenshots/plugin-permissions-demo.png`.

Estimated PR size: ~600 LoC implementation + ~400 LoC tests + docs.
Single PR, conventional commit `feat(plugin): permission/audit
model with default-deny + grants`.

## 9. Council review verdict (2026-05-13)

Run via `dootsabha council --agents claude,codex` (gemini parked on
[`indrasvat/dootsabha#14`](https://github.com/indrasvat/dootsabha/issues/14)).
Full transcript at [`council-review.txt`](./council-review.txt) —
this section captures the locked deltas.

### 9.1 Tier 3 split (Q1)

Tier 3 was too coarse. Final policy:

| Method | Tier | Grant path? |
|---|---|---|
| `plugin.install`, `plugin.kill`, `plugin.reload` | 3a | flat-deny, no grant possible |
| `state.apply` | 3b | grantable via `shux plugin grant <name> state.apply` (conductor v0.7 needs atomic multi-pane updates; "loop pane.set_title" would destroy atomicity it actually needs) |

The conductor README will document the `state.apply` grant with a
loud warning.

### 9.2 Identity is a per-install UUID, not the plugin name (NEW threat T7)

**Council caught this — the most important deliverable of the
review.** The original design keyed `grants.toml` and
`created_by_plugin` on plugin *name*. That means a reinstalled
plugin with the same name inherits all grants AND ownership of every
entity the predecessor created. A user who uninstalls "conductor"
because they don't trust it, then later installs an attacker's
plugin that picks the same name, gives the attacker every grant the
original had — and ownership of every pane the original spawned.

Fix:

- On install, generate `PluginId = Uuid::new_v4()` and persist it
  in the manifest registry alongside the name.
- `created_by_plugin: Option<PluginId>` on Pane/Window/Session
  (UUID-typed, not String).
- `grants.toml` lives at `.shux/plugins/by-id/<uuid>/grants.toml`
  with a symlink-or-mapping `.shux/plugins/by-name/<name>` →
  the UUID dir for human navigation.
- Uninstall deletes the by-name link and (by default) the by-id
  directory too. `--keep-grants` keeps the by-id dir; the next
  install with the same name does NOT auto-adopt it. Install
  surfaces "found orphan grants from prior install <uuid> with
  same name — adopt? [y/N]".

### 9.3 Audit covers plugin-only methods too

The check/audit chokepoint moves to immediately after parsing the
frame, BEFORE the plugin-only intercept block at `lib.rs:895`.
`event.publish` and `plugin.state.*` are recorded with
`decision: "allow"` and `reason: "plugin_self_namespace"`. Cheap
(one line of JSON per call), prevents the "where did all this state
churn come from" forensic gap.

### 9.4 Hot-reload diffs manifest subscribes (NEW threat T8)

The original design left manifest `subscribes:` open at handshake
time and gated only post-handshake `events.watch` calls. Council
flagged: hot reload re-runs handshake → a plugin author can edit the
manifest mid-session to add `subscribes: ["pane.input.*"]` and the
existing grant file doesn't cover it.

Fix: on every reload, the manager compares `old_manifest.subscribes`
vs `new_manifest.subscribes`. Net-new entries fail handshake unless
already present in `grants.toml` under the key
`manifest.subscribes`. Existing entries flow through unchanged so
benign reloads aren't disrupted.

### 9.5 Atomic grant writes + reject symlinks

`shux plugin grant` writes via temp file + `rename(2)` (same pattern
already used by `plugin.state.set` in
`crates/shux-plugin/src/lib.rs:837`).

Both `grants.toml` and `audit.log` paths are rejected if any
component is a symlink — `std::fs::symlink_metadata()` walk before
open. Closes a TOCTOU vector where an attacker swaps the file under
the daemon between check and use.

### 9.6 Sensitivity tier on `register()` + startup assertion (Q6)

Add an enum:

```rust
pub enum Sensitivity {
    Public,         // Tier 0
    ContentRead,    // Tier 1
    OwnedMutation,  // Tier 2
    Grantable,      // Tier 3b: state.apply
    PluginsForbidden, // Tier 3a: plugin.install/.kill/.reload
}
```

Plus a parameter-aware classifier closure for methods like
`events.watch` where the answer depends on the filter:

```rust
builder.register_with_policy(
    "events.watch",
    Policy::param_aware(|params, plugin_id| {
        let f = params.get("filter").and_then(|v| v.as_str()).unwrap_or("");
        if f.starts_with(&format!("plugin.{plugin_id}.")) {
            Sensitivity::Public
        } else {
            Sensitivity::ContentRead
        }
    }),
    handler,
);
```

Startup assertion: `router.assert_every_route_has_policy()` panics
on boot if a registered route lacks a tier. Closes the "oh I added
a method and forgot to classify it" gap.

### 9.7 `plugin.audit` deferred — tradeoff acknowledged

Council noted that cutting the `plugin.audit` RPC violates the
"CLI == API" principle (`feedback_agent_first_cli`). It's worth
cutting in v0 anyway — implementation complexity is real, use case
is narrow, schema is unstable — but call it out: **v0.next must
restore it as a proper RPC method.** Until then, the CLI
`shux plugin audit <name>` shells out to `tail` on the per-plugin
log file, with a `--json` flag that just re-formats. This is
unsatisfying and documented as a known limitation.

### 9.8 Threat-model additions

| # | New threat | Mitigation |
|---|---|---|
| T7 | Plugin name re-use after uninstall inherits predecessor's grants and ownership | Install-time UUID; name is display-only; uninstall purges by-id by default |
| T8 | Hot reload silently expands manifest `subscribes:` | Diff old/new on reload; gate net-new through grants |
| T9 | TOCTOU on grants.toml between user's `grant` write and daemon's `check` read | Atomic write via temp+rename |
| T10 | Symlink swap on grants.toml or audit.log path | Reject symlinks in path components |

### 9.9 Updated implementation checklist (locked)

1. `crates/shux-plugin/src/permissions.rs` — `Sensitivity`, `Policy`,
   `Decision`, `check()`.
2. `crates/shux-plugin/src/audit.rs` — append-only NDJSON, atomic
   write + symlink rejection, 1 MiB rotation, last 5 kept.
3. `crates/shux-plugin/src/grants.rs` — TOML parser; atomic write;
   re-read on every check (no caching for now — we can profile and
   add a `mtime`-keyed cache later if it shows up).
4. `crates/shux-plugin/src/lib.rs` — install generates a
   `PluginId(Uuid)`; manifest registry maps both name→id and id→meta;
   `dispatch_plugin_frame` runs `audit::record` THEN
   `permissions::check` for both router-bound and plugin-only frames.
5. `crates/shux-core/src/model.rs` — `created_by_plugin:
   Option<PluginId>` on Pane, Window, Session (typed UUID, not
   `String`).
6. `crates/shux-rpc` `Router` — `register_with_policy(method,
   policy, handler)`; `assert_every_route_has_policy()` called at
   daemon boot.
7. `crates/shux/src/main.rs` — register `plugin.grant`,
   `plugin.revoke`, `plugin.grants_list`; CLI subcommands
   `shux plugin grant|revoke|grants|audit`.
8. Hot-reload path in `crates/shux-plugin/src/lib.rs::reload()` —
   diff old/new manifest subscribes; deny handshake if net-new
   without grant.
9. `skills/shux/references/plugins.md`, `docs/PRD.md`,
   `pages/index.html`, `CHANGELOG.md` — doc-sync.
10. `.shux/scripts/plugin_permissions_shoot.sh` →
    `pages/screenshots/plugin-permissions-demo.png` — visual proof.

End-of-review state: design is locked. Implementation PR is a
mechanical port of this §9-augmented spec.
