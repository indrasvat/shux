use proptest::prelude::*;
use shux_vt::VirtualTerminal;

fn final_text_for_chunks(input: &[u8], split: usize) -> String {
    let mut vt = VirtualTerminal::new(24, 80);
    let split = split.min(input.len());
    vt.process(&input[..split]);
    vt.process(&input[split..]);
    vt.capture_text(None)
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 32,
        failure_persistence: None,
        .. ProptestConfig::default()
    })]

    #[test]
    fn replay_is_invariant_across_chunk_boundaries(split in 0usize..512) {
        let input = b"\x1b[31mred\x1b[0m plain \x1b[10;20Hcursor\r\nnext";
        let full = final_text_for_chunks(input, input.len());
        let chunked = final_text_for_chunks(input, split);
        prop_assert_eq!(chunked, full);
    }
}

#[test]
fn replay_handles_invalid_bytes_without_panicking() {
    let mut vt = VirtualTerminal::new(8, 24);
    vt.process(b"before");
    vt.process(&[0xff, 0x80, 0x1b, b'[', b'3']);
    let text = vt.capture_text(None);
    assert!(text.contains("before"));
}

#[test]
fn replay_response_fixture_is_deterministic() {
    let mut vt = VirtualTerminal::new(24, 80);
    let responses = vt.process_with_responses(b"\x1b[5;10H\x1b[6n\x1b[5n");
    assert_eq!(responses, vec![b"\x1b[5;10R".to_vec(), b"\x1b[0n".to_vec()]);
}
