use std::time::Instant;

use shux_vt::{Cell, VirtualTerminal};

fn main() {
    let rows = 24;
    let cols = 80;
    let scrollback_lines = 5_000;
    let capture_iters = 1_000;

    let mut vt = VirtualTerminal::new(rows, cols);
    let line = "ascii performance line 0123456789 abcdefghijklmnopqrstuvwxyz ABCDEFGHIJKLMNOP\n";
    for _ in 0..(scrollback_lines + rows) {
        vt.process(line.as_bytes());
    }

    let warmup = vt.capture_text(None);
    assert!(!warmup.is_empty());

    let started = Instant::now();
    let mut total_bytes = 0usize;
    for _ in 0..capture_iters {
        total_bytes += vt.capture_text(None).len();
    }
    let elapsed = started.elapsed();
    let seconds = elapsed.as_secs_f64();
    let captures_per_second = capture_iters as f64 / seconds.max(f64::EPSILON);
    let bytes_per_second = total_bytes as f64 / seconds.max(f64::EPSILON);

    println!(
        concat!(
            "{{",
            "\"rows\":{rows},",
            "\"cols\":{cols},",
            "\"scrollback_lines\":{scrollback_lines},",
            "\"capture_iters\":{capture_iters},",
            "\"cell_size_bytes\":{cell_size},",
            "\"elapsed_seconds\":{elapsed:.9},",
            "\"captures_per_second\":{captures_per_second:.3},",
            "\"bytes_per_second\":{bytes_per_second:.3}",
            "}}"
        ),
        rows = rows,
        cols = cols,
        scrollback_lines = scrollback_lines,
        capture_iters = capture_iters,
        cell_size = std::mem::size_of::<Cell>(),
        elapsed = seconds,
        captures_per_second = captures_per_second,
        bytes_per_second = bytes_per_second
    );
}
