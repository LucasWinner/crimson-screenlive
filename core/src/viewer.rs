//! Viewer: webrtcbin -> decodebin (hardware decode when available) -> native video window.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use gst::prelude::*;
use gst_video::prelude::VideoOverlayExtManual;
use tokio::sync::mpsc;

use crate::encoder;
use crate::ice::IceServer;
use crate::protocol::{Payload, Peer, ServerMsg};
use crate::signaling::SignalingHandle;
use crate::webrtc;

#[derive(Default, Clone)]
pub struct ViewConfig {
    /// Watch the streamer with this nickname; otherwise the first one streaming.
    pub target: Option<String>,
    /// Decode into fakesinks instead of opening a window (tests/CI).
    pub headless: bool,
    /// Stop after this long.
    pub duration: Option<Duration>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ViewStats {
    pub video_frames: u64,
    pub audio_buffers: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewEnd {
    /// The streamer stopped or left.
    StreamEnded,
    /// We stopped (Ctrl-C, timeout, or the video window was closed).
    UserStopped,
}

/// Codecs this viewer can decode, ordered by preference.
pub fn supported_codecs() -> Vec<String> {
    let can_av1 = gst::ElementFactory::find("rtpav1depay").is_some()
        && gst::ElementFactory::find("av1parse").is_some()
        && ["d3d11av1dec", "d3d12av1dec", "nvav1dec", "dav1ddec", "vaav1dec"]
            .iter()
            .any(|f| gst::ElementFactory::find(f).is_some());
    if can_av1 {
        vec!["av1".into(), "h264".into()]
    } else {
        vec!["h264".into()]
    }
}

fn is_target(p: &Peer, cfg: &ViewConfig) -> bool {
    p.streaming && cfg.target.as_ref().is_none_or(|t| t.eq_ignore_ascii_case(&p.name))
}

/// Waits until a matching streamer is in the room and returns it.
pub async fn find_streamer(
    peers: &[Peer],
    events: &mut mpsc::UnboundedReceiver<ServerMsg>,
    cfg: &ViewConfig,
) -> Result<Peer> {
    if let Some(p) = peers.iter().find(|p| is_target(p, cfg)) {
        return Ok(p.clone());
    }
    log::info!("waiting for someone to start streaming...");
    loop {
        match events.recv().await {
            Some(ServerMsg::PeerUpdated { peer } | ServerMsg::PeerJoined { peer }) if is_target(&peer, cfg) => {
                return Ok(peer)
            }
            Some(_) => {}
            None => bail!("lost connection to signaling server"),
        }
    }
}

pub struct Viewer {
    pipeline: gst::Pipeline,
    webrtc: gst::Element,
    streamer: Peer,
    sig: SignalingHandle,
    video_frames: Arc<AtomicU64>,
    audio_buffers: Arc<AtomicU64>,
    volume: Arc<Mutex<f64>>,
    muted: Arc<AtomicBool>,
    volume_elem: Arc<Mutex<Option<gst::Element>>>,
    stopped: AtomicBool,
}

impl Drop for Viewer {
    fn drop(&mut self) {
        if let Some(bus) = self.pipeline.bus() {
            bus.unset_sync_handler();
        }
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

impl Viewer {
    pub fn new(streamer: Peer, sig: SignalingHandle, ice: &[IceServer], headless: bool) -> Result<Self> {
        Self::new_with_window(streamer, sig, ice, headless, None)
    }

    pub fn new_with_window(
        streamer: Peer,
        sig: SignalingHandle,
        ice: &[IceServer],
        headless: bool,
        window_handle: Option<usize>,
    ) -> Result<Self> {
        let pipeline = gst::Pipeline::with_name("viewer");
        let webrtc = webrtc::new_webrtcbin("recv", ice, &sig, &streamer.id)?;
        // Jitter buffer: small enough to feel live, big enough to absorb Wi-Fi jitter.
        webrtc.set_property("latency", 120u32);
        pipeline.add(&webrtc)?;

        if let (false, Some(hwnd)) = (headless, window_handle) {
            if let Some(bus) = pipeline.bus() {
                bus.set_sync_handler(move |_, msg| {
                    if gst_video::is_video_overlay_prepare_window_handle_message(msg) {
                        if let Some(src) = msg.src() {
                            if let Some(overlay) = src.dynamic_cast_ref::<gst_video::VideoOverlay>() {
                                unsafe {
                                    overlay.set_window_handle(hwnd);
                                }
                            }
                        }
                    }
                    gst::BusSyncReply::Pass
                });
            }
        }

        {
            let pipe_weak = pipeline.downgrade();
            webrtc.connect_notify(Some("connection-state"), move |w, _| {
                use gst_webrtc::WebRTCPeerConnectionState as S;
                if matches!(w.property::<S>("connection-state"), S::Failed) {
                    if let Some(pipeline) = pipe_weak.upgrade() {
                        let _ = pipeline.post_message(gst::message::Application::new(
                            gst::Structure::new_empty("connection-failed"),
                        ));
                    }
                }
            });
        }

        let video_frames = Arc::new(AtomicU64::new(0));
        let audio_buffers = Arc::new(AtomicU64::new(0));
        let volume = Arc::new(Mutex::new(1.0f64));
        let muted = Arc::new(AtomicBool::new(false));
        let volume_elem: Arc<Mutex<Option<gst::Element>>> = Arc::new(Mutex::new(None));
        let (vf, ab, vol_val, mute_val, vol_el) =
            (video_frames.clone(), audio_buffers.clone(), volume.clone(), muted.clone(), volume_elem.clone());
        let embedded = window_handle.is_some();
        let pipe_weak = pipeline.downgrade();
        webrtc.connect_pad_added(move |_, pad| {
            if pad.direction() != gst::PadDirection::Src {
                return;
            }
            let Some(pipeline) = pipe_weak.upgrade() else { return };
            if let Err(e) = on_incoming_stream(
                &pipeline,
                pad,
                headless,
                embedded,
                vf.clone(),
                ab.clone(),
                vol_val.clone(),
                mute_val.clone(),
                vol_el.clone(),
            ) {
                log::error!("could not play incoming stream: {e:#}");
            }
        });

        Ok(Self {
            pipeline,
            webrtc,
            streamer,
            sig,
            video_frames,
            audio_buffers,
            volume,
            muted,
            volume_elem,
            stopped: AtomicBool::new(false),
        })
    }

    pub fn set_volume(&self, volume: f64, muted: bool) {
        let clamped = volume.clamp(0.0, 2.0);
        *self.volume.lock().unwrap() = clamped;
        self.muted.store(muted, Ordering::Relaxed);
        if let Some(el) = self.volume_elem.lock().unwrap().as_ref() {
            el.set_property("volume", clamped);
            el.set_property("mute", muted);
        }
    }

    pub fn link_stats(&self) -> webrtc::LinkStats {
        webrtc::stats(&self.webrtc).map(|s| webrtc::link_stats(&s)).unwrap_or_default()
    }

    pub fn stats(&self) -> ViewStats {
        ViewStats {
            video_frames: self.video_frames.load(Ordering::Relaxed),
            audio_buffers: self.audio_buffers.load(Ordering::Relaxed),
        }
    }

    /// Whether this connection goes through a TURN relay (None while connecting).
    pub fn is_relayed(&self) -> Option<bool> {
        webrtc::is_relayed(&webrtc::stats(&self.webrtc)?)
    }

    pub fn streamer(&self) -> &Peer {
        &self.streamer
    }

    pub fn pipeline(&self) -> &gst::Pipeline {
        &self.pipeline
    }

    /// Starts the pipeline and asks the streamer for its stream.
    pub fn start(&self) -> Result<()> {
        self.pipeline.set_state(gst::State::Playing).context("could not start viewer pipeline")?;
        self.sig.signal(&self.streamer.id, Payload::Watch { codecs: supported_codecs() });
        log::info!("watching {}", self.streamer.name);
        Ok(())
    }

    /// Feeds a room message to the viewer. Returns `Some` when watching should end.
    pub fn handle(&self, msg: &ServerMsg) -> Result<Option<ViewEnd>> {
        let sid = &self.streamer.id;
        match msg {
            ServerMsg::Signal { from, payload } if from == sid => match payload {
                Payload::Offer { sdp } => {
                    let (sig, to) = (self.sig.clone(), sid.clone());
                    webrtc::accept_offer(&self.webrtc, sdp, move |sdp| sig.signal(&to, Payload::Answer { sdp }))?;
                }
                Payload::Ice { candidate, mline } => webrtc::add_ice(&self.webrtc, *mline, candidate),
                Payload::Bye => return Ok(Some(ViewEnd::StreamEnded)),
                _ => {}
            },
            ServerMsg::PeerLeft { id } if id == sid => return Ok(Some(ViewEnd::StreamEnded)),
            ServerMsg::PeerUpdated { peer } if &peer.id == sid && !peer.streaming => {
                return Ok(Some(ViewEnd::StreamEnded))
            }
            _ => {}
        }
        Ok(None)
    }

    /// Interprets a pipeline bus message. Returns `Some` when watching should end.
    pub fn handle_bus(&self, msg: &gst::Message) -> Result<Option<ViewEnd>> {
        match msg.view() {
            gst::MessageView::Error(e) => {
                // The native video window was closed by the user.
                let err = e.error();
                let text = err.message();
                let from_videosink = e
                    .src()
                    .is_some_and(|s| s.type_().name().to_ascii_lowercase().contains("videosink"));
                if text.contains("Output window was closed")
                    || text.contains("Window was closed")
                    || (from_videosink && text.contains("closed"))
                {
                    return Ok(Some(ViewEnd::UserStopped));
                }
            }
            gst::MessageView::Application(app)
                if app.structure().is_some_and(|s| s.name() == "connection-failed") =>
            {
                return Err(anyhow!("P2P connection failed (network/NAT blocked the connection)"));
            }
            _ => {}
        }
        match webrtc::check_bus_message(msg)? {
            true => Ok(None),
            false => Ok(Some(ViewEnd::StreamEnded)),
        }
    }

    /// Toggles fullscreen on the native video window. Returns the new state,
    /// or None if no video sink with fullscreen support exists yet.
    pub fn toggle_fullscreen(&self) -> Option<bool> {
        let sink = self
            .pipeline
            .iterate_recurse()
            .into_iter()
            .flatten()
            .find(|e| e.find_property("fullscreen").is_some())?;
        configure_video_sink(&sink, false);
        let on = !sink.property::<bool>("fullscreen");
        sink.set_property("fullscreen", on);
        Some(on)
    }

    pub fn stop(&self) {
        if !self.stopped.swap(true, Ordering::SeqCst) {
            self.sig.signal(&self.streamer.id, Payload::Bye);
        }
        if let Some(bus) = self.pipeline.bus() {
            bus.unset_sync_handler();
        }
        let _ = self.pipeline.set_state(gst::State::Null);
    }

    /// Standalone loop (CLI): runs until the stream ends, Ctrl-C, or `duration`.
    pub async fn run(
        &self,
        mut events: mpsc::UnboundedReceiver<ServerMsg>,
        duration: Option<Duration>,
    ) -> Result<ViewEnd> {
        self.start()?;
        let mut bus_stream = self.pipeline.bus().expect("bus").stream();
        let deadline = duration.map(|d| Instant::now() + d);
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        let (mut last_frames, mut last_at) = (0u64, Instant::now());

        let result = loop {
            tokio::select! {
                ev = events.recv() => match ev {
                    Some(ServerMsg::Error { message }) => log::warn!("server: {message}"),
                    Some(msg) => match self.handle(&msg) {
                        Ok(Some(end)) => break Ok(end),
                        Ok(None) => {}
                        Err(e) => break Err(e),
                    },
                    None => break Err(anyhow::anyhow!("lost connection to signaling server")),
                },
                Some(msg) = bus_stream.next() => match self.handle_bus(&msg) {
                    Ok(Some(end)) => break Ok(end),
                    Ok(None) => {}
                    Err(e) => break Err(e),
                },
                _ = tick.tick() => {
                    let frames = self.video_frames.load(Ordering::Relaxed);
                    let secs = last_at.elapsed().as_secs_f64();
                    if frames > 0 {
                        log::info!("receiving {:.0} fps", (frames - last_frames) as f64 / secs);
                    }
                    (last_frames, last_at) = (frames, Instant::now());
                    if deadline.is_some_and(|d| Instant::now() >= d) {
                        break Ok(ViewEnd::UserStopped);
                    }
                }
                _ = tokio::signal::ctrl_c() => break Ok(ViewEnd::UserStopped),
            }
        };

        self.stop();
        result
    }
}

fn configure_video_sink(elem: &gst::Element, embedded: bool) {
    // d3d11videosink ignores `fullscreen` unless `fullscreen-toggle-mode`
    // includes `property`; enable Alt+Enter on d3d11/d3d12 as well.
    encoder::set_if(elem, "fullscreen-toggle-mode", "property+alt-enter");
    encoder::set_if(elem, "fullscreen-on-alt-enter", "true");
    if embedded {
        encoder::set_if(elem, "error-on-closed", "false");
    }
}

#[allow(clippy::too_many_arguments)]
fn on_incoming_stream(
    pipeline: &gst::Pipeline,
    pad: &gst::Pad,
    headless: bool,
    embedded: bool,
    video_frames: Arc<AtomicU64>,
    audio_buffers: Arc<AtomicU64>,
    volume: Arc<Mutex<f64>>,
    muted: Arc<AtomicBool>,
    volume_elem: Arc<Mutex<Option<gst::Element>>>,
) -> Result<()> {
    let decodebin = gst::ElementFactory::make("decodebin").build()?;
    let pipe_weak = pipeline.downgrade();
    decodebin.connect_pad_added(move |_, dpad| {
        let Some(pipeline) = pipe_weak.upgrade() else { return };
        let caps = dpad.current_caps().or_else(|| Some(dpad.query_caps(None)));
        let media = caps
            .as_ref()
            .and_then(|c| c.structure(0))
            .map(|s| s.name().to_string())
            .unwrap_or_default();
        let (desc, counter, is_video) = if media.starts_with("video/") {
            let sink = if headless { "fakesink sync=false" } else { "autovideosink" };
            (format!("queue max-size-buffers=3 ! {sink}"), video_frames.clone(), true)
        } else if media.starts_with("audio/") {
            let d = if headless {
                "queue ! audioconvert ! audioresample ! fakesink sync=false".to_owned()
            } else {
                "queue ! volume name=watch-vol ! audioconvert ! audioresample ! autoaudiosink".to_owned()
            };
            (d, audio_buffers.clone(), false)
        } else {
            log::warn!("ignoring unknown stream {media}");
            return;
        };
        let res: Result<()> = (|| {
            let bin = gst::parse::bin_from_description(&desc, true)?;
            if is_video && !headless {
                bin.connect_deep_element_added(move |_, _, elem| configure_video_sink(elem, embedded));
            }
            if !is_video && !headless {
                if let Some(vol) = bin.by_name("watch-vol") {
                    vol.set_property("volume", *volume.lock().unwrap());
                    vol.set_property("mute", muted.load(Ordering::Relaxed));
                    *volume_elem.lock().unwrap() = Some(vol);
                }
            }
            pipeline.add(&bin)?;
            bin.sync_state_with_parent()?;
            let sink = bin.static_pad("sink").unwrap();
            sink.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
                counter.fetch_add(1, Ordering::Relaxed);
                gst::PadProbeReturn::Ok
            });
            dpad.link(&sink)?;
            Ok(())
        })();
        match res {
            Ok(()) => log::info!("playing {media}"),
            Err(e) => log::error!("could not link {media}: {e:#}"),
        }
    });
    pipeline.add(&decodebin)?;
    decodebin.sync_state_with_parent()?;
    pad.link(&decodebin.static_pad("sink").unwrap())?;
    Ok(())
}
