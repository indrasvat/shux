use crate::limits::MAX_RECORDS;
use crate::model::{Record, RecordId, RecordView, Report};
use crate::rank::order_key;
use crate::summary::{StateCounts, StatusSummary};

/// One terminal's records. Records belong to the terminal, not to a screen.
#[derive(Debug, Clone, Default)]
pub struct StatusTable {
    entries: Vec<Entry>,
    seq: u64,
    revision: u64,
}

#[derive(Debug, Clone)]
struct Entry {
    id: Option<RecordId>,
    record: Record,
    /// Last report naming this record, identical or not; drives eviction.
    reported: u64,
    /// Last time the content changed; drives display order.
    updated: u64,
}

impl StatusTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bumps whenever the stored records change. Never bumps for a report
    /// that repeats what is already stored.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn apply(&mut self, report: Report) {
        match report {
            Report::Set { id, record } => self.set(id, record),
            Report::Clear { id: None } => self.retain(|_| false),
            Report::Clear { id: Some(id) } => {
                self.retain(|e| !e.id.as_ref().is_some_and(|eid| eid.is_within(&id)))
            }
        }
    }

    /// OSC 133 `A`: a new shell prompt began.
    pub fn on_prompt_start(&mut self) {
        self.retain(|e| e.record.state.survives_exit());
    }

    /// The process attached to the terminal exited.
    pub fn on_process_exit(&mut self) {
        self.retain(|e| e.record.state.survives_exit());
    }

    /// RIS. DECSTR leaves records alone.
    pub fn on_full_reset(&mut self) {
        self.retain(|_| false);
    }

    /// Every record, most urgent first, with `app` inherited.
    pub fn records(&self) -> Vec<RecordView> {
        let mut order: Vec<&Entry> = self.entries.iter().collect();
        order.sort_by_key(|e| order_key(e.record.state, e.id.is_none(), e.updated));
        order.into_iter().map(|e| self.view(e)).collect()
    }

    /// `None` when there are no records.
    pub fn summary(&self) -> Option<StatusSummary> {
        let top = self
            .entries
            .iter()
            .min_by_key(|e| order_key(e.record.state, e.id.is_none(), e.updated))?;
        let mut counts = StateCounts::default();
        for e in &self.entries {
            counts.add(e.record.state);
        }
        Some(StatusSummary {
            state: top.record.state,
            kind: top.record.kind,
            progress: top.record.progress,
            app: self.effective_app(top).map(str::to_owned),
            id: top.id.clone(),
            counts,
        })
    }

    fn set(&mut self, id: Option<RecordId>, record: Record) {
        self.seq += 1;
        let seq = self.seq;
        if let Some(e) = self.entries.iter_mut().find(|e| e.id == id) {
            e.reported = seq;
            if e.record != record {
                e.record = record;
                e.updated = seq;
                self.revision += 1;
            }
            return;
        }
        if self.entries.len() >= MAX_RECORDS
            && let Some(oldest) = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| e.reported)
                .map(|(i, _)| i)
        {
            self.entries.swap_remove(oldest);
        }
        self.entries.push(Entry {
            id,
            record,
            reported: seq,
            updated: seq,
        });
        self.revision += 1;
    }

    fn retain(&mut self, keep: impl Fn(&Entry) -> bool) {
        let before = self.entries.len();
        self.entries.retain(keep);
        if self.entries.len() != before {
            self.revision += 1;
        }
    }

    /// A record without `app` takes it from its nearest ancestor that has
    /// one; the root record is every id's outermost ancestor.
    fn effective_app<'a>(&'a self, entry: &'a Entry) -> Option<&'a str> {
        if let Some(app) = entry.record.app.as_deref() {
            return Some(app);
        }
        // `None` here is the root, searched last.
        let mut ancestor = entry.id.as_ref()?.parent();
        loop {
            let app = self
                .entries
                .iter()
                .find(|e| e.id == ancestor)
                .and_then(|e| e.record.app.as_deref());
            if app.is_some() {
                return app;
            }
            ancestor = ancestor?.parent();
        }
    }

    fn view(&self, e: &Entry) -> RecordView {
        RecordView {
            id: e.id.clone(),
            state: e.record.state,
            kind: e.record.kind,
            progress: e.record.progress,
            app: self.effective_app(e).map(str::to_owned),
            title: e.record.title.clone(),
            msg: e.record.msg.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::MAX_RECORDS;
    use crate::model::{Kind, State};
    use crate::wire::{Message, decode};

    fn apply(t: &mut StatusTable, payload: &str) {
        match decode(payload.as_bytes()) {
            Ok(Message::Report(r)) => t.apply(r),
            other => panic!("{payload:?}: {other:?}"),
        }
    }

    fn ids(t: &StatusTable) -> Vec<Option<String>> {
        t.records()
            .into_iter()
            .map(|r| r.id.map(|i| i.as_str().to_owned()))
            .collect()
    }

    #[test]
    fn a_report_replaces_its_record_completely() {
        let mut t = StatusTable::new();
        apply(&mut t, "state=working:progress=40:app=cargo");
        apply(&mut t, "state=done");
        let r = &t.records()[0];
        assert_eq!(r.state, State::Done);
        assert_eq!(r.progress, None);
        assert_eq!(
            r.app, None,
            "a key missing from the report is missing afterwards"
        );
    }

    #[test]
    fn root_and_children_coexist() {
        let mut t = StatusTable::new();
        apply(&mut t, "state=working");
        apply(&mut t, "state=working:id=a");
        apply(&mut t, "state=working:id=a/b");
        assert_eq!(t.len(), 3);
    }

    #[test]
    fn clear_removes_the_subtree_and_bare_clear_removes_everything() {
        let mut t = StatusTable::new();
        for p in [
            "state=idle",
            "state=idle:id=a",
            "state=idle:id=a/b",
            "state=idle:id=ab",
        ] {
            apply(&mut t, p);
        }
        apply(&mut t, "state=clear:id=a");
        assert_eq!(ids(&t), vec![None, Some("ab".into())]);
        apply(&mut t, "state=clear");
        assert!(t.is_empty());
    }

    #[test]
    fn identical_reports_do_not_bump_the_revision() {
        let mut t = StatusTable::new();
        apply(&mut t, "state=working:app=x");
        let rev = t.revision();
        apply(&mut t, "state=working:app=x");
        assert_eq!(t.revision(), rev);
        apply(&mut t, "state=working:app=y");
        assert!(t.revision() > rev);
        let rev = t.revision();
        apply(&mut t, "state=clear:id=nothing-here");
        assert_eq!(
            t.revision(),
            rev,
            "a clear that removes nothing changes nothing"
        );
    }

    #[test]
    fn at_the_cap_the_least_recently_reported_record_is_evicted() {
        let mut t = StatusTable::new();
        for i in 0..MAX_RECORDS {
            apply(&mut t, &format!("state=idle:id=r{i}"));
        }
        // Re-reporting r0 unchanged still makes it recent (kitty's rule).
        apply(&mut t, "state=idle:id=r0");
        apply(&mut t, "state=idle:id=new");
        assert_eq!(t.len(), MAX_RECORDS);
        let ids = ids(&t);
        assert!(ids.contains(&Some("r0".into())));
        assert!(ids.contains(&Some("new".into())));
        assert!(
            !ids.contains(&Some("r1".into())),
            "r1 was least recently reported"
        );
    }

    #[test]
    fn exit_and_prompt_drop_working_blocked_and_idle_but_keep_done_and_error() {
        for event in [StatusTable::on_process_exit, StatusTable::on_prompt_start] {
            let mut t = StatusTable::new();
            for p in [
                "state=idle:id=i",
                "state=working:id=w",
                "state=blocked:id=b",
                "state=done:id=d",
                "state=error:id=e",
            ] {
                apply(&mut t, p);
            }
            event(&mut t);
            let mut left = ids(&t);
            left.sort();
            assert_eq!(left, vec![Some("d".into()), Some("e".into())]);
        }
    }

    #[test]
    fn full_reset_removes_every_record() {
        let mut t = StatusTable::new();
        apply(&mut t, "state=done");
        apply(&mut t, "state=error:id=x");
        t.on_full_reset();
        assert!(t.is_empty());
        assert_eq!(t.summary(), None);
    }

    #[test]
    fn app_is_inherited_from_the_nearest_ancestor_at_read_time() {
        let mut t = StatusTable::new();
        apply(&mut t, "state=working:app=deploy");
        apply(&mut t, "state=working:id=eu/fra:app=eu-runner");
        apply(&mut t, "state=working:id=eu/fra/step");
        apply(&mut t, "state=working:id=us/iad");
        let app = |t: &StatusTable, id: &str| {
            t.records()
                .into_iter()
                .find(|r| r.id.as_ref().map(RecordId::as_str) == Some(id))
                .and_then(|r| r.app)
        };
        assert_eq!(app(&t, "eu/fra/step").as_deref(), Some("eu-runner"));
        assert_eq!(
            app(&t, "us/iad").as_deref(),
            Some("deploy"),
            "root is the outer ancestor"
        );
        apply(&mut t, "state=working:app=renamed");
        assert_eq!(
            app(&t, "us/iad").as_deref(),
            Some("renamed"),
            "not copied at write time"
        );
    }

    #[test]
    fn summary_leads_with_the_most_urgent_then_root_then_newest() {
        let mut t = StatusTable::new();
        apply(&mut t, "state=working:id=a");
        apply(&mut t, "state=done");
        apply(&mut t, "state=blocked:kind=question:id=b");
        let s = t.summary().unwrap();
        assert_eq!(s.state, State::Blocked);
        assert_eq!(s.kind, Some(Kind::Question));
        assert_eq!(s.id, RecordId::parse("b"));
        assert_eq!(
            (s.counts.working, s.counts.done, s.counts.blocked),
            (1, 1, 1)
        );

        let mut t = StatusTable::new();
        apply(&mut t, "state=done:id=x");
        apply(&mut t, "state=done");
        assert_eq!(t.summary().unwrap().id, None, "root wins among equals");

        let mut t = StatusTable::new();
        apply(&mut t, "state=done:id=old");
        apply(&mut t, "state=done:id=new");
        assert_eq!(t.summary().unwrap().id, RecordId::parse("new"));
    }

    #[test]
    fn summary_never_carries_program_text() {
        let mut t = StatusTable::new();
        apply(&mut t, "state=blocked:title=VGl0bGU=:msg=TXNn");
        let json = serde_json::to_string(&t.summary().unwrap()).unwrap();
        assert!(!json.contains("Title") && !json.contains("Msg"), "{json}");
    }
}
