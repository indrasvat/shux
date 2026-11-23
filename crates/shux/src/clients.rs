//! The attached clients: who is watching which session, and a way to end one.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::SystemTime;

use shux_core::event::ClientId;
use shux_core::model::SessionId;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientInfo {
    pub id: ClientId,
    pub session_id: SessionId,
    pub cols: u16,
    pub rows: u16,
    pub attached_at: SystemTime,
}

struct Entry {
    info: ClientInfo,
    end: CancellationToken,
}

#[derive(Clone, Default)]
pub struct ClientRegistry(Arc<Mutex<HashMap<ClientId, Entry>>>);

impl ClientRegistry {
    fn entries(&self) -> MutexGuard<'_, HashMap<ClientId, Entry>> {
        // A panic while holding this lock leaves a map that is still whole.
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Record a client until the returned guard drops; cancelling `end`
    /// ends that client's connection.
    pub fn register(&self, info: ClientInfo, end: CancellationToken) -> Registered {
        let id = info.id;
        self.entries().insert(id, Entry { info, end });
        Registered {
            registry: self.clone(),
            id,
        }
    }

    pub fn list(&self) -> Vec<ClientInfo> {
        let mut all: Vec<ClientInfo> = self.entries().values().map(|e| e.info.clone()).collect();
        all.sort_by_key(|c| c.attached_at);
        all
    }

    pub fn count_for(&self, session: SessionId) -> usize {
        self.entries()
            .values()
            .filter(|e| e.info.session_id == session)
            .count()
    }

    pub fn set_size(&self, id: ClientId, cols: u16, rows: u16) {
        if let Some(e) = self.entries().get_mut(&id) {
            e.info.cols = cols;
            e.info.rows = rows;
        }
    }

    /// Unlist a client and end its connection. False when none is attached.
    pub fn remove(&self, id: ClientId) -> bool {
        self.entries().remove(&id).map(|e| e.end.cancel()).is_some()
    }
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
        self.registry.entries().remove(&self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(session: SessionId) -> ClientInfo {
        ClientInfo {
            id: ClientId::new(),
            session_id: session,
            cols: 80,
            rows: 24,
            attached_at: SystemTime::now(),
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
}
