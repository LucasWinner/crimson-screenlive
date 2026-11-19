//! Streamer: capture -> raw tee -> encoder tier(s) -> tee -> one webrtcbin per viewer.
//!
//! Every viewer starts on the full-quality tier. Viewers whose connection goes
//! through a TURN relay (paid bandwidth) are moved to a lazily-created 720p
//! low-bitrate tier, so direct viewers keep full quality. Each viewer has its
//! own RTP payloader, so switching tiers keeps its RTP stream continuous.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
    /// Move TURN-relayed viewers to a cheaper 720p stream.
    pub relay_saver: bool,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            video: VideoSource::MonitorIndex(0),
            audio: AudioSource::System,
            format: VideoFormat { width: 1920, height: 1080, fps: 60 },
            bitrate_kbps: 7000,
            encoder: None,
            relay_saver: true,
        }
    }
}

/// Low tier used for relayed viewers.
pub const RELAY_FORMAT: (u32, u32) = (1280, 720);
pub const RELAY_KBPS: u32 = 3500;
pub const MAX_VIEWERS: usize = 4;

const PARSE: &str = "h264parse config-interval=-1";
const PEER_VIDEO: &str = "queue leaky=downstream max-size-buffers=0 max-size-bytes=0 max-size-time=300000000 \
    ! rtph264pay config-interval=-1 aggregate-mode=zero-latency pt=96 mtu=1200 \
    ! capsfilter caps=\"application/x-rtp,media=video,encoding-name=H264,payload=96,clock-rate=90000\"";
const AUDIO_RTP: &str = "rtpopuspay pt=111 \
    ! capsfilter caps=\"application/x-rtp,media=audio,encoding-name=OPUS,payload=111,clock-rate=48000\"";
/// Per-viewer queue: drop old data instead of building latency on a slow link.
const PEER_QUEUE: &str = "queue leaky=downstream max-size-buffers=0 max-size-bytes=0 max-size-time=300000000";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tier {
    High,
    Low,
}

struct VideoTier {
    encoder: gst::Element,
    tee: gst::Element,
}

/// A per-viewer branch hanging off a tee.
struct Branch {
    tee_pad: gst::Pad,
    elem: gst::Element,
}

struct PeerConn {
    webrtc: gst::Element,
    video: Branch,
    audio: Option<Branch>,
    tier: Tier,
    relayed: Option<bool>,
    last_bytes: u64,
}

struct Inner {
    pipeline: gst::Pipeline,
    raw_tee: gst::Element,
    high: VideoTier,
    low: Mutex<Option<VideoTier>>,
    atee: Option<gst::Element>,
    peers: Mutex<HashMap<String, PeerConn>>,
    sig: SignalingHandle,
    ice: Vec<IceServer>,
    source: VideoSource,
    spec: EncoderSpec,
    fps: u32,
    relay_saver: AtomicBool,
    relay_bytes: AtomicU64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ViewerInfo {
    pub id: String,
    /// None while still connecting.
    pub relayed: Option<bool>,
    pub reduced_quality: bool,
}

pub struct Streamer {
    inner: Arc<Inner>,
    pub encoder: EncoderSpec,
}

fn make(factory: &str) -> Result<gst::Element> {
    gst::ElementFactory::make(factory).build().with_context(|| format!("missing element {factory}"))
}

/// raw_tee -> queue -> [scale] -> encoder -> h264parse -> tee. Syncs state so
/// it can be added to a running pipeline.
fn build_tier(
    pipeline: &gst::Pipeline,
    raw_tee: &gst::Element,
    spec: &EncoderSpec,
    scale: Option<String>,
    settings: &EncoderSettings,
) -> Result<VideoTier> {
    let queue = gst::parse::launch("queue max-size-buffers=2 leaky=downstream")?;
    let scaler = scale.map(|s| gst::parse::bin_from_description(&s, true)).transpose()?;
    let enc = encoder::build(spec, settings)?;
    let parse = gst::parse::launch(PARSE)?;
    let tee = make("tee")?;
    tee.set_property("allow-not-linked", true);

    let mut chain: Vec<gst::Element> = vec![queue];
    if let Some(s) = scaler {
        chain.push(s.upcast());
    }
    chain.extend([enc.clone(), parse, tee.clone()]);
    pipeline.add_many(&chain)?;
    gst::Element::link_many(&chain)?;
    for e in chain.iter().rev() {
        e.sync_state_with_parent()?;
    }
    let src = raw_tee.request_pad_simple("src_%u").context("raw tee pad")?;
    src.link(&chain[0].static_pad("sink").unwrap())?;
    Ok(VideoTier { encoder: enc, tee })
}

impl Streamer {
    pub fn new(cfg: &StreamConfig, sig: SignalingHandle, ice: Vec<IceServer>) -> Result<Self> {
        let spec = encoder::pick(cfg.encoder.as_deref())?;
        let pipeline = gst::Pipeline::with_name("streamer");

        let vsrc = gst::parse::bin_from_description(&capture::video_fragment(&cfg.video, &spec, &cfg.format)?, true)
            .context("building video source")?;
        let raw_tee = make("tee")?;
        raw_tee.set_property("allow-not-linked", true);
        pipeline.add_many([vsrc.upcast_ref(), &raw_tee])?;
        vsrc.link(&raw_tee)?;
        let high = build_tier(
            &pipeline,
            &raw_tee,
            &spec,
            None,
            &EncoderSettings { bitrate_kbps: cfg.bitrate_kbps, fps: cfg.format.fps, keyframe_secs: 2 },
        )?;

        let atee = match capture::audio_fragment(cfg.audio) {
            Some(frag) => {
                let abin = gst::parse::bin_from_description(&format!("{frag} ! {AUDIO_RTP}"), true)
                    .context("building audio source")?;
                let atee = make("tee")?;
                atee.set_property("allow-not-linked", true);
                pipeline.add_many([abin.upcast_ref(), &atee])?;
                abin.link(&atee)?;
                Some(atee)
            }
            None => None,
        };

        // Relayed viewers only save bandwidth if the main stream is above 720p.
        let relay_saver = cfg.relay_saver && cfg.format.height > RELAY_FORMAT.1;
        Ok(Self {
            inner: Arc::new(Inner {
                pipeline,
                raw_tee,
                high,
                low: Mutex::new(None),
                atee,
                peers: Mutex::default(),
                sig,
                ice,
                source: cfg.video.clone(),
                spec,
                fps: cfg.format.fps,
                relay_saver: AtomicBool::new(relay_saver),
                relay_bytes: AtomicU64::new(0),
            }),
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
        self.report_relay_usage();
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

    /// Call about once a second: checks each viewer's network path, moves
    /// relayed viewers to the low tier, and accumulates relayed bytes.
    pub fn tick(&self) -> Vec<ViewerInfo> {
        let snapshot: Vec<(String, gst::Element)> =
            self.inner.peers.lock().unwrap().iter().map(|(id, p)| (id.clone(), p.webrtc.clone())).collect();
        let mut out = Vec::new();
        for (id, w) in snapshot {
            let stats = webrtc::stats(&w);
            let relayed = stats.as_ref().and_then(webrtc::is_relayed);
            let bytes = stats.as_ref().map(webrtc::bytes_sent).unwrap_or(0);

            let needs_switch = {
                let mut peers = self.inner.peers.lock().unwrap();
                let Some(p) = peers.get_mut(&id) else { continue };
                if p.relayed != relayed {
                    log::info!("viewer {id}: {}", if relayed == Some(true) { "via TURN relay" } else { "direct" });
                }
                p.relayed = relayed;
                if relayed == Some(true) {
                    self.inner.relay_bytes.fetch_add(bytes.saturating_sub(p.last_bytes), Ordering::Relaxed);
                }
                p.last_bytes = bytes;
                self.inner.relay_saver.load(Ordering::Relaxed) && relayed == Some(true) && p.tier == Tier::High
            };
            if needs_switch {
                if let Err(e) = switch_tier(&self.inner, &id, Tier::Low) {
                    // Don't retry every tick; relayed viewers just keep full quality.
                    log::warn!("could not create the relay quality tier, disabling it: {e:#}");
                    self.inner.relay_saver.store(false, Ordering::Relaxed);
                }
            }
            let peers = self.inner.peers.lock().unwrap();
            if let Some(p) = peers.get(&id) {
                out.push(ViewerInfo { id, relayed: p.relayed, reduced_quality: p.tier == Tier::Low });
            }
        }
        out
    }

    /// Bytes pushed through TURN since the last call.
    pub fn take_relay_bytes(&self) -> u64 {
        self.inner.relay_bytes.swap(0, Ordering::Relaxed)
    }

    /// Reports accumulated relay usage to the server (for the monthly TURN cap).
    pub fn report_relay_usage(&self) {
        let bytes = self.take_relay_bytes();
        if bytes > 0 {
            self.inner.sig.send(ClientMsg::TurnUsage { bytes });
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
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
        let mut ticks = 0u64;
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
                _ = tick.tick() => {
                    self.tick();
                    ticks += 1;
                    if ticks % 15 == 0 {
                        self.report_relay_usage();
                    }
                }
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
        gst::parse::launch(PARSE).unwrap();
        gst::parse::bin_from_description(PEER_VIDEO, true).unwrap();
        gst::parse::bin_from_description(&format!("opusenc ! {AUDIO_RTP}"), true).unwrap();
        gst::parse::launch(PEER_QUEUE).unwrap();
    }
}

fn tier_tee(inner: &Inner, tier: Tier) -> Result<(gst::Element, gst::Element)> {
    if tier == Tier::High {
        return Ok((inner.high.tee.clone(), inner.high.encoder.clone()));
    }
    let mut low = inner.low.lock().unwrap();
    if low.is_none() {
        let (w, h) = RELAY_FORMAT;
        log::info!("starting {w}x{h} tier for relayed viewers");
        *low = Some(build_tier(
            &inner.pipeline,
            &inner.raw_tee,
            &inner.spec,
            Some(capture::downscale_fragment(&inner.source, &inner.spec, w, h)),
            &EncoderSettings { bitrate_kbps: RELAY_KBPS, fps: inner.fps, keyframe_secs: 2 },
        )?);
    }
    let t = low.as_ref().unwrap();
    Ok((t.tee.clone(), t.encoder.clone()))
}

/// Moves a viewer's video branch to another tier's tee without a gap in its RTP stream.
fn switch_tier(inner: &Inner, id: &str, to: Tier) -> Result<()> {
    let (tee, enc) = tier_tee(inner, to)?;
    let (old_pad, sink) = {
        let peers = inner.peers.lock().unwrap();
        let p = peers.get(id).context("viewer left")?;
        (p.video.tee_pad.clone(), p.video.elem.static_pad("sink").unwrap())
    };
    let new_pad = tee.request_pad_simple("src_%u").context("tee pad")?;
    {
        let mut peers = inner.peers.lock().unwrap();
        let p = peers.get_mut(id).context("viewer left")?;
        p.video.tee_pad = new_pad.clone();
        p.tier = to;
    }

    old_pad.add_probe(gst::PadProbeType::IDLE, move |pad, _| {
        let _ = pad.unlink(&sink);
        if let Err(e) = new_pad.link(&sink) {
            log::error!("relinking viewer branch failed: {e:?}");
        }
        encoder::request_keyframe(&enc);
        let old = pad.clone();
        // Release the old request pad outside the streaming thread.
        std::thread::spawn(move || {
            if let Some(t) = old.parent_element() {
                t.release_request_pad(&old);
            }
        });
        gst::PadProbeReturn::Remove
    });
    log::info!("viewer {id}: switched to {to:?} quality");
    Ok(())
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

    let attach = |tee: &gst::Element, elem: gst::Element| -> Result<Branch> {
        inner.pipeline.add(&elem)?;
        let tee_pad = tee.request_pad_simple("src_%u").context("tee pad")?;
        tee_pad.link(&elem.static_pad("sink").unwrap())?;
        let sink = webrtc.request_pad_simple("sink_%u").context("webrtcbin sink pad")?;
        elem.static_pad("src").unwrap().link(&sink)?;
        let transceiver = sink.property::<gst_webrtc::WebRTCRTPTransceiver>("transceiver");
        transceiver.set_property("direction", gst_webrtc::WebRTCRTPTransceiverDirection::Sendonly);
        elem.sync_state_with_parent()?;
        Ok(Branch { tee_pad, elem })
    };
    let video = attach(&inner.high.tee, gst::parse::bin_from_description(PEER_VIDEO, true)?.upcast())?;
    let audio = match &inner.atee {
        Some(atee) => Some(attach(atee, gst::parse::launch(PEER_QUEUE)?)?),
        None => None,
    };
    webrtc.sync_state_with_parent()?;

    inner.peers.lock().unwrap().insert(
        id.to_owned(),
        PeerConn { webrtc, video, audio, tier: Tier::High, relayed: None, last_bytes: 0 },
    );
    encoder::request_keyframe(&inner.high.encoder);
    log::info!("viewer {id} connecting ({} total)", inner.peers.lock().unwrap().len());
    Ok(())
}

fn remove_peer(inner: &Inner, id: &str) {
    let Some(peer) = inner.peers.lock().unwrap().remove(id) else { return };
    log::info!("viewer {id} left");
    let branches: Vec<Branch> = std::iter::once(peer.video).chain(peer.audio).collect();

    // Unlink each branch from its tee while the tee pad is idle, so we never
    // cut a buffer in half mid-push.
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    for b in &branches {
        let tx = tx.clone();
        b.tee_pad.add_probe(gst::PadProbeType::IDLE, move |pad, _| {
            if let Some(peer_pad) = pad.peer() {
                let _ = pad.unlink(&peer_pad);
            }
            let _ = tx.send(());
            gst::PadProbeReturn::Remove
        });
    }
    drop(tx);

    let pipeline = inner.pipeline.clone();
    let webrtc = peer.webrtc;
    std::thread::spawn(move || {
        for _ in 0..branches.len() {
            if rx.recv_timeout(std::time::Duration::from_secs(2)).is_err() {
                break;
            }
        }
        for b in &branches {
            if let Some(tee) = b.tee_pad.parent_element() {
                tee.release_request_pad(&b.tee_pad);
            }
            let _ = b.elem.set_state(gst::State::Null);
            let _ = pipeline.remove(&b.elem);
        }
        let _ = webrtc.set_state(gst::State::Null);
        let _ = pipeline.remove(&webrtc);
    });
}
