# Sightline TUI QA

Sightline is the first-party verifier for repeatable TUI QA with shux. Use it
when the task asks you to check terminal UI layout, alignment, keyboard
navigation, rendered colors, or PR-ready screenshot evidence. Prefer it before
writing an ad hoc `send-keys` / `snapshot` script unless the target needs a
custom interaction flow Sightline cannot express.

## What it proves

Sightline writes evidence under `.shux/out/sightline/` by default:

- `summary.json` with a PASS/FAIL verdict and structured checks
- `SIGHTLINE.md` with a human-readable report
- text captures for required strings and keyboard probes
- `color-probe.raw` for byte-exact truecolor, 256-color, and basic SGR evidence
- `pane_*.png` snapshots with PNG dimensions, grid dimensions, nonblank pixels,
  and rendered color samples

Routine screenshots and reports belong in `.shux/out/` and PR comments, not in
git. Commit screenshots only when they are durable goldens with an explicit
manifest and review justification.

## If the shux repo is checked out

From the repository root:

```bash
plugins/sightline/bin/sightline verify \
  --session "$SESSION" \
  --pane "$PANE_ID" \
  --viewport 80x24 \
  --viewport 120x40 \
  --color-probe-shell \
  --expect-text "$EXPECTED_TEXT"
```

Use `--window <name>` when a multi-window session is clearer than a pane UUID;
Sightline resolves the active pane inside that window and keeps the probe,
capture, resize, and snapshot operations on that concrete pane.

Run the package lifecycle smoke separately when relevant:

```bash
shux plugin install plugins/sightline --no-watch
shux plugin list
shux plugin stop sightline
```

That proves the package manifest and process-plugin handshake. It does not run
the verifier; the direct runner above is the v1 product.

## If the shux repo is not checked out

`shux plugin install` does not yet support registry search, remote URLs, or
`shux plugin run`. It accepts a local executable or local package directory.
Avoid cloning the full repository just to use Sightline; bootstrap the minimal
package into gitignored scratch space:

From the target project root, run this skill's helper script
(`scripts/install-sightline.sh`, resolved relative to the shux skill directory):

```bash
/path/to/shux-skill/scripts/install-sightline.sh
```

The script downloads only `shux-plugin.toml`, `bin/sightline`, and `README.md`
into `.shux/out/plugins/sightline/` by default, then prints the direct runner
path. Use that path exactly like the checked-out runner:

```bash
.shux/out/plugins/sightline/bin/sightline verify \
  --session "$SESSION" \
  --pane "$PANE_ID" \
  --viewport 80x24 \
  --color-probe-shell \
  --expect-text "$EXPECTED_TEXT"
```

If the helper is not available in the installed skill bundle, download the same
three files from the shux repository into a local package directory and
`chmod +x bin/sightline`.

## When not to use it

- You only need a one-off static screenshot and no verdict.
- The TUI requires a long domain-specific interaction script; use shux
  primitives directly, then use Sightline for final state checks if possible.
- You need browser or desktop UI QA; use a browser/desktop automation skill
  instead.
