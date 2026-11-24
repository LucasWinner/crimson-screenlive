//! WebSocket client for the room signaling server.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use url::Url;

use crate::protocol::{ClientMsg, Payload, ServerMsg};

enum Outgoing {
    Msg(ClientMsg),
    Close,
}

/// Cheap, cloneable sender usable from GStreamer callback threads.
#[derive(Clone)]
pub struct SignalingHandle {
    tx: mpsc::UnboundedSender<Outgoing>,
}

impl SignalingHandle {
    pub fn send(&self, msg: ClientMsg) {
        if self.tx.send(Outgoing::Msg(msg)).is_err() {
            log::debug!("signaling connection closed; dropping message");
        }
    }

    /// Leaves the room now (after flushing already-queued messages), even if
    /// other clones of this handle are still alive.
    pub fn close(&self) {
        let _ = self.tx.send(Outgoing::Close);
    }

    pub fn signal(&self, to: &str, payload: Payload) {
        self.send(ClientMsg::Signal { to: to.to_owned(), payload });
    }
}

/// Room codes: 6 chars, no look-alikes (0/O, 1/I). Accepts "kx7-p2q" style input.
/// Must match `normalizeCode` in signaling/src/index.ts.
pub fn normalize_code(raw: &str) -> Option<String> {
    const ALPHABET: &str = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let code: String = raw.chars().filter(|c| !c.is_whitespace() && *c != '-').collect::<String>().to_uppercase();
    (code.len() == 6 && code.chars().all(|c| ALPHABET.contains(c))).then_some(code)
}

/// A joined room: the connection plus what the server told us on arrival.
pub struct Joined {
    pub sig: SignalingHandle,
    pub events: mpsc::UnboundedReceiver<ServerMsg>,
    pub me: String,
    pub peers: Vec<crate::protocol::Peer>,
    pub ice: Vec<crate::ice::IceServer>,
    pub turn_capped: bool,
}

/// HTTP base URL of the signaling server, e.g. `https://x.workers.dev`.
#[derive(Clone, Debug)]
pub struct Server {
    base: Url,
    key: Option<String>,
    http_client: reqwest::Client,
}

/// Header carrying the app access key (checked by the server).
pub const KEY_HEADER: &str = "x-screenlive-key";

/// The app key: SCREENLIVE_APP_KEY at runtime, else the one compiled in from `.app-key`.
fn app_key() -> Option<String> {
    std::env::var("SCREENLIVE_APP_KEY")
        .ok()
        .or_else(|| option_env!("SCREENLIVE_APP_KEY").map(str::to_owned))
        .map(|k| k.trim().to_owned())
        .filter(|k| !k.is_empty())
}

impl Server {
    pub fn new(base: &str) -> Result<Self> {
        let base = Url::parse(base).with_context(|| format!("invalid server URL: {base}"))?;
        if !matches!(base.scheme(), "http" | "https") {
            bail!("server URL must be http(s)://");
        }
        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .context("building HTTP client")?;
        Ok(Self { base, key: app_key(), http_client })
    }

    pub fn http(&self, path: &str) -> Url {
        self.base.join(path).expect("static path")
    }

    /// Asks the server for a fresh random room code.
    pub async fn create_room(&self) -> Result<String> {
        #[derive(serde::Deserialize)]
        struct R {
            code: String,
        }
        let mut req = self.http_client.post(self.http("/rooms"));
        if let Some(k) = &self.key {
            req = req.header(KEY_HEADER, k);
        }
        let res = req.send().await.context("could not reach signaling server")?;
        if res.status() == reqwest::StatusCode::UNAUTHORIZED {
            bail!("invalid app key: this version of the app can't use this server");
        }
        let r: R = res.error_for_status()?.json().await?;
        Ok(r.code)
    }

    /// Joins a room and waits for the server's welcome (our id, who's there, ICE config).
    pub async fn enter(&self, room: &str, name: &str) -> Result<Joined> {
        let (sig, mut events) = self.join(room, name).await?;
        match tokio::time::timeout(Duration::from_secs(10), events.recv()).await {
            Ok(Some(ServerMsg::Welcome { id, peers, ice_servers, turn_capped })) => {
                if turn_capped {
                    log::warn!("monthly TURN budget reached: only direct connections this month");
                }
                Ok(Joined {
                    sig,
                    events,
                    me: id,
                    peers,
                    ice: if ice_servers.is_empty() { crate::ice::public_stun() } else { ice_servers },
                    turn_capped,
                })
            }
            Ok(Some(other)) => bail!("unexpected first message from server: {other:?}"),
            _ => bail!("the server did not answer"),
        }
    }

    /// Connects to a room. Returns a send handle and the stream of server messages.
    pub async fn join(
        &self,
        room: &str,
        name: &str,
    ) -> Result<(SignalingHandle, mpsc::UnboundedReceiver<ServerMsg>)> {
        let Some(room) = normalize_code(room) else {
            bail!("invalid room code '{room}': use 6 letters/digits (no 0, O, 1 or I)");
        };
        if name.trim().is_empty() {
            bail!("nickname can't be empty");
        }
        let mut ws_url = self.http(&format!("/rooms/{room}/ws"));
        ws_url.set_scheme(if self.base.scheme() == "https" { "wss" } else { "ws" }).unwrap();
        ws_url.query_pairs_mut().append_pair("name", name);

        let mut request = ws_url.as_str().into_client_request()?;
        if let Some(k) = &self.key {
            request.headers_mut().insert(KEY_HEADER, HeaderValue::from_str(k).context("bad app key")?);
        }
        let (ws, _) = match tokio::time::timeout(Duration::from_secs(15), tokio_tungstenite::connect_async(request))
            .await
        {
            Ok(Ok(ok)) => ok,
            Ok(Err(tokio_tungstenite::tungstenite::Error::Http(resp))) if resp.status().as_u16() == 401 => {
                bail!("invalid app key: this version of the app can't use this server")
            }
            Ok(Err(e)) => return Err(anyhow::Error::new(e).context(format!("could not join room {room}"))),
            Err(_) => bail!("could not reach signaling server (connection timed out)"),
        };
        let (mut sink, mut stream) = ws.split();

        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Outgoing>();
        let (in_tx, in_rx) = mpsc::unbounded_channel::<ServerMsg>();

        tokio::spawn(async move {
            let mut keepalive = tokio::time::interval(Duration::from_secs(20));
            keepalive.tick().await;
            let mut awaiting_pong = false;
            loop {
                tokio::select! {
                    out = out_rx.recv() => {
                        let Some(Outgoing::Msg(msg)) = out else { break };
                        let text = serde_json::to_string(&msg).expect("serializable");
                        if sink.send(Message::text(text)).await.is_err() { break }
                    }
                    _ = keepalive.tick() => {
                        if awaiting_pong {
                            log::warn!("signaling keepalive timed out");
                            break;
                        }
                        let text = serde_json::to_string(&ClientMsg::Ping).unwrap();
                        if sink.send(Message::text(text)).await.is_err() { break }
                        awaiting_pong = true;
                    }
                    incoming = stream.next() => {
                        match incoming {
                            Some(Ok(Message::Text(text))) => {
                                awaiting_pong = false;
                                match serde_json::from_str::<ServerMsg>(&text) {
                                    Ok(ServerMsg::Pong) => {}
                                    Ok(msg) => { if in_tx.send(msg).is_err() { break } }
                                    Err(e) => log::warn!("ignoring bad server message ({e}): {text}"),
                                }
                            }
                            Some(Ok(Message::Close(_))) | None => break,
                            Some(Ok(_)) => { awaiting_pong = false; }
                            Some(Err(e)) => { log::warn!("signaling socket error: {e}"); break }
                        }
                    }
                }
            }
            let _ = sink.close().await;
            log::info!("signaling connection closed");
        });

        Ok((SignalingHandle { tx: out_tx }, in_rx))
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_code;

    #[test]
    fn room_codes() {
        assert_eq!(normalize_code("kx7-p2q").as_deref(), Some("KX7P2Q"));
        assert_eq!(normalize_code(" KX7 P2Q ").as_deref(), Some("KX7P2Q"));
        assert_eq!(normalize_code("MULTIA"), None); // I is not allowed
        assert_eq!(normalize_code("ABC10Z"), None); // 1 and 0 are not allowed
        assert_eq!(normalize_code("ABCDE"), None);
    }
}
