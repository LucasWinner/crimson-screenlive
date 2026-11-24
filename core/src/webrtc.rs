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

fn stat(stats: &gst::Structure, id: &str) -> Option<gst::Structure> {
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

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LinkStats {
    pub rtt_ms: Option<f64>,
    pub loss_pct: Option<f64>,
    pub jitter_ms: Option<f64>,
}

/// Extracts connection health (RTT, packet loss %, jitter) from webrtcbin stats.
pub fn link_stats(stats: &gst::Structure) -> LinkStats {
    let mut rtt_ms: Option<f64> = None;
    let mut loss_pct: Option<f64> = None;
    let mut jitter_ms: Option<f64> = None;

    for (_, v) in stats.iter() {
        let Ok(s) = v.get::<gst::Structure>() else { continue };
        match s.name().as_str() {
            "remote-inbound-rtp" => {
                if let Ok(rtt) = s.get::<f64>("round-trip-time") {
                    if rtt >= 0.0 {
                        let ms = rtt * 1000.0;
                        rtt_ms = Some(rtt_ms.map_or(ms, |cur| cur.max(ms)));
                    }
                }
                if let Ok(frac) = s.get::<f64>("fraction-lost") {
                    if frac >= 0.0 {
                        let pct = (frac * 100.0).clamp(0.0, 100.0);
                        loss_pct = Some(loss_pct.map_or(pct, |cur| cur.max(pct)));
                    }
                }
                if let Ok(jit) = s.get::<f64>("jitter") {
                    if jit >= 0.0 {
                        let ms = jit * 1000.0;
                        jitter_ms = Some(jitter_ms.map_or(ms, |cur| cur.max(ms)));
                    }
                }
            }
            "inbound-rtp" => {
                if let Ok(jit) = s.get::<f64>("jitter") {
                    if jit >= 0.0 {
                        let ms = jit * 1000.0;
                        jitter_ms = Some(jitter_ms.map_or(ms, |cur| cur.max(ms)));
                    }
                }
            }
            _ => {}
        }
    }
    LinkStats { rtt_ms, loss_pct, jitter_ms }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_stats(local_type: &str, remote_type: &str, bytes_v: u64, bytes_a: u64) -> gst::Structure {
        let local = gst::Structure::builder("local-candidate").field("candidate-type", local_type).build();
        let remote = gst::Structure::builder("remote-candidate").field("candidate-type", remote_type).build();
        let pair = gst::Structure::builder("candidate-pair")
            .field("local-candidate-id", "loc")
            .field("remote-candidate-id", "rem")
            .build();
        let transport = gst::Structure::builder("transport").field("selected-candidate-pair-id", "pair").build();
        let out_v = gst::Structure::builder("outbound-rtp").field("bytes-sent", bytes_v).build();
        let out_a = gst::Structure::builder("outbound-rtp").field("bytes-sent", bytes_a).build();
        let in_v = gst::Structure::builder("inbound-rtp").field("bytes-received", 999u64).build();
        gst::Structure::builder("application/x-webrtc-stats")
            .field("loc", local)
            .field("rem", remote)
            .field("pair", pair)
            .field("tr", transport)
            .field("out_v", out_v)
            .field("out_a", out_a)
            .field("in_v", in_v)
            .build()
    }

    #[test]
    fn detects_relayed_and_sums_bytes() {
        gst::init().unwrap();
        let direct = mk_stats("host", "srflx", 1000, 250);
        assert_eq!(is_relayed(&direct), Some(false));
        assert_eq!(bytes_sent(&direct), 1250);

        let local_relay = mk_stats("relay", "host", 4000, 500);
        assert_eq!(is_relayed(&local_relay), Some(true));
        assert_eq!(bytes_sent(&local_relay), 4500);

        let remote_relay = mk_stats("srflx", "relay", 0, 0);
        assert_eq!(is_relayed(&remote_relay), Some(true));

        let empty = gst::Structure::new_empty("application/x-webrtc-stats");
        assert_eq!(is_relayed(&empty), None);
        assert_eq!(bytes_sent(&empty), 0);
        assert_eq!(link_stats(&empty), LinkStats::default());

        let remote_in = gst::Structure::builder("remote-inbound-rtp")
            .field("round-trip-time", 0.042f64)
            .field("fraction-lost", 0.025f64)
            .field("jitter", 0.005f64)
            .build();
        let health = gst::Structure::builder("application/x-webrtc-stats").field("r", remote_in).build();
        assert_eq!(
            link_stats(&health),
            LinkStats { rtt_ms: Some(42.0), loss_pct: Some(2.5), jitter_ms: Some(5.0) }
        );
    }

    #[test]
    fn av1_webrtc_loopback() {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::Arc;
        use std::time::Duration;

        gst::init().unwrap();
        let Ok(spec) = crate::encoder::pick_for_codec(None, crate::encoder::Codec::Av1) else {
            return; // skip on machines without hardware AV1 encoder
        };

        let pipe_s = gst::Pipeline::new();
        let src = gst::parse::bin_from_description(
            "videotestsrc is-live=true ! capsfilter caps=\"video/x-raw,format=NV12,width=320,height=240,framerate=30/1\"",
            true,
        )
        .unwrap();
        let enc = crate::encoder::build(
            &spec,
            &crate::encoder::EncoderSettings { bitrate_kbps: 1500, fps: 30, keyframe_secs: 2 },
        )
        .unwrap();
        let pay = gst::parse::bin_from_description(
            "queue leaky=downstream max-size-buffers=0 max-size-bytes=0 max-size-time=300000000 ! av1parse ! rtpav1pay pt=96 mtu=1200 ! capsfilter caps=\"application/x-rtp,media=video,encoding-name=AV1,payload=96,clock-rate=90000\"",
            true,
        )
        .unwrap();
        let ws = gst::ElementFactory::make("webrtcbin").build().unwrap();
        pipe_s.add_many([src.upcast_ref(), &enc, pay.upcast_ref(), &ws]).unwrap();
        gst::Element::link_many([src.upcast_ref(), &enc, pay.upcast_ref(), &ws]).unwrap();

        let pipe_r = gst::Pipeline::new();
        let wr = gst::ElementFactory::make("webrtcbin").build().unwrap();
        pipe_r.add(&wr).unwrap();

        let wr_c = wr.clone();
        ws.connect("on-ice-candidate", false, move |v| {
            wr_c.emit_by_name::<()>("add-ice-candidate", &[&v[1].get::<u32>().unwrap(), &v[2].get::<String>().unwrap()]);
            None
        });
        let ws_c = ws.clone();
        wr.connect("on-ice-candidate", false, move |v| {
            ws_c.emit_by_name::<()>("add-ice-candidate", &[&v[1].get::<u32>().unwrap(), &v[2].get::<String>().unwrap()]);
            None
        });

        let (tx, rx) = std::sync::mpsc::channel::<String>();
        ws.connect("on-negotiation-needed", false, move |v| {
            let w = v[0].get::<gst::Element>().unwrap();
            let tx = tx.clone();
            create_offer(&w, move |sdp| {
                let _ = tx.send(sdp);
            });
            None
        });

        let frames = Arc::new(AtomicU64::new(0));
        let frames_c = frames.clone();
        let pipe_r_weak = pipe_r.downgrade();
        wr.connect_pad_added(move |_, pad| {
            let Some(pipe_r) = pipe_r_weak.upgrade() else { return };
            let decodebin = gst::ElementFactory::make("decodebin").build().unwrap();
            let pipe_r_w2 = pipe_r.downgrade();
            let fc = frames_c.clone();
            decodebin.connect_pad_added(move |_, dpad| {
                let Some(pipe_r) = pipe_r_w2.upgrade() else { return };
                let bin = gst::parse::bin_from_description("queue max-size-buffers=3 ! fakesink sync=false", true).unwrap();
                pipe_r.add(&bin).unwrap();
                bin.sync_state_with_parent().unwrap();
                let sink = bin.static_pad("sink").unwrap();
                let f = fc.clone();
                sink.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
                    f.fetch_add(1, Ordering::Relaxed);
                    gst::PadProbeReturn::Ok
                });
                dpad.link(&sink).unwrap();
            });
            pipe_r.add(&decodebin).unwrap();
            decodebin.sync_state_with_parent().unwrap();
            pad.link(&decodebin.static_pad("sink").unwrap()).unwrap();
        });

        pipe_r.set_state(gst::State::Playing).unwrap();
        pipe_s.set_state(gst::State::Playing).unwrap();

        let offer_sdp = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(offer_sdp.contains("AV1/90000"), "SDP should offer AV1: {offer_sdp}");
        let (tx2, rx2) = std::sync::mpsc::channel::<String>();
        accept_offer(&wr, &offer_sdp, move |sdp| {
            let _ = tx2.send(sdp);
        })
        .unwrap();
        let answer_sdp = rx2.recv_timeout(Duration::from_secs(5)).unwrap();
        accept_answer(&ws, &answer_sdp).unwrap();

        for _ in 0..30 {
            if frames.load(Ordering::Relaxed) >= 5 {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let got = frames.load(Ordering::Relaxed);
        let s = stats(&ws).unwrap();
        assert!(got >= 5, "expected decoded AV1 RTP buffers, got {got}");
        assert!(bytes_sent(&s) > 0, "expected outbound bytes_sent > 0");

        pipe_s.set_state(gst::State::Null).unwrap();
        pipe_r.set_state(gst::State::Null).unwrap();
    }
}
