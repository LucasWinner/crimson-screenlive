//! Streamer: capture -> encode once -> tee -> one webrtcbin per viewer.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use anyhow::{Context, Result};
use futures_util::StreamExt;
use gst::prelude::*;
use tokio::sync::mpsc;

use crate::capture::{self, AudioSource, VideoFormat, VideoSource};
use crate::encoder::{self, EncoderSettings, EncoderSpec};
use crate::ice::IceServer;
use crate::protocol::{ClientMsg, Payload, ServerMsg};
use crate::signaling::SignalingHandle;
use crate::webrtc;

pub struct StreamConfig {
    pub video: VideoSource,
    pub audio: AudioSource,
    pub format: VideoFormat,
    pub bitrate_kbps: u32,
    pub encoder: Option<String>,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            video: VideoSource::MonitorIndex(0),
            audio: AudioSource::System,
            format: VideoFormat { width: 1920, height: 1080, fps: 60 },
            bitrate_kbps: 7000,
            encoder: None,
        }
    }
}

const VIDEO_RTP: &str = "h264parse config-interval=-1 \
    ! rtph264pay config-interval=-1 aggregate-mode=zero-latency pt=96 mtu=1200 \
    ! capsfilter caps=\"application/x-rtp,media=video,encoding-name=H264,payload=96,clock-rate=90000\"";
const AUDIO_RTP: &str = "rtpopuspay pt=111 \
    ! capsfilter caps=\"application/x-rtp,media=audio,encoding-name=OPUS,payload=111,clock-rate=48000\"";
/// Per-viewer queue: drop old data instead of building latency on a slow link.
const PEER_QUEUE: &str = "queue leaky=downstream max-size-buffers=0 max-size-bytes=0 max-size-time=300000000";
pub const MAX_VIEWERS: usize = 4;

struct PeerConn {
    webrtc: gst::Element,
    /// (tee request pad, queue) for each media branch.
    branches: Vec<(gst::Pad, gst::Element)>,
}

struct Inner {
    pipeline: gst::Pipeline,
    encoder: gst::Element,
    tees: Vec<gst::Element>,
    peers: Mutex<HashMap<String, PeerConn>>,
    sig: SignalingHandle,
    ice: Vec<IceServer>,
}

pub struct Streamer {
    inner: Arc<Inner>,
    pub encoder: EncoderSpec,
}

impl Streamer {
    pub fn new(cfg: &StreamConfig, sig: SignalingHandle, ice: Vec<IceServer>) -> Result<Self> {
        let spec = encoder::pick(cfg.encoder.as_deref())?;
        let pipeline = gst::Pipeline::with_name("streamer");

        let vsrc = gst::parse::bin_from_description(&capture::video_fragment(&cfg.video, &spec, &cfg.format)?, true)
            .context("building video source")?;
        let enc = encoder::build(
            &spec,
            &EncoderSettings { bitrate_kbps: cfg.bitrate_kbps, fps: cfg.format.fps, keyframe_secs: 2 },
        )?;
        let vpay = gst::parse::bin_from_description(VIDEO_RTP, true)?;
        let vtee = gst::ElementFactory::make("tee").name("vtee").property("allow-not-linked", true).build()?;
        pipeline.add_many([vsrc.upcast_ref(), &enc, vpay.upcast_ref(), &vtee])?;
        gst::Element::link_many([vsrc.upcast_ref(), &enc, vpay.upcast_ref(), &vtee])?;
        let mut tees = vec![vtee];

        if let Some(frag) = capture::audio_fragment(cfg.audio) {
            let abin = gst::parse::bin_from_description(&format!("{frag} ! {AUDIO_RTP}"), true)
                .context("building audio source")?;
            let atee = gst::ElementFactory::make("tee").name("atee").property("allow-not-linked", true).build()?;
            pipeline.add_many([abin.upcast_ref(), &atee])?;
            abin.link(&atee)?;
            tees.push(atee);
        }

        Ok(Self {
            inner: Arc::new(Inner { pipeline, encoder: enc, tees, peers: Mutex::default(), sig, ice }),
            encoder: spec,
        })
    }

    pub fn pipeline(&self) -> &gst::Pipeline {
        &self.inner.pipeline
    }

    pub fn viewer_count(&self) -> usize {
        self.inner.peers.lock().unwrap().len()
    }

    pub fn start(&self) -> Result<()> {
        self.inner.pipeline.set_state(gst::State::Playing).context("could not start capture pipeline")?;
        self.inner.sig.send(ClientMsg::Stream { streaming: true });
        Ok(())
    }

    pub fn stop(&self) {
        let ids: Vec<String> = self.inner.peers.lock().unwrap().keys().cloned().collect();
        for id in ids {
            self.inner.sig.signal(&id, Payload::Bye);
            remove_peer(&self.inner, &id);
        }
        self.inner.sig.send(ClientMsg::Stream { streaming: false });
        let _ = self.inner.pipeline.set_state(gst::State::Null);
    }

    pub fn handle(&self, msg: ServerMsg) {
        match msg {
            ServerMsg::Signal { from, payload } => self.handle_payload(&from, payload),
            ServerMsg::PeerLeft { id } => remove_peer(&self.inner, &id),
            ServerMsg::Error { message } => log::warn!("server: {message}"),
            _ => {}
        }
    }

    fn handle_payload(&self, from: &str, payload: Payload) {
        let webrtc = || self.inner.peers.lock().unwrap().get(from).map(|p| p.webrtc.clone());
        let res = match payload {
            Payload::Watch => add_peer(&self.inner, from),
            Payload::Answer { sdp } => match webrtc() {
                Some(w) => webrtc::accept_answer(&w, &sdp),
                None => Ok(()),
            },
            Payload::Ice { candidate, mline } => {
                if let Some(w) = webrtc() {
                    webrtc::add_ice(&w, mline, &candidate);
                }
                Ok(())
            }
            Payload::Bye => {
                remove_peer(&self.inner, from);
                Ok(())
            }
            Payload::Offer { .. } => Ok(()), // streamers only send offers
        };
        if let Err(e) = res {
            log::error!("viewer {from}: {e:#}");
        }
    }

    /// Interprets a pipeline bus message: Ok(false) on EOS, Err on pipeline error.
    pub fn handle_bus(&self, msg: &gst::Message) -> Result<bool> {
        webrtc::check_bus_message(msg)
    }

    /// Runs until Ctrl-C, a pipeline error, or the signaling connection drops.
    pub async fn run(&self, mut events: mpsc::UnboundedReceiver<ServerMsg>) -> Result<()> {
        let bus = self.inner.pipeline.bus().expect("pipeline bus");
        let mut bus_stream = bus.stream();
        let result = loop {
            tokio::select! {
                ev = events.recv() => match ev {
                    Some(msg) => self.handle(msg),
                    None => break Err(anyhow::anyhow!("lost connection to signaling server")),
                },
                Some(msg) = bus_stream.next() => match webrtc::check_bus_message(&msg) {
                    Ok(true) => {}
                    Ok(false) => break Ok(()),
                    Err(e) => break Err(e),
                },
                _ = tokio::signal::ctrl_c() => break Ok(()),
            }
        };
        self.stop();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtp_fragments_parse() {
        gst::init().unwrap();
        gst::parse::bin_from_description(VIDEO_RTP, true).unwrap();
        gst::parse::bin_from_description(&format!("opusenc ! {AUDIO_RTP}"), true).unwrap();
        gst::parse::launch(PEER_QUEUE).unwrap();
    }
}

fn add_peer(inner: &Arc<Inner>, id: &str) -> Result<()> {
    if inner.peers.lock().unwrap().contains_key(id) {
        remove_peer(inner, id); // viewer reconnecting: start fresh
    }
    if inner.peers.lock().unwrap().len() >= MAX_VIEWERS {
        log::warn!("refusing viewer {id}: limit of {MAX_VIEWERS} reached");
        inner.sig.signal(id, Payload::Bye);
        return Ok(());
    }

    let webrtc = webrtc::new_webrtcbin(&format!("peer-{id}"), &inner.ice, &inner.sig, id)?;
    {
        let (sig, remote) = (inner.sig.clone(), id.to_owned());
        webrtc.connect("on-negotiation-needed", false, move |values| {
            let w = values[0].get::<gst::Element>().expect("webrtcbin");
            let (sig, remote) = (sig.clone(), remote.clone());
            webrtc::create_offer(&w, move |sdp| sig.signal(&remote, Payload::Offer { sdp }));
            None
        });
    }
    {
        let weak: Weak<Inner> = Arc::downgrade(inner);
        let remote = id.to_owned();
        webrtc.connect_notify(Some("connection-state"), move |w, _| {
            use gst_webrtc::WebRTCPeerConnectionState as S;
            if matches!(w.property::<S>("connection-state"), S::Failed) {
                let (weak, remote) = (weak.clone(), remote.clone());
                // Tear down off the webrtcbin thread to avoid deadlocking it.
                std::thread::spawn(move || {
                    if let Some(inner) = weak.upgrade() {
                        log::warn!("viewer {remote}: connection failed");
                        remove_peer(&inner, &remote);
                    }
                });
            }
        });
    }

    inner.pipeline.add(&webrtc)?;
    let mut branches = Vec::new();
    for tee in &inner.tees {
        let queue = gst::parse::launch(PEER_QUEUE)?;
        inner.pipeline.add(&queue)?;
        let tee_pad = tee.request_pad_simple("src_%u").context("tee pad")?;
        tee_pad.link(&queue.static_pad("sink").unwrap())?;
        let sink = webrtc.request_pad_simple("sink_%u").context("webrtcbin sink pad")?;
        queue.static_pad("src").unwrap().link(&sink)?;
        let transceiver = sink.property::<gst_webrtc::WebRTCRTPTransceiver>("transceiver");
        transceiver.set_property("direction", gst_webrtc::WebRTCRTPTransceiverDirection::Sendonly);
        queue.sync_state_with_parent()?;
        branches.push((tee_pad, queue));
    }
    webrtc.sync_state_with_parent()?;

    inner.peers.lock().unwrap().insert(id.to_owned(), PeerConn { webrtc, branches });
    encoder::request_keyframe(&inner.encoder);
    log::info!("viewer {id} connecting ({} total)", inner.peers.lock().unwrap().len());
    Ok(())
}

fn remove_peer(inner: &Inner, id: &str) {
    let Some(peer) = inner.peers.lock().unwrap().remove(id) else { return };
    log::info!("viewer {id} left");

    // Unlink each branch from its tee while the tee pad is idle, so we never
    // cut a buffer in half mid-push.
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    for (tee_pad, _) in &peer.branches {
        let tx = tx.clone();
        tee_pad.add_probe(gst::PadProbeType::IDLE, move |pad, _| {
            if let Some(peer_pad) = pad.peer() {
                let _ = pad.unlink(&peer_pad);
            }
            let _ = tx.send(());
            gst::PadProbeReturn::Remove
        });
    }
    drop(tx);

    let pipeline = inner.pipeline.clone();
    std::thread::spawn(move || {
        for _ in 0..peer.branches.len() {
            if rx.recv_timeout(std::time::Duration::from_secs(2)).is_err() {
                break;
            }
        }
        for (tee_pad, queue) in &peer.branches {
            if let Some(tee) = tee_pad.parent_element() {
                tee.release_request_pad(tee_pad);
            }
            let _ = queue.set_state(gst::State::Null);
            let _ = pipeline.remove(queue);
        }
        let _ = peer.webrtc.set_state(gst::State::Null);
        let _ = pipeline.remove(&peer.webrtc);
    });
}
