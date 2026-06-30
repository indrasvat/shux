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

`shux plugin install` accepts local executables or local package directories
today; it does not yet search a registry or install remote URLs. If the shux
repo is not checked out, use the `shux` skill's `install-sightline.sh` helper
to download the minimal package into a user-scoped plugin cache instead of
cloning the whole repository. Sightline's run evidence remains project-local
under `.shux/out/sightline/`.

Sightline writes scratch evidence under `.shux/out/sightline/` by default:
Markdown report, JSON summary, text captures, raw PTY/color evidence when
requested, pixel metrics, and PNG snapshots. Review-worthy screenshots should be
attached to PR comments instead of committed to the repository.
