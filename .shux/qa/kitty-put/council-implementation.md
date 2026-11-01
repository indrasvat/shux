# DootSabha implementation-diff councils: fix/kitty-put

Three bounded rounds covering 96d6cd3..c2a5c9c. Raw outputs: /tmp/shux234bin/council-kp{,2,3}.json.

## 96d6cd3..0446e8d (full diff)

Providers: codex (ok), agy (ok)

### Chair synthesis (verbatim)

Ārya, the consolidated review supports **four MAJOR defects and one MINOR defect on committed HEAD `0446e8d`**, with no BLOCKER. This synthesis relies on the supplied reproductions and cross-reviews; it does not independently verify the current working tree.

| Severity | Defect | Observable consequence | Required correction |
|---|---|---|---|
| **MAJOR** | Uppercase deletion sweeps unrelated stored images | `d=I,i=7` also removes unplaced image 8, so a later `a=p,i=8` fails. Existing placements retain their pixels through their `Arc`s. | Make cleanup respect the delete selector. Verify bulk uppercase selectors separately against Kitty’s behavior. |
| **MAJOR** | Clipping an enlarged image rounds the visible source extent to zero | A visible strip of a scaled `1×1` image disappears from attached-terminal output, although raster rendering displays it. | Preserve both visibility and scale. `.max(1)` or ceiling endpoints alone is not a complete clipping solution. |
| **MAJOR** | Main and alternate screens share one image store | An alternate-screen transmission reusing an image ID changes what subsequent primary-screen puts retrieve. Restored placements can still show their original pixels. | Give each screen its own image namespace and implement the corresponding swap/reset lifecycle. |
| **MAJOR** | Empty-crop retransmission bypasses old-placement cleanup | `a=T` replaces the stored image but leaves its previous placement visible when geometry resolution returns early. | Remove prior placements when accepting the retransmission, before resolving or admitting its replacement placement. |
| **MINOR** | Raster size-budget arithmetic overflows | Malformed declared dimensions trigger a checked-build panic before image decoding rejects the payload. | Use checked or saturating budget arithmetic; also audit narrowing conversions in `drawn_px`. No allocation-budget bypass was demonstrated. |

The following claims should **not** remain in the defect list:

- **Dirty flags prevent graphics updates:** contradicted by the reviewed compositor and a passing hidden-cursor, clean-grid reproduction. Graphics emission still runs without dirty text cells.
- **Duplicate emitter transmissions constitute a correctness defect:** no failure was established. This is an efficiency observation with conservative per-placement budgeting.
- **An empty crop must increment `refusals`:** unsupported without an explicit project contract.
- **Frozen placements require retaining future store availability:** frozen pixels and subsequent image lookup are separate concerns.

The reviews agree that immutable image `Arc`s preserve synchronized-output presentation, placement byte accounting shows no demonstrated drift, and `d=a` correctly removes placements intersecting the visible viewport. Memory quotas are independent: **32 MiB is not an aggregate pane limit**, and the suggested 160 MiB sum is a logical payload-budget estimate, not a process-memory ceiling.

Before closing the fixes, validate selector-specific deletion, main/alternate ID reuse, retransmission with an undisplayable replacement, and clipped multicolor images through both rendering paths. The clipping check must verify pixel scale as well as command emission.

Claims about uncommitted fixes remain provisional. Several explanatory details in agy’s reports—including compositor control flow, aspect-fit dimensions, budget arithmetic, and overflow calculations—are inaccurate; retain the demonstrated failures without carrying those explanations into the final review.

## 0446e8d..8bb054f (round-1 fixes)

Providers: codex (ok), agy (error: invoke agy: agy: Individual quota reached. Please upgrade your subscription to i)

### Chair synthesis (verbatim)

Reviewed `0446e8d..8bb054f`. **No blockers; two major and two minor correctness gaps remain.**

| Severity | Finding | Required correction |
|---|---|---|
| **Major** | Z-targeted deletion compares against constant zero, so placements at nonzero z-indices survive. | Retain each placement’s requested z-index and use it for `q/Q/z/Z` matching. |
| **Major** | `d=R` leaves unplaced images in the requested ID range stored. | Include all matching stored images when applying uppercase range deletion. |
| **Minor** | `r/R` rejects a zero lower bound, although ID ranges are inclusive and may start at zero. | Allow zero for ID-range bounds while preserving positive cell-coordinate validation. |
| **Minor** | `d=I,i=7,p=99` frees image 7 even when placement 99 does not exist. This also occurs at the base commit. | Apply unconditional named-image freeing only when no placement ID was supplied. |

The relevant deletion logic is in [lib.rs](crates/shux-vt/src/lib.rs:732). These findings follow the [Kitty deletion specification](https://sw.kovidgoyal.net/kitty/graphics-protocol/#deleting-images) and [reference implementation](https://github.com/kovidgoyal/kitty/blob/master/kitty/graphics.c#L2531).

The remaining changes are supported by the review: coordinate and cursor targeting, refusal of unsupported deletes, preservation of unrelated unplaced images, per-screen store lifecycle, retransmission and replacement behavior, anonymous placement identity, emitter crop bounds, and saturating arithmetic.

Verification included isolated A/B probes against both commits. All **17 `graphics_put` tests**, the refused-replacement mutation test, and the combined screen-transition/resize/synchronized-output probe passed at HEAD. No tracked files changed.

**Recommendation:** address the four deletion gaps before calling Kitty deletion support complete; retain the other fixes.

## 8bb054f..c2a5c9c (round-2 fixes, sync-deadline test switch)

Providers: codex (ok), agy (error: invoke agy: agy: Individual quota reached. Please upgrade your subscription to i)

### Chair synthesis (verbatim)

AGREE

## Disposition

The round-1 findings were fixed in 8bb054f and the round-2 findings in 5e9ebe3; the commit messages list each one. Round 3 returned AGREE. agy failed on quota in rounds 2 and 3, so those two rounds rest on codex alone.
