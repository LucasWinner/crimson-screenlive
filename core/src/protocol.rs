//! Wire types shared with the signaling server (`signaling/src/index.ts`).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Peer {
    pub id: String,
    pub name: String,
    pub streaming: bool,
}

/// Messages the server sends to us.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ServerMsg {
    Welcome { id: String, peers: Vec<Peer> },
    PeerJoined { peer: Peer },
    PeerLeft { id: String },
    PeerUpdated { peer: Peer },
    Signal { from: String, payload: Payload },
    Error { message: String },
    Pong,
}

/// Messages we send to the server.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ClientMsg {
    Signal { to: String, payload: Payload },
    Stream { streaming: bool },
    /// Bytes we pushed through TURN since the last report (for the monthly cap).
    TurnUsage { bytes: u64 },
    Ping,
}

/// Peer-to-peer payload relayed verbatim by the server inside `signal`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Payload {
    /// Viewer -> streamer: "please send me your stream".
    Watch,
    /// Streamer -> viewer.
    Offer { sdp: String },
    /// Viewer -> streamer.
    Answer { sdp: String },
    /// Both directions (trickle ICE).
    Ice { candidate: String, mline: u32 },
    /// Either side is ending this connection.
    Bye,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_messages_parse() {
        let m: ServerMsg = serde_json::from_str(
            r#"{"type":"welcome","id":"a","peers":[{"id":"b","name":"Bob","streaming":true}]}"#,
        )
        .unwrap();
        assert!(matches!(m, ServerMsg::Welcome { ref peers, .. } if peers[0].streaming));

        let m: ServerMsg = serde_json::from_str(
            r#"{"type":"signal","from":"b","payload":{"kind":"ice","candidate":"c","mline":0}}"#,
        )
        .unwrap();
        assert!(matches!(m, ServerMsg::Signal { payload: Payload::Ice { mline: 0, .. }, .. }));
    }

    #[test]
    fn client_messages_serialize() {
        let s = serde_json::to_string(&ClientMsg::Signal { to: "x".into(), payload: Payload::Watch }).unwrap();
        assert_eq!(s, r#"{"type":"signal","to":"x","payload":{"kind":"watch"}}"#);
        let s = serde_json::to_string(&ClientMsg::Stream { streaming: true }).unwrap();
        assert_eq!(s, r#"{"type":"stream","streaming":true}"#);
    }
}
