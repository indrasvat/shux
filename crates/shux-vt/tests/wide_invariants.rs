use proptest::prelude::*;
use shux_vt::{Grid, VirtualTerminal};

fn assert_grid_wide_invariants(grid: &Grid) {
    for row_idx in 0..grid.total_lines() {
        let row = grid.row(row_idx).expect("row exists");
        for col in 0..row.len() {
            let cell = &row[col];
            if cell.is_wide_continuation() {
                assert_eq!(
                    cell.ch, ' ',
                    "continuation at row {row_idx} col {col} carries glyph"
                );
                assert!(col > 0, "orphan continuation at row {row_idx} col 0");
                assert!(
                    row[col - 1].is_wide(),
                    "orphan continuation at row {row_idx} col {col}"
                );
            }
            if cell.is_wide() {
                assert!(
                    col + 1 < row.len(),
                    "wide head at row {row_idx} final col {col}"
                );
                assert!(
                    row[col + 1].is_wide_continuation(),
                    "wide head at row {row_idx} col {col} missing tail"
                );
            }
        }
    }
}

fn process_csi(vt: &mut VirtualTerminal, sequence: String) {
    vt.process(sequence.as_bytes());
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        failure_persistence: None,
        .. ProptestConfig::default()
    })]

    #[test]
    fn wide_cell_invariants_hold_after_operation_sequences(
        ops in prop::collection::vec((0u8..14, 0u8..64), 1..128)
    ) {
        let mut vt = VirtualTerminal::new(4, 8);

        for (op, arg) in ops {
            match op {
                0 => vt.process(b"A"),
                1 => vt.process("界".as_bytes()),
                2 => vt.process("好".as_bytes()),
                3 => process_csi(
                    &mut vt,
                    format!("\x1b[{};{}H", (arg as usize % 4) + 1, (arg as usize % 8) + 1),
                ),
                4 => process_csi(&mut vt, format!("\x1b[{}@", (arg as usize % 4) + 1)),
                5 => process_csi(&mut vt, format!("\x1b[{}P", (arg as usize % 4) + 1)),
                6 => process_csi(&mut vt, format!("\x1b[{}X", (arg as usize % 4) + 1)),
                7 => vt.process(b"\x1b[0K"),
                8 => vt.process(b"\x1b[1K"),
                9 => vt.process(b"\x1b[2K"),
                10 => vt.process(b"\r\n"),
                11 => vt.resize((arg as usize % 5) + 1, (arg as usize % 10) + 1),
                12 => {
                    if vt.is_alternate_screen() {
                        vt.process(b"\x1b[?1049l");
                    } else {
                        vt.process(b"\x1b[?1049h");
                    }
                }
                _ => process_csi(&mut vt, format!("\x1b[{}b", (arg as usize % 3) + 1)),
            }

            assert_grid_wide_invariants(vt.grid());
        }
    }
}
