//! A command's boundaries, its exit code, the output kept for it and what
//! the screen showed for each line the shell read do not depend on how the
//! pane's output was cut into reads.
#![no_main]
use libfuzzer_sys::fuzz_target;
use shux_vt::{CommandLookup, VirtualTerminal};

type Observed = (
    &'static str,
    u64,
    Vec<(Option<i32>, &'static str, Option<(Vec<u8>, bool)>)>,
    Option<Vec<u8>>,
    Option<Vec<String>>,
);

fn drive(chunks: &[&[u8]]) -> Observed {
    let mut vt = VirtualTerminal::new(8, 30);
    let floor = shux_vt::program_status_as_of();
    for c in chunks {
        vt.process_with_responses(c);
    }
    let mut ended = Vec::new();
    let mut at = floor;
    while let CommandLookup::Found(c) = vt.command_after(at) {
        at = c.started;
        let out = vt.command_output(c.started).map(|o| {
            assert!(o.bytes.len() <= shux_vt::COMMAND_OUTPUT_LIMIT);
            (o.bytes.to_vec(), o.truncated)
        });
        ended.push((c.exit_code, c.ended_by.as_str(), out));
    }
    let running = vt.running_command_output(floor).map(|o| o.bytes.to_vec());
    // What the screen shows for malformed UTF-8 depends on where vte's reads
    // were cut; a typed command is always well-formed.
    let submitted = std::str::from_utf8(chunks.concat().as_slice())
        .is_ok()
        .then(|| vt.submitted_after(floor))
        .flatten()
        .map(|lines| lines.iter().map(|s| format!("{s:?}")).collect());
    (vt.shell_state().as_str(), vt.prompts_seen(), ended, running, submitted)
}

fuzz_target!(|input: (u16, u16, &[u8])| {
    let (a, b, bytes) = input;
    if bytes.len() > 4096 {
        return;
    }
    let (mut i, mut j) = (usize::from(a) % (bytes.len() + 1), usize::from(b) % (bytes.len() + 1));
    if i > j {
        std::mem::swap(&mut i, &mut j);
    }
    let whole = drive(&[bytes]);
    assert_eq!(whole, drive(&[&bytes[..i], &bytes[i..j], &bytes[j..]]));
    let bytewise: Vec<&[u8]> = bytes.chunks(1).collect();
    assert_eq!(whole, drive(&bytewise));
});
