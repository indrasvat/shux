//! `share.*`: links that show one session, read-only, in a guest's browser.
//!
//! Every method is owner-local (the daemon's own socket only) and closed to
//! plugins. Only `share.create` ever returns a link's URL; nothing else, no
//! event and no log, carries its secret.

use std::collections::HashSet;
use std::num::NonZeroU32;
use std::time::Duration;

use shux_core::model::SessionId;
use shux_rpc::{ErrorCode, Policy, RpcError, Sensitivity};

use crate::lens_scratch::ScratchRegistry;
use crate::share::grants::MAX_VIEWERS_PER_LINK;
use crate::share::{
    CreateError, CreateOpts, Gateway, GrantId, ShareError, TunnelError, Uses, ViewerId,
};

pub(crate) const METHODS: [&str; 6] = [
    "share.create",
    "share.confirm",
    "share.list",
    "share.revoke",
    "share.kick",
    "share.stop",
];

use crate::share::rfc3339;

/// A refusal with a stable `reason`, and the `detail` the CLI prints.
fn refusal(code: ErrorCode, reason: &str, detail: &str) -> RpcError {
    RpcError::with_data(
        code,
        serde_json::json!({ "reason": reason, "detail": detail }),
    )
}

fn not_found(resource: &str, id: &str, hint: &str) -> RpcError {
    RpcError::with_data(
        ErrorCode::NotFound,
        serde_json::json!({ "resource": resource, "id": id, "hint": hint }),
    )
}

fn share_error(e: ShareError, id: &str) -> RpcError {
    match e {
        ShareError::NoSession(s) => not_found("session", &s.to_string(), "it ended"),
        ShareError::Create(CreateError::Full) => refusal(
            ErrorCode::ResourceExhausted,
            "full",
            "too many links are live; `shux share revoke` one first",
        ),
        ShareError::Create(e @ (CreateError::Ttl | CreateError::Label)) => {
            refusal(ErrorCode::InvalidParams, "invalid", &e.to_string())
        }
        ShareError::NoGrant => {
            not_found("share link", id, "`shux share list` shows the live links")
        }
        ShareError::NoViewer => {
            not_found("viewer", id, "`shux share list` shows each link's viewers")
        }
        ShareError::Tunnel(t) => RpcError::with_data(
            ErrorCode::SpawnFailed,
            serde_json::json!({
                "reason": t.code(),
                "detail": match t {
                    TunnelError::NoCloudflared => "cloudflared was not found",
                    _ => "the share tunnel did not come up",
                },
                "hint": match t {
                    TunnelError::NoCloudflared => crate::share::tunnel::INSTALL_HINT.to_string(),
                    _ => t.to_string(),
                },
            }),
        ),
        ShareError::Teardown(n) => refusal(
            ErrorCode::InternalError,
            "teardown",
            &format!(
                "{n} guest stream(s) were still open after the cutoff; their links and credentials are gone, but check `shux share list`"
            ),
        ),
        other => refusal(ErrorCode::InternalError, "internal", &other.to_string()),
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateParams {
    session: Option<String>,
    /// Required. `null`: the caller says it is outside shux. Anything else
    /// (`SHUX_PANE_ID`): it is in a pane, whatever that names.
    #[serde(default, deserialize_with = "present")]
    from_pane: Option<Option<String>>,
    ttl_secs: Option<u64>,
    /// 0 is any number of guests.
    uses: Option<u32>,
    label: Option<String>,
    /// The caller can put the link on a clipboard instead of printing it.
    #[serde(default)]
    can_copy: bool,
    #[serde(default)]
    qr: bool,
    /// Revoke the link if this connection closes before `share.confirm`.
    #[serde(default)]
    hold: bool,
}

/// A field that was given, `null` included: missing stays `None`.
fn present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
    Ok(Some(serde::Deserialize::deserialize(d)?))
}

/// No parameters at all, and nothing unknown.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NoParams {}

fn params<T: serde::de::DeserializeOwned>(p: Option<serde_json::Value>) -> Result<T, RpcError> {
    serde_json::from_value(p.unwrap_or_else(|| serde_json::json!({})))
        .map_err(|e| RpcError::invalid_params(&e.to_string()))
}

/// The session a pane is in, if the pane exists.
fn session_of(snap: &shux_core::graph::SessionGraphSnapshot, pane: &str) -> Option<SessionId> {
    let id = snap.resolve_pane_ref(pane).ok()?;
    let window = snap.panes.get(&id)?.window_id;
    Some(snap.windows.get(&window)?.session_id)
}

/// What to share: the named session, else the caller's own, else the only
/// one there is that is not a lens scratch session.
fn target(
    snap: &shux_core::graph::SessionGraphSnapshot,
    named: Option<&str>,
    caller: Option<SessionId>,
    scratch: &HashSet<SessionId>,
) -> Result<SessionId, RpcError> {
    if let Some(name) = named {
        // A name is an exact match; anything else is an id or its prefix.
        if let Some(s) = snap.find_session_by_name(name) {
            return Ok(s.id);
        }
        return snap.resolve_session_ref(name).map_err(|e| match e {
            e @ shux_core::idref::RefError::Ambiguous { .. } => {
                super::params::ref_error_to_rpc(e, "session")
            }
            _ => not_found("session", name, "`shux session list` shows the sessions"),
        });
    }
    if let Some(s) = caller {
        return Ok(s);
    }
    let mut eligible = snap.sessions.keys().filter(|s| !scratch.contains(s));
    match (eligible.next(), eligible.next()) {
        (Some(s), None) => Ok(*s),
        (None, _) => Err(refusal(
            ErrorCode::InvalidParams,
            "no_session",
            "there is no session to share; start one with `shux session create`",
        )),
        _ => Err(refusal(
            ErrorCode::InvalidParams,
            "ambiguous",
            "more than one session runs; name one with `shux share -s <session>`",
        )),
    }
}

pub(crate) fn register_share_methods(
    builder: shux_rpc::RouterBuilder,
    graph: shux_core::graph::GraphHandle,
    scratch: ScratchRegistry,
    gw: Gateway,
) -> shux_rpc::RouterBuilder {
    let forbidden = || Policy::fixed(Sensitivity::PluginsForbidden);
    let (g_create, g_list, g_revoke, g_kick, g_confirm, g_stop) = (
        gw.clone(),
        gw.clone(),
        gw.clone(),
        gw.clone(),
        gw.clone(),
        gw,
    );
    let (graph_create, graph_list) = (graph.clone(), graph);
    let builder = builder
        .register_with_policy("share.create", forbidden(), move |p: Option<serde_json::Value>| {
            let (gw, graph, scratch) = (g_create.clone(), graph_create.clone(), scratch.clone());
            async move {
                let p: CreateParams = params(p)?;
                let uses = match p.uses.unwrap_or(1) {
                    0 if p.ttl_secs.is_none() => {
                        return Err(refusal(
                            ErrorCode::InvalidParams,
                            "unbounded",
                            "a link for any number of guests must say how long it lasts: add --for",
                        ));
                    }
                    0 => Uses::Unlimited,
                    n if n as usize > MAX_VIEWERS_PER_LINK => {
                        return Err(refusal(
                            ErrorCode::InvalidParams,
                            "too_many_uses",
                            &format!(
                                "a link lets in at most {MAX_VIEWERS_PER_LINK} guests; use --uses 0 --for <time> for as many as that"
                            ),
                        ));
                    }
                    n => Uses::Limited(NonZeroU32::new(n).unwrap_or(NonZeroU32::MIN)),
                };
                // Inside a shux pane, any pane, a link is never printed:
                // whoever watches that terminal, now or later, would read it.
                let Some(from_pane) = p.from_pane else {
                    return Err(refusal(
                        ErrorCode::InvalidParams,
                        "from_pane",
                        "from_pane is required: the caller's SHUX_PANE_ID, or null outside shux",
                    ));
                };
                let snap = graph.snapshot();
                let caller = from_pane.as_deref().and_then(|pane| session_of(&snap, pane));
                let session = target(&snap, p.session.as_deref(), caller, &scratch.ids())?;
                let deliver = match (from_pane.is_some(), p.qr, p.can_copy) {
                    (false, ..) => "print",
                    (true, true, _) => {
                        return Err(refusal(
                            ErrorCode::PermissionDenied,
                            "in_pane",
                            "a QR code in a shux pane would show the link to whoever watches it; run `shux share --qr` in a terminal outside shux",
                        ));
                    }
                    (true, false, true) => "clipboard",
                    (true, false, false) => {
                        return Err(refusal(
                            ErrorCode::PermissionDenied,
                            "in_pane",
                            "inside a shux pane the link is never printed, and there is no clipboard here; run `shux share` in a terminal outside shux",
                        ));
                    }
                };
                let mut opts = CreateOpts::new(session);
                if let Some(secs) = p.ttl_secs {
                    opts.ttl = Duration::from_secs(secs);
                }
                opts.uses = uses;
                opts.label = p.label;
                let label = opts.label.clone();
                let hold = match p.hold {
                    false => None,
                    true => Some(shux_rpc::current_connection().ok_or_else(|| {
                        refusal(
                            ErrorCode::InvalidParams,
                            "hold",
                            "hold needs a connection to the daemon's socket",
                        )
                    })?),
                };
                let created = gw.open(opts, hold).await.map_err(|e| share_error(e, ""))?;
                let snap = graph.snapshot();
                let name = snap.sessions.get(&session).map(|s| s.name.clone());
                let until = chrono::DateTime::<chrono::Local>::from(created.expires_wall).format("%H:%M");
                let notice = format!(
                    "Anyone with this link sees everything in {}, live, until {until}. End it with: shux share stop",
                    name.as_deref().unwrap_or("the session"),
                );
                Ok(serde_json::json!({
                    "notice": notice,
                    "id": created.id.as_str(),
                    "url": created.url,
                    "session_id": session.to_string(),
                    "session": name,
                    "expires_at": rfc3339(created.expires_wall),
                    "uses": match uses { Uses::Limited(n) => Some(n.get()), Uses::Unlimited => None },
                    "label": label,
                    "deliver": deliver,
                }))
            }
        })
        .register_with_policy("share.list", forbidden(), move |p: Option<serde_json::Value>| {
            let (gw, graph) = (g_list.clone(), graph_list.clone());
            async move {
                let NoParams {} = params(p)?;
                let snap = graph.snapshot();
                let links: Vec<serde_json::Value> = gw
                    .list()
                    .into_iter()
                    .map(|g| {
                        serde_json::json!({
                            "id": g.id.as_str(),
                            "session_id": g.session.to_string(),
                            "session": snap.sessions.get(&g.session).map(|s| s.name.clone()),
                            "label": g.label,
                            "expires_at": rfc3339(g.expires_wall),
                            "uses_left": g.uses_left,
                            "redeemed": g.redeemed,
                            "viewers": g.viewers.iter().map(|v| serde_json::json!({
                                "id": v.id.as_str(),
                                "connected": v.connected,
                            })).collect::<Vec<_>>(),
                        })
                    })
                    .collect();
                Ok(serde_json::json!({ "links": links, "tunnel": gw.tunnel_state() }))
            }
        })
        .register_with_policy("share.revoke", forbidden(), move |p: Option<serde_json::Value>| {
            let gw = g_revoke.clone();
            async move {
                #[derive(serde::Deserialize)]
                #[serde(deny_unknown_fields)]
                struct P {
                    id: String,
                }
                let P { id } = params(p)?;
                let revoked = if id == "all" {
                    let (revoked, settled) = gw.revoke_all().await;
                    settled.map_err(|e| share_error(e, &id))?;
                    revoked
                } else {
                    let g = GrantId::parse(id.trim())
                        .ok_or_else(|| share_error(ShareError::NoGrant, &id))?;
                    gw.revoke(&g).await.map_err(|e| share_error(e, &id))?;
                    vec![g]
                };
                let revoked: Vec<&str> = revoked.iter().map(GrantId::as_str).collect();
                Ok(serde_json::json!({ "revoked": revoked }))
            }
        })
        .register_with_policy("share.confirm", forbidden(), move |p: Option<serde_json::Value>| {
            let gw = g_confirm.clone();
            async move {
                #[derive(serde::Deserialize)]
                #[serde(deny_unknown_fields)]
                struct P {
                    id: String,
                }
                let P { id } = params(p)?;
                let g = GrantId::parse(id.trim())
                    .ok_or_else(|| share_error(ShareError::NoGrant, &id))?;
                gw.confirm(&g).map_err(|e| share_error(e, &id))?;
                Ok(serde_json::json!({ "confirmed": g.as_str() }))
            }
        })
        .register_with_policy("share.kick", forbidden(), move |p: Option<serde_json::Value>| {
            let gw = g_kick.clone();
            async move {
                #[derive(serde::Deserialize)]
                #[serde(deny_unknown_fields)]
                struct P {
                    viewer: String,
                }
                let P { viewer } = params(p)?;
                let v = ViewerId::parse(viewer.trim())
                    .ok_or_else(|| share_error(ShareError::NoViewer, &viewer))?;
                gw.kick(&v).await.map_err(|e| share_error(e, &viewer))?;
                Ok(serde_json::json!({ "kicked": v.as_str() }))
            }
        })
        .register_with_policy("share.stop", forbidden(), move |p: Option<serde_json::Value>| {
            let gw = g_stop.clone();
            async move {
                let NoParams {} = params(p)?;
                let ended = gw.list().len();
                gw.stop().await.map_err(|e| share_error(e, ""))?;
                Ok(serde_json::json!({ "stopped": true, "links_ended": ended }))
            }
        });
    METHODS.into_iter().fold(builder, |b, m| b.owner_local(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap_with(names: &[&str]) -> (shux_core::graph::SessionGraphSnapshot, Vec<SessionId>) {
        let mut snap = shux_core::graph::SessionGraphSnapshot::default();
        let ids: Vec<SessionId> = names
            .iter()
            .map(|n| {
                let s = shux_core::model::Session::new(*n, shux_core::model::WindowId::new());
                let id = s.id;
                snap.sessions.insert(id, s);
                id
            })
            .collect();
        (snap, ids)
    }

    #[test]
    fn the_target_is_named_else_the_callers_else_the_only_one() {
        let (snap, ids) = snap_with(&["work", "scratch"]);
        let scratch: HashSet<SessionId> = [ids[1]].into();
        assert_eq!(
            target(&snap, None, None, &scratch).unwrap(),
            ids[0],
            "a scratch session is never the only one"
        );
        assert_eq!(target(&snap, None, Some(ids[1]), &scratch).unwrap(), ids[1]);
        assert_eq!(
            target(&snap, Some("scratch"), Some(ids[0]), &scratch).unwrap(),
            ids[1]
        );
        let both = target(&snap, None, None, &HashSet::new()).unwrap_err();
        assert_eq!(both.data.unwrap()["reason"], "ambiguous");
        let (empty, _) = snap_with(&[]);
        let none = target(&empty, None, None, &HashSet::new()).unwrap_err();
        assert_eq!(none.data.unwrap()["reason"], "no_session");
    }

    /// An id prefix two sessions share is ambiguous, as everywhere else in
    /// the API, not "not found".
    #[test]
    fn an_ambiguous_session_prefix_says_so() {
        let mut snap = shux_core::graph::SessionGraphSnapshot::default();
        for id in [
            "aaaa1111-0000-4000-8000-000000000000",
            "aaaa2222-0000-4000-8000-000000000000",
        ] {
            let mut s = shux_core::model::Session::new(id, shux_core::model::WindowId::new());
            s.id = SessionId::from(uuid::Uuid::parse_str(id).unwrap());
            snap.sessions.insert(s.id, s);
        }
        let err = target(&snap, Some("aaaa"), None, &HashSet::new()).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidParams.code(), "{err:?}");
        assert!(
            err.data
                .as_ref()
                .is_some_and(|d| d.get("candidates").is_some()),
            "{err:?}"
        );
    }
}
