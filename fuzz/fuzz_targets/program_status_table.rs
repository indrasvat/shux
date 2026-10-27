//! Structured report sequences against the record table's invariants.
#![no_main]
use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use shux_program_status::limits::MAX_RECORDS;
use shux_program_status::wire::{Message, decode};
use shux_program_status::StatusTable;

#[derive(Arbitrary, Debug)]
enum Op {
    Report { id: u8, depth: u8, state: u8, app: Option<u8>, extra: Vec<u8> },
    Clear { id: Option<(u8, u8)> },
    Prompt,
    Exit,
    Reset,
}

fn path(id: u8, depth: u8) -> String {
    // A tiny alphabet so parents, children and prefixes collide.
    (0..=depth % 3)
        .map(|d| ["a", "ab", "b"][usize::from(id.wrapping_add(d)) % 3])
        .collect::<Vec<_>>()
        .join("/")
}

fuzz_target!(|ops: Vec<Op>| {
    let mut table = StatusTable::new();
    for op in ops {
        let rev = table.revision();
        match op {
            Op::Report { id, depth, state, app, extra } => {
                let state = ["idle", "working", "done", "blocked", "error"][usize::from(state) % 5];
                let mut p = format!("state={state}");
                if id % 4 != 0 {
                    p.push_str(&format!(":id={}", path(id, depth)));
                } else {
                    // Many distinct ids push past the cap.
                    p.push_str(&format!(":id=w{id}{depth}"));
                }
                if let Some(a) = app {
                    p.push_str(&format!(":app=x{}", a % 3));
                }
                let mut bytes = p.into_bytes();
                bytes.extend(extra.into_iter().take(32));
                if let Ok(Message::Report(r)) = decode(&bytes) {
                    table.apply(r);
                }
            }
            Op::Clear { id } => {
                let p = match id {
                    Some((i, d)) => format!("state=clear:id={}", path(i, d)),
                    None => "state=clear".to_owned(),
                };
                if let Ok(Message::Report(r)) = decode(p.as_bytes()) {
                    table.apply(r);
                }
            }
            Op::Prompt => table.on_prompt_start(),
            Op::Exit => table.on_process_exit(),
            Op::Reset => table.on_full_reset(),
        }
        assert!(table.revision() >= rev);
        assert!(table.len() <= MAX_RECORDS);
        let records = table.records();
        assert_eq!(records.len(), table.len());
        match table.summary() {
            None => assert!(table.is_empty()),
            Some(s) => {
                assert_eq!(Some(s.state), records.first().map(|r| r.state));
                let c = s.counts;
                let total = c.idle + c.working + c.done + c.blocked + c.error;
                assert_eq!(usize::from(total), table.len());
            }
        }
    }
});
