//! The attached clients: who is watching which session, who sizes its panes,
//! which panes are pinned, and a way to end one client.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use shux_core::event::ClientId;
use shux_core::model::{PaneId, SessionId};
use tokio::sync::watch;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// How long the driver keeps the panes after its last input before another
/// client's input can take them: two people typing do not flip every pane's
/// size on alternate keys.
pub const SETTLE: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientInfo {
    pub id: ClientId,
    pub session_id: SessionId,
    pub cols: u16,
    pub rows: u16,
    pub attached_at: SystemTime,
    /// May size the session's panes. False only for a read-only watcher.
    pub votes: bool,
    /// The last input this client committed to the session: keys or a paste
    /// reaching a pane, a layout action, a mouse press that did something.
    pub last_input: Option<Instant>,
}

/// Where a client sits among the session's other clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seat {
    /// The earliest attached writer: it keeps the seat through sizing
    /// handoffs, and the next writer takes it when it leaves.
    Host,
    /// Any other writable client.
    Writer,
    Viewer,
}

/// Who else is on a client's session, for its sharing badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sharing {
    pub seat: Seat,
    /// Other clients that can type.
    pub writers: usize,
    /// Other read-only clients.
    pub viewers: usize,
    /// A share link to the session can still be redeemed, or a guest can
    /// still reconnect (#297).
    pub link: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionView {
    pub attached: usize,
    /// The asking client is the one that sizes the panes now.
    pub driving: bool,
    /// `None` for a frame no attached client asked for.
    pub sharing: Option<Sharing>,
    /// The session's other clients, and whether each can type.
    pub peers: Vec<(ClientId, bool)>,
    pub pinned: std::collections::HashSet<PaneId>,
}

struct Entry {
    info: ClientInfo,
    end: CancellationToken,
    /// Run when an administrator removes it, before its connection hears.
    on_remove: Option<Box<dyn FnOnce() + Send>>,
    /// Registration order: before anyone types, the first voter drives.
    seq: u64,
}

impl State {
    fn count(&self, session: SessionId) -> usize {
        let ours = |e: &&Entry| e.info.session_id == session;
        self.clients.values().filter(ours).count()
    }

    /// The session's voters in attach order, and who of them drives now.
    fn voters_and_elected(&self, session: SessionId) -> (Vec<&ClientInfo>, Option<ClientId>) {
        let mut voters: Vec<&Entry> = self
            .clients
            .values()
            .filter(|e| e.info.session_id == session && e.info.votes)
            .collect();
        voters.sort_by_key(|e| e.seq);
        let infos: Vec<&ClientInfo> = voters.iter().map(|e| &e.info).collect();
        let elected = elect(self.drivers.get(&session).copied(), &infos);
        (infos, elected)
    }

    /// Take a client off the list; a session left with none forgets its driver.
    fn unlist(&mut self, id: ClientId) -> Option<Entry> {
        let entry = self.clients.remove(&id)?;
        let session = entry.info.session_id;
        if !self.clients.values().any(|e| e.info.session_id == session) {
            self.drivers.remove(&session);
        }
        Some(entry)
    }
}

#[derive(Clone, Copy)]
struct Pin {
    generation: u64,
    kept: bool,
}

#[derive(Default)]
struct State {
    clients: HashMap<ClientId, Entry>,
    drivers: HashMap<SessionId, ClientId>,
    /// Each pin carries the generation that set it, so a failed resize can
    /// take back only its own.
    pins: HashMap<PaneId, Pin>,
    next_pin: u64,
    next_seq: u64,
    /// Sessions a share link still reaches.
    shared: std::collections::HashSet<SessionId>,
}

/// Every attached client, plus the sizing decisions made from them. A change
/// to anything that decides a pane's size bumps [`ClientRegistry::changed`];
/// any change a status bar shows, sizing included, bumps
/// [`ClientRegistry::presence`]. A read-only client's comings, goings and
/// resizes bump only the latter: they never wake the sizing reconciler.
#[derive(Clone)]
pub struct ClientRegistry {
    state: Arc<Mutex<State>>,
    changed: Arc<watch::Sender<u64>>,
    presence: Arc<watch::Sender<u64>>,
    /// One sizing pass at a time: deciding the driver and writing its sizes
    /// is one step, so two passes cannot interleave stale sizes.
    sizing: Arc<tokio::sync::Mutex<()>>,
}

impl Default for ClientRegistry {
    fn default() -> Self {
        Self {
            state: Arc::default(),
            changed: Arc::new(watch::channel(0).0),
            presence: Arc::new(watch::channel(0).0),
            sizing: Arc::default(),
        }
    }
}

impl ClientRegistry {
    fn state(&self) -> MutexGuard<'_, State> {
        // A panic while holding this lock leaves maps that are still whole.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn bump(&self) {
        self.changed.send_modify(|n| *n = n.wrapping_add(1));
        self.bump_presence();
    }

    fn bump_presence(&self) {
        self.presence.send_modify(|n| *n = n.wrapping_add(1));
    }

    /// A change to a client that sizes (`votes`) is a sizing change.
    fn bump_for(&self, votes: bool) {
        if votes {
            self.bump();
        } else {
            self.bump_presence();
        }
    }

    /// Hold for a whole sizing pass.
    pub async fn sizing_turn(&self) -> tokio::sync::OwnedMutexGuard<()> {
        self.sizing.clone().lock_owned().await
    }

    #[cfg(test)]
    pub fn sizing_busy(&self) -> bool {
        self.sizing.try_lock().is_err()
    }

    /// Wakes whenever a size decision may have changed.
    pub fn changed(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    /// Wakes whenever what a status bar shows of the clients may have
    /// changed: who is attached, a share link, or a size decision.
    pub fn presence(&self) -> watch::Receiver<u64> {
        self.presence.subscribe()
    }

    /// The sessions a share link reaches now. A host's badge shows `shared`
    /// while its session is among them, though nobody watches yet; a change
    /// wakes every bar, and never the sizing reconciler.
    pub fn set_shared(&self, sessions: std::collections::HashSet<SessionId>) {
        let changed = {
            let mut s = self.state();
            let changed = s.shared != sessions;
            s.shared = sessions;
            changed
        };
        if changed {
            self.bump_presence();
        }
    }

    /// Ask for the sizes to be worked out again: a pane gained its resizer.
    pub fn touch(&self) {
        self.bump();
    }

    /// Record a client until the returned guard drops; cancelling `end`
    /// ends that client's connection.
    pub fn register(&self, info: ClientInfo, end: CancellationToken) -> Registered {
        self.register_with_removal(info, end, None)
    }

    /// [`Self::register`], with `on_remove` run if [`Self::remove`] ends it:
    /// for a client whose leaving by force must revoke more than its socket.
    pub fn register_with_removal(
        &self,
        info: ClientInfo,
        end: CancellationToken,
        on_remove: Option<Box<dyn FnOnce() + Send>>,
    ) -> Registered {
        let (id, votes) = (info.id, info.votes);
        {
            let mut s = self.state();
            let seq = s.next_seq;
            s.next_seq += 1;
            let entry = Entry {
                info,
                end,
                on_remove,
                seq,
            };
            s.clients.insert(id, entry);
        }
        self.bump_for(votes);
        Registered {
            registry: self.clone(),
            id,
        }
    }

    /// The read-only clients on `session`: what a host's badge counts as
    /// watching.
    pub fn watching(&self, session: SessionId) -> usize {
        self.state()
            .clients
            .values()
            .filter(|e| e.info.session_id == session && !e.info.votes)
            .count()
    }

    pub fn list(&self) -> Vec<ClientInfo> {
        let s = self.state();
        let mut all: Vec<&Entry> = s.clients.values().collect();
        all.sort_by_key(|e| e.seq);
        all.into_iter().map(|e| e.info.clone()).collect()
    }

    /// The clients whose forced removal revokes more than their socket: a
    /// share guest's revokes its credential.
    pub fn guests(&self) -> std::collections::HashSet<ClientId> {
        self.state()
            .clients
            .values()
            .filter(|e| e.on_remove.is_some())
            .map(|e| e.info.id)
            .collect()
    }

    pub fn count_for(&self, session: SessionId) -> usize {
        self.state().count(session)
    }

    pub fn set_size(&self, id: ClientId, cols: u16, rows: u16) {
        let changed = match self.state().clients.get_mut(&id) {
            Some(e) if (e.info.cols, e.info.rows) != (cols, rows) => {
                e.info.cols = cols;
                e.info.rows = rows;
                Some(e.info.votes)
            }
            _ => None,
        };
        // Only a voter's size shows anywhere but its own screen and the list.
        if changed == Some(true) {
            self.bump();
        }
    }

    /// The client committed input to its session just now. True when that
    /// may hand it the panes. Input from another client while the driver is
    /// within [`SETTLE`] of its own last input is not recorded at all, so a
    /// takeover only ever happens on the new driver's own input, never later
    /// on some unrelated change.
    pub fn mark_input(&self, id: ClientId, now: Instant) -> bool {
        let mut s = self.state();
        let Some(session) = s
            .clients
            .get(&id)
            .filter(|e| e.info.votes)
            .map(|e| e.info.session_id)
        else {
            return false;
        };
        let driver = s.drivers.get(&session).copied();
        let held = driver.filter(|d| *d != id).is_some_and(|d| {
            s.clients
                .get(&d)
                .and_then(|e| e.info.last_input)
                .is_some_and(|t| now.duration_since(t) < SETTLE)
        });
        if held {
            return false;
        }
        if let Some(e) = s.clients.get_mut(&id) {
            e.info.last_input = Some(now);
        }
        // The driver typing changes nothing about who drives. A takeover is
        // recorded here, so the next key is held against this one.
        let takes = driver != Some(id);
        if takes {
            s.drivers.insert(session, id);
        }
        drop(s);
        if takes {
            self.bump();
        }
        takes
    }

    /// Unlist a client and end its connection. False when none is attached.
    pub fn remove(&self, id: ClientId) -> bool {
        let entry = self.state().unlist(id);
        let gone = entry.map(|e| {
            if let Some(on_remove) = e.on_remove {
                on_remove();
            }
            e.end.cancel();
            e.info.votes
        });
        if let Some(votes) = gone {
            self.bump_for(votes);
        }
        gone.is_some()
    }

    /// The client whose size the session's panes take, decided now. None
    /// when no voting client is attached: sizes then stay as they are.
    pub fn driver(&self, session: SessionId) -> Option<ClientInfo> {
        let mut s = self.state();
        let (voters, elected) = s.voters_and_elected(session);
        let info = elected.and_then(|id| voters.into_iter().find(|c| c.id == id).cloned());
        match elected {
            Some(id) => s.drivers.insert(session, id),
            None => s.drivers.remove(&session),
        };
        info
    }

    /// What a frame for `client` (none for a snapshot) shows about `session`,
    /// read under one lock: who is attached, whether the client drives, who
    /// else is there, and which of `panes` are pinned. Decides nothing: the driver is the one
    /// [`ClientRegistry::driver`] would elect now.
    pub fn session_view(
        &self,
        session: SessionId,
        client: Option<ClientId>,
        panes: &[PaneId],
    ) -> SessionView {
        let s = self.state();
        let (voters, driver) = s.voters_and_elected(session);
        let me = client.and_then(|id| s.clients.get(&id));
        let attached = s.count(session);
        let sharing = me.map(|e| {
            let seat = if !e.info.votes {
                Seat::Viewer
            } else if voters.first().map(|c| c.id) == Some(e.info.id) {
                Seat::Host
            } else {
                Seat::Writer
            };
            let writers = voters.len() - usize::from(e.info.votes);
            Sharing {
                seat,
                writers,
                viewers: attached - voters.len() - usize::from(!e.info.votes),
                link: s.shared.contains(&session),
            }
        });
        let mut peers: Vec<(ClientId, bool)> = s
            .clients
            .values()
            .filter(|e| e.info.session_id == session && Some(e.info.id) != client)
            .map(|e| (e.info.id, e.info.votes))
            .collect();
        peers.sort_by_key(|(id, _)| id.0);
        SessionView {
            attached,
            driving: client.is_some() && driver == client,
            sharing,
            peers,
            pinned: panes
                .iter()
                .copied()
                .filter(|p| s.pins.contains_key(p))
                .collect(),
        }
    }

    /// Whether any attached client may size `session` now.
    pub fn has_voter(&self, session: SessionId) -> bool {
        self.state()
            .clients
            .values()
            .any(|e| e.info.session_id == session && e.info.votes)
    }

    pub fn is_pinned(&self, pane: PaneId) -> bool {
        self.state().pins.contains_key(&pane)
    }

    /// Pin a pane at the size it was set to: no client's size moves it.
    pub fn pin(&self, pane: PaneId) {
        self.place_pin(pane, true);
    }

    /// A pin that lasts only if [`ClientRegistry::keep`] confirms it; until
    /// then [`ClientRegistry::release`] with its generation takes it back.
    fn pin_provisionally(&self, pane: PaneId) -> u64 {
        self.place_pin(pane, false)
    }

    fn place_pin(&self, pane: PaneId, kept: bool) -> u64 {
        let (generation, new) = {
            let mut s = self.state();
            s.next_pin += 1;
            let generation = s.next_pin;
            // Replacing a kept pin keeps it kept: a later failure must not
            // take back a pin an earlier call reported.
            let kept = kept || s.pins.get(&pane).is_some_and(|p| p.kept);
            (
                generation,
                s.pins.insert(pane, Pin { generation, kept }).is_none(),
            )
        };
        if new {
            self.bump();
        }
        generation
    }

    /// Confirm a pane's pin: its set_size reported success. Placed again if a
    /// cancelled call overlapping this one took it back meanwhile.
    fn keep(&self, pane: PaneId) {
        self.place_pin(pane, true);
    }

    /// Take back the provisional pin `generation` placed, unless a later pin
    /// replaced it or a call confirmed it.
    fn release(&self, pane: PaneId, generation: u64) {
        let released = {
            let mut s = self.state();
            let ours = s
                .pins
                .get(&pane)
                .is_some_and(|p| p.generation == generation && !p.kept);
            if ours {
                s.pins.remove(&pane);
            }
            ours
        };
        if released {
            self.bump();
        }
    }

    /// Hand a pane back to the layout. True when it was pinned.
    pub fn unpin(&self, pane: PaneId) -> bool {
        let was = self.state().pins.remove(&pane).is_some();
        if was {
            self.bump();
        }
        was
    }
}

/// Who drives a session, given the current driver and the voters in the
/// order they attached: the most recent recorded input (the settle rule
/// lives in [`ClientRegistry::mark_input`]). Before anyone types, the
/// current driver keeps the panes, or the first voter takes them; when the
/// driver leaves, the next takes over at once.
fn elect(current: Option<ClientId>, voters: &[&ClientInfo]) -> Option<ClientId> {
    // Ties go to the earlier attach: `voters` is in attach order and
    // `max_by_key` keeps the last maximum, so iterate in reverse.
    let latest = voters
        .iter()
        .rev()
        .filter(|c| c.last_input.is_some())
        .max_by_key(|c| c.last_input);
    latest
        .or_else(|| current.and_then(|id| voters.iter().find(|c| c.id == id)))
        .or_else(|| voters.first())
        .map(|c| c.id)
}

/// Removes its client from the registry when dropped, however the
/// connection ended.
pub struct Registered {
    registry: ClientRegistry,
    id: ClientId,
}

impl Registered {
    pub fn id(&self) -> ClientId {
        self.id
    }
}

impl Drop for Registered {
    fn drop(&mut self) {
        let gone = self.registry.state().unlist(self.id).map(|e| e.info.votes);
        if let Some(votes) = gone {
            self.registry.bump_for(votes);
        }
    }
}

/// A `pane.set_size` pin, taken back on drop unless kept, so a call that fails
/// or is cancelled leaves no pin behind; a pin some call reported stays.
pub struct PinGuard {
    clients: ClientRegistry,
    pane: PaneId,
    generation: Option<u64>,
}

impl PinGuard {
    pub fn take(clients: &ClientRegistry, pane: PaneId) -> Self {
        Self {
            clients: clients.clone(),
            pane,
            generation: Some(clients.pin_provisionally(pane)),
        }
    }

    pub fn keep(mut self) {
        self.clients.keep(self.pane);
        self.generation = None;
    }
}

impl Drop for PinGuard {
    fn drop(&mut self) {
        if let Some(generation) = self.generation {
            self.clients.release(self.pane, generation);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn info(session: SessionId) -> ClientInfo {
        ClientInfo {
            id: ClientId::new(),
            session_id: session,
            cols: 80,
            rows: 24,
            attached_at: SystemTime::now(),
            votes: true,
            last_input: None,
        }
    }

    #[test]
    fn remove_ends_only_the_named_client_and_drop_unlists_it() {
        let reg = ClientRegistry::default();
        let s = SessionId::new();
        let (a, b) = (info(s), info(s));
        let (end_a, end_b) = (CancellationToken::new(), CancellationToken::new());
        let guard_a = reg.register(a.clone(), end_a.clone());
        let _guard_b = reg.register(b.clone(), end_b.clone());
        assert_eq!(reg.count_for(s), 2);

        assert!(reg.remove(a.id));
        assert!(end_a.is_cancelled());
        assert!(!end_b.is_cancelled(), "removing one client ended another");

        drop(guard_a);
        assert_eq!(
            reg.list().iter().map(|c| c.id).collect::<Vec<_>>(),
            vec![b.id]
        );
        assert!(!reg.remove(a.id), "a gone client can still be removed");
        assert_eq!(reg.count_for(SessionId::new()), 0);
    }

    /// A frame's view of its session, without electing anyone: before anybody
    /// types the first voter drives and a later one views, input hands the
    /// role over, a read-only client is read-only, and a snapshot has none.
    #[tokio::test(start_paused = true)]
    async fn the_session_view_names_each_clients_role_and_the_pins() {
        let reg = ClientRegistry::default();
        let s = SessionId::new();
        let (a, b) = (info(s), info(s));
        let watcher = ClientInfo {
            votes: false,
            ..info(s)
        };
        let _ga = reg.register(a.clone(), CancellationToken::new());
        let _gb = reg.register(b.clone(), CancellationToken::new());
        let _gw = reg.register(watcher.clone(), CancellationToken::new());
        let (pinned, free) = (PaneId::new(), PaneId::new());
        reg.pin(pinned);
        let driving = |id| reg.session_view(s, Some(id), &[pinned, free]).driving;
        assert!(driving(a.id), "the first voter, untyped");
        assert!(!driving(b.id) && !driving(watcher.id));
        assert!(reg.mark_input(b.id, Instant::now()));
        assert!(driving(b.id) && !driving(a.id));
        let peers = reg.session_view(s, Some(a.id), &[]).peers;
        let mut want = vec![(b.id, true), (watcher.id, false)];
        want.sort_by_key(|(id, _)| id.0);
        assert_eq!(
            peers, want,
            "a client's peers are the others, with their votes"
        );
        let view = reg.session_view(s, None, &[pinned, free]);
        assert!(!view.driving);
        assert_eq!(view.attached, 3);
        assert_eq!(view.pinned, [pinned].into_iter().collect());
        assert_eq!(view.sharing, None);
        assert!(reg.has_voter(s) && !reg.has_voter(SessionId::new()));
    }

    /// A viewer's whole life, removal by `client detach` included, wakes the
    /// status bars and never the sizing reconciler; a voter's wakes both.
    #[test]
    fn a_viewer_never_moves_the_sizing_generation() {
        let reg = ClientRegistry::default();
        let s = SessionId::new();
        let (mut sizing, mut presence) = (reg.changed(), reg.presence());
        let mut settle = || {
            let moved = (
                sizing.has_changed().unwrap(),
                presence.has_changed().unwrap(),
            );
            sizing.borrow_and_update();
            presence.borrow_and_update();
            moved
        };
        let viewer = ClientInfo {
            votes: false,
            ..info(s)
        };
        let guard = reg.register(viewer.clone(), CancellationToken::new());
        assert_eq!(settle(), (false, true), "register");
        reg.set_size(viewer.id, 200, 60);
        assert_eq!(settle(), (false, false), "resize");
        assert!(!reg.mark_input(viewer.id, Instant::now()));
        assert_eq!(settle(), (false, false), "input");
        drop(guard);
        assert_eq!(settle(), (false, true), "drop");
        let removed = ClientInfo {
            votes: false,
            ..info(s)
        };
        let _g = reg.register(removed.clone(), CancellationToken::new());
        settle();
        assert!(reg.remove(removed.id));
        assert_eq!(settle(), (false, true), "client detach");
        reg.set_shared([s].into());
        assert_eq!(settle(), (false, true), "a share link");
        reg.set_shared([s].into());
        assert_eq!(settle(), (false, false), "the same links again");
        reg.set_shared(Default::default());
        assert_eq!(settle(), (false, true), "the last link ended");

        let _w = reg.register(info(s), CancellationToken::new());
        assert_eq!(settle(), (true, true), "a voter");
    }

    /// The host is the earliest writer, through a sizing handoff; the next
    /// writer takes the seat when it leaves. Counts are of the others.
    #[tokio::test(start_paused = true)]
    async fn the_host_is_the_earliest_writer_and_counts_exclude_the_client() {
        let reg = ClientRegistry::default();
        let s = SessionId::new();
        let viewer = ClientInfo {
            votes: false,
            ..info(s)
        };
        let _gv = reg.register(viewer.clone(), CancellationToken::new());
        let (a, b) = (info(s), info(s));
        let ga = reg.register(a.clone(), CancellationToken::new());
        let _gb = reg.register(b.clone(), CancellationToken::new());
        let sharing = |id| reg.session_view(s, Some(id), &[]).sharing.unwrap();
        let seat = |id| sharing(id).seat;
        assert_eq!(seat(a.id), Seat::Host);
        assert_eq!(seat(b.id), Seat::Writer);
        assert_eq!(seat(viewer.id), Seat::Viewer);
        assert_eq!((sharing(a.id).writers, sharing(a.id).viewers), (1, 1));
        assert_eq!(
            (sharing(viewer.id).writers, sharing(viewer.id).viewers),
            (2, 0)
        );
        assert!(reg.mark_input(b.id, Instant::now()));
        assert!(reg.session_view(s, Some(b.id), &[]).driving);
        assert_eq!(seat(a.id), Seat::Host, "a handoff moved the host");
        assert!(!sharing(a.id).link);
        reg.set_shared([s].into());
        assert!(sharing(a.id).link && sharing(viewer.id).link);
        reg.set_shared([SessionId::new()].into());
        assert!(!sharing(a.id).link, "another session's link");
        drop(ga);
        assert_eq!(seat(b.id), Seat::Host, "the next writer took the seat");
        assert_eq!((sharing(b.id).writers, sharing(b.id).viewers), (0, 1));
    }

    #[tokio::test(start_paused = true)]
    async fn the_first_voter_drives_until_another_types_and_settles() {
        let reg = ClientRegistry::default();
        let s = SessionId::new();
        let (a, b) = (info(s), info(s));
        let _ga = reg.register(a.clone(), CancellationToken::new());
        let gb = reg.register(b.clone(), CancellationToken::new());
        let driver = |reg: &ClientRegistry| reg.driver(s).map(|c| c.id);
        assert_eq!(
            driver(&reg),
            Some(a.id),
            "before anyone types, the first voter drives"
        );

        reg.mark_input(b.id, Instant::now());
        assert_eq!(
            driver(&reg),
            Some(b.id),
            "a quiet driver gives way to input"
        );

        // Inside the settle window, A's key does not take the panes back.
        tokio::time::advance(Duration::from_millis(300)).await;
        reg.mark_input(a.id, Instant::now());
        assert_eq!(
            driver(&reg),
            Some(b.id),
            "alternating keys flipped the driver"
        );
        // A key refused inside the window is not banked: once B settles,
        // nothing but A's next input hands A the panes.
        tokio::time::advance(SETTLE).await;
        reg.set_size(a.id, 120, 40);
        reg.pin(PaneId::new());
        let _late = reg.register(info(s), CancellationToken::new());
        assert_eq!(
            driver(&reg),
            Some(b.id),
            "a refused key took the panes later"
        );
        reg.mark_input(a.id, Instant::now());
        assert_eq!(driver(&reg), Some(a.id), "a settled driver kept the panes");

        drop(gb);
        assert_eq!(driver(&reg), Some(a.id));
    }

    /// Two clients type back to back with no sizing pass between: the first
    /// takes the panes and the second is held off by its settle window.
    #[tokio::test(start_paused = true)]
    async fn competing_keys_hand_the_panes_over_once() {
        let reg = ClientRegistry::default();
        let s = SessionId::new();
        let (a, b, c) = (info(s), info(s), info(s));
        let _ga = reg.register(a.clone(), CancellationToken::new());
        let _gb = reg.register(b.clone(), CancellationToken::new());
        let _gc = reg.register(c.clone(), CancellationToken::new());
        reg.mark_input(a.id, Instant::now());
        assert_eq!(reg.driver(s).map(|d| d.id), Some(a.id));
        tokio::time::advance(SETTLE).await;
        assert!(reg.mark_input(b.id, Instant::now()));
        tokio::time::advance(Duration::from_millis(10)).await;
        assert!(
            !reg.mark_input(c.id, Instant::now()),
            "a second key also took the panes"
        );
        assert_eq!(reg.driver(s).map(|d| d.id), Some(b.id));
    }

    #[test]
    fn a_watcher_never_drives_and_no_voter_means_no_driver() {
        let reg = ClientRegistry::default();
        let s = SessionId::new();
        let watcher = ClientInfo {
            votes: false,
            ..info(s)
        };
        let _g = reg.register(watcher.clone(), CancellationToken::new());
        reg.mark_input(watcher.id, Instant::now());
        assert_eq!(reg.driver(s), None);
    }

    /// Two set_size calls overlap; the later one is cancelled and takes its
    /// pin back; the earlier one then succeeds. It reported a pin, so the
    /// pane is pinned.
    #[test]
    fn a_success_keeps_its_pin_though_a_cancelled_call_took_one_back() {
        let reg = ClientRegistry::default();
        let pane = PaneId::new();
        let first = PinGuard::take(&reg, pane);
        let second = PinGuard::take(&reg, pane);
        drop(second);
        first.keep();
        assert!(reg.is_pinned(pane), "a successful set_size left no pin");
    }

    #[test]
    fn pins_bump_and_unpin_reports_what_it_did() {
        let reg = ClientRegistry::default();
        let mut changed = reg.changed();
        let pane = PaneId::new();
        reg.pin(pane);
        assert!(changed.has_changed().unwrap());
        changed.borrow_and_update();
        assert!(reg.is_pinned(pane));
        assert!(reg.unpin(pane));
        assert!(!reg.unpin(pane), "a second unpin claimed a pin");
        assert!(!reg.is_pinned(pane));
    }

    proptest! {
        /// Over any sequence of attaches, inputs, leaves and time passing:
        /// there is a driver exactly when a voter is attached, and it votes.
        #[test]
        fn at_most_one_driver_and_it_always_votes(ops in proptest::collection::vec((0u8..4, 0usize..4, 0u64..1500), 1..60)) {
            let rt = tokio::runtime::Builder::new_current_thread().enable_time().start_paused(true).build().unwrap();
            rt.block_on(async {
                let reg = ClientRegistry::default();
                let s = SessionId::new();
                let mut guards: Vec<(ClientInfo, Registered)> = Vec::new();
                for (op, who, ms) in ops {
                    match op {
                        0 => {
                            let c = ClientInfo { votes: who != 3, ..info(s) };
                            let g = reg.register(c.clone(), CancellationToken::new());
                            guards.push((c, g));
                        }
                        1 if !guards.is_empty() => {
                            let id = guards[who % guards.len()].0.id;
                            reg.mark_input(id, Instant::now());
                        }
                        2 if !guards.is_empty() => {
                            guards.remove(who % guards.len());
                        }
                        _ => tokio::time::advance(Duration::from_millis(ms)).await,
                    }
                    let voters: Vec<&ClientInfo> = guards.iter().map(|(c, _)| c).filter(|c| c.votes).collect();
                    let d = reg.driver(s);
                    prop_assert_eq!(d.is_some(), !voters.is_empty());
                    if let Some(d) = d {
                        prop_assert!(d.votes);
                        prop_assert!(voters.iter().any(|c| c.id == d.id));
                    }
                }
                Ok(())
            })?;
        }
    }
}
