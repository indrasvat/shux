# shux VT QA Evidence

Completed VT Quality Track tasks must commit the auditable subset of their QA
evidence here:

```text
.shux/qa/<task-slug>/
  SOLID-QA.md
  evidence-manifest.json
  pixel-<case>.json
  <case>-actual.png
  <case>-expected.png
  <case>-diff.png
```

`SOLID-QA.md` must start with exactly:

```text
VERDICT: PASS
```

`evidence-manifest.json` must include these top-level keys:

- `task`
- `solid_qa_report`
- `dootsabha_design`
- `dootsabha_implementation`
- `screenshots`
- `pixel_metrics`

All artifact paths in the manifest must be relative to the task's
`.shux/qa/<task>/` directory. `screenshots` and `pixel_metrics` must be
non-empty arrays. Pixel metric JSON files must be produced by
`.claude/automations/pixel_verify.py`, must have `"status": "pass"`, and must
use the task-approved threshold values.

Large intermediate captures, live recordings, logs, and contact sheets can stay
under `.shux/out/<task>/`. They do not satisfy the hard gate unless the final
reviewable evidence above is committed here.
