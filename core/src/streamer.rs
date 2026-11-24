//! Streamer: capture -> raw tee -> encoder tier(s) -> tee -> one webrtcbin per viewer.
//!
//! Every viewer gets the full-quality tier. With `relay_saver` on, viewers whose connection goes
//! through a TURN relay (paid bandwidth) are moved to a lazily-created 720p
//! low-bitrate tier, so direct viewers keep full quality. Each viewer has its
//! own RTP payloader, so switching tiers keeps its RTP stream continuous.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use anyhow::{Context, Result};
use futures_util::StreamExt;
use gst::prelude::*;
use tokio::sync::mpsc;

use crate::capture::{self, AudioSource, VideoFormat, VideoSource};
use crate::encoder::{self, Codec, EncoderSettings, EncoderSpec, VideoCodec};
use crate::ice::IceServer;
use crate::protocol::{ClientMsg, Payload, ServerMsg};
use crate::signaling::SignalingHandle;
use crate::webrtc;

pub struct StreamConfig {
    pub video: VideoSource,
    pub audio: AudioSource,
    pub format: VideoFormat,
    pub bitrate_kbps: u32,
    /// Automatically lower bitrate temporarily if a viewer experiences packet loss.
    pub auto_bitrate: bool,
    pub codec: Codec,
    pub encoder: Option<String>,
    /// Move TURN-relayed viewers to a cheaper 720p stream (off by default:
    /// users preferred full quality; the monthly TURN cap still applies).
    pub relay_saver: bool,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            video: VideoSource::MonitorIndex(0),
            audio: AudioSource::System,
            format: VideoFormat { width: 1920, height: 1080, fps: 60 },
            bitrate_kbps: 7000,
            auto_bitrate: true,
            codec: Codec::Auto,
            encoder: None,
            relay_saver: false,
        }
    }
}

/// Low tier used for relayed viewers.
pub const RELAY_FORMAT: (u32, u32) = (1280, 720);
pub const RELAY_KBPS: u32 = 3500;
pub const MAX_VIEWERS: usize = 4;

const PARSE_H264: &str = "h264parse config-interval=-1";
const PEER_VIDEO_H264: &str = "queue leaky=downstream max-size-buffers=0 max-size-bytes=0 max-size-time=300000000 \
    ! rtph264pay config-interval=-1 aggregate-mode=zero-latency pt=96 mtu=1200 \
    ! capsfilter caps=\"application/x-rtp,media=video,encoding-name=H264,payload=96,clock-rate=90000\"";
const PARSE_AV1: &str = "av1parse";
const PEER_VIDEO_AV1: &str = "queue leaky=downstream max-size-buffers=0 max-size-bytes=0 max-size-time=300000000 \
    ! rtpav1pay pt=96 mtu=1200 \
    ! capsfilter caps=\"application/x-rtp,media=video,encoding-name=AV1,payload=96,clock-rate=90000\"";
const AUDIO_RTP: &str = "rtpopuspay pt=111 \
    ! capsfilter caps=\"application/x-rtp,media=audio,encoding-name=OPUS,payload=111,clock-rate=48000\"";
/// Per-viewer queue: drop old data instead of building latency on a slow link.
const PEER_QUEUE: &str = "queue leaky=downstream max-size-buffers=0 max-size-bytes=0 max-size-time=300000000";

fn parse_desc(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::H264 => PARSE_H264,
        VideoCodec::Av1 => PARSE_AV1,
    }
}

fn peer_video_desc(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::H264 => PEER_VIDEO_H264,
        VideoCodec::Av1 => PEER_VIDEO_AV1,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tier {
    High,
    CompatH264,
    Low,
}

struct VideoTier {
    spec: EncoderSpec,
    encoder: gst::Element,
    tee: gst::Element,
    raw_pad: gst::Pad,
    chain: Vec<gst::Element>,
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
    codec: VideoCodec,
    relayed: Option<bool>,
    last_bytes: u64,
}

struct Inner {
    pipeline: gst::Pipeline,
    raw_tee: gst::Element,
    high: VideoTier,
    compat_h264: Mutex<Option<VideoTier>>,
    low: Mutex<Option<VideoTier>>,
    atee: Option<gst::Element>,
    audio_bin: Mutex<Option<gst::Bin>>,
    peers: Mutex<HashMap<String, PeerConn>>,
    sig: Mutex<SignalingHandle>,
    ice: Mutex<Vec<IceServer>>,
    source: VideoSource,
    spec: EncoderSpec,
    fps: u32,
    relay_saver: AtomicBool,
    relay_bytes: AtomicU64,
    next_peer_seq: AtomicU64,
    target_bitrate_kbps: u32,
    current_bitrate_kbps: AtomicU32,
    auto_bitrate: bool,
    low_loss_streak: AtomicU32,
}

impl Inner {
    fn send_sig(&self, msg: ClientMsg) {
        self.sig.lock().unwrap().send(msg);
    }

    fn signal(&self, to: &str, payload: Payload) {
        self.sig.lock().unwrap().signal(to, payload);
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ViewerInfo {
    pub id: String,
    /// None while still connecting.
    pub relayed: Option<bool>,
    pub reduced_quality: bool,
    pub codec: VideoCodec,
    pub rtt_ms: Option<f64>,
    pub loss_pct: Option<f64>,
}

pub struct Streamer {
    inner: Arc<Inner>,
    pub encoder: EncoderSpec,
}

fn make(factory: &str) -> Result<gst::Element> {
    gst::ElementFactory::make(factory).build().with_context(|| format!("missing element {factory}"))
}

/// raw_tee -> queue -> [scale] -> encoder -> parser -> tee. Syncs state so
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
    let parse = gst::parse::launch(parse_desc(spec.codec))?;
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
    Ok(VideoTier { spec: *spec, encoder: enc, tee, raw_pad: src, chain })
}

fn teardown_tier(pipeline: &gst::Pipeline, tier: VideoTier) {
    let raw_pad = tier.raw_pad;
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    raw_pad.add_probe(gst::PadProbeType::IDLE, move |pad, _| {
        if let Some(peer) = pad.peer() {
            let _ = pad.unlink(&peer);
        }
        let _ = tx.send(());
        gst::PadProbeReturn::Remove
    });
    let _ = rx.recv_timeout(std::time::Duration::from_secs(1));
    if let Some(tee) = raw_pad.parent_element() {
        tee.release_request_pad(&raw_pad);
    }
    for e in tier.chain.into_iter().rev() {
        let _ = e.set_state(gst::State::Null);
        let _ = pipeline.remove(&e);
    }
}

impl Streamer {
    pub fn new(cfg: &StreamConfig, sig: SignalingHandle, ice: Vec<IceServer>) -> Result<Self> {
        let spec = encoder::pick_for_codec(cfg.encoder.as_deref(), cfg.codec)?;
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

        let (atee, audio_bin) = match capture::audio_fragment(cfg.audio) {
            Some(frag) => {
                let abin = gst::parse::bin_from_description(&format!("{frag} ! {AUDIO_RTP}"), true)
                    .context("building audio source")?;
                let atee = make("tee")?;
                atee.set_property("allow-not-linked", true);
                pipeline.add_many([abin.upcast_ref(), &atee])?;
                abin.link(&atee)?;
                (Some(atee), Some(abin))
            }
            None => (None, None),
        };

        // Relayed viewers only save bandwidth if the main stream is above 720p.
        let relay_saver = cfg.relay_saver && cfg.format.height > RELAY_FORMAT.1;
        Ok(Self {
            inner: Arc::new(Inner {
                pipeline,
                raw_tee,
                high,
                compat_h264: Mutex::default(),
                low: Mutex::default(),
                atee,
                audio_bin: Mutex::new(audio_bin),
                peers: Mutex::default(),
                sig: Mutex::new(sig),
                ice: Mutex::new(ice),
                source: cfg.video.clone(),
                spec,
                fps: cfg.format.fps,
                relay_saver: AtomicBool::new(relay_saver),
                relay_bytes: AtomicU64::new(0),
                next_peer_seq: AtomicU64::new(1),
                target_bitrate_kbps: cfg.bitrate_kbps,
                current_bitrate_kbps: AtomicU32::new(cfg.bitrate_kbps),
                auto_bitrate: cfg.auto_bitrate,
                low_loss_streak: AtomicU32::new(0),
            }),
            encoder: spec,
        })
    }

    pub fn current_bitrate_kbps(&self) -> u32 {
        self.inner.current_bitrate_kbps.load(Ordering::Relaxed)
    }

    /// Hot-swaps the audio capture source (for example to exclude Discord once
    /// it starts) without interrupting video or disconnecting viewers.
    pub fn set_audio_source(&self, src: AudioSource) -> Result<()> {
        let Some(atee) = &self.inner.atee else { return Ok(()) };
        let Some(frag) = capture::audio_fragment(src) else { return Ok(()) };
        let new_bin = gst::parse::bin_from_description(&format!("{frag} ! {AUDIO_RTP}"), true)
            .context("building replacement audio source")?;
        let mut guard = self.inner.audio_bin.lock().unwrap();
        let Some(old_bin) = guard.take() else { return Ok(()) };

        self.inner.pipeline.add(&new_bin)?;
        let old_src = old_bin.static_pad("src").context("old audio src pad")?;
        let new_src = new_bin.static_pad("src").context("new audio src pad")?;
        let atee_sink = atee.static_pad("sink").context("atee sink pad")?;

        let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let old_src_c = old_src.clone();
        old_src.add_probe(gst::PadProbeType::IDLE, move |_, _| {
            let _ = old_src_c.unlink(&atee_sink);
            let res = new_src.link(&atee_sink).map(|_| ()).map_err(|e| format!("{e:?}"));
            let _ = tx.send(res);
            gst::PadProbeReturn::Remove
        });
        match rx.recv_timeout(std::time::Duration::from_secs(2)) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                let _ = self.inner.pipeline.remove(&new_bin);
                *guard = Some(old_bin);
                anyhow::bail!("relinking audio source failed: {e}");
            }
            Err(_) => {
                // Pipeline may not be Playing; unlink/relink directly.
                let atee_sink = atee.static_pad("sink").unwrap();
                let _ = old_src.unlink(&atee_sink);
                if let Err(e) = new_bin.static_pad("src").unwrap().link(&atee_sink) {
                    let _ = self.inner.pipeline.remove(&new_bin);
                    *guard = Some(old_bin);
                    return Err(e.into());
                }
            }
        }
        if let Err(e) = new_bin.sync_state_with_parent() {
            log::warn!("new audio source state sync failed: {e}");
        }
        *guard = Some(new_bin);
        drop(guard);

        let pipeline = self.inner.pipeline.clone();
        std::thread::spawn(move || {
            let _ = old_bin.set_state(gst::State::Null);
            let _ = pipeline.remove(&old_bin);
        });
        log::info!("audio source swapped seamlessly");
        Ok(())
    }

    pub fn pipeline(&self) -> &gst::Pipeline {
        &self.inner.pipeline
    }

    pub fn viewer_count(&self) -> usize {
        self.inner.peers.lock().unwrap().len()
    }

    pub fn start(&self) -> Result<()> {
        self.inner.pipeline.set_state(gst::State::Playing).context("could not start capture pipeline")?;
        self.inner.send_sig(ClientMsg::Stream { streaming: true });
        Ok(())
    }

    /// Replaces the signaling handle after an automatic room reconnect while
    /// keeping the capture and encoder pipeline running uninterrupted.
    pub fn reconnected(&self, sig: SignalingHandle, ice: Vec<IceServer>) {
        let stale_ids: Vec<String> = self.inner.peers.lock().unwrap().keys().cloned().collect();
        for id in stale_ids {
            remove_peer(&self.inner, &id);
        }
        *self.inner.sig.lock().unwrap() = sig;
        *self.inner.ice.lock().unwrap() = ice;
        self.inner.send_sig(ClientMsg::Stream { streaming: true });
    }

    pub fn stop(&self) {
        let peers: Vec<(String, PeerConn)> = self.inner.peers.lock().unwrap().drain().collect();
        for (id, _) in &peers {
            self.inner.signal(id, Payload::Bye);
        }
        self.report_relay_usage();
        self.inner.send_sig(ClientMsg::Stream { streaming: false });
        let _ = self.inner.pipeline.set_state(gst::State::Null);
        for (_, peer) in peers {
            for b in std::iter::once(peer.video).chain(peer.audio) {
                if let Some(tee) = b.tee_pad.parent_element() {
                    tee.release_request_pad(&b.tee_pad);
                }
                let _ = self.inner.pipeline.remove(&b.elem);
            }
            let _ = self.inner.pipeline.remove(&peer.webrtc);
        }
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
            Payload::Watch { codecs } => add_peer(&self.inner, from, &codecs),
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
    /// relayed viewers to the low tier, adapts bitrate on sustained packet loss,
    /// and accumulates relayed bytes.
    pub fn tick(&self) -> Vec<ViewerInfo> {
        let snapshot: Vec<(String, gst::Element)> =
            self.inner.peers.lock().unwrap().iter().map(|(id, p)| (id.clone(), p.webrtc.clone())).collect();
        let mut out = Vec::new();
        let mut worst_loss: Option<f64> = None;

        for (id, w) in snapshot {
            let stats = webrtc::stats(&w);
            let relayed = stats.as_ref().and_then(webrtc::is_relayed);
            let bytes = stats.as_ref().map(webrtc::bytes_sent).unwrap_or(0);
            let link = stats.as_ref().map(webrtc::link_stats).unwrap_or_default();
            if let Some(loss) = link.loss_pct {
                worst_loss = Some(worst_loss.map_or(loss, |cur| cur.max(loss)));
            }

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
                out.push(ViewerInfo {
                    id,
                    relayed: p.relayed,
                    reduced_quality: p.tier == Tier::Low,
                    codec: p.codec,
                    rtt_ms: link.rtt_ms,
                    loss_pct: link.loss_pct,
                });
            }
        }

        if self.inner.auto_bitrate {
            if let Some(loss) = worst_loss {
                let cur = self.inner.current_bitrate_kbps.load(Ordering::Relaxed);
                let target = self.inner.target_bitrate_kbps;
                let floor = (target / 2).max(1500).min(target);
                let update_encoders = |kbps: u32| {
                    encoder::set_bitrate(&self.inner.high.encoder, &self.inner.spec, kbps);
                    if let Some(compat) = self.inner.compat_h264.lock().unwrap().as_ref() {
                        encoder::set_bitrate(&compat.encoder, &compat.spec, kbps);
                    }
                };
                if loss > 5.0 {
                    self.inner.low_loss_streak.store(0, Ordering::Relaxed);
                    let next = ((cur as f64 * 0.85).round() as u32).max(floor);
                    if next < cur {
                        log::info!("packet loss {loss:.1}%: stepping bitrate {cur} -> {next} kbps");
                        self.inner.current_bitrate_kbps.store(next, Ordering::Relaxed);
                        update_encoders(next);
                    }
                } else if loss < 1.0 && cur < target {
                    let streak = self.inner.low_loss_streak.fetch_add(1, Ordering::Relaxed) + 1;
                    if streak >= 5 {
                        self.inner.low_loss_streak.store(0, Ordering::Relaxed);
                        let next = ((cur as f64 * 1.10).round() as u32).max(cur + 250).min(target);
                        if next > cur {
                            log::info!("network healthy: recovering bitrate {cur} -> {next} kbps");
                            self.inner.current_bitrate_kbps.store(next, Ordering::Relaxed);
                            update_encoders(next);
                        }
                    }
                } else {
                    self.inner.low_loss_streak.store(0, Ordering::Relaxed);
                }
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
            self.inner.send_sig(ClientMsg::TurnUsage { bytes });
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
                    if ticks.is_multiple_of(15) {
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

fn tier_tee(inner: &Inner, tier: Tier) -> Result<(gst::Element, gst::Element, VideoCodec)> {
    match tier {
        Tier::High => Ok((inner.high.tee.clone(), inner.high.encoder.clone(), inner.high.spec.codec)),
        Tier::CompatH264 => {
            let mut compat = inner.compat_h264.lock().unwrap();
            if compat.is_none() {
                let spec = encoder::pick_h264_for_memory(inner.high.spec.memory)?;
                let kbps = inner.current_bitrate_kbps.load(Ordering::Relaxed);
                log::info!("starting parallel {} tier for H.264-only viewers ({kbps} kbps)", spec.label);
                *compat = Some(build_tier(
                    &inner.pipeline,
                    &inner.raw_tee,
                    &spec,
                    None,
                    &EncoderSettings { bitrate_kbps: kbps, fps: inner.fps, keyframe_secs: 2 },
                )?);
            }
            let t = compat.as_ref().unwrap();
            Ok((t.tee.clone(), t.encoder.clone(), t.spec.codec))
        }
        Tier::Low => {
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
            Ok((t.tee.clone(), t.encoder.clone(), t.spec.codec))
        }
    }
}

/// Moves a viewer's video branch to another tier's tee without a gap in its RTP stream.
fn switch_tier(inner: &Inner, id: &str, to: Tier) -> Result<()> {
    let (tee, enc, _) = tier_tee(inner, to)?;
    let (old_pad, sink) = {
        let peers = inner.peers.lock().unwrap();
        let p = peers.get(id).context("viewer left")?;
        if p.tier == Tier::CompatH264 {
            return Ok(());
        }
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

fn add_peer(inner: &Arc<Inner>, id: &str, codecs: &[String]) -> Result<()> {
    if inner.peers.lock().unwrap().contains_key(id) {
        remove_peer(inner, id); // viewer reconnecting: start fresh
    }
    if inner.peers.lock().unwrap().len() >= MAX_VIEWERS {
        log::warn!("refusing viewer {id}: limit of {MAX_VIEWERS} reached");
        inner.signal(id, Payload::Bye);
        return Ok(());
    }

    let wants_av1 = codecs.iter().any(|c| c.eq_ignore_ascii_case("av1"));
    let target_tier = if inner.high.spec.codec == VideoCodec::Av1 && !wants_av1 {
        Tier::CompatH264
    } else {
        Tier::High
    };
    let (video_tee, tier_enc, codec) = tier_tee(inner, target_tier)?;

    let sig = inner.sig.lock().unwrap().clone();
    let ice = inner.ice.lock().unwrap().clone();
    let seq = inner.next_peer_seq.fetch_add(1, Ordering::Relaxed);
    let webrtc = webrtc::new_webrtcbin(&format!("peer-{id}-{seq}"), &ice, &sig, id)?;
    {
        let remote = id.to_owned();
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
        if let Err(e) = (|| -> Result<()> {
            tee_pad.link(&elem.static_pad("sink").unwrap())?;
            let sink = webrtc.request_pad_simple("sink_%u").context("webrtcbin sink pad")?;
            elem.static_pad("src").unwrap().link(&sink)?;
            let transceiver = sink.property::<gst_webrtc::WebRTCRTPTransceiver>("transceiver");
            transceiver.set_property("direction", gst_webrtc::WebRTCRTPTransceiverDirection::Sendonly);
            elem.sync_state_with_parent()?;
            Ok(())
        })() {
            tee.release_request_pad(&tee_pad);
            let _ = elem.set_state(gst::State::Null);
            let _ = inner.pipeline.remove(&elem);
            return Err(e);
        }
        Ok(Branch { tee_pad, elem })
    };
    let setup = (|| -> Result<(Branch, Option<Branch>)> {
        let video = attach(
            &video_tee,
            gst::parse::bin_from_description(peer_video_desc(codec), true)?.upcast(),
        )?;
        let audio = match &inner.atee {
            Some(atee) => match attach(atee, gst::parse::launch(PEER_QUEUE)?) {
                Ok(a) => Some(a),
                Err(e) => {
                    if let Some(t) = video.tee_pad.parent_element() {
                        t.release_request_pad(&video.tee_pad);
                    }
                    let _ = video.elem.set_state(gst::State::Null);
                    let _ = inner.pipeline.remove(&video.elem);
                    return Err(e);
                }
            },
            None => None,
        };
        webrtc.sync_state_with_parent()?;
        Ok((video, audio))
    })();
    let (video, audio) = match setup {
        Ok(pair) => pair,
        Err(e) => {
            let _ = webrtc.set_state(gst::State::Null);
            let _ = inner.pipeline.remove(&webrtc);
            return Err(e);
        }
    };

    inner.peers.lock().unwrap().insert(
        id.to_owned(),
        PeerConn { webrtc, video, audio, tier: target_tier, codec, relayed: None, last_bytes: 0 },
    );
    encoder::request_keyframe(&tier_enc);
    log::info!(
        "viewer {id} connecting ({codec:?}, {} total)",
        inner.peers.lock().unwrap().len()
    );
    Ok(())
}

fn remove_peer(inner: &Inner, id: &str) {
    let (peer, drop_compat) = {
        let mut peers = inner.peers.lock().unwrap();
        let Some(peer) = peers.remove(id) else { return };
        let still_compat = peers.values().any(|p| p.tier == Tier::CompatH264);
        let drop_compat = if !still_compat { inner.compat_h264.lock().unwrap().take() } else { None };
        (peer, drop_compat)
    };
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
        if let Some(compat) = drop_compat {
            teardown_tier(&pipeline, compat);
            log::info!("stopped parallel H.264 tier (no H.264-only viewers left)");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtp_fragments_parse() {
        gst::init().unwrap();
        gst::parse::launch(PARSE_H264).unwrap();
        gst::parse::bin_from_description(PEER_VIDEO_H264, true).unwrap();
        gst::parse::launch(PARSE_AV1).unwrap();
        gst::parse::bin_from_description(PEER_VIDEO_AV1, true).unwrap();
        gst::parse::bin_from_description(&format!("opusenc ! {AUDIO_RTP}"), true).unwrap();
        gst::parse::launch(PEER_QUEUE).unwrap();
    }

    #[test]
    fn audio_hot_swap_works() {
        gst::init().unwrap();
        let sig = SignalingHandle::dummy();
        let cfg = StreamConfig {
            video: VideoSource::Test,
            audio: AudioSource::Test,
            format: VideoFormat { width: 320, height: 240, fps: 30 },
            bitrate_kbps: 1500,
            auto_bitrate: true,
            codec: Codec::H264,
            encoder: None,
            relay_saver: false,
        };
        let s = Streamer::new(&cfg, sig, Vec::new()).unwrap();
        s.start().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(150));
        s.set_audio_source(AudioSource::Test).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(150));
        s.stop();
    }

    #[test]
    fn dual_codec_compat_tier_works() {
        gst::init().unwrap();
        if encoder::pick_for_codec(None, Codec::Av1).is_err() {
            return;
        }
        let sig = SignalingHandle::dummy();
        let cfg = StreamConfig {
            video: VideoSource::Test,
            audio: AudioSource::Test,
            format: VideoFormat { width: 640, height: 360, fps: 30 },
            bitrate_kbps: 3000,
            auto_bitrate: true,
            codec: Codec::Av1,
            encoder: None,
            relay_saver: false,
        };
        let s = Streamer::new(&cfg, sig, Vec::new()).unwrap();
        s.start().unwrap();
        s.handle(ServerMsg::Signal {
            from: "v_av1".into(),
            payload: Payload::Watch { codecs: vec!["av1".into(), "h264".into()] },
        });
        s.handle(ServerMsg::Signal {
            from: "v_h264".into(),
            payload: Payload::Watch { codecs: vec!["h264".into()] },
        });
        let info = s.tick();
        assert_eq!(info.len(), 2);
        assert!(info.iter().any(|v| v.id == "v_av1" && v.codec == VideoCodec::Av1));
        assert!(info.iter().any(|v| v.id == "v_h264" && v.codec == VideoCodec::H264));
        assert!(s.inner.compat_h264.lock().unwrap().is_some());
        s.stop();
    }
}
