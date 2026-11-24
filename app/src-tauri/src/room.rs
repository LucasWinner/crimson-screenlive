//! Room session: one signaling connection per room feeding the peer list (UI),
//! our own stream (if any) and the stream we're watching (if any).
//!
//! Events emitted to the UI:
//!   "room-updated"  RoomView      peers changed
//!   "room-closed"   { reason }    connection lost
//!   "stream-status" StreamStatus  our stream started/stopped/stats
//!   "watch-status"  WatchStatus   watching started/stopped/stats

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use gst::prelude::*;
use screenlive_core::capture::{self, MonitorInfo, VideoFormat, VideoSource};
use screenlive_core::encoder;
use screenlive_core::ice::IceServer;
use screenlive_core::protocol::{Peer, ServerMsg};
use screenlive_core::signaling::{normalize_code, Server, SignalingHandle};
use screenlive_core::streamer::{StreamConfig, Streamer};
use screenlive_core::viewer::{ViewEnd, Viewer};
use serde::Serialize;
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Emitter, State};

use crate::settings;

type Shared = Arc<Mutex<Option<Room>>>;
type CmdResult<T> = Result<T, String>;

#[derive(Default)]
pub struct AppState {
    room: Shared,
}

impl AppState {
    pub fn shutdown(&self) {
        if let Some(room) = self.room.lock().unwrap().take() {
            shutdown(room);
        }
    }
}

/// A running pipeline plus the task watching its bus.
struct Active<T> {
    obj: Arc<T>,
    bus_task: JoinHandle<()>,
}

struct Room {
    generation: u64,
    code: String,
    me: String,
    name: String,
    peers: Vec<Peer>,
    sig: SignalingHandle,
    ice: Vec<IceServer>,
    streamer: Option<Active<Streamer>>,
    viewer: Option<Active<Viewer>>,
    loop_task: Option<JoinHandle<()>>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RoomView {
    code: String,
    me: String,
    name: String,
    peers: Vec<Peer>,
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct StreamStatus {
    active: bool,
    encoder: Option<String>,
    viewers: usize,
    /// How many viewers go through the TURN relay.
    relayed: usize,
    error: Option<String>,
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct WatchStatus {
    active: bool,
    streamer_id: Option<String>,
    streamer_name: Option<String>,
    fps: f64,
    /// Our connection to the streamer goes through the TURN relay.
    relayed: bool,
    /// Why watching ended: "ended" (streamer stopped), "closed" (window closed), or an error.
    ended: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemInfo {
    monitors: Vec<MonitorInfo>,
    encoder: Option<String>,
    hardware: bool,
    av1_encoder: Option<String>,
    h264_encoder: Option<String>,
    h264_hardware: bool,
}

impl Room {
    fn view(&self) -> RoomView {
        RoomView { code: self.code.clone(), me: self.me.clone(), name: self.name.clone(), peers: self.peers.clone() }
    }
}

static GENERATION: AtomicU64 = AtomicU64::new(1);

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// "<720|1080|1440>p<30|60>" -> format and video bitrate (kbps). 30 fps needs
/// roughly 2/3 of the bits of 60 fps for the same sharpness.
/// For AV1, 720p/1080p save a bit of bandwidth (~15%) while 1440p keeps full bitrate for maximum sharpness.
fn quality(preset: &str, codec: encoder::VideoCodec, bitrate_override: Option<u32>) -> (VideoFormat, u32) {
    let (res, fps) = preset.split_once('p').unwrap_or(("1080", "60"));
    let fps: u32 = if fps == "30" { 30 } else { 60 };
    let (width, height, kbps60) = match (res, codec) {
        ("720", encoder::VideoCodec::Av1) => (1280, 720, 3800),
        ("720", encoder::VideoCodec::H264) => (1280, 720, 4500),
        ("1440", _) => (2560, 1440, 12000),
        (_, encoder::VideoCodec::Av1) => (1920, 1080, 6000),
        (_, encoder::VideoCodec::H264) => (1920, 1080, 7000),
    };
    let auto_kbps = if fps == 30 { kbps60 * 2 / 3 } else { kbps60 };
    let kbps = bitrate_override.map(|b| b.clamp(1000, 30_000)).unwrap_or(auto_kbps);
    (VideoFormat { width, height, fps }, kbps)
}

#[cfg(test)]
mod tests {
    use screenlive_core::encoder::VideoCodec;

    #[test]
    fn quality_presets() {
        let (f, k) = super::quality("1440p30", VideoCodec::H264, None);
        assert_eq!((f.width, f.height, f.fps, k), (2560, 1440, 30, 8000));
        let (f, k) = super::quality("1440p60", VideoCodec::Av1, None);
        assert_eq!((f.width, f.height, f.fps, k), (2560, 1440, 60, 12000));
        let (f, k) = super::quality("1080p60", VideoCodec::Av1, None);
        assert_eq!((f.width, f.height, f.fps, k), (1920, 1080, 60, 6000));
        let (f, k) = super::quality("720p60", VideoCodec::H264, None);
        assert_eq!((f.width, f.height, f.fps, k), (1280, 720, 60, 4500));
        let (f, k) = super::quality("1080p60", VideoCodec::Av1, Some(9000));
        assert_eq!((f.width, f.height, f.fps, k), (1920, 1080, 60, 9000));
        let (f, _) = super::quality("garbage", VideoCodec::H264, None);
        assert_eq!((f.height, f.fps), (1080, 60));
    }
}

// ---------------------------------------------------------------- commands

#[tauri::command]
pub fn system_info() -> SystemInfo {
    let best = encoder::pick_for_codec(None, encoder::Codec::Auto).ok();
    let av1 = encoder::pick_for_codec(None, encoder::Codec::Av1).ok();
    let h264 = encoder::pick_for_codec(None, encoder::Codec::H264).ok();
    SystemInfo {
        monitors: capture::list_monitors(),
        encoder: best.map(|e| e.label.to_string()),
        hardware: best.is_some_and(|e| e.hardware),
        av1_encoder: av1.map(|e| e.label.to_string()),
        h264_encoder: h264.map(|e| e.label.to_string()),
        h264_hardware: h264.is_some_and(|e| e.hardware),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioSources {
    /// Per-app capture available (Windows 10 build 20348+ / Windows 11).
    supported: bool,
    discord_running: bool,
    apps: Vec<screenlive_core::procs::AppInfo>,
}

#[tauri::command]
pub fn audio_sources() -> AudioSources {
    use screenlive_core::procs;
    AudioSources {
        supported: procs::per_app_audio_supported(),
        discord_running: procs::discord_root_pid().is_some(),
        apps: procs::list_apps(),
    }
}

#[tauri::command]
pub async fn create_room(app: AppHandle, state: State<'_, AppState>, name: String) -> CmdResult<RoomView> {
    let server = Server::new(&settings::load(&app).server_url).map_err(err)?;
    let code = server.create_room().await.map_err(|e| format!("{e:#}"))?;
    join(app, state.room.clone(), server, code, name).await
}

#[tauri::command]
pub async fn join_room(app: AppHandle, state: State<'_, AppState>, code: String, name: String) -> CmdResult<RoomView> {
    let server = Server::new(&settings::load(&app).server_url).map_err(err)?;
    join(app, state.room.clone(), server, code, name).await
}

#[tauri::command]
pub fn leave_room(state: State<'_, AppState>) {
    state.shutdown();
}

#[tauri::command]
pub fn start_stream(
    app: AppHandle,
    state: State<'_, AppState>,
    monitor_id: Option<u64>,
    quality_preset: String,
    bitrate_kbps: Option<u32>,
) -> CmdResult<StreamStatus> {
    // Resolve the audio choice (PIDs) and codec before locking: it enumerates processes.
    let saved = settings::load(&app);
    let choice = saved.audio_choice();
    let codec = saved.codec_choice();
    let bitrate_override = bitrate_kbps.or(saved.bitrate_kbps);
    let audio = screenlive_core::procs::resolve_audio(&choice).map_err(|e| {
        let msg = e.to_string();
        match msg.strip_suffix(" is not running") {
            Some(exe) => format!("o programa {exe} não está aberto. Abra ele ou mude o áudio em ⚙️ Configurações"),
            None => msg,
        }
    })?;
    let spec = encoder::pick_for_codec(None, codec).map_err(|e| {
        if codec == encoder::Codec::Av1 {
            "sua placa de vídeo não tem codificador AV1 por hardware. Mude o codec para Automático ou H.264 em ⚙️ Configurações.".into()
        } else {
            format!("{e:#}")
        }
    })?;
    let shared = state.room.clone();
    let (gen, sig, ice) = {
        let guard = shared.lock().unwrap();
        let room = guard.as_ref().ok_or("você não está em uma sala")?;
        if room.streamer.is_some() {
            return Err("você já está transmitindo".into());
        }
        (room.generation, room.sig.clone(), room.ice.clone())
    };

    let (format, bitrate_kbps) = quality(&quality_preset, spec.codec, bitrate_override);
    let monitors = capture::list_monitors();
    let monitor = monitor_id
        .filter(|id| monitors.iter().any(|m| m.id == *id))
        .or_else(|| monitors.first().map(|m| m.id));
    let cfg = StreamConfig {
        video: match monitor {
            Some(id) => VideoSource::Monitor(id),
            None => VideoSource::MonitorIndex(0),
        },
        audio,
        format,
        bitrate_kbps,
        codec,
        encoder: Some(spec.factory.to_string()),
        relay_saver: false,
    };

    let streamer = Arc::new(Streamer::new(&cfg, sig, ice).map_err(|e| format!("{e:#}"))?);
    streamer.start().map_err(|e| format!("{e:#}"))?;

    let status = StreamStatus {
        active: true,
        encoder: Some(streamer.encoder.label.to_string()),
        viewers: 0,
        relayed: 0,
        error: None,
    };

    {
        let mut guard = shared.lock().unwrap();
        let Some(room) = guard.as_mut().filter(|r| r.generation == gen) else {
            drop(guard);
            streamer.stop();
            return Err("você saiu da sala".into());
        };
        if room.streamer.is_some() {
            drop(guard);
            streamer.stop();
            return Err("você já está transmitindo".into());
        }
        let bus_task = {
            let (app, shared) = (app.clone(), shared.clone());
            let bus = streamer.pipeline().bus().expect("bus");
            let s = streamer.clone();
            tauri::async_runtime::spawn(async move {
                let mut msgs = bus.stream();
                while let Some(msg) = msgs.next().await {
                    match s.handle_bus(&msg) {
                        Ok(true) => {}
                        Ok(false) => {
                            log::info!("stream pipeline ended (EOS)");
                            end_stream(&app, &shared, gen, None);
                            return;
                        }
                        Err(e) => {
                            log::error!("stream pipeline: {e:#}");
                            end_stream(&app, &shared, gen, Some(format!("{e:#}")));
                            return;
                        }
                    }
                }
            })
        };
        room.streamer = Some(Active { obj: streamer, bus_task });
    }

    let _ = app.emit("stream-status", status.clone());
    Ok(status)
}

#[tauri::command]
pub fn stop_stream(app: AppHandle, state: State<'_, AppState>) {
    let gen = state.room.lock().unwrap().as_ref().map(|r| r.generation);
    if let Some(gen) = gen {
        end_stream(&app, &state.room, gen, None);
    }
}

#[tauri::command]
pub fn watch(app: AppHandle, state: State<'_, AppState>, peer_id: String) -> CmdResult<WatchStatus> {
    let shared = state.room.clone();
    let (gen, peer, sig, ice, old) = {
        let mut guard = shared.lock().unwrap();
        let room = guard.as_mut().ok_or("você não está em uma sala")?;
        let peer = room.peers.iter().find(|p| p.id == peer_id).cloned().ok_or("essa pessoa saiu da sala")?;
        if !peer.streaming {
            return Err(format!("{} não está transmitindo", peer.name));
        }
        (room.generation, peer, room.sig.clone(), room.ice.clone(), room.viewer.take())
    };
    if let Some(old) = old {
        old.bus_task.abort();
        old.obj.stop();
    }

    let viewer = Arc::new(Viewer::new(peer.clone(), sig, &ice, false).map_err(|e| format!("{e:#}"))?);
    viewer.start().map_err(|e| format!("{e:#}"))?;

    let status = WatchStatus {
        active: true,
        streamer_id: Some(peer.id),
        streamer_name: Some(peer.name),
        ..Default::default()
    };

    let displaced = {
        let mut guard = shared.lock().unwrap();
        let Some(room) = guard.as_mut().filter(|r| r.generation == gen) else {
            drop(guard);
            viewer.stop();
            return Err("você saiu da sala".into());
        };
        let bus_task = {
            let (app, shared) = (app.clone(), shared.clone());
            let bus = viewer.pipeline().bus().expect("bus");
            let v = viewer.clone();
            tauri::async_runtime::spawn(async move {
                let mut msgs = bus.stream();
                while let Some(msg) = msgs.next().await {
                    let end = match v.handle_bus(&msg) {
                        Ok(None) => continue,
                        Ok(Some(ViewEnd::UserStopped)) => "closed".to_string(),
                        Ok(Some(ViewEnd::StreamEnded)) => "ended".to_string(),
                        Err(e) => format!("{e:#}"),
                    };
                    end_watch(&app, &shared, gen, &v, end);
                    return;
                }
            })
        };
        room.viewer.replace(Active { obj: viewer, bus_task })
    };
    if let Some(old) = displaced {
        old.bus_task.abort();
        old.obj.stop();
    }

    let _ = app.emit("watch-status", status.clone());
    Ok(status)
}

#[tauri::command]
pub fn stop_watch(app: AppHandle, state: State<'_, AppState>) {
    let current = state.room.lock().unwrap().as_ref().and_then(|r| Some((r.generation, r.viewer.as_ref()?.obj.clone())));
    if let Some((gen, v)) = current {
        end_watch(&app, &state.room, gen, &v, "closed".into());
    }
}

#[tauri::command]
pub fn toggle_fullscreen(state: State<'_, AppState>) -> CmdResult<bool> {
    let viewer = state.room.lock().unwrap().as_ref().and_then(|r| Some(r.viewer.as_ref()?.obj.clone()));
    let viewer = viewer.ok_or("você não está assistindo nada")?;
    viewer.toggle_fullscreen().ok_or_else(|| "a janela de vídeo ainda não abriu".into())
}

// ---------------------------------------------------------------- internals

async fn join(app: AppHandle, shared: Shared, server: Server, code: String, name: String) -> CmdResult<RoomView> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("escolha um apelido primeiro".into());
    }
    let code = normalize_code(&code).ok_or("código inválido: use 6 letras/números (sem 0, O, 1 ou I)")?;

    if let Some(old) = shared.lock().unwrap().take() {
        shutdown(old);
    }

    let joined = server.enter(&code, &name).await.map_err(|e| {
        let msg = format!("{e:#}");
        if msg.contains("invalid app key") {
            "esta versão do app não é aceita pelo servidor. Peça o instalador mais novo".to_string()
        } else if msg.contains("did not answer") {
            "o servidor não respondeu".to_string()
        } else {
            msg
        }
    })?;
    let (sig, events, me, peers, ice) = (joined.sig, joined.events, joined.me, joined.peers, joined.ice);

    let generation = GENERATION.fetch_add(1, Ordering::Relaxed);
    let room = Room {
        generation,
        code,
        me,
        name,
        peers,
        sig,
        ice,
        streamer: None,
        viewer: None,
        loop_task: None,
    };
    let view = room.view();
    let previous = shared.lock().unwrap().replace(room);
    if let Some(old) = previous {
        shutdown(old);
    }

    let task = tauri::async_runtime::spawn(room_loop(app, shared.clone(), generation, events));
    let mut guard = shared.lock().unwrap();
    if let Some(r) = guard.as_mut().filter(|r| r.generation == generation) {
        r.loop_task = Some(task);
    } else {
        task.abort();
    }
    Ok(view)
}

fn shutdown(mut room: Room) {
    if let Some(s) = room.streamer.take() {
        s.bus_task.abort();
        s.obj.stop();
    }
    if let Some(v) = room.viewer.take() {
        v.bus_task.abort();
        v.obj.stop();
    }
    room.sig.close();
    if let Some(t) = room.loop_task.take() {
        t.abort();
    }
}

async fn room_loop(app: AppHandle, shared: Shared, gen: u64, mut events: tokio::sync::mpsc::UnboundedReceiver<ServerMsg>) {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let (mut last_frames, mut last_at) = (0u64, Instant::now());
    let mut ticks = 0u64;
    loop {
        tokio::select! {
            msg = events.recv() => match msg {
                Some(msg) => on_message(&app, &shared, gen, msg),
                None => {
                    let mut guard = shared.lock().unwrap();
                    if guard.as_ref().is_some_and(|r| r.generation == gen) {
                        let mut room = guard.take().unwrap();
                        room.loop_task = None; // don't abort ourselves mid-cleanup
                        drop(guard);
                        shutdown(room);
                        let _ = app.emit("stream-status", StreamStatus::default());
                        let _ = app.emit("watch-status", WatchStatus::default());
                        let _ = app.emit("room-closed", serde_json::json!({ "reason": "conexão com o servidor perdida" }));
                    }
                    return;
                }
            },
            _ = tick.tick() => {
                let (streamer, viewer) = {
                    let guard = shared.lock().unwrap();
                    let Some(room) = guard.as_ref().filter(|r| r.generation == gen) else { return };
                    (room.streamer.as_ref().map(|s| s.obj.clone()), room.viewer.as_ref().map(|v| v.obj.clone()))
                };
                ticks += 1;
                if let Some(s) = streamer {
                    let s_tick = s.clone();
                    if let Ok(viewers) = tauri::async_runtime::spawn_blocking(move || s_tick.tick()).await {
                        if ticks.is_multiple_of(15) {
                            s.report_relay_usage();
                        }
                        let _ = app.emit("stream-status", StreamStatus {
                            active: true,
                            encoder: Some(s.encoder.label.to_string()),
                            viewers: viewers.len(),
                            relayed: viewers.iter().filter(|v| v.relayed == Some(true)).count(),
                            error: None,
                        });
                    }
                }
                match viewer {
                    Some(v) => {
                        let frames = v.stats().video_frames;
                        let fps = frames.saturating_sub(last_frames) as f64 / last_at.elapsed().as_secs_f64();
                        (last_frames, last_at) = (frames, Instant::now());
                        let v_relay = v.clone();
                        let relayed = tauri::async_runtime::spawn_blocking(move || v_relay.is_relayed())
                            .await
                            .ok()
                            .flatten()
                            == Some(true);
                        let _ = app.emit("watch-status", WatchStatus {
                            active: true,
                            streamer_id: Some(v.streamer().id.clone()),
                            streamer_name: Some(v.streamer().name.clone()),
                            fps: fps.round(),
                            relayed,
                            ended: None,
                        });
                    }
                    None => (last_frames, last_at) = (0, Instant::now()),
                }
            }
        }
    }
}

fn on_message(app: &AppHandle, shared: &Shared, gen: u64, msg: ServerMsg) {
    let (streamer, viewer, changed) = {
        let mut guard = shared.lock().unwrap();
        let Some(room) = guard.as_mut().filter(|r| r.generation == gen) else { return };
        let changed = match &msg {
            ServerMsg::PeerJoined { peer } => {
                if let Some(existing) = room.peers.iter_mut().find(|p| p.id == peer.id) {
                    *existing = peer.clone();
                } else {
                    room.peers.push(peer.clone());
                }
                true
            }
            ServerMsg::PeerLeft { id } => {
                room.peers.retain(|p| &p.id != id);
                true
            }
            ServerMsg::PeerUpdated { peer } => {
                if let Some(p) = room.peers.iter_mut().find(|p| p.id == peer.id) {
                    *p = peer.clone();
                }
                true
            }
            ServerMsg::Error { message } => {
                log::warn!("server: {message}");
                false
            }
            _ => false,
        };
        (
            room.streamer.as_ref().map(|s| s.obj.clone()),
            room.viewer.as_ref().map(|v| v.obj.clone()),
            changed.then(|| room.view()),
        )
    };

    if let Some(view) = changed {
        let _ = app.emit("room-updated", view);
    }
    if let Some(v) = viewer {
        match v.handle(&msg) {
            Ok(None) => {}
            Ok(Some(ViewEnd::StreamEnded)) => end_watch(app, shared, gen, &v, "ended".into()),
            Ok(Some(ViewEnd::UserStopped)) => end_watch(app, shared, gen, &v, "closed".into()),
            Err(e) => end_watch(app, shared, gen, &v, format!("{e:#}")),
        }
    }
    if let Some(s) = streamer {
        s.handle(msg);
    }
}

fn end_stream(app: &AppHandle, shared: &Shared, gen: u64, error: Option<String>) {
    let active = {
        let mut guard = shared.lock().unwrap();
        guard.as_mut().filter(|r| r.generation == gen).and_then(|r| r.streamer.take())
    };
    let had_stream = active.is_some();
    if let Some(s) = active {
        s.bus_task.abort();
        s.obj.stop();
    }
    if had_stream || error.is_some() {
        let _ = app.emit("stream-status", StreamStatus { error, ..Default::default() });
    }
}

/// Stops watching `which` (ignored if we've since switched to another stream).
fn end_watch(app: &AppHandle, shared: &Shared, gen: u64, which: &Arc<Viewer>, reason: String) {
    let active = {
        let mut guard = shared.lock().unwrap();
        let room = guard.as_mut().filter(|r| r.generation == gen);
        match room {
            Some(r) if r.viewer.as_ref().is_some_and(|v| Arc::ptr_eq(&v.obj, which)) => r.viewer.take(),
            _ => None,
        }
    };
    if let Some(v) = active {
        v.bus_task.abort();
        v.obj.stop();
        let _ = app.emit("watch-status", WatchStatus { ended: Some(reason), ..Default::default() });
    }
}
