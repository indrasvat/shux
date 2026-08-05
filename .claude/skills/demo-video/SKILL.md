---
name: demo-video
description: Record a before/after demo video that shows the REAL product failing and then working — a live terminal session, not a script narrating its own verdicts. Use when a bug or fix needs to be *seen* rather than described: "make a demo video", "record a screencast", "show it in action", "show the crash happening", "screenshots can't capture this", or when attaching visual proof to a PR/artifact for a behavioural change. Covers VHS (real terminal emulator → MP4), the PTY + rasterizer fallback when no browser is available, and the verification steps that stop you shipping a blank or self-congratulatory video.
---

# Demo video

A screenshot proves a state. A video proves a *transition* — the thing working, then
not working. That is the only reason to make one.

## The rule that decides whether it is worth anything

**Record the product's own UI. Never record a harness narrating itself.**

The first cut of this on shux #107 was a script printing `marker visible: False` and
`RESULT: PANE DEAD` in colour. Every fact in it was true and it demonstrated nothing —
the reviewer's response was "all I see is commands and supposed outputs saying failed
or passed". The rebuild attached a real client to a real session and let the reviewer
watch a counter stop. Same bug, same binaries, completely different evidence.

If your recording would still make sense with the product replaced by `echo`, it is a
log with colours.

## Make the failure visible

A broken thing and a working thing often look identical in a still frame. Give the
recording **continuous motion that stops**:

- run a ticking counter in the component under test
  (`n=0; while :; do n=$((n+1)); printf 'tick %04d\n' $n; sleep 0.5; done`)
- a frozen counter reads as broken instantly, with no narration
- keep some *other* part of the UI alive (a status bar) so "the app died" and "this
  component died" are visually distinct — that distinction is usually the finding

**Label inside the content**, not as a post-hoc overlay: bake `BEFORE <version>` into
the ticking line itself. A caption added later can drift from the footage it labels.

## Capture

Prefer VHS: it drives a real terminal emulator and writes MP4 directly, so there is no
frame pipeline to get wrong. See `vhs.md` for tape syntax, the root/sandbox failure,
and the traps that cost time.

No browser available → `vhs.md` also covers the fallback: record the PTY with
timings, replay through the app's own renderer, `ffmpeg` the frames. More moving parts,
but it renders authentic output and needs nothing but a PTY.

Triggering a state change the recorder cannot perform (VHS cannot resize mid-tape):
drive it **out of band** — start a background helper during the hidden setup that acts
on the app at a known offset. Verify the trigger works on its own first; debugging a
silent trigger through a video is miserable.

## Verify before you believe it

Every one of these has produced a green-looking result that proved nothing:

1. **Extract frames from the ENCODED file and look at them.** Not the source frames —
   the `.mp4`. `ffmpeg -ss <t> -i out.mp4 -frames:v 1 f.png`, then actually open them.
2. **Check the video is not blank.** Two recordings that both end with the program
   quitting compare byte-identical and are both empty. Assert on content — a frame
   with under ~20 non-blank cells is not evidence.
3. **Check duration and dimensions** (`ffprobe`). A tape that failed early still
   produces a valid, short, useless file.
4. **Check both halves survived concatenation** — sample a frame from each side.
5. **Confirm the "before" side actually fails.** If the bug did not fire, you have
   recorded two working takes and captioned one of them "before".

## Before/after in one file

Record two takes, concat with ffmpeg:

```bash
printf "file 'before.mp4'\nfile 'after.mp4'\n" > list.txt
ffmpeg -y -f concat -safe 0 -i list.txt -c:v libx264 -preset slow -crf 28 \
       -pix_fmt yuv420p -movflags +faststart demo.mp4
```

`yuv420p` for player compatibility; `+faststart` so it streams. Terminal text at
~1300×700 encodes to a few hundred KB for 40s — small enough to inline as a data URI
in an artifact.

## Finish

- Kill anything the demo spawned. Identify processes by pidfile or `/proc/<pid>/cmdline`
  argv[0] — `pgrep -f <name>` matches the checking shell's own arguments and reports
  phantoms.
- Keep the video out of the repo. Publish it in an artifact and link that.
- If the video only shows part of the story (memory growth is not watchable), say so
  and keep the rest as measurements. Do not stage what you cannot film.
