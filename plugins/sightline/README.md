# Sightline

Sightline is a first-party Shux plugin package for deterministic TUI QA.

For v1, the direct runner is the product:

```bash
plugins/sightline/bin/sightline verify --session <name>
```

The process-plugin manifest is lifecycle smoke only:

```bash
shux plugin install plugins/sightline
shux plugin list
shux plugin stop sightline
```

Shux does not dispatch package commands yet, so `shux plugin run sightline ...`
and `shux sightline ...` are intentionally not supported.

Sightline writes scratch evidence under `.shux/out/sightline/` by default:
Markdown report, JSON summary, text captures, raw PTY/color evidence when
requested, pixel metrics, and PNG snapshots. Review-worthy screenshots should be
attached to PR comments instead of committed to the repository.
