//! Tier-1 fuzzing of the payload codec.
//!
//! `spec_decode` is a second decoder written from the spec text alone — its
//! own base64, its own grammar checks — so the differential below catches a
//! codec that is self-consistent but wrong. Inputs are grammar-built reports
//! with point mutations, which reach the deep branches that random bytes do
//! not.

use proptest::prelude::*;
use shux_program_status::limits::*;
use shux_program_status::wire::{Message, decode, encode_payload};
use shux_program_status::{Kind, Record, RecordId, Report, State};

/// What a decoder concluded, in a shape both decoders can produce.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    Query,
    Discard,
    Clear(Option<String>),
    Set {
        id: Option<String>,
        state: String,
        kind: Option<String>,
        progress: Option<u8>,
        app: Option<String>,
        title: Option<String>,
        msg: Option<String>,
    },
}

fn crate_outcome(payload: &[u8]) -> Outcome {
    match decode(payload) {
        Err(_) => Outcome::Discard,
        Ok(Message::Query) => Outcome::Query,
        Ok(Message::Report(Report::Clear { id })) => {
            Outcome::Clear(id.map(|i| i.as_str().to_owned()))
        }
        Ok(Message::Report(Report::Set { id, record })) => Outcome::Set {
            id: id.map(|i| i.as_str().to_owned()),
            state: record.state.as_str().to_owned(),
            kind: record.kind.map(|k| k.as_str().to_owned()),
            progress: record.progress,
            app: record.app,
            title: record.title,
            msg: record.msg,
        },
    }
}

// ---- the spec model ------------------------------------------------------

const VALUE_SET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_.,+/=-";
const ID_SET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_.+-";
const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn trim_ws(b: &[u8]) -> &[u8] {
    let mut s = b;
    while let [b' ' | b'\t', rest @ ..] = s {
        s = rest;
    }
    while let [rest @ .., b' ' | b'\t'] = s {
        s = rest;
    }
    s
}

/// Standard base64, padding optional, trailing bits must be zero.
fn spec_b64(v: &[u8]) -> Option<Vec<u8>> {
    let body: &[u8] = match v.iter().position(|&c| c == b'=') {
        None => v,
        Some(p) => {
            let pad = &v[p..];
            if pad.iter().any(|&c| c != b'=') || !v.len().is_multiple_of(4) || pad.len() > 2 {
                return None;
            }
            &v[..p]
        }
    };
    if body.len() % 4 == 1 {
        return None;
    }
    let mut bits: u32 = 0;
    let mut nbits = 0;
    let mut out = Vec::new();
    for &c in body {
        let d = B64.iter().position(|&x| x == c)? as u32;
        bits = (bits << 6) | d;
        nbits += 6;
        if nbits >= 8 {
            nbits -= 8;
            out.push((bits >> nbits) as u8);
            bits &= (1 << nbits) - 1;
        }
    }
    (bits == 0).then_some(out)
}

fn spec_id(v: &[u8]) -> bool {
    if v.is_empty() || v.len() > 128 {
        return false;
    }
    let segs: Vec<&[u8]> = v.split(|&c| c == b'/').collect();
    segs.len() <= 8
        && segs
            .iter()
            .all(|s| (1..=32).contains(&s.len()) && s.iter().all(|c| ID_SET.contains(c)))
}

enum Text {
    Absent,
    Present(String),
    Bad,
}

fn spec_text(v: &[u8], max_decoded: usize) -> Text {
    if v.is_empty() {
        return Text::Absent;
    }
    let Some(bytes) = spec_b64(v) else {
        return Text::Bad;
    };
    if bytes.len() > max_decoded {
        return Text::Bad;
    }
    let Ok(s) = String::from_utf8(bytes) else {
        return Text::Bad;
    };
    let control = |c: char| matches!(c as u32, 0x00..=0x1f | 0x7f | 0x80..=0x9f);
    if s.chars().any(control) {
        return Text::Bad;
    }
    if s.is_empty() {
        Text::Absent
    } else {
        Text::Present(s)
    }
}

fn spec_decode(p: &[u8]) -> Outcome {
    if p.len() > 4096 - 9 {
        return Outcome::Discard;
    }
    if trim_ws(p) == b"?" {
        return Outcome::Query;
    }
    let (mut state, mut id, mut kind, mut progress, mut app, mut title, mut msg) =
        (None, None, None, None, None, None, None);
    for pair in p.split(|&c| c == b':') {
        let Some(eq) = pair.iter().position(|&c| c == b'=') else {
            continue;
        };
        let (k, v) = (trim_ws(&pair[..eq]), trim_ws(&pair[eq + 1..]));
        if k.is_empty() {
            continue;
        }
        if k.len() > 16 {
            return Outcome::Discard;
        }
        if !k.iter().all(|c| c.is_ascii_lowercase()) {
            continue;
        }
        let cap = match k {
            b"msg" => 2732,
            b"title" => 256,
            b"app" => 32,
            b"id" => 128,
            _ => usize::MAX,
        };
        if v.len() > cap {
            return Outcome::Discard;
        }
        if k == b"id" {
            if !spec_id(v) {
                return Outcome::Discard;
            }
            id = Some(String::from_utf8(v.to_vec()).unwrap());
            continue;
        }
        if !v.iter().all(|c| VALUE_SET.contains(c)) {
            continue;
        }
        let s = String::from_utf8(v.to_vec()).unwrap();
        match k {
            b"state" => state = Some(s),
            b"kind" => {
                kind = ["permission", "question", "auth"]
                    .contains(&s.as_str())
                    .then_some(s)
            }
            b"progress" => {
                progress = (!s.is_empty() && s.len() <= 3 && s.bytes().all(|c| c.is_ascii_digit()))
                    .then(|| s.parse::<u8>().ok())
                    .flatten()
                    .filter(|&n| n <= 100)
            }
            b"app" => app = (!s.is_empty() && s.bytes().all(|c| ID_SET.contains(&c))).then_some(s),
            b"title" | b"msg" => {
                let max = if k == b"msg" { 2048 } else { 192 };
                let slot = if k == b"msg" { &mut msg } else { &mut title };
                match spec_text(v, max) {
                    Text::Bad => return Outcome::Discard,
                    Text::Absent => *slot = None,
                    Text::Present(t) => *slot = Some(t),
                }
            }
            _ => {}
        }
    }
    let Some(state) = state else {
        return Outcome::Discard;
    };
    if state == "clear" {
        return Outcome::Clear(id);
    }
    if !["idle", "working", "done", "blocked", "error"].contains(&state.as_str()) {
        return Outcome::Discard;
    }
    Outcome::Set {
        kind: kind.filter(|_| state == "blocked"),
        progress: progress.filter(|_| state == "working" || state == "blocked"),
        id,
        state,
        app,
        title,
        msg,
    }
}

// ---- generators ----------------------------------------------------------

fn b64(s: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(s)
}

/// A value for `key` that sits near the interesting boundaries.
fn value_for(key: &'static str) -> BoxedStrategy<String> {
    let text = |max: usize| {
        prop_oneof![
            "[ -~]{0,24}".prop_map(|s| b64(&s)),
            "[ -~]{0,24}".prop_map(|s| b64(&s).trim_end_matches('=').to_owned()),
            (max - 2..=max + 2).prop_map(|n| b64(&"x".repeat(n))),
            "[\\x00-\\x1f\\x7f\\u{80}-\\u{9f}]".prop_map(|c| b64(&format!("a{c}b"))),
            Just("b2t".to_owned()),
            Just("a".to_owned()),
            Just(String::new()),
        ]
        .boxed()
    };
    match key {
        "state" => prop_oneof![
            prop::sample::select(vec!["idle", "working", "done", "blocked", "error", "clear"])
                .prop_map(str::to_owned),
            "[a-zA-Z]{0,8}",
        ]
        .boxed(),
        "kind" => prop_oneof![
            prop::sample::select(vec!["permission", "question", "auth"]).prop_map(str::to_owned),
            "[a-z]{0,6}",
        ]
        .boxed(),
        "progress" => {
            prop_oneof![(0u16..=102).prop_map(|n| n.to_string()), "[-+0-9_.]{0,5}",].boxed()
        }
        "app" => prop_oneof!["[A-Za-z0-9_.+-]{0,34}", "[a-z,/=@]{1,6}"].boxed(),
        "id" => prop_oneof![
            prop::collection::vec("[a-z0-9]{1,3}", 1..10).prop_map(|v| v.join("/")),
            (30usize..=34).prop_map(|n| "s".repeat(n)),
            Just(vec!["x".repeat(32); 4].join("/")),
            "[a-z/@.]{0,8}",
        ]
        .boxed(),
        "title" => text(MAX_TITLE_DECODED),
        "msg" => text(MAX_MSG_DECODED).boxed(),
        _ => "[A-Za-z0-9=]{0,6}".boxed(),
    }
}

fn pair() -> impl Strategy<Value = String> {
    prop::sample::select(vec![
        "state", "state", "state", "id", "kind", "progress", "app", "title", "msg", "future",
    ])
    .prop_flat_map(|k| {
        (Just(k), value_for(k), " {0,1}").prop_map(|(k, v, ws)| format!("{ws}{k}{ws}={ws}{v}"))
    })
}

#[derive(Debug, Clone)]
enum Mutation {
    None,
    Flip(usize, u8),
    Truncate(usize),
    Insert(usize, u8),
}

fn mutation() -> impl Strategy<Value = Mutation> {
    prop_oneof![
        4 => Just(Mutation::None),
        1 => (any::<usize>(), any::<u8>()).prop_map(|(i, b)| Mutation::Flip(i, b)),
        1 => any::<usize>().prop_map(Mutation::Truncate),
        1 => (any::<usize>(), prop::sample::select(b":=;/ ?\t".to_vec()))
            .prop_map(|(i, b)| Mutation::Insert(i, b)),
    ]
}

fn mutate(mut p: Vec<u8>, m: &Mutation) -> Vec<u8> {
    match *m {
        Mutation::None => {}
        Mutation::Flip(i, b) if !p.is_empty() => {
            let n = p.len();
            p[i % n] = b;
        }
        Mutation::Truncate(i) if !p.is_empty() => {
            let n = p.len();
            p.truncate(i % n);
        }
        Mutation::Insert(i, b) => {
            let n = p.len() + 1;
            p.insert(i % n, b);
        }
        _ => {}
    }
    p
}

fn report() -> impl Strategy<Value = Report> {
    let id = prop::option::of(
        prop::collection::vec("[A-Za-z0-9_.+-]{1,12}", 1..=4)
            .prop_map(|v| RecordId::parse(&v.join("/")).unwrap()),
    );
    let text = |max: usize| {
        prop::option::of(
            proptest::string::string_regex(&format!("[^\\x00-\\x1f\\x7f-\\u{{9f}}]{{1,{max}}}"))
                .unwrap(),
        )
    };
    let state = prop::sample::select(State::ALL.to_vec());
    let set = (
        id.clone(),
        state,
        prop::option::of(prop::sample::select(Kind::ALL.to_vec())),
        prop::option::of(0u8..=100),
        prop::option::of("[A-Za-z0-9_.+-]{1,32}"),
        text(40),
        text(200),
    )
        .prop_map(|(id, state, kind, progress, app, title, msg)| Report::Set {
            id,
            record: Record {
                state,
                kind: kind.filter(|_| state == State::Blocked),
                progress: progress.filter(|_| matches!(state, State::Working | State::Blocked)),
                app,
                title,
                msg,
            },
        });
    prop_oneof![4 => set, 1 => id.prop_map(|id| Report::Clear { id })]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4096))]

    #[test]
    fn crate_decoder_agrees_with_the_spec_model(
        pairs in prop::collection::vec(pair(), 0..7),
        m in mutation(),
    ) {
        let payload = mutate(pairs.join(":").into_bytes(), &m);
        prop_assert_eq!(crate_outcome(&payload), spec_decode(&payload), "payload {:?}",
            String::from_utf8_lossy(&payload));
    }

    #[test]
    fn crate_decoder_agrees_with_the_spec_model_on_raw_bytes(
        bytes in prop::collection::vec(
            prop_oneof![any::<u8>(), prop::sample::select(b"state=idle:=?/ ".to_vec())], 0..300),
    ) {
        prop_assert_eq!(crate_outcome(&bytes), spec_decode(&bytes));
    }

    #[test]
    fn every_valid_report_round_trips(r in report()) {
        let payload = encode_payload(&r);
        prop_assert!(payload.len() <= MAX_PAYLOAD_BYTES);
        prop_assert_eq!(decode(&payload), Ok(Message::Report(r)));
    }
}

#[test]
fn the_spec_model_itself_rejects_what_the_spec_says() {
    // Guards the oracle: a model that accepted everything would make the
    // differential vacuous.
    for bad in [
        "state=done:msg=b2t",
        "state=done:id=a//b",
        "state=clear:id=a@b",
        "app=x",
        "state=Idle",
    ] {
        assert_eq!(spec_decode(bad.as_bytes()), Outcome::Discard, "{bad}");
    }
    assert_eq!(spec_b64(b"b2s"), Some(b"ok".to_vec()));
    assert_eq!(spec_b64(b"b2s="), Some(b"ok".to_vec()));
    assert_eq!(spec_b64(b"b2t"), None);
}
