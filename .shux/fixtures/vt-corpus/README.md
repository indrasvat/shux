# VT Corpus Fixtures

This directory stores committed, deterministic VT fixtures used by the VT
Quality Track. Live tools can be re-recorded into `.shux/out/`, but task
verification must replay committed fixtures so local installation differences do
not skip the real-TUI layer.

`rich-tui/` currently contains raw PTY recordings captured through shux for:

- `btop`
- `lazygit`
- `nvim`
- `vicaya-tui`
- `vivecaka`

See `rich-tui/manifest.json` for the command, terminal size, duration, byte
count, and SHA-256 of each stream.
