//! The daemon's services in one process, with the share gateway driven from
//! stdin: what `make test-share-browser` puts behind its TLS proxy. Stands in
//! for the `share.*` RPCs until they exist.
//!
//! Serves `$SHUX_SOCKET`, prints `{"addr": "127.0.0.1:N"}`, then answers one
//! JSON line per request line:
//! `{"op":"origin","origin":"https://…"}`, `{"op":"create","session":ID,"uses":N}`
//! (`uses` 0 is unlimited), `{"op":"revoke","id":ID}`, `{"op":"list"}`,
//! `{"op":"stop"}`, `{"op":"quit"}`.

use serde_json::{Value, json};
use shux::share::{CreateOpts, GrantId, Origin, Uses};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let socket = std::env::var_os("SHUX_SOCKET")
        .ok_or_else(|| anyhow::anyhow!("SHUX_SOCKET is required"))?;
    let daemon = shux::daemon_boot::InProcess::start(socket.into()).await?;
    let gw = daemon.gateway.clone();
    let addr = gw.listen("127.0.0.1:0".parse()?).await?;
    let mut out = tokio::io::stdout();
    let say = |v: Value| format!("{v}\n");
    out.write_all(say(json!({ "addr": addr.to_string() })).as_bytes())
        .await?;
    out.flush().await?;
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        let req: Value = serde_json::from_str(&line)?;
        let field = |k: &str| req[k].as_str().unwrap_or_default().to_string();
        let reply = match req["op"].as_str() {
            Some("origin") => {
                gw.set_origin(Origin::parse(&field("origin"))?).await;
                json!({ "ok": true })
            }
            Some("create") => {
                let mut opts = CreateOpts::new(field("session").parse()?);
                if let Some(n) = req["uses"].as_u64() {
                    opts.uses = std::num::NonZeroU32::new(u32::try_from(n)?).map_or(Uses::Unlimited, Uses::Limited);
                }
                match gw.create(opts) {
                    Ok(c) => json!({ "id": c.id.as_str(), "url": c.url }),
                    Err(e) => json!({ "error": e.to_string() }),
                }
            }
            Some("revoke") => match GrantId::parse(&field("id")) {
                Some(id) => json!({ "ok": gw.revoke(&id).await.is_ok() }),
                None => json!({ "error": "bad id" }),
            },
            Some("list") => json!(gw
                .list()
                .iter()
                .map(|g| json!({
                    "id": g.id.as_str(),
                    "uses_left": g.uses_left,
                    "redeemed": g.redeemed,
                    "viewers": g.viewers.iter().map(|v| json!({ "id": v.id.as_str(), "connected": v.connected })).collect::<Vec<_>>(),
                }))
                .collect::<Vec<_>>()),
            Some("stop") => {
                gw.stop().await;
                json!({ "ok": true })
            }
            Some("quit") => break,
            _ => json!({ "error": "unknown op" }),
        };
        out.write_all(say(reply).as_bytes()).await?;
        out.flush().await?;
    }
    daemon.shutdown().await;
    Ok(())
}
