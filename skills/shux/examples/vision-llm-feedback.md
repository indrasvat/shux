# Example: agent self-corrects a TUI via vision-LLM feedback

You're an agent building a Bubbletea (Go) app. You wrote the layout
code, but the UI doesn't render the way you intended — text overlaps,
columns are misaligned, a status bar is missing on certain terminal
sizes. Without shux you'd need a human to screenshot it for you.
With shux you can just look at it yourself.

## The loop

```
1. Compile + run your app under shux.
2. pane.snapshot → PNG.
3. Send PNG to a vision-capable LLM with a prompt like
   "Critique this TUI's layout: what looks wrong?"
4. Fix the code based on the feedback.
5. Re-run, re-snapshot, re-critique. Stop when the model is happy.
```

## A concrete sequence

```bash
#!/usr/bin/env bash
set -euo pipefail
SESSION="self-review"
SHUX="shux"

iteration=0
while [ $iteration -lt 10 ]; do
  iteration=$((iteration + 1))
  echo "── iteration $iteration ──"

  go build -o ./mytui ./cmd/mytui

  $SHUX session kill "$SESSION" >/dev/null 2>&1 || true
  $SHUX session create "$SESSION" -d -- ./mytui
  $SHUX pane set-size -s "$SESSION" --cols 160 --rows 48 >/dev/null
  sleep 2

  snap_path="iter_${iteration}.png"
  $SHUX --format json pane snapshot -s "$SESSION" \
    | jq -r .png_base64 | base64 -d > "$snap_path"

  $SHUX session kill "$SESSION" >/dev/null

  # Hand the snapshot to your vision model.
  critique=$(claude --vision "$snap_path" "$(cat <<'EOF'
You are reviewing a TUI screenshot. Be specific.

Report:
1. Any text that's misaligned, overlapping, or clipped.
2. Anything that looks like a bug (wrong color, missing border, broken table cell).
3. Suggested code change.

Format your response as JSON: {"ok": bool, "issues": [{"area": "...", "suggested_fix": "..."}]}
EOF
)")

  ok=$(printf '%s' "$critique" | jq -r .ok)
  if [ "$ok" = "true" ]; then
    echo "✓ no further issues — stopping."
    break
  fi

  echo "$critique" | jq .
  # Feed the critique back into your code-editing agent for the next round.
done
```

## Why this works

- **No human in the loop.** The agent generates code, sees the rendered
  output, judges it, edits, repeats — without ever asking a human to
  screenshot the terminal.
- **Cheap.** A 1800×1140 PNG is ~80–150 KB. Vision LLMs accept it
  directly.
- **Deterministic.** Same `pane.set_size` → same rasterizer output → if
  the agent's fix changes the rendering, the next snapshot reflects it.

## What the model sees (briefly)

The shux rasterizer bundles JetBrains Mono Regular and renders all 16
ANSI colors + the 256-cube + truecolor, plus bold/dim/inverse/underline/
strikethrough attributes. Color emoji and CJK glyphs render as `.notdef`
tofu in the current build — if your TUI uses those, ask the model to
ignore them (P2 roadmap fixes this with a fallback font chain).

## Going further

For continuous self-improvement during dev:

```bash
# Bind to your editor's save hook — re-render + critique on every save.
# (Pseudo-code; wire to your editor's after-save hook.)
on_file_save() {
  bash ./snap-and-critique.sh > .vision-feedback.json
  # Show the critique in the editor's bottom panel.
}
```

Or have the agent itself watch:

```bash
fswatch -o ./src | while read; do
  bash ./snap-and-critique.sh
done
```
