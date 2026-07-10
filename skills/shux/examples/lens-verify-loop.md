# Example: find and fix a visual bug with lens, no eyeballing required

You're told "there's a rendering bug in `my-tui` somewhere" — no repro
steps, no stack trace, nothing text-based to grep for. This is exactly the
class of bug `pane.capture` cannot see (a misaligned border, a color that
should differ but doesn't, a status bar that's one row short and gets
clipped) and `lens` exists for. This example is the generic pattern; treat
it as the shape to follow for whatever TUI and bug you're actually
chasing.

## 1. Run it hidden, settle, glance — establish ground truth

```bash
RUN=$(shux --format json lens run --size 120x30 -- ./target/debug/my-tui)
PANE=$(echo "$RUN" | jq -r .result.pane_id)
SID=$(echo "$RUN"  | jq -r .result.session_id)

shux pane wait-settled "$PANE" --quiet 300ms --timeout 10s
shux pane glance "$PANE" --png before.png --checkpoint
```

Open `before.png` (or hand it to a vision-capable reviewer — see
[vision-llm-feedback.md](vision-llm-feedback.md)) and actually look. Text
capture would have told you the border glyphs are all present; the pixels
tell you a segment of the border is drawn as a space instead of `─`, or a
label is off by a column, or a color that should be cyan renders as the
default foreground. That's the bug. You would not have found it from
`pane.capture` output alone — that's the entire reason this loop exists.

## 2. Fix the source, rebuild

Normal edit-compile loop. Nothing lens-specific here.

## 3. Re-run, settle, glance — compare

```bash
shux session kill "$SID"    # old build is gone; start clean

RUN=$(shux --format json lens run --size 120x30 -- ./target/debug/my-tui)
PANE=$(echo "$RUN" | jq -r .result.pane_id)
SID=$(echo "$RUN"  | jq -r .result.session_id)

shux pane wait-settled "$PANE" --quiet 300ms --timeout 10s
AFTER=$(shux --format json pane glance "$PANE" --checkpoint --png after.png)
```

## 4. Diff — prove it, don't just assert it

A fresh `checkpoint` from a killed-and-restarted pane has no shared
revision history with the "before" checkpoint, so the `diff` step here
isn't `pane diff --since` against the old run — it's a direct visual
compare of `before.png` vs `after.png` (pixel diff, or a vision model, or
your own eyes). **`pane diff --since` is for proving what changed WITHIN
one running pane** — e.g. confirming a specific keystroke moved exactly the
cells you expected and nothing else:

```bash
REV=$(echo "$AFTER" | jq -r .result.revision)
shux pane send-keys -s "$SID" -p "$PANE" --text 'j'
shux pane wait-settled "$PANE" --quiet 300ms --timeout 10s
shux pane diff "$PANE" --since "$REV" --heat move.png
```

`move.png` overlays the changed cells in red and desaturates everything
else — if the keyboard-navigation code you touched moved a selection
marker, this proves the change was confined to the marker's old and new
cells and nothing else on screen shifted.

## 5. Clean up

```bash
shux session kill "$SID"
```

(Or skip this — `lens run`'s default `--ttl 30s` reaps the scratch session
automatically once the process exits. Explicit `session kill` is for when
you're done immediately and don't want to wait even 30s.)

## Why this beats "run it, screenshot, eyeball, repeat"

- **No sleeps.** `wait-settled` is event-driven off the pane's own repaint
  signal — you get the first genuinely-still frame, not "probably done by
  now" guessing.
- **Atomic.** `glance`'s PNG and text come from the *same* grid clone.
  Snapshot-then-capture as two separate calls can tear under concurrent
  writes; glance can't.
- **Hidden and disposable.** `lens run` never touches a session a human (or
  another agent) might be looking at — it's a throwaway pane that reaps
  itself.
- **The diff step is proof, not narration.** "I moved the selection down
  one row" is a claim. A heat-map PNG showing exactly 2 cells changed, at
  exactly the marker's old and new position, is evidence.
