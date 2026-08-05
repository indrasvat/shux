# Capture mechanics

## VHS — real terminal emulator, MP4 out

`vhs` (charmbracelet) drives ttyd + Chromium and writes MP4/GIF/WebM directly.

### It will not run as root

```
could not launch browser: Running as root without --no-sandbox is not supported
```

VHS gives no flag for this. Run it as a non-root user:

```bash
useradd -m -s /bin/bash vhsuser
mkdir -p /tmp/vhs && chmod -R 777 /tmp/vhs && chown -R vhsuser:vhsuser /tmp/vhs
su vhsuser -c "cd /tmp/vhs && HOME=/home/vhsuser vhs demo.tape"
```

Everything the tape touches must be readable **and executable** by that user. A binary
sitting under a root-owned scratch directory fails with `Permission denied` mid-recording
— which you only notice if you look at the frames. Copy binaries and scripts into the
shared directory and `chmod 755` them; it also keeps the on-screen command short
instead of a 90-character path.

### Tape traps

| Trap | Detail |
|---|---|
| `Output /abs/path.mp4` | Fails to parse. Use a relative path and `cd` into the directory. |
| `Set Width`/`Set Height` | **Pixels**, not rows/cols. Rows follow from `Set FontSize`. |
| No resize command | There is no mid-tape terminal resize. Trigger such changes out of band. |
| `Hide` / `Show` | Wrap setup in `Hide … Show` so the recording starts at the interesting moment. |
| `Set TypingSpeed` | Default typing is slow enough to eat seconds of runtime. |

### Shape

```
Output demo.mp4
Set FontSize 20
Set Width 1300
Set Height 700
Set Shell bash
Set TypingSpeed 30ms

Hide
Type "export PS1='$ ' PATH=/tmp/vhs:$PATH"
Enter
Type "clear; python3 setup.py >/dev/null 2>&1"
Enter
Sleep 12s
Type "clear; (python3 trigger.py 11 >/dev/null 2>&1 &)"
Enter
Show
Type "app attach demo"
Enter
Sleep 30s
```

The trigger is launched during `Hide` with its own delay, so it fires at a known offset
into the *visible* recording.

## Fallback — PTY + the app's own renderer

No browser, or you want the app's exact rasterizer:

1. **Record** — `pty.fork()`, run the client, write JSONL of `{"t": secs, "b": base64}`
   per read. Resize mid-recording with `TIOCSWINSZ` + `SIGWINCH` if you need it (VHS
   cannot, this can).
2. **Replay** — feed the stream into the app's terminal emulator, emitting a frame at
   each `1/fps` boundary; re-render only when the grid changed and repeat the previous
   PNG otherwise, so ffmpeg gets a constant frame rate cheaply.
3. **Encode** — `ffmpeg -framerate 12 -i frame-%05d.png -c:v libx264 -pix_fmt yuv420p`.

Put the blank-frame assertion in the renderer itself, so a recording that captured
nothing fails loudly instead of producing a valid empty video.

## Environment gotchas

- **`AF_UNIX path too long`** — socket paths cap at ~108 bytes. Deep scratch directories
  blow it. Use a short runtime dir (`/tmp/d1`), not the session scratchpad.
- **Daemonising apps fork** — the process you spawned exits immediately. Track the real
  one by pidfile, and on cleanup *wait* for it to exit rather than sampling right after
  the stop command; a slow graceful shutdown looks exactly like a leak.
- **Long background builds** can outlive a container restart. Run them in the
  foreground when the result gates everything after it.
