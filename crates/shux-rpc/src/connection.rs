//! The lifetime of the connection serving the current request.
//!
//! A handler that starts something meant to last only as long as its caller
//! takes [`current_connection`] and watches it: the token is cancelled once
//! the server stops serving that connection, for any reason (the client hung
//! up or died, the server shut down, a write failed). A half-closed client is
//! still being served until its queued requests are answered.
//!
//! Like [`crate::caller`], this rides a task-local: it is set around each
//! request the socket server dispatches, and absent elsewhere (plugin
//! dispatch, daemon-internal tasks), where [`current_connection`] is `None`.
//! It does not cross `tokio::spawn`; capture it in the handler.

use std::future::Future;

use tokio_util::sync::CancellationToken;

tokio::task_local! {
    static RPC_CONNECTION: CancellationToken;
}

/// The serving connection's lifetime, if this request came over a socket.
pub fn current_connection() -> Option<CancellationToken> {
    RPC_CONNECTION.try_with(CancellationToken::clone).ok()
}

/// Run `fut` as a request served on the connection whose lifetime is `life`.
pub async fn with_connection<F: Future>(life: CancellationToken, fut: F) -> F::Output {
    RPC_CONNECTION.scope(life, fut).await
}

/// The socket a request arrived on. An owner-local method
/// ([`crate::RouterBuilder::owner_local`]) answers only [`Transport::Uds`]:
/// the daemon's own socket, which only its user can open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Uds,
    Tcp,
}

tokio::task_local! {
    static RPC_TRANSPORT: Transport;
}

/// The transport of the request being served; `None` outside a socket
/// connection (plugin dispatch, daemon-internal calls).
pub fn current_transport() -> Option<Transport> {
    RPC_TRANSPORT.try_with(|t| *t).ok()
}

/// Run `fut` as a connection served on `transport`.
pub async fn with_transport<F: Future>(transport: Transport, fut: F) -> F::Output {
    RPC_TRANSPORT.scope(transport, fut).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn absent_outside_a_served_request() {
        assert!(current_connection().is_none());
        assert!(current_transport().is_none());
    }
}
