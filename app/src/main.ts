import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

// ---- types mirrored from src-tauri/src/room.rs and settings.rs
interface Peer { id: string; name: string; streaming: boolean }
interface RoomView { code: string; me: string; name: string; peers: Peer[] }
interface Monitor { id: number; name: string; width: number; height: number; primary: boolean }
interface SystemInfo { monitors: Monitor[]; encoder: string | null; hardware: boolean }
interface StreamStatus { active: boolean; encoder: string | null; viewers: number; relayed: number; error: string | null }
interface WatchStatus { active: boolean; streamerId: string | null; streamerName: string | null; fps: number; relayed: boolean; ended: string | null }
interface Settings {
  nickname: string; serverUrl: string; monitorId: number | null; quality: string;
  audioMode: "all-except" | "app" | "all" | "none"; audioExclude: string; audioApp: string;
}
interface AppInfo { pid: number; exe: string; title: string }
interface AudioSources { supported: boolean; discordRunning: boolean; apps: AppInfo[] }

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
  resolution: $<HTMLSelectElement>("resolution"),
  fps: $<HTMLSelectElement>("fps"),
  qualityHint: $("quality-hint"),
  watchRelay: $("watch-relay"),
  streamRelay: $("stream-relay"),
  streamRelayText: $("stream-relay-text"),
  audioSummary: $("audio-summary"),
  changeAudio: $("change-audio"),
  audioExclude: $<HTMLSelectElement>("audio-exclude"),
  audioApp: $<HTMLSelectElement>("audio-app"),
  audioHelp: $("audio-help"),
  refreshApps: $("refresh-apps"),
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
  settingsForm: document.querySelector<HTMLFormElement>("#settings form")!,
  settingsError: $("settings-error"),
  openSettings: $("open-settings"),
  serverUrl: $<HTMLInputElement>("server-url"),
  toast: $("toast"),
};

let settings: Settings = {
  nickname: "",
  serverUrl: "",
  monitorId: null,
  quality: "1080p60",
  audioMode: "all-except",
  audioExclude: "Discord.exe",
  audioApp: "",
};
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
  if (/could not (reach|join)|timed out|error sending request|Connection refused|os error 10061|dns/i.test(s))
    return "Não foi possível conectar ao servidor. Verifique sua internet ou o endereço nas configurações.";
  if (/P2P connection failed/i.test(s))
    return "Não foi possível estabelecer conexão de vídeo com quem transmite (rede ou firewall bloqueou).";
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
  ui.streamRelay.hidden = !(s.active && s.relayed > 0);
  ui.toggleStream.textContent = s.active ? "Parar transmissão" : "Transmitir";
  ui.toggleStream.classList.toggle("primary", !s.active);
  ui.toggleStream.classList.toggle("danger", s.active);
  if (s.active) {
    const v = s.viewers === 1 ? "1 espectador" : `${s.viewers} espectadores`;
    ui.streamStats.textContent = `${v} · ${s.encoder ?? ""}`;
    ui.streamRelayText.textContent =
      s.relayed === 1 ? "1 espectador está recebendo pelo servidor (usa a cota do TURN)."
      : `${s.relayed} espectadores estão recebendo pelo servidor (usa a cota do TURN).`;
  }
  if (s.error) toast(`A transmissão parou: ${friendly(s.error)}`);
  if (was !== streaming) renderPeople();
}

function renderWatch(w: WatchStatus) {
  const before = watchingId;
  watchingId = w.active ? w.streamerId : null;
  ui.watching.hidden = !w.active;
  ui.watchRelay.hidden = !(w.active && w.relayed);
  if (w.active) {
    ui.watchName.textContent = w.streamerName ?? "";
    ui.watchStats.textContent = w.fps > 0 ? `${w.fps} fps${w.relayed ? "" : " · conexão direta"}` : "Conectando…";
  }
  if (w.ended && w.ended !== "closed") {
    toast(w.ended === "ended" ? "A transmissão terminou." : `Erro ao assistir: ${friendly(w.ended)}`);
  }
  if (before !== watchingId) renderPeople();
}

// ---- actions

async function enterRoom(action: "create" | "join") {
  await ready;
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
  settings.quality = `${ui.resolution.value}p${ui.fps.value}`;
  await persist(); // the backend reads the audio choice from saved settings
  const s = await run(ui.toggleStream, () =>
    invoke<StreamStatus>("start_stream", {
      monitorId: settings.monitorId,
      qualityPreset: settings.quality,
    }),
  );
  if (s) renderStream(s);
}

// ---- quality

/** Mirrors quality() in src-tauri/src/room.rs. */
function videoKbps(res: string, fps: string) {
  const at60 = res === "720" ? 4500 : res === "1440" ? 12000 : 7000;
  return fps === "30" ? Math.round((at60 * 2) / 3) : at60;
}

function renderQualityHint() {
  const mbps = (videoKbps(ui.resolution.value, ui.fps.value) + 128) / 1000;
  const tip = ui.fps.value === "30" ? "bom para vídeos e filmes" : "melhor para jogos";
  ui.qualityHint.textContent = `~${mbps.toFixed(1).replace(".", ",")} Mbps de upload por espectador · ${tip}`;
}
ui.resolution.onchange = renderQualityHint;
ui.fps.onchange = renderQualityHint;

// ---- audio source settings

let audioInfo: AudioSources | null = null;

function exeLabel(exe: string) {
  const app = audioInfo?.apps.find((a) => a.exe.toLowerCase() === exe.toLowerCase());
  if (/^discord/i.test(exe)) return "Discord (vozes da call)";
  return app ? app.title : exe.replace(/\.exe$/i, "");
}

function renderAudioSummary() {
  const m = settings.audioMode;
  ui.audioSummary.textContent =
    m === "none" ? "desligado"
    : m === "all" ? "PC inteiro"
    : m === "app" ? `só ${exeLabel(settings.audioApp)}`
    : `PC inteiro, menos ${/^discord/i.test(settings.audioExclude) ? "o Discord" : exeLabel(settings.audioExclude)}`;
}

function option(value: string, text: string) {
  const o = document.createElement("option");
  o.value = value;
  o.textContent = text;
  return o;
}

function selectMode(mode: Settings["audioMode"]) {
  const r = document.querySelector<HTMLInputElement>(`input[name="audio-mode"][value="${mode}"]`);
  if (r) r.checked = true;
  syncAudioInputs();
}

function syncAudioInputs() {
  const mode = document.querySelector<HTMLInputElement>('input[name="audio-mode"]:checked')?.value;
  ui.audioExclude.disabled = mode !== "all-except";
  ui.audioApp.disabled = mode !== "app";
}

async function loadAudioSources() {
  try {
    audioInfo = await invoke<AudioSources>("audio_sources");
  } catch (e) {
    toast(friendly(e));
    return;
  }
  const apps = audioInfo.apps.map((a) => option(a.exe, `${a.title}  (${a.exe})`));
  // "Exclude" always offers Discord first, even when it is closed right now.
  ui.audioExclude.replaceChildren(option("Discord.exe", "Discord (vozes da call)"), ...apps.map((o) => o.cloneNode(true) as HTMLOptionElement));
  ui.audioApp.replaceChildren(...apps);
  // Keep saved picks selectable even if that program is closed right now.
  for (const [sel, exe] of [[ui.audioExclude, settings.audioExclude], [ui.audioApp, settings.audioApp]] as const) {
    if (exe && ![...sel.options].some((o) => o.value.toLowerCase() === exe.toLowerCase())) {
      sel.append(option(exe, `${exe.replace(/\.exe$/i, "")} (fechado agora)`));
    }
    const match = [...sel.options].find((o) => o.value.toLowerCase() === exe.toLowerCase());
    if (match) sel.value = match.value;
  }
  if (!ui.audioApp.options.length) ui.audioApp.append(option("", "nenhum programa aberto"));
  const perApp = document.querySelectorAll<HTMLInputElement>('input[name="audio-mode"][value="all-except"], input[name="audio-mode"][value="app"]');
  perApp.forEach((r) => (r.disabled = !audioInfo!.supported));
  ui.audioHelp.textContent = audioInfo.supported
    ? "Tirar o Discord evita que seus amigos ouçam a própria voz de volta. Para jogos, \"Só um programa\" deixa só o som do jogo."
    : "Seu Windows não permite escolher programas (precisa do Windows 11 ou Windows 10 atualizado). Será transmitido o PC inteiro.";
  syncAudioInputs();
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
  await invoke("leave_room").catch((e) => toast(friendly(e)));
  showRoom(null);
};
ui.toggleStream.onclick = toggleStream;
ui.stopWatch.onclick = () => invoke("stop_watch").catch((e) => toast(friendly(e)));
$("fullscreen").onclick = () => invoke("toggle_fullscreen").catch((e) => toast(friendly(e)));
async function openSettings() {
  await ready;
  ui.settingsError.hidden = true;
  ui.serverUrl.value = settings.serverUrl;
  const radio = document.querySelector<HTMLInputElement>(`input[name="audio-mode"][value="${settings.audioMode}"]`);
  if (radio) radio.checked = true;
  void loadAudioSources();
  ui.settingsDialog.showModal();
}
ui.openSettings.onclick = () => void openSettings();
ui.changeAudio.onclick = () => void openSettings();
ui.refreshApps.onclick = () => void loadAudioSources();
document.querySelectorAll<HTMLInputElement>('input[name="audio-mode"]').forEach((r) => (r.onchange = syncAudioInputs));
ui.audioApp.onchange = () => selectMode("app");
ui.audioExclude.onchange = () => selectMode("all-except");
ui.settingsForm.addEventListener("submit", (e) => {
  const submitter = e.submitter as HTMLButtonElement | null;
  if (submitter && submitter.value !== "save") return;
  const url = ui.serverUrl.value.trim().replace(/\/+$/, "");
  if (!/^https?:\/\/.+/.test(url)) {
    e.preventDefault();
    ui.settingsError.textContent = "O endereço do servidor precisa começar com http:// ou https://";
    ui.settingsError.hidden = false;
    return;
  }
  const mode = (document.querySelector<HTMLInputElement>('input[name="audio-mode"]:checked')?.value ??
    settings.audioMode) as Settings["audioMode"];
  if (mode === "app" && !ui.audioApp.value) {
    e.preventDefault();
    ui.settingsError.textContent = "Escolha um programa para usar \"Só um programa\".";
    ui.settingsError.hidden = false;
    return;
  }
  ui.settingsError.hidden = true;
  settings.serverUrl = url;
  settings.audioMode = mode;
  if (ui.audioExclude.value) settings.audioExclude = ui.audioExclude.value;
  if (ui.audioApp.value) settings.audioApp = ui.audioApp.value;
  renderAudioSummary();
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
  const [res, fps] = (settings.quality || "1080p60").split("p");
  ui.resolution.value = ["720", "1080", "1440"].includes(res) ? res : "1080";
  ui.fps.value = fps === "30" ? "30" : "60";
  renderQualityHint();
  renderAudioSummary();

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

const ready = init().catch((e) => toast(friendly(e)));
