#!/usr/bin/env bash
# Mutation battery for issue #174.
#
# Half of this change is code that REFUSES to act: it withholds a mouse report
# from an app whose mode did not subscribe to that event, from a pane shux
# cannot encode coordinates for, from a gesture shux already owns, and from a
# click that landed on the status bar. A test asserting "nothing was forwarded"
# passes on a tree where nothing is EVER forwarded, so those tests prove nothing
# until each guard has been seen removed.
#
# Same contract as the #135 battery, and for the same reasons:
#
#   1. The suite must be GREEN unmutated, or one already-red test credits every
#      mutation.
#   2. The kill must come from the NAMED anchor. "Some test failed" is not the
#      claim being made here.
#   3. A mutation whose edit matched nothing is a FAILURE, not a kill.
#
#   .shux/scripts/issue_174_mutation_check.sh
#
# Output: a table of mutation -> killed-by, and a non-zero exit on any survivor.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${REPO_ROOT}"

ATTACH="crates/shux/src/attach.rs"
HANDLE="crates/shux-pty/src/handle.rs"
RPC="crates/shux-rpc/src/attach.rs"
BACKUP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/shux-174-mutation-XXXXXX")"
cp "${ATTACH}" "${BACKUP_DIR}/attach.rs"
cp "${HANDLE}" "${BACKUP_DIR}/handle.rs"
cp "${RPC}" "${BACKUP_DIR}/rpc_attach.rs"

restore() {
  cp "${BACKUP_DIR}/attach.rs" "${ATTACH}"
  cp "${BACKUP_DIR}/handle.rs" "${HANDLE}"
  cp "${BACKUP_DIR}/rpc_attach.rs" "${RPC}"
}
trap 'restore; rm -rf "${BACKUP_DIR}"' EXIT

# name | anchor test that must catch it | file | python expression over `s`
MUTATIONS=(
  # ── Half A: the declared winsize ──────────────────────────────────────────
  "spawn_declares_no_pixels|winsize_declares_pixel_geometry_to_the_child_at_spawn_and_on_resize|${HANDLE}|s.replace('let winsize = winsize_for(config.size);', '''let winsize = Winsize { ws_row: config.size.rows, ws_col: config.size.cols, ws_xpixel: 0, ws_ypixel: 0 };''')"
  "resize_drops_the_pixels|winsize_declares_pixel_geometry_to_the_child_at_spawn_and_on_resize|${HANDLE}|s.replace('let winsize = winsize_for(new_size);', '''let winsize = Winsize { ws_row: new_size.rows, ws_col: new_size.cols, ws_xpixel: 0, ws_ypixel: 0 };''')"
  "overflow_saturates_instead_of_declaring_nothing|handle::winsize_tests::overflow_declares_nothing_on_both_axes|${HANDLE}|s.replace('        (Some(x), Some(y)) => (x, y),\n        _ => (0, 0),', '        (Some(x), Some(y)) => (x, y),\n        _ => (size.cols.saturating_mul(cell_w), size.rows.saturating_mul(cell_h)),')"
  "the_axes_can_disagree|handle::winsize_tests::overflow_declares_nothing_on_both_axes|${HANDLE}|s.replace('        (Some(x), Some(y)) => (x, y),\n        _ => (0, 0),', '        (x, y) => (x.unwrap_or(0), y.unwrap_or(u16::MAX)),')"

  # ── Half B: the encoder ───────────────────────────────────────────────────
  "legacy_coordinates_clamp_instead_of_refusing|attach::tests::encode_mouse_report_refuses_legacy_coordinates_it_cannot_carry|${ATTACH}|s.replace('    if col > X10_MOUSE_LIMIT || row > X10_MOUSE_LIMIT || cb > X10_MOUSE_LIMIT {\n        return None;\n    }', '')"
  "sgr_release_uses_M_like_a_press|attach::tests::encode_mouse_report_is_byte_exact_for_every_button_and_action|${ATTACH}|s.replace(\"let fin = if release { 'm' } else { 'M' };\", \"let fin = 'M';\")"
  "sgr_release_forgets_which_button_came_up|attach::tests::encode_mouse_report_is_byte_exact_for_every_button_and_action|${ATTACH}|s.replace('    if sgr {\n        let fin =', '    let cb = if release { 3 } else { cb };\n    if sgr {\n        let fin =')"
  "motion_bit_dropped_from_a_drag|attach::tests::encode_mouse_report_is_byte_exact_for_every_button_and_action|${ATTACH}|s.replace('    let motion = if action == ButtonAction::Drag { 32 } else { 0 };', '    let motion = 0;')"
  "a_buttonless_press_becomes_a_left_click|attach::tests::button_cb_refuses_to_invent_a_button|${ATTACH}|s.replace('            ButtonAction::Press | ButtonAction::Release => return None,', '            ButtonAction::Press | ButtonAction::Release => 0,')"
  "modifier_bits_are_off_by_a_power_of_two|attach::tests::button_cb_carries_alt_and_ctrl_but_never_shift|${ATTACH}|s.replace('    let mods = if alt { 8 } else { 0 } | if ctrl { 16 } else { 0 };', '    let mods = if alt { 4 } else { 0 } | if ctrl { 8 } else { 0 };')"

  # ── Half B: what the routing REFUSES ──────────────────────────────────────
  "mode_1000_is_told_about_motion|attach::tests::a_drag_is_withheld_from_a_mode_1000_app_and_not_given_to_shux|${ATTACH}|s.replace('        MouseMode::Normal => action != ButtonAction::Drag,', '        MouseMode::Normal => true,')"
  "coordinate_modes_shux_cannot_encode_are_ignored|attach::tests::an_app_in_a_coordinate_mode_shux_cannot_encode_gets_nothing|${ATTACH}|s.replace('    !modes.utf8_mouse && !modes.urxvt_mouse && !modes.pixel_mouse', '    true')"
  "an_in_flight_shux_gesture_is_re_decided|attach::tests::route_app_mouse_precedence|${ATTACH}|s.replace('    if gesture != SelectionDrag::None || border_drag_active || copy_active {\n        return AppMouseRoute::Shux;\n    }', '    if border_drag_active || copy_active {\n        return AppMouseRoute::Shux;\n    }')"
  "a_border_resize_loses_the_mouse|attach::tests::a_border_resize_in_flight_is_not_hijacked_by_the_pane_it_drags_over|${ATTACH}|s.replace('    if gesture != SelectionDrag::None || border_drag_active || copy_active {', '    if gesture != SelectionDrag::None || copy_active {')"
  "copy_mode_and_the_menu_lose_the_mouse|attach::tests::copy_mode_and_the_copy_menu_keep_the_mouse|${ATTACH}|s.replace('    if gesture != SelectionDrag::None || border_drag_active || copy_active {', '    if gesture != SelectionDrag::None || border_drag_active {')"
  "shift_stops_reserving_the_mouse|attach::tests::shift_hands_the_click_back_to_shux|${ATTACH}|s.replace('    if shift {\n        return AppMouseRoute::Shux;\n    }', '')"
  "a_stray_drag_opens_a_gesture|attach::tests::a_drag_with_no_gesture_in_flight_is_not_the_apps|${ATTACH}|s.replace('    if action != ButtonAction::Press {\n        return AppMouseRoute::Shux;\n    }', '')"
  "the_wheel_is_stolen_from_handle_wheel|attach::tests::the_wheel_is_never_taken_from_handle_wheel|${ATTACH}|s.replace('        MouseKind::ScrollUp | MouseKind::ScrollDown | MouseKind::Move => {\n            return Ok(AppMouse::NotHandled);\n        }', '        MouseKind::ScrollUp | MouseKind::ScrollDown | MouseKind::Move => ButtonAction::Drag,')"
  "the_pane_hit_test_falls_back_to_the_focused_pane|attach::tests::a_click_on_the_status_bar_never_reaches_a_zoomed_app|${ATTACH}|s.replace('    let inside =\n        col >= rect.x && col < rect.x + rect.width && row >= rect.y && row < rect.y + rect.height;', '    let inside = true;')"
  "a_dropped_press_still_opens_a_gesture|attach::tests::a_dropped_press_does_not_open_a_gesture|${ATTACH}|s.replace('    if !forward_bytes_to_pane(io_state, pane_id, bytes).await {', '    if false {')"
  "an_abandoned_gesture_leaves_the_app_button_held|attach::tests::abandoning_a_gesture_synthesizes_the_release_the_app_is_waiting_for|${ATTACH}|s.replace('        if let Some(bytes) = encode_mouse_report(cb, true, sgr, last.0, last.1) {\n            forward_bytes_to_pane(io_state, pane_id, bytes).await;\n        }', '')"
  "a_multi_button_gesture_ends_on_the_first_release|attach::tests::app_gesture_ends_only_when_every_button_is_up|${ATTACH}|s.replace('    *buttons = if bit != 0 && *buttons & bit != 0 {\n        *buttons & !bit\n    } else {\n        0\n    };', '    *buttons = 0;')"
  "an_unattributable_release_strands_the_gesture|attach::tests::app_gesture_ends_only_when_every_button_is_up|${ATTACH}|s.replace('    *buttons = if bit != 0 && *buttons & bit != 0 {\n        *buttons & !bit\n    } else {\n        0\n    };', '    *buttons &= !bit;')"
  "a_forwarded_click_no_longer_focuses_its_pane|attach::tests::a_click_in_a_second_pane_is_local_to_that_pane_and_focuses_it|${ATTACH}|s.replace('        if pane_id != attached.active_pane_id {\n            let _ = graph.focus_pane(pane_id).await;\n            session.lock().await.active_pane_id = pane_id;\n            redraw = true;\n        }', '')"
  "a_stale_selection_survives_a_forwarded_click|attach::tests::forwarding_a_press_clears_a_stale_shux_selection|${ATTACH}|s.replace('        if s.mouse_selection.is_some() || s.copy_menu.is_some() {\n            s.mouse_selection = None;\n            s.copy_menu = None;\n            redraw = true;\n        }', '')"
  "coordinates_stay_screen_global|attach::tests::a_click_in_a_second_pane_is_local_to_that_pane_and_focuses_it|${ATTACH}|s.replace('    let local_col = col\n        .saturating_sub(rect.x)', '    let local_col = col\n        .saturating_sub(0)')"

  # ── The wire ──────────────────────────────────────────────────────────────
  "an_older_clients_mouse_frame_stops_parsing|attach::tests::a_mouse_frame_without_modifiers_still_deserializes|${RPC}|s.replace('        #[serde(default)]\n        shift: bool,', '        shift: bool,')"

  # ── The pane viewport shared with the compositor ───────────────────────────
  "the_hit_test_insets_even_without_an_outline|attach::tests::the_pane_hit_test_agrees_with_the_compositor_under_every_border_style|${ATTACH}|s.replace('    shux_ui::pane_viewport(current_content_rect(client_size).await, border_style, false)', '''    let _ = border_style;\n    let (cols, rows) = *client_size.lock().await;\n    let content_h = rows.saturating_sub(STATUS_BAR_ROWS);\n    if cols >= 3 && content_h >= 3 {\n        Rect::new(1, 1, cols - 2, content_h - 2)\n    } else {\n        Rect::new(0, 0, cols, content_h)\n    }''')"
  # The defect that actually shipped: not the arithmetic, but the plumbing --
  # a cached style that the render loop only republished ON CHANGE, so a user
  # configured `none` who never edited it kept the default forever.
  "the_hit_test_ignores_the_configured_style|attach::tests::the_pane_hit_test_agrees_with_the_compositor_under_every_border_style|${ATTACH}|s.replace('    let border_style = BorderStyle::parse(&config.current().appearance.border_style);', '    let border_style = BorderStyle::default();')"
)

# `RUSTFLAGS=-Awarnings`, pinned for the WHOLE battery including the baseline.
# A mutation that deletes a guard usually leaves a parameter or field unused,
# and under the repo's default `-Dwarnings` that is a build failure, not a
# result — the battery would report "DID NOT COMPILE" for a mutation the tests
# would have caught cleanly. Pinned rather than toggled so the baseline and the
# mutated runs compile under identical flags.
run_suite() {
  export RUSTFLAGS=-Awarnings
  case "$1" in
    "${HANDLE}") make -s test-mutation-suite CRATE=shux-pty TARGET= FILTER= 2>&1 || true ;;
    "${RPC}") make -s test-mutation-suite CRATE=shux-rpc TARGET=--lib FILTER=attach::tests 2>&1 || true ;;
    *) make -s test-mutation-suite CRATE=shux TARGET=--lib FILTER=attach::tests 2>&1 || true ;;
  esac
}

failures_of() {
  printf '%s\n' "$1" | sed -n 's/^    \([a-z0-9_:]*\)$/\1/p' | sort -u | tr '\n' ' '
}

has_run() { printf '%s\n' "$1" | grep -q '^test result:'; }

declare -A BASELINE
echo "Baseline (unmutated):"
for f in "${HANDLE}" "${RPC}" "${ATTACH}"; do
  log="$(run_suite "${f}")"
  if ! has_run "${log}"; then
    echo "  ${f}: cargo never ran the tests — build or harness failure, not a result." >&2
    printf '%s\n' "${log}" | tail -30 >&2
    exit 2
  fi
  fails="$(failures_of "${log}")"
  if [ -n "${fails}" ]; then
    echo "  ${f}: ALREADY RED — ${fails}" >&2
    echo "Refusing to run: a red baseline credits every mutation in this suite." >&2
    exit 2
  fi
  BASELINE["${f}"]=""
  printf '  %-42s green\n' "${f}"
done
echo

printf '%-52s %s\n' "MUTATION" "KILLED BY"
printf '%-52s %s\n' "----------------------------------------------------" "---------"

survivors=()
for entry in "${MUTATIONS[@]}"; do
  name="${entry%%|*}"
  rest="${entry#*|}"
  anchor="${rest%%|*}"
  rest="${rest#*|}"
  file="${rest%%|*}"
  expr="${rest#*|}"

  restore
  if ! MUT_FILE="${file}" MUT_EXPR="${expr}" python3 - <<'PY'
import os, sys
path = os.environ["MUT_FILE"]
expr = os.environ["MUT_EXPR"]
s = open(path).read()
out = eval(expr, {"s": s})
if out == s:
    sys.stderr.write("mutation matched nothing\n")
    sys.exit(3)
open(path, "w").write(out)
PY
  then
    printf '%-52s %s\n' "${name}" "PATCH MATCHED NOTHING"
    survivors+=("${name} (patch matched nothing)")
    continue
  fi

  log="$(run_suite "${file}")"

  if ! has_run "${log}"; then
    if printf '%s\n' "${log}" | grep -q '^error'; then
      printf '%-52s %s\n' "${name}" "DID NOT COMPILE"
      survivors+=("${name} (did not compile)")
      continue
    fi
    echo "${name}: cargo never ran the tests and did not report a compile error." >&2
    printf '%s\n' "${log}" | tail -30 >&2
    exit 2
  fi

  killers=""
  for t in $(failures_of "${log}"); do
    case " ${BASELINE["${file}"]} " in *" ${t} "*) continue ;; esac
    killers="${killers}${t} "
  done
  killers="${killers% }"

  if [ -z "${killers}" ]; then
    printf '%-52s %s\n' "${name}" "SURVIVED"
    survivors+=("${name}")
  elif ! printf '%s\n' " ${killers} " | grep -qF " ${anchor} "; then
    printf '%-52s %s\n' "${name}" "ANCHOR MISSED (want ${anchor}, got ${killers})"
    survivors+=("${name} (anchor ${anchor} did not fire; failures: ${killers})")
  else
    printf '%-52s %s\n' "${name}" "${killers}"
  fi
done

restore
echo
if [ "${#survivors[@]}" -ne 0 ]; then
  echo "SURVIVORS (${#survivors[@]}):"
  printf '  - %s\n' "${survivors[@]}"
  exit 1
fi
echo "All ${#MUTATIONS[@]} mutations killed."
