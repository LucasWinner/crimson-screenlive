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
use screenlive_core::ice::{self, IceServer};
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
    /// How many viewers go through the TURN relay (they get 720p).
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

fn quality(preset: &str) -> (VideoFormat, u32) {
    match preset {
        "720p60" => (VideoFormat { width: 1280, height: 720, fps: 60 }, 4500),
        "1440p60" => (VideoFormat { width: 2560, height: 1440, fps: 60 }, 12000),
        _ => (VideoFormat { width: 1920, height: 1080, fps: 60 }, 7000),
    }
}

// ---------------------------------------------------------------- commands

#[tauri::command]
pub fn system_info() -> SystemInfo {
    let best = encoder::pick(None).ok();
    SystemInfo {
        monitors: capture::list_monitors(),
        encoder: best.map(|e| e.label.to_string()),
        hardware: best.is_some_and(|e| e.hardware),
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
    if let Some(room) = state.room.lock().unwrap().take() {
        shutdown(room);
    }
}

#[tauri::command]
pub fn start_stream(
    app: AppHandle,
    state: State<'_, AppState>,
    monitor_id: Option<u64>,
    quality_preset: String,
) -> CmdResult<StreamStatus> {
    // Resolve the audio choice (PIDs) before locking: it enumerates processes.
    let choice = settings::load(&app).audio_choice();
    let audio = screenlive_core::procs::resolve_audio(&choice).map_err(|e| {
        let msg = e.to_string();
        match msg.strip_suffix(" is not running") {
            Some(exe) => format!("o programa {exe} não está aberto. Abra ele ou mude o áudio em ⚙️ Configurações"),
            None => msg,
        }
    })?;
    let shared = state.room.clone();
    let mut guard = shared.lock().unwrap();
    let room = guard.as_mut().ok_or("você não está em uma sala")?;
    if room.streamer.is_some() {
        return Err("você já está transmitindo".into());
    }

    let (format, bitrate_kbps) = quality(&quality_preset);
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
        encoder: None,
        relay_saver: true,
    };

    let streamer = Arc::new(Streamer::new(&cfg, room.sig.clone(), room.ice.clone()).map_err(|e| format!("{e:#}"))?);
    streamer.start().map_err(|e| format!("{e:#}"))?;

    let bus_task = {
        let (app, shared, gen) = (app.clone(), shared.clone(), room.generation);
        let bus = streamer.pipeline().bus().expect("bus");
        let s = streamer.clone();
        tauri::async_runtime::spawn(async move {
            let mut msgs = bus.stream();
            while let Some(msg) = msgs.next().await {
                if let Err(e) = s.handle_bus(&msg) {
                    log::error!("stream pipeline: {e:#}");
                    end_stream(&app, &shared, gen, Some(format!("{e:#}")));
                    return;
                }
            }
        })
    };

    let status = StreamStatus {
        active: true,
        encoder: Some(streamer.encoder.label.to_string()),
        viewers: 0,
        relayed: 0,
        error: None,
    };
    room.streamer = Some(Active { obj: streamer, bus_task });
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
    let mut guard = shared.lock().unwrap();
    let room = guard.as_mut().ok_or("você não está em uma sala")?;
    let peer = room.peers.iter().find(|p| p.id == peer_id).cloned().ok_or("essa pessoa saiu da sala")?;
    if !peer.streaming {
        return Err(format!("{} não está transmitindo", peer.name));
    }
    if let Some(old) = room.viewer.take() {
        old.bus_task.abort();
        old.obj.stop();
    }

    let viewer = Arc::new(Viewer::new(peer.clone(), room.sig.clone(), &room.ice, false).map_err(|e| format!("{e:#}"))?);
    viewer.start().map_err(|e| format!("{e:#}"))?;

    let bus_task = {
        let (app, shared, gen) = (app.clone(), shared.clone(), room.generation);
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

    let status = WatchStatus {
        active: true,
        streamer_id: Some(peer.id),
        streamer_name: Some(peer.name),
        ..Default::default()
    };
    room.viewer = Some(Active { obj: viewer, bus_task });
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

    let ice = ice::fetch(&server).await;
    let (sig, mut events) = server.join(&code, &name).await.map_err(|e| format!("{e:#}"))?;
    let (me, peers) = match tokio::time::timeout(Duration::from_secs(10), events.recv()).await {
        Ok(Some(ServerMsg::Welcome { id, peers })) => (id, peers),
        _ => return Err("o servidor não respondeu".into()),
    };

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
    *shared.lock().unwrap() = Some(room);

    let task = tauri::async_runtime::spawn(room_loop(app, shared.clone(), generation, events));
    if let Some(r) = shared.lock().unwrap().as_mut().filter(|r| r.generation == generation) {
        r.loop_task = Some(task);
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
                    let viewers = s.tick();
                    if ticks % 15 == 0 {
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
                match viewer {
                    Some(v) => {
                        let frames = v.stats().video_frames;
                        let fps = frames.saturating_sub(last_frames) as f64 / last_at.elapsed().as_secs_f64();
                        (last_frames, last_at) = (frames, Instant::now());
                        let _ = app.emit("watch-status", WatchStatus {
                            active: true,
                            streamer_id: Some(v.streamer().id.clone()),
                            streamer_name: Some(v.streamer().name.clone()),
                            fps: fps.round(),
                            relayed: v.is_relayed() == Some(true),
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
                room.peers.push(peer.clone());
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
    if let Some(s) = active {
        s.obj.stop();
        s.bus_task.abort();
    }
    let _ = app.emit("stream-status", StreamStatus { error, ..Default::default() });
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
        v.obj.stop();
        v.bus_task.abort();
        let _ = app.emit("watch-status", WatchStatus { ended: Some(reason), ..Default::default() });
    }
}
