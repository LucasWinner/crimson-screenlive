//! ICE server config (STUN/TURN) fetched from the signaling server and applied to webrtcbin.

use anyhow::Result;
use gst::prelude::*;
use serde::Deserialize;

use crate::signaling::Server;

#[derive(Debug, Clone, Deserialize)]
pub struct IceServer {
    #[serde(deserialize_with = "one_or_many")]
    pub urls: Vec<String>,
    pub username: Option<String>,
    pub credential: Option<String>,
}

fn one_or_many<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum V {
        One(String),
        Many(Vec<String>),
    }
    Ok(match V::deserialize(d)? {
        V::One(s) => vec![s],
        V::Many(v) => v,
    })
}

pub async fn fetch(server: &Server) -> Vec<IceServer> {
    #[derive(Deserialize)]
    struct R {
        #[serde(rename = "iceServers")]
        ice_servers: Vec<IceServer>,
    }
    let res: Result<R> = async {
        Ok(reqwest::get(server.http("/ice")).await?.error_for_status()?.json().await?)
    }
    .await;
    match res {
        Ok(r) => r.ice_servers,
        Err(e) => {
            log::warn!("could not fetch ICE servers ({e}); using public STUN");
            vec![IceServer {
                urls: vec!["stun:stun.cloudflare.com:3478".into()],
                username: None,
                credential: None,
            }]
        }
    }
}

fn enc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>().replace('+', "%20")
}

/// Converts W3C-style ICE URLs into webrtcbin's URI format.
/// `stun:host:port` -> `stun://host:port`
/// `turn:host:port?transport=udp` -> `turn://user:pass@host:port?transport=udp`
pub fn to_gst_uris(servers: &[IceServer]) -> (Option<String>, Vec<String>) {
    let mut stun = None;
    let mut turn = Vec::new();
    for s in servers {
        for u in &s.urls {
            if let Some(rest) = u.strip_prefix("stun:") {
                stun.get_or_insert_with(|| format!("stun://{}", rest.trim_start_matches("//")));
            } else if let Some((scheme, rest)) = u.split_once(':') {
                if scheme != "turn" && scheme != "turns" {
                    continue;
                }
                let (Some(user), Some(pass)) = (&s.username, &s.credential) else { continue };
                turn.push(format!("{scheme}://{}:{}@{}", enc(user), enc(pass), rest.trim_start_matches("//")));
            }
        }
    }
    (stun, turn)
}

pub fn apply(webrtcbin: &gst::Element, servers: &[IceServer]) {
    let (stun, turn) = to_gst_uris(servers);
    if let Some(stun) = stun {
        webrtcbin.set_property("stun-server", stun);
    }
    for t in turn {
        let ok = webrtcbin.emit_by_name::<bool>("add-turn-server", &[&t]);
        if !ok {
            log::warn!("webrtcbin rejected a TURN server");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_urls() {
        let servers: Vec<IceServer> = serde_json::from_str(
            r#"[{"urls":["stun:stun.cloudflare.com:3478","stun:stun.l.google.com:19302"]},
                {"urls":["turn:turn.cloudflare.com:3478?transport=udp","turns:turn.cloudflare.com:5349?transport=tcp"],
                 "username":"u1","credential":"a+b/c="}]"#,
        )
        .unwrap();
        let (stun, turn) = to_gst_uris(&servers);
        assert_eq!(stun.as_deref(), Some("stun://stun.cloudflare.com:3478"));
        assert_eq!(
            turn,
            vec![
                "turn://u1:a%2Bb%2Fc%3D@turn.cloudflare.com:3478?transport=udp",
                "turns://u1:a%2Bb%2Fc%3D@turn.cloudflare.com:5349?transport=tcp",
            ]
        );
    }

    #[test]
    fn single_url_string() {
        let s: IceServer = serde_json::from_str(r#"{"urls":"stun:x:1"}"#).unwrap();
        assert_eq!(s.urls, vec!["stun:x:1"]);
    }
}
