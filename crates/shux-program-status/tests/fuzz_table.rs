//! Tier-1 model-based fuzzing of `StatusTable`.
//!
//! Random operation sequences run against the real table and against a
//! deliberately naive reference built from the spec text; after every step
//! the two must agree on contents, order, summary and whether the revision
//! moved. Ids come from a tiny alphabet so parents, children and subtree
//! clears collide constantly, and long runs push past the record cap.

use std::collections::BTreeMap;

use proptest::prelude::*;
use shux_program_status::limits::MAX_RECORDS;
use shux_program_status::{Record, RecordId, Report, State, StatusTable};

#[derive(Debug, Clone)]
enum Op {
    Set(Option<String>, Record),
    Clear(Option<String>),
    Prompt,
    Exit,
    Reset,
}

#[derive(Debug, Clone)]
struct ModelEntry {
    record: Record,
    reported: u64,
    updated: u64,
}

/// Key `None` is the root record.
#[derive(Default)]
struct Model {
    entries: BTreeMap<Option<String>, ModelEntry>,
    seq: u64,
}

fn within(id: &str, prefix: &str) -> bool {
    id == prefix || id.starts_with(&format!("{prefix}/"))
}

fn urgency(s: State) -> u8 {
    match s {
        State::Blocked => 4,
        State::Error => 3,
        State::Done => 2,
        State::Working => 1,
        State::Idle => 0,
    }
}

impl Model {
    /// Applies `op`; returns whether the stored contents changed.
    fn apply(&mut self, op: &Op) -> bool {
        let before: Vec<(Option<String>, Record)> = self.snapshot();
        match op {
            Op::Set(id, record) => {
                self.seq += 1;
                let seq = self.seq;
                if let Some(e) = self.entries.get_mut(id) {
                    e.reported = seq;
                    if e.record != *record {
                        e.record = record.clone();
                        e.updated = seq;
                    }
                } else {
                    if self.entries.len() >= MAX_RECORDS {
                        let victim = self
                            .entries
                            .iter()
                            .min_by_key(|(_, e)| e.reported)
                            .map(|(k, _)| k.clone())
                            .unwrap();
                        self.entries.remove(&victim);
                    }
                    self.entries.insert(
                        id.clone(),
                        ModelEntry {
                            record: record.clone(),
                            reported: seq,
                            updated: seq,
                        },
                    );
                }
            }
            Op::Clear(None) | Op::Reset => self.entries.clear(),
            Op::Clear(Some(prefix)) => self
                .entries
                .retain(|k, _| !k.as_deref().is_some_and(|id| within(id, prefix))),
            Op::Prompt | Op::Exit => self
                .entries
                .retain(|_, e| matches!(e.record.state, State::Done | State::Error)),
        }
        before != self.snapshot()
    }

    fn snapshot(&self) -> Vec<(Option<String>, Record)> {
        self.entries
            .iter()
            .map(|(k, e)| (k.clone(), e.record.clone()))
            .collect()
    }

    fn ordered_ids(&self) -> Vec<Option<String>> {
        let mut v: Vec<_> = self.entries.iter().collect();
        v.sort_by(|(ka, a), (kb, b)| {
            urgency(b.record.state)
                .cmp(&urgency(a.record.state))
                .then(kb.is_none().cmp(&ka.is_none()))
                .then(b.updated.cmp(&a.updated))
        });
        v.into_iter().map(|(k, _)| k.clone()).collect()
    }

    fn app_of(&self, id: &Option<String>) -> Option<String> {
        let mut cursor = id.clone();
        loop {
            if let Some(app) = self.entries.get(&cursor).and_then(|e| e.record.app.clone()) {
                return Some(app);
            }
            let c = cursor?;
            cursor = c.rsplit_once('/').map(|(p, _)| p.to_owned());
        }
    }
}

fn id_strategy() -> impl Strategy<Value = Option<String>> {
    prop::option::weighted(
        0.85,
        prop::collection::vec(prop::sample::select(vec!["a", "ab", "b"]), 1..=3)
            .prop_map(|v| v.join("/")),
    )
}

fn record_strategy() -> impl Strategy<Value = Record> {
    (
        prop::sample::select(State::ALL.to_vec()),
        prop::option::of(prop::sample::select(vec!["x", "y"])),
        prop::option::of(prop::sample::select(vec!["m1", "m2"])),
    )
        .prop_map(|(state, app, msg)| Record {
            state,
            kind: None,
            progress: None,
            app: app.map(str::to_owned),
            title: None,
            msg: msg.map(str::to_owned),
        })
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        12 => (id_strategy(), record_strategy()).prop_map(|(i, r)| Op::Set(i, r)),
        2 => id_strategy().prop_map(Op::Clear),
        1 => Just(Op::Prompt),
        1 => Just(Op::Exit),
        1 => Just(Op::Reset),
    ]
}

/// Many distinct ids so the 256-record cap and eviction are exercised.
fn wide_set_strategy() -> impl Strategy<Value = Op> {
    (0u16..400, record_strategy()).prop_map(|(n, r)| Op::Set(Some(format!("w{n}")), r))
}

fn to_report(op: &Op) -> Option<Report> {
    let id = |s: &Option<String>| s.as_deref().map(|s| RecordId::parse(s).unwrap());
    match op {
        Op::Set(i, r) => Some(Report::Set {
            id: id(i),
            record: r.clone(),
        }),
        Op::Clear(i) => Some(Report::Clear { id: id(i) }),
        _ => None,
    }
}

fn run(ops: &[Op]) -> Result<(), TestCaseError> {
    let mut table = StatusTable::new();
    let mut model = Model::default();
    for (step, op) in ops.iter().enumerate() {
        let rev = table.revision();
        match to_report(op) {
            Some(r) => table.apply(r),
            None => match op {
                Op::Prompt => table.on_prompt_start(),
                Op::Exit => table.on_process_exit(),
                _ => table.on_full_reset(),
            },
        }
        let changed = model.apply(op);
        prop_assert_eq!(table.revision() != rev, changed, "step {} {:?}", step, op);
        prop_assert!(table.len() <= MAX_RECORDS);

        let views = table.records();
        let got: Vec<Option<String>> = views
            .iter()
            .map(|v| v.id.as_ref().map(|i| i.as_str().to_owned()))
            .collect();
        prop_assert_eq!(&got, &model.ordered_ids(), "order at step {}", step);
        for v in &views {
            let key = v.id.as_ref().map(|i| i.as_str().to_owned());
            let e = &model.entries[&key];
            prop_assert_eq!(v.state, e.record.state);
            prop_assert_eq!(&v.msg, &e.record.msg);
            prop_assert_eq!(&v.app, &model.app_of(&key), "inherited app for {:?}", key);
        }
        match table.summary() {
            None => prop_assert!(model.entries.is_empty()),
            Some(s) => {
                let top = model.ordered_ids()[0].clone();
                prop_assert_eq!(s.id.as_ref().map(|i| i.as_str().to_owned()), top.clone());
                prop_assert_eq!(s.app, model.app_of(&top));
                let total = s.counts.idle
                    + s.counts.working
                    + s.counts.done
                    + s.counts.blocked
                    + s.counts.error;
                prop_assert_eq!(usize::from(total), model.entries.len());
            }
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn table_matches_the_reference_model(ops in prop::collection::vec(op_strategy(), 0..200)) {
        run(&ops)?;
    }

    #[test]
    fn table_matches_the_reference_model_past_the_cap(
        ops in prop::collection::vec(prop_oneof![9 => wide_set_strategy(), 1 => op_strategy()], 250..600)
    ) {
        run(&ops)?;
    }
}
