# Visual Testing (L4)

Visual tests use iterm2-driver to automate iTerm2 for screenshot-based regression testing.

```bash
uv run .claude/automations/<test>.py   # Run a visual test script
```

Screenshots are saved to `.claude/screenshots/` (gitignored).
Visual test scripts live in `.claude/automations/` and are added per-task as needed.
