# shux VT QA Evidence

Any diff that touches VT rendering — `crates/shux-vt/`, `crates/shux-raster/`, or
`crates/shux-pty/src/capture.rs` — must carry the auditable subset of its QA
evidence here, in the same diff. `make check-vt-qa` enforces both halves from the
diff itself; a diff touching none of those paths is asked for nothing.

```text
.shux/qa/<scope>/
  SOLID-QA.md
  evidence-manifest.json
  pixel-<case>.json
  <case>-actual.png      # only when a committed baseline exists — see below
  <case>-expected.png
  <case>-diff.png
```

`<scope>` is free-form: name the folder after the change. Nothing derives it from
a task number, and no field in `evidence-manifest.json` is compared against it.
(The TUI manifest in the same tree, `tui-evidence-manifest.json`, is different —
`scripts/check-tui-qa.sh` does require its `scope` field to equal the folder
name.)

`SOLID-QA.md` must start with exactly:

```text
VERDICT: PASS
```

`evidence-manifest.json` must include these top-level keys:

- `solid_qa_report`
- `dootsabha_design`
- `dootsabha_implementation`
- `screenshots`
- `pixel_metrics`

Every artifact reference must be a **path string** relative to the audit's
`.shux/qa/<scope>/` directory, and must resolve to a tracked regular file —
not a directory, and not a symlink (a symlink's content is not in this repo).
`pixel_metrics` must be a non-empty array. Pixel metric JSON files must be
produced by `.claude/automations/pixel_verify.py` — the shape is asserted, so an
arbitrary JSON file will not pass — must have `"status": "pass"`, and must use
exact thresholds (`0` / `0`). There is no PR-defined threshold: a case that
cannot be exact does not belong in committed evidence.

Both `SOLID-QA.md` and `evidence-manifest.json` must appear in the diff, and each
must gain at least one line of real content. A verdict is issued for the change
in front of it; touching some other file under an existing scope does not select
that scope, and appending whitespace to an old report does not renew it. What
this cannot detect is a wholesale copy of someone else's audit — that is a
reviewer's job, not a guard's.

## Screenshots are conditional

Commit an `*-actual.png` when a pixel metric compares against a baseline this
repo actually tracks — that baseline is what makes the screenshot reviewable.
When there is nothing committed to diff it against, the pixel-metric JSON stands
alone and no PNG is owed. Committing one anyway just adds a binary that no
reviewer can check, which is what "no screenshots committed unless justified as
durable baselines" is there to prevent.

Large intermediate captures, live recordings, logs, and contact sheets stay under
`.shux/out/<scope>/`. They do not satisfy the hard gate.

Folders predating this contract (`086`, `090`, `102`, `lens-p2`, `lens-p4`, …)
are left as they are, and the gate never goes looking for them: it reads the
diff. The one way an old folder is examined is if a diff touches **both** its
`SOLID-QA.md` and its `evidence-manifest.json` — then it is treated as evidence
being offered, and several of the pre-contract folders will fail (their
`pixel_metrics` hold objects and prose where paths are required). Put new
evidence in a new scope rather than editing an archived one.
