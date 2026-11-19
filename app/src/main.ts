import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

// ---- types mirrored from src-tauri/src/room.rs and settings.rs
interface Peer { id: string; name: string; streaming: boolean }
interface RoomView { code: string; me: string; name: string; peers: Peer[] }
interface Monitor { id: number; name: string; width: number; height: number; primary: boolean }
interface SystemInfo { monitors: Monitor[]; encoder: string | null; hardware: boolean }
interface StreamStatus { active: boolean; encoder: string | null; viewers: number; relayed: number; error: string | null }
interface WatchStatus { active: boolean; streamerId: string | null; streamerName: string | null; fps: number; relayed: boolean; ended: string | null }
interface Settings { nickname: string; serverUrl: string; monitorId: number | null; quality: string; audio: boolean }

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;

const ui = {
  welcome: $("welcome"),
  room: $("room"),
  nickname: $<HTMLInputElement>("nickname"),
  roomCode: $<HTMLInputElement>("room-code"),
  createRoom: $<HTMLButtonElement>("create-room"),
  joinForm: $<HTMLFormElement>("join-form"),
  codeText: $("code-text"),
  copyCode: $("copy-code"),
  leave: $("leave"),
  people: $("people"),
  monitor: $<HTMLSelectElement>("monitor"),
  quality: $<HTMLSelectElement>("quality"),
  audio: $<HTMLInputElement>("audio"),
  streamForm: $("stream-form"),
  streamLive: $("stream-live"),
  streamStats: $("stream-stats"),
  toggleStream: $<HTMLButtonElement>("toggle-stream"),
  encoderInfo: $("encoder-info"),
  watching: $("watching"),
  watchName: $("watch-name"),
  watchStats: $("watch-stats"),
  stopWatch: $("stop-watch"),
  settingsDialog: $<HTMLDialogElement>("settings"),
  openSettings: $("open-settings"),
  serverUrl: $<HTMLInputElement>("server-url"),
  toast: $("toast"),
};

let settings: Settings;
let room: RoomView | null = null;
let streaming = false;
let watchingId: string | null = null;
let busy = false;

// ---- helpers

let toastTimer: number | undefined;
function toast(msg: string, ok = false) {
  ui.toast.textContent = msg;
  ui.toast.classList.toggle("ok", ok);
  ui.toast.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => (ui.toast.hidden = true), 4500);
}

/** Turns backend errors into something a friend can understand. */
function friendly(e: unknown): string {
  const s = String(e);
  if (/could not (reach|join)|error sending request|Connection refused|os error 10061|dns/i.test(s))
    return "Não foi possível conectar ao servidor. Verifique sua internet ou o endereço nas configurações.";
  if (/room is full|409/i.test(s)) return "A sala está cheia.";
  if (/no H\.264 encoder/i.test(s)) return "Nenhum codificador de vídeo encontrado neste PC.";
  return s.replace(/^Error: /, "");
}

function formatCode(code: string) {
  return code.length === 6 ? `${code.slice(0, 3)}-${code.slice(3)}` : code;
}

async function persist() {
  try {
    await invoke("save_settings", { settings });
  } catch (e) {
    console.warn("could not save settings", e);
  }
}

async function run<T>(btn: HTMLButtonElement | null, fn: () => Promise<T>): Promise<T | undefined> {
  if (busy) return;
  busy = true;
  if (btn) btn.disabled = true;
  try {
    return await fn();
  } catch (e) {
    toast(friendly(e));
  } finally {
    busy = false;
    if (btn) btn.disabled = false;
  }
}

// ---- rendering

function showRoom(view: RoomView | null) {
  room = view;
  ui.welcome.hidden = !!view;
  ui.room.hidden = !view;
  if (!view) {
    streaming = false;
    watchingId = null;
    renderStream({ active: false, encoder: null, viewers: 0, relayed: 0, error: null });
    ui.watching.hidden = true;
    return;
  }
  ui.codeText.textContent = formatCode(view.code);
  renderPeople();
}

function renderPeople() {
  if (!room) return;
  const me = room.me;
  const everyone: Peer[] = [{ id: me, name: room.name, streaming }, ...room.peers];
  ui.people.replaceChildren(
    ...everyone.map((p) => {
      const li = document.createElement("li");
      li.className = "person" + (p.streaming ? " live" : "");

      const avatar = document.createElement("div");
      avatar.className = "avatar";
      avatar.textContent = (p.name.trim()[0] ?? "?").toUpperCase();

      const info = document.createElement("div");
      info.className = "grow";
      const name = document.createElement("div");
      name.className = "name";
      name.textContent = p.name;
      if (p.id === me) {
        const you = document.createElement("span");
        you.className = "you";
        you.textContent = " (você)";
        name.append(you);
      }
      info.append(name);
      li.append(avatar, info);

      if (p.streaming) {
        const badge = document.createElement("span");
        badge.className = "badge live";
        badge.textContent = "AO VIVO";
        li.append(badge);
      }
      if (p.streaming && p.id !== me) {
        const btn = document.createElement("button");
        const isWatching = watchingId === p.id;
        btn.className = "btn small " + (isWatching ? "ghost" : "primary");
        btn.textContent = isWatching ? "Assistindo" : "Assistir";
        btn.disabled = isWatching;
        btn.onclick = () => startWatching(p, btn);
        li.append(btn);
      }
      return li;
    }),
  );
  if (room.peers.length === 0) {
    const li = document.createElement("li");
    li.className = "empty";
    li.textContent = "Ninguém mais por aqui ainda. Mande o código para seus amigos!";
    ui.people.append(li);
  }
}

function renderStream(s: StreamStatus) {
  const was = streaming;
  streaming = s.active;
  ui.streamForm.hidden = s.active;
  ui.streamLive.hidden = !s.active;
  ui.toggleStream.textContent = s.active ? "Parar transmissão" : "Transmitir";
  ui.toggleStream.classList.toggle("primary", !s.active);
  ui.toggleStream.classList.toggle("danger", s.active);
  if (s.active) {
    const v = s.viewers === 1 ? "1 espectador" : `${s.viewers} espectadores`;
    const relay = s.relayed > 0 ? ` (${s.relayed} via servidor, em 720p)` : "";
    ui.streamStats.textContent = `${v}${relay} · ${s.encoder ?? ""}`;
  }
  if (s.error) toast(`A transmissão parou: ${friendly(s.error)}`);
  if (was !== streaming) renderPeople();
}

function renderWatch(w: WatchStatus) {
  const before = watchingId;
  watchingId = w.active ? w.streamerId : null;
  ui.watching.hidden = !w.active;
  if (w.active) {
    ui.watchName.textContent = w.streamerName ?? "";
    const path = w.relayed ? " · via servidor (720p para economizar)" : " · conexão direta";
    ui.watchStats.textContent = w.fps > 0 ? `${w.fps} fps${path}` : "Conectando…";
  }
  if (w.ended && w.ended !== "closed") {
    toast(w.ended === "ended" ? "A transmissão terminou." : `Erro ao assistir: ${friendly(w.ended)}`);
  }
  if (before !== watchingId) renderPeople();
}

// ---- actions

async function enterRoom(action: "create" | "join") {
  const name = ui.nickname.value.trim();
  if (!name) {
    ui.nickname.focus();
    toast("Escolha um apelido primeiro.");
    return;
  }
  settings.nickname = name;
  void persist();
  const btn = action === "create" ? ui.createRoom : (ui.joinForm.querySelector("button") as HTMLButtonElement);
  const view = await run(btn, () =>
    action === "create"
      ? invoke<RoomView>("create_room", { name })
      : invoke<RoomView>("join_room", { code: ui.roomCode.value, name }),
  );
  if (view) showRoom(view);
}

async function startWatching(p: Peer, btn: HTMLButtonElement) {
  const w = await run(btn, () => invoke<WatchStatus>("watch", { peerId: p.id }));
  if (w) renderWatch(w);
}

async function toggleStream() {
  if (streaming) {
    await run(ui.toggleStream, () => invoke("stop_stream"));
    return;
  }
  settings.monitorId = ui.monitor.value ? Number(ui.monitor.value) : null;
  settings.quality = ui.quality.value;
  settings.audio = ui.audio.checked;
  void persist();
  const s = await run(ui.toggleStream, () =>
    invoke<StreamStatus>("start_stream", {
      monitorId: settings.monitorId,
      qualityPreset: settings.quality,
      audio: settings.audio,
    }),
  );
  if (s) renderStream(s);
}

// ---- wiring

ui.createRoom.onclick = () => enterRoom("create");
ui.joinForm.onsubmit = (e) => {
  e.preventDefault();
  enterRoom("join");
};
ui.roomCode.oninput = () => {
  const raw = ui.roomCode.value.toUpperCase().replace(/[^A-Z0-9]/g, "").slice(0, 6);
  ui.roomCode.value = raw.length > 3 ? `${raw.slice(0, 3)}-${raw.slice(3)}` : raw;
};
ui.copyCode.onclick = async () => {
  if (!room) return;
  try {
    await navigator.clipboard.writeText(formatCode(room.code));
    toast("Código copiado! Cole no Discord para seus amigos.", true);
  } catch {
    toast(`Código: ${formatCode(room.code)}`);
  }
};
ui.leave.onclick = async () => {
  await invoke("leave_room");
  showRoom(null);
};
ui.toggleStream.onclick = toggleStream;
ui.stopWatch.onclick = () => invoke("stop_watch");
$("fullscreen").onclick = () => invoke("toggle_fullscreen").catch((e) => toast(friendly(e)));
ui.openSettings.onclick = () => {
  ui.serverUrl.value = settings.serverUrl;
  ui.settingsDialog.showModal();
};
ui.settingsDialog.addEventListener("close", () => {
  if (ui.settingsDialog.returnValue !== "save") return;
  const url = ui.serverUrl.value.trim().replace(/\/+$/, "");
  if (!/^https?:\/\/.+/.test(url)) {
    toast("O endereço do servidor precisa começar com http:// ou https://");
    return;
  }
  settings.serverUrl = url;
  void persist();
  toast("Configurações salvas.", true);
});

void listen<RoomView>("room-updated", (e) => {
  if (room) {
    room = e.payload;
    renderPeople();
  }
});
void listen<{ reason: string }>("room-closed", (e) => {
  showRoom(null);
  toast(`Você saiu da sala: ${e.payload.reason}`);
});
void listen<StreamStatus>("stream-status", (e) => renderStream(e.payload));
void listen<WatchStatus>("watch-status", (e) => renderWatch(e.payload));

async function init() {
  settings = await invoke<Settings>("get_settings");
  ui.nickname.value = settings.nickname;
  ui.quality.value = settings.quality;
  ui.audio.checked = settings.audio;

  const info = await invoke<SystemInfo>("system_info");
  ui.monitor.replaceChildren(
    ...info.monitors.map((m, i) => {
      const o = document.createElement("option");
      o.value = String(m.id);
      o.textContent = `Tela ${i + 1}: ${m.name} (${m.width}×${m.height})${m.primary ? " · principal" : ""}`;
      return o;
    }),
  );
  if (settings.monitorId && info.monitors.some((m) => m.id === settings.monitorId)) {
    ui.monitor.value = String(settings.monitorId);
  }
  ui.encoderInfo.textContent = info.encoder
    ? `Codificação: ${info.encoder}${info.hardware ? " (placa de vídeo, não pesa no jogo)" : " (processador)"}`
    : "Nenhum codificador de vídeo encontrado.";

  (settings.nickname ? ui.roomCode : ui.nickname).focus();
}

init().catch((e) => toast(friendly(e)));
