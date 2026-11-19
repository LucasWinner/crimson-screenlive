//! Small helpers around webrtcbin's signal/promise API shared by streamer and viewer.

use anyhow::{anyhow, Context, Result};
use gst::prelude::*;
use gst_webrtc::{WebRTCSDPType, WebRTCSessionDescription};

use crate::ice::{self, IceServer};
use crate::protocol::Payload;
use crate::signaling::SignalingHandle;

/// Creates a webrtcbin wired to trickle its ICE candidates to `remote`.
pub fn new_webrtcbin(name: &str, ice_servers: &[IceServer], sig: &SignalingHandle, remote: &str) -> Result<gst::Element> {
    let webrtc = gst::ElementFactory::make("webrtcbin")
        .name(name)
        .property_from_str("bundle-policy", "max-bundle")
        .build()
        .context("webrtcbin missing (gst-plugins-bad)")?;
    ice::apply(&webrtc, ice_servers);
    // Testing aid: SCREENLIVE_RELAY_ONLY=1 forces all media through TURN.
    if std::env::var("SCREENLIVE_RELAY_ONLY").is_ok_and(|v| v == "1") {
        webrtc.set_property_from_str("ice-transport-policy", "relay");
        log::info!("{name}: relay-only (TURN) mode");
    }

    let (sig, remote) = (sig.clone(), remote.to_owned());
    webrtc.connect("on-ice-candidate", false, move |values| {
        let mline = values[1].get::<u32>().expect("mline");
        let candidate = values[2].get::<String>().expect("candidate");
        sig.signal(&remote, Payload::Ice { candidate, mline });
        None
    });

    webrtc.connect_notify(Some("connection-state"), |w, _| {
        let state = w.property::<gst_webrtc::WebRTCPeerConnectionState>("connection-state");
        log::info!("{}: connection {state:?}", w.name());
    });
    webrtc.connect_notify(Some("ice-connection-state"), |w, _| {
        let state = w.property::<gst_webrtc::WebRTCICEConnectionState>("ice-connection-state");
        log::debug!("{}: ICE {state:?}", w.name());
    });
    Ok(webrtc)
}

fn parse(kind: WebRTCSDPType, sdp: &str) -> Result<WebRTCSessionDescription> {
    let msg = gst_sdp::SDPMessage::parse_buffer(sdp.as_bytes()).map_err(|_| anyhow!("invalid SDP"))?;
    Ok(WebRTCSessionDescription::new(kind, msg))
}

/// Creates an offer (or answer), sets it as local description, and hands the SDP text to `done`.
fn create_local(webrtc: &gst::Element, kind: WebRTCSDPType, done: impl FnOnce(String) + Send + 'static) {
    let (signal, field) = match kind {
        WebRTCSDPType::Offer => ("create-offer", "offer"),
        _ => ("create-answer", "answer"),
    };
    let weak = webrtc.downgrade();
    let promise = gst::Promise::with_change_func(move |reply| {
        let Some(webrtc) = weak.upgrade() else { return };
        let desc = match reply {
            Ok(Some(s)) => s.get::<WebRTCSessionDescription>(field),
            other => {
                log::error!("{signal} failed: {other:?}");
                return;
            }
        };
        let Ok(desc) = desc else {
            log::error!("{signal}: reply had no '{field}'");
            return;
        };
        webrtc.emit_by_name::<()>("set-local-description", &[&desc, &None::<gst::Promise>]);
        match desc.sdp().as_text() {
            Ok(text) => done(text),
            Err(e) => log::error!("could not serialize SDP: {e}"),
        }
    });
    webrtc.emit_by_name::<()>(signal, &[&None::<gst::Structure>, &promise]);
}

pub fn create_offer(webrtc: &gst::Element, done: impl FnOnce(String) + Send + 'static) {
    create_local(webrtc, WebRTCSDPType::Offer, done);
}

/// Applies a remote offer and replies with an answer.
pub fn accept_offer(webrtc: &gst::Element, sdp: &str, done: impl FnOnce(String) + Send + 'static) -> Result<()> {
    let offer = parse(WebRTCSDPType::Offer, sdp)?;
    let weak = webrtc.downgrade();
    let promise = gst::Promise::with_change_func(move |_| {
        if let Some(webrtc) = weak.upgrade() {
            create_local(&webrtc, WebRTCSDPType::Answer, done);
        }
    });
    webrtc.emit_by_name::<()>("set-remote-description", &[&offer, &promise]);
    Ok(())
}

pub fn accept_answer(webrtc: &gst::Element, sdp: &str) -> Result<()> {
    let answer = parse(WebRTCSDPType::Answer, sdp)?;
    webrtc.emit_by_name::<()>("set-remote-description", &[&answer, &None::<gst::Promise>]);
    Ok(())
}

pub fn add_ice(webrtc: &gst::Element, mline: u32, candidate: &str) {
    webrtc.emit_by_name::<()>("add-ice-candidate", &[&mline, &candidate]);
}

/// Synchronously fetches webrtcbin's stats (blocks briefly; call off the
/// streaming threads).
pub fn stats(webrtc: &gst::Element) -> Option<gst::Structure> {
    let promise = gst::Promise::new();
    webrtc.emit_by_name::<()>("get-stats", &[&None::<gst::Pad>, &promise]);
    match promise.wait() {
        gst::PromiseResult::Replied => promise.get_reply().map(|s| s.to_owned()),
        _ => None,
    }
}

fn stat<'a>(stats: &'a gst::Structure, id: &str) -> Option<gst::Structure> {
    stats.get::<gst::Structure>(id).ok()
}

/// True if the selected ICE path goes through a TURN relay on either side.
/// None until a candidate pair has been selected.
pub fn is_relayed(stats: &gst::Structure) -> Option<bool> {
    let pair_id = stats.iter().find_map(|(_, v)| {
        let s = v.get::<gst::Structure>().ok()?;
        s.get::<String>("selected-candidate-pair-id").ok()
    })?;
    let pair = stat(stats, &pair_id)?;
    let kind = |field: &str| -> Option<String> {
        let cand = stat(stats, &pair.get::<String>(field).ok()?)?;
        cand.get::<String>("candidate-type").ok()
    };
    let (local, remote) = (kind("local-candidate-id")?, kind("remote-candidate-id")?);
    Some(local == "relay" || remote == "relay")
}

/// Total RTP payload+header bytes sent on all outgoing streams.
pub fn bytes_sent(stats: &gst::Structure) -> u64 {
    stats
        .iter()
        .filter_map(|(_, v)| v.get::<gst::Structure>().ok())
        .filter(|s| s.name() == "outbound-rtp")
        .filter_map(|s| s.get::<u64>("bytes-sent").ok())
        .sum()
}

/// Turns the first error/EOS on the bus into a Result; logs warnings.
pub fn check_bus_message(msg: &gst::Message) -> Result<bool> {
    use gst::MessageView;
    match msg.view() {
        MessageView::Error(e) => Err(anyhow!(
            "{} (from {})\n{}",
            e.error(),
            e.src().map(|s| s.path_string().to_string()).unwrap_or_default(),
            e.debug().map(|d| d.to_string()).unwrap_or_default()
        )),
        MessageView::Warning(w) => {
            log::warn!("{}", w.error());
            Ok(true)
        }
        MessageView::Eos(_) => Ok(false),
        _ => Ok(true),
    }
}
