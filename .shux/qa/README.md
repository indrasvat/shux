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
a task number, and no field in the manifest is compared against it.

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

All artifact paths in the manifest must be relative to the audit's
`.shux/qa/<scope>/` directory, and every one of them must be tracked.
`pixel_metrics` must be a non-empty array. Pixel metric JSON files must be
produced by `.claude/automations/pixel_verify.py`, must have `"status": "pass"`,
and must use exact thresholds (`0` / `0`).

## Screenshots are conditional

Commit an `*-actual.png` when a pixel metric compares against a baseline this
repo actually tracks — that baseline is what makes the screenshot reviewable.
When there is nothing committed to diff it against, the pixel-metric JSON stands
alone and no PNG is owed. Committing one anyway just adds a binary that no
reviewer can check, which is what "no screenshots committed unless justified as
durable baselines" is there to prevent.

Large intermediate captures, live recordings, logs, and contact sheets stay under
`.shux/out/<scope>/`. They do not satisfy the hard gate.

Folders predating this contract (`086`, `090`, `102`, …) are left as they are.
The gate reads the diff, so finished audits are never revalidated.
