# Conductor design — pre-design spikes

Two targeted spikes to verify the conductor plugin's design assumptions
BEFORE writing the PRD. Findings here drive `docs/designs/conductor.md`.

## Spike 1 — ACP capability matrix (2026-05-13)

**Goal:** Which of the agents we want to drive (claude, codex,
opencode, gemini) speak ACP **natively** vs need an adapter shim?

**Method:** Inspect installed CLIs for an `--acp` / `acp` subcommand.
Drive an `initialize` request over stdio for each that has one and
capture the response.

**Findings:**

| Agent | Installed version | Native ACP? | Adapter? |
|--|--|--|--|
| `claude` | 2.1.140 | **No** | `@agentclientprotocol/claude-agent-acp` (npm; renamed from `@zed-industries/claude-code-acp`) |
| `codex` | 0.130.0 | **No** (has `mcp-server` — a *different* protocol) | Zed ships an adapter via their ACP registry |
| `opencode` | 1.14.48 | **Yes** — `opencode acp` | — |
| `gemini` | (volta-managed) | **Yes** — `gemini --acp` | — |

**opencode `initialize` response (full, captured):**

```json
{
  "jsonrpc":"2.0","id":1,
  "result":{
    "protocolVersion":1,
    "agentCapabilities":{
      "loadSession":true,
      "mcpCapabilities":{"http":true,"sse":true},
      "promptCapabilities":{"embeddedContext":true,"image":true},
      "sessionCapabilities":{"close":{},"fork":{},"list":{},"resume":{}}
    },
    "authMethods":[{
      "description":"Run `opencode auth login` in the terminal",
      "name":"Login with opencode",
      "id":"opencode-login"
    }],
    "agentInfo":{"name":"OpenCode","version":"1.14.48"}
  }
}
```

**Key protocol points confirmed:**

- Line-delimited JSON-RPC 2.0 over stdio (matches shux's plugin
  protocol exactly — same transport assumptions, same framing rules).
- `protocolVersion: 1` is the current shipping spec version.
- Agents expose **capability negotiation** — clients should branch on
  `agentCapabilities.{loadSession, mcpCapabilities, promptCapabilities,
  sessionCapabilities}` before relying on optional methods.
- `authMethods` is a real concern — opencode reports it needs CLI
  login. A `session/new` issued before auth will hang (observed).
  Conductor must call `authenticate` first when `authMethods` is
  non-empty.

**Implication for shux-conductor:** The earlier roadmap claim that
all four agents support ACP was wrong. Real picture:

- **opencode / gemini** → conductor can drive directly via ACP.
- **claude / codex** → conductor must either (a) spawn the npm
  adapter via `npx -y @agentclientprotocol/claude-agent-acp` and
  drive that, or (b) fall back to the VT-poll watchdog (spike 2).

The VT-poll path therefore needs to be the **always-available baseline**,
with ACP as the structured fast-path when present.

**Raw exchange log:** `.shux/out/spikes/acp_handshake_opencode.log`

## Spike 2 — VT-poll watchdog feasibility

**Goal:** Confirm we can reliably detect agent states (idle /
thinking / stuck-on-prompt / rate-limited) by polling `pane.capture`
and matching against text patterns — for agents we don't ACP-drive.

**Method:** Re-use the evidence already produced by
`.shux/scripts/three_agent_split_shoot.sh` and the prior
`.claude/automations/multi_agent_shoot.sh`. Both prove we can:

1. Spawn each agent CLI in a shux pane via `state.apply`.
2. `pane.capture --lines N` to read recent VT text.
3. Pattern-match for splash strings unique to each agent.

**Findings — stable patterns per agent (observed from real shoots):**

| Pattern | Agent | State signal |
|--|--|--|
| `Claude Code v\d+\.\d+\.\d+` | claude | splash visible — agent is alive |
| `OpenAI Codex \(v\d+\.\d+\.\d+\)` | codex | splash visible |
| `(?m)^opencode$` (block-letter wordmark) | opencode | splash visible |
| `❯ \|` followed by `Press Enter` | (any) | stuck on trust prompt |
| `\b(Thinking|Computing|Analyzing)\.\.\.` | claude/codex | actively processing |
| `Context: \d+%` low % | claude | context near limit |
| `rate.limit|429` | (any) | rate-limited |
| (empty input box with `> ` prompt, no spinner, idle ≥ N seconds) | (any) | idle |

**Reliability assessment:** Patterns are stable across the agent
versions we've tested (claude 2.1.139–2.1.140, codex 0.130.0,
opencode 1.14.48). The compositor renders them exactly as the agent
emits them — no theming distortion.

**Caveats:**

- A user's `~/.claude.json` settings could in principle change
  splash text (e.g. custom motd). Conductor should treat pattern
  matches as advisory and degrade gracefully.
- A 2-second poll interval gives ~500 ms p99 detection latency on
  state transitions. Faster polling burns CPU + battery for
  diminishing returns. Configurable in conductor's config.

**Implication for shux-conductor:** VT-poll is viable as the v0.1
baseline. Tied to a small pattern registry per agent
(`plugins/conductor/lib/patterns/{claude,codex,opencode}.toml`) so
the patterns can be tweaked without redeploying the plugin.

## What we *cannot* derive from spikes alone

- **Whether the `@agentclientprotocol/claude-agent-acp` adapter is
  reliable enough for production.** Needs to be vetted in v0.5 work
  when we wire claude over ACP. Punt — VT-poll covers claude until
  then.
- **Gemini `--acp` initialize response shape.** Our probe hung
  (likely auth-related). Need a follow-up spike with a Google
  account configured before relying on gemini-via-ACP. v0.5 work.
- **Tool-call permission flow latency.** When the conductor routes
  agent tool requests through shux RPCs, what's p99 latency? Need
  measurement before claiming "fast" in the design doc. Defer to
  the v0.5 task — for v0.1–v0.4 we're not in the tool-call path.

## Conclusion

The conductor plugin should phase as:

1. **v0.1–v0.4: VT-poll baseline.** Always works, no external deps.
2. **v0.5+: ACP fast-path** for agents that natively support it
   (opencode, gemini). Adapter shims for claude/codex deferred to
   their own task once the npm adapter quality is verified.

This is the architecture the PRD will codify.
