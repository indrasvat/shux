# `shux share`: read-only browser links

A link shows one session, live and read-only, in a guest's browser, through a
Cloudflare quick tunnel that the daemon starts on the first link and closes
when nothing can use it any more. No keys, paste, clicks or resize from the
guest reach any pane. Needs `cloudflared` (`SHUX_CLOUDFLARED` = absolute path,
else `PATH`; the **daemon's** environment counts). Human guide:
`docs/share.md`. The rules an agent keeps are in SKILL.md.

Limits: 24 h per link; at most 32 guests per link (`--uses 0` = up to 32); 4
watching one link at once; 5 reconnects per guest per 10 s; a departed
guest's credential lasts as long as its link (10 minutes offline on an
unlimited link); 64 links and 16 streams per daemon; wrong
secrets are limited to 10 a minute per address (`CF-Connecting-IP`) and 300
a minute in all, past which redemption answers 429.

## Delivery: never printed inside shux

A caller in a shux pane never gets the link on screen.

- The CLI treats any `SHUX_PANE_ID`, even one the daemon does not know, as
  in a pane: it copies the link with `pbcopy`, `wl-copy`, `xclip` or `xsel`
  and prints only id, uses and expiry. No clipboard, or `--qr`: refused
  before minting. A copy that fails, times out (10 s) or is interrupted
  revokes the link.
- The RPC requires `from_pane`. `null` asserts the caller is outside shux
  (`deliver: "print"`). Any other value means clipboard (`can_copy: true`)
  or refuse, whatever pane it names.
- `shux rpc call share.create` inside a pane is refused (`in_pane`) before
  anything is minted, whatever the params say: it prints its answer.
- With `hold: true` the link is revoked if the connection closes before
  `share.confirm` names it. `shux share` holds every link it makes and
  confirms it only once printed or copied, so a CLI killed mid-delivery
  leaves no live link.
- `--qr` with `--format json` is refused (`qr_json`): the QR is only drawn
  as text.

## RPCs

All six are **owner-local** (answered only on the daemon's Unix socket;
TCP and plugins get `permission_denied` with `reason: "owner_local"`) and
`PluginsForbidden`. Unknown fields are refused.

```jsonc
// share.create (shux share / shux share create)
{ "session": "demo",          // name or id; default: the caller's pane's session,
                              // else the only non-scratch session
  "from_pane": null,          // required: null outside shux, else SHUX_PANE_ID
  "ttl_secs": 3600,           // default 3600, max 86400
  "uses": 1,                  // default 1, max 32; 0 = up to 32 (needs ttl_secs)
  "label": "review",          // ≤64 printable chars, for list only
  "can_copy": false,          // the caller can put the link on a clipboard
  "qr": false,
  "hold": false }             // revoke unless share.confirm comes before the connection closes
→ { "id": "ab12cd34", "url": "https://<words>.trycloudflare.com/v/ab12cd34#<secret>",
    "session_id": "<uuid>", "session": "demo", "expires_at": "2026-11-24T15:00:00Z",
    "expires_in_ms": 3600000, // by the daemon's clock, as the guest page has it
    "uses": 1,                // null when unlimited
    "label": "review", "deliver": "print",    // or "clipboard"
    "notice": "Anyone with this link sees everything in demo, live, for the next 1h. End it with: shux share stop" }

// share.list (shux share list)
{} → { "tunnel": "up",        // "starting" or "down"
       "links": [{ "id": "ab12cd34", "session_id": "<uuid>", "session": "demo",
                   "label": "review", "expires_at": "…", "expires_in_ms": 1500000, "uses_left": 0,
                   "redeemed": 1,
                   "viewers": [{ "id": "v9x8y7w6", "connected": true }] }] }

// share.confirm: a held link was delivered; it stays
{ "id": "ab12cd34" } → { "confirmed": "ab12cd34" }

// share.revoke (shux share revoke <id|all>): ends the link and its viewers;
// "all" ends every link first, then reports any teardown that failed
{ "id": "ab12cd34" } → { "revoked": ["ab12cd34"] }

// share.kick (shux share kick <viewer>): drops one viewer, keeps the link
{ "viewer": "v9x8y7w6" } → { "kicked": "v9x8y7w6" }

// share.stop (shux share stop): idempotent
{} → { "stopped": true, "links_ended": 1 }
```

A viewer is `connected` once its stream is drawing, not while it connects.
`--format plain`: `share` prints the URL alone; `share list` prints one
tab-separated line per link: id, session, `expires_at`, uses left (`-` for
unlimited), live viewers.

A guest also shows in `client.list` with `"share": true`; `client.detach`
of one is a kick (credential invalidated, the guest told `kicked`).

The guest's page (`shux.view.v1`) gets `{"t":"watching","n":2}`: the badge's
count of read-only clients on the session, on a change and at most once a
second, never who. Before Watch session it asks the redeem endpoint with
`"peek": true`, which checks the secret, spends no use, and answers
`expires_in_ms` and `uses_left`; a wrong secret counts as a failed guess.

## Errors

| `data.reason` | When |
|--|--|
| `in_pane` | a caller in a pane with no clipboard, or asking for a QR |
| `qr_json` | `--qr` with `--format json` |
| `unbounded` | `uses: 0` without `ttl_secs` |
| `too_many_uses` | `uses` above 32 |
| `invalid` | ttl outside 1s–24h, or a bad label |
| `ambiguous` / `no_session` | no session named and none or several to choose from |
| `full` | 64 links live already |
| `no_cloudflared` | cloudflared not found (`hint` says how to install) |
| `rate_limited` | Cloudflare returned 429 for new quick tunnels; wait a few minutes |
| `exited` / `timeout` / `unreachable` / `spawn_failed` / `cancelled` | the tunnel did not come up, or `share stop` ended its startup |

## Events

Public ids and reason codes only; never a URL, secret, label, origin or
anything about a guest. Browser guests publish no `client.*` events.

| Event | `data` |
|--|--|
| `share.created` | `link_id`, `session_id`, `expires_at` |
| `share.ended` | `link_id`, `session_id`, `reason`: `revoked` · `expired` · `stopped` · `session_ended` · `tunnel_lost` |
| `share.tunnel` | `state`: `up` · `down` (`reason`: `idle` · `stopped` · `lost`) · `failed` (`reason`: an error code above) |

## Lifecycle

- The first link starts the tunnel (a few seconds). A link is issued only
  once the public origin answers with this tunnel's marker: at every edge
  address the daemon can reach, or after 30 s at one. In the first minute a
  guest may get "no such site" or Cloudflare's 1016: wait and reload.
- The tunnel closes by itself once no guest can reach the daemon. If
  cloudflared dies, every link ends (`tunnel_lost`) and a new link gets a
  new address.
- The host's badge: amber `◉ shared · 0 watching` while a link is live, then
  `◉ shared · N watching`.
