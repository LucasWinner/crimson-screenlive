import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type Update } from "@tauri-apps/plugin-updater";

// ---- types mirrored from src-tauri/src/room.rs and settings.rs
interface Peer { id: string; name: string; streaming: boolean }
interface RoomView { code: string; me: string; name: string; peers: Peer[] }
interface Monitor { id: number; name: string; width: number; height: number; primary: boolean }
interface WindowItem { hwnd: number; title: string; exe: string }
interface SystemInfo {
  monitors: Monitor[];
  windows: WindowItem[];
  encoder: string | null;
  hardware: boolean;
  av1Encoder: string | null;
  h264Encoder: string | null;
  h264Hardware: boolean;
}
interface StreamStatus {
  active: boolean;
  encoder: string | null;
  viewers: number;
  relayed: number;
  bitrateKbps: number;
  rttMs: number | null;
  lossPct: number | null;
  discordStartedLate: boolean;
  error: string | null;
}
interface WatchStatus {
  active: boolean;
  streamerId: string | null;
  streamerName: string | null;
  fps: number;
  relayed: boolean;
  rttMs: number | null;
  lossPct: number | null;
  ended: string | null;
}
interface Settings {
  nickname: string; serverUrl: string; serverKey: string; monitorId: number | null; quality: string;
  audioMode: "all-except" | "app" | "all" | "none"; audioExclude: string; audioApp: string;
  codec: "auto" | "av1" | "h264"; bitrateKbps: number | null;
  watchVolume: number; watchMuted: boolean;
}
interface AppInfo { pid: number; exe: string; title: string }
interface AudioSources { supported: boolean; discordRunning: boolean; apps: AppInfo[] }

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;

const ui = {
  welcome: $("welcome"),
  room: $("room"),
  serverSetup: $("server-setup"),
  inviteInput: $<HTMLInputElement>("invite-input"),
  applyInvite: $<HTMLButtonElement>("apply-invite"),
  setupManual: $("setup-manual"),
  nickname: $<HTMLInputElement>("nickname"),
  roomCode: $<HTMLInputElement>("room-code"),
  createRoom: $<HTMLButtonElement>("create-room"),
  joinForm: $<HTMLFormElement>("join-form"),
  codeText: $("code-text"),
  copyCode: $("copy-code"),
  leave: $("leave"),
  people: $("people"),
  monitor: $<HTMLSelectElement>("monitor"),
  refreshSources: $("refresh-sources"),
  resolution: $<HTMLSelectElement>("resolution"),
  fps: $<HTMLSelectElement>("fps"),
  bitrate: $<HTMLSelectElement>("bitrate"),
  qualityHint: $("quality-hint"),
  watchRelay: $("watch-relay"),
  watchMute: $<HTMLButtonElement>("watch-mute"),
  watchVolume: $<HTMLInputElement>("watch-volume"),
  watchVolumeLabel: $("watch-volume-label"),
  streamRelay: $("stream-relay"),
  streamRelayText: $("stream-relay-text"),
  streamDiscordWarn: $("stream-discord-warn"),
  audioSummary: $("audio-summary"),
  changeAudio: $("change-audio"),
  audioExclude: $<HTMLSelectElement>("audio-exclude"),
  audioApp: $<HTMLSelectElement>("audio-app"),
  audioHelp: $("audio-help"),
  codecHelp: $("codec-help"),
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
  updateBanner: $("update-banner"),
  updateTitle: $("update-title"),
  updateSubtitle: $("update-subtitle"),
  installUpdate: $<HTMLButtonElement>("install-update"),
  checkUpdate: $<HTMLButtonElement>("check-update"),
  appVersion: $("app-version"),
  settingsDialog: $<HTMLDialogElement>("settings"),
  settingsForm: document.querySelector<HTMLFormElement>("#settings form")!,
  settingsError: $("settings-error"),
  settingsOk: $("settings-ok"),
  openSettings: $("open-settings"),
  serverUrl: $<HTMLInputElement>("server-url"),
  serverKey: $<HTMLInputElement>("server-key"),
  toggleKey: $("toggle-key"),
  pasteInvite: $("paste-invite"),
  copyInvite: $("copy-invite"),
  toast: $("toast"),
};

let settings: Settings = {
  nickname: "",
  serverUrl: "",
  serverKey: "",
  monitorId: null,
  quality: "1080p60",
  audioMode: "all-except",
  audioExclude: "Discord.exe",
  audioApp: "",
  codec: "auto",
  bitrateKbps: null,
  watchVolume: 1.0,
  watchMuted: false,
};
let sysInfo: SystemInfo | null = null;
let pendingUpdate: Update | null = null;
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
  if (/invalid app key|chave de acesso/i.test(s))
    return "Chave de acesso inválida ou ausente para este servidor. Cole o convite csl1:... abaixo.";
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
    renderStream({
      active: false,
      encoder: null,
      viewers: 0,
      relayed: 0,
      bitrateKbps: 0,
      rttMs: null,
      lossPct: null,
      discordStartedLate: false,
      error: null,
    });
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

function formatHealth(rttMs: number | null, lossPct: number | null): string {
  const parts: string[] = [];
  if (rttMs !== null && rttMs > 0) parts.push(`${Math.round(rttMs)} ms`);
  if (lossPct !== null && lossPct >= 1) parts.push(`${lossPct.toFixed(0)}% perda`);
  return parts.join(" · ");
}

function renderStream(s: StreamStatus) {
  const was = streaming;
  streaming = s.active;
  ui.streamForm.hidden = s.active;
  ui.streamLive.hidden = !s.active;
  ui.streamRelay.hidden = !(s.active && s.relayed > 0);
  ui.streamDiscordWarn.hidden = !(s.active && s.discordStartedLate);
  ui.toggleStream.textContent = s.active ? "Parar transmissão" : "Transmitir";
  ui.toggleStream.classList.toggle("primary", !s.active);
  ui.toggleStream.classList.toggle("danger", s.active);
  if (s.active) {
    const v = s.viewers === 1 ? "1 espectador" : `${s.viewers} espectadores`;
    const mbps = s.bitrateKbps > 0 ? ` · ${(s.bitrateKbps / 1000).toFixed(1).replace(".", ",")} Mbps` : "";
    const health = s.viewers > 0 ? formatHealth(s.rttMs, s.lossPct) : "";
    ui.streamStats.textContent = `${v} · ${s.encoder ?? ""}${mbps}${health ? ` · ${health}` : ""}`;
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
    if (w.fps > 0) {
      const health = formatHealth(w.rttMs, w.lossPct);
      const path = w.relayed ? "" : " · conexão direta";
      ui.watchStats.textContent = `${w.fps} fps${health ? ` · ${health}` : ""}${path}`;
    } else {
      ui.watchStats.textContent = "Conectando…";
    }
  }
  if (w.ended && w.ended !== "closed") {
    toast(w.ended === "ended" ? "A transmissão terminou." : `Erro ao assistir: ${friendly(w.ended)}`);
  }
  if (before !== watchingId) renderPeople();
}

// ---- actions

function encodeInvite(url: string, key: string): string {
  const payload = key ? { s: url, k: key } : { s: url };
  const bytes = new TextEncoder().encode(JSON.stringify(payload));
  let bin = "";
  for (const b of bytes) bin += String.fromCharCode(b);
  return "csl1:" + btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function parseInvite(raw: string): { serverUrl: string; serverKey: string } | null {
  const trimmed = raw.trim();
  if (!trimmed) return null;
  if (/^https?:\/\/.+/i.test(trimmed)) {
    return { serverUrl: trimmed.replace(/\/+$/, ""), serverKey: "" };
  }
  const body = trimmed.startsWith("csl1:") ? trimmed.slice(5) : trimmed;
  try {
    const b64 = body.replace(/-/g, "+").replace(/_/g, "/") + "===".slice((body.length + 3) % 4);
    const bin = atob(b64);
    const bytes = Uint8Array.from(bin, (c) => c.charCodeAt(0));
    const parsed = JSON.parse(new TextDecoder().decode(bytes)) as { s?: unknown; k?: unknown };
    if (typeof parsed.s !== "string" || !/^https?:\/\/.+/.test(parsed.s.trim())) return null;
    return {
      serverUrl: parsed.s.trim().replace(/\/+$/, ""),
      serverKey: typeof parsed.k === "string" ? parsed.k.trim() : "",
    };
  } catch {
    return null;
  }
}

function renderServerSetup() {
  ui.serverSetup.hidden = Boolean(settings.serverUrl);
}

async function enterRoom(action: "create" | "join") {
  await ready;
  if (!settings.serverUrl) {
    renderServerSetup();
    ui.inviteInput.focus();
    toast("Cole o convite do servidor ou configure em ⚙️ Configurações.");
    return;
  }
  const name = ui.nickname.value.trim();
  if (!name) {
    ui.nickname.focus();
    toast("Escolha um apelido primeiro.");
    return;
  }
  settings.nickname = name;
  await persist();
  const btn = action === "create" ? ui.createRoom : (ui.joinForm.querySelector("button") as HTMLButtonElement);
  if (busy) return;
  busy = true;
  btn.disabled = true;
  try {
    const view = await (action === "create"
      ? invoke<RoomView>("create_room", { name })
      : invoke<RoomView>("join_room", { code: ui.roomCode.value, name }));
    showRoom(view);
  } catch (e) {
    const msg = String(e);
    if (/invalid app key|chave de acesso/i.test(msg)) {
      ui.serverSetup.hidden = false;
      ui.inviteInput.focus();
    }
    toast(friendly(e));
  } finally {
    busy = false;
    btn.disabled = false;
  }
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
  const selected = ui.monitor.value;
  let monitorId: number | null = null;
  let windowHwnd: number | null = null;
  if (selected.startsWith("window:")) {
    windowHwnd = Number(selected.slice("window:".length));
  } else if (selected.startsWith("monitor:")) {
    monitorId = Number(selected.slice("monitor:".length));
  }
  if (monitorId !== null) settings.monitorId = monitorId;
  settings.quality = `${ui.resolution.value}p${ui.fps.value}`;
  settings.bitrateKbps = ui.bitrate.value === "auto" ? null : Number(ui.bitrate.value);
  await persist(); // the backend reads the audio and codec choice from saved settings
  const s = await run(ui.toggleStream, () =>
    invoke<StreamStatus>("start_stream", {
      monitorId,
      windowHwnd,
      qualityPreset: settings.quality,
      bitrateKbps: settings.bitrateKbps,
    }),
  );
  if (s) renderStream(s);
}

// ---- quality

function activeCodec(): "av1" | "h264" {
  if (settings.codec === "h264") return "h264";
  if (settings.codec === "av1") return "av1";
  return sysInfo?.av1Encoder ? "av1" : "h264";
}

/** Mirrors quality() in src-tauri/src/room.rs. */
function videoKbps(res: string, fps: string, codec: "av1" | "h264") {
  if (ui.bitrate.value !== "auto") return Number(ui.bitrate.value);
  const at60 =
    res === "720" ? (codec === "av1" ? 3800 : 4500)
    : res === "1440" ? 12000
    : codec === "av1" ? 6000 : 7000;
  return fps === "30" ? Math.round((at60 * 2) / 3) : at60;
}

function renderQualityHint() {
  const codec = activeCodec();
  const mbps = (videoKbps(ui.resolution.value, ui.fps.value, codec) + 128) / 1000;
  const tip = ui.fps.value === "30" ? "bom para vídeos e filmes" : "melhor para jogos";
  ui.qualityHint.textContent = `~${mbps.toFixed(1).replace(".", ",")} Mbps de upload por espectador · ${codec.toUpperCase()} · ${tip}`;
}
function renderEncoderInfo() {
  if (!sysInfo) return;
  const codec = activeCodec();
  const enc = codec === "av1" ? sysInfo.av1Encoder : sysInfo.h264Encoder;
  const hw = codec === "av1" ? Boolean(sysInfo.av1Encoder) : sysInfo.h264Hardware;
  ui.encoderInfo.textContent = enc
    ? `Codificação: ${enc}${hw ? " (placa de vídeo, não pesa no jogo)" : " (processador)"}`
    : "Nenhum codificador de vídeo encontrado.";
}
ui.resolution.onchange = renderQualityHint;
ui.fps.onchange = renderQualityHint;
ui.bitrate.onchange = renderQualityHint;

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
function renderWatchVolume() {
  const pct = Math.round(settings.watchVolume * 100);
  ui.watchVolume.value = String(pct);
  ui.watchVolumeLabel.textContent = settings.watchMuted ? "Mudo" : `${pct}%`;
  ui.watchMute.textContent = settings.watchMuted ? "Ativar som" : "Mudo";
}

let volTimer: number | undefined;
function pushWatchVolume(immediate = false) {
  renderWatchVolume();
  clearTimeout(volTimer);
  const send = () =>
    void invoke("set_watch_volume", { volume: settings.watchVolume, muted: settings.watchMuted }).catch((e) =>
      console.warn("set_watch_volume failed", e),
    );
  if (immediate) send();
  else volTimer = window.setTimeout(send, 120);
}

ui.watchVolume.oninput = () => {
  settings.watchVolume = Number(ui.watchVolume.value) / 100;
  if (settings.watchVolume > 0 && settings.watchMuted) settings.watchMuted = false;
  pushWatchVolume(false);
};
ui.watchVolume.onchange = () => pushWatchVolume(true);
ui.watchMute.onclick = () => {
  settings.watchMuted = !settings.watchMuted;
  pushWatchVolume(true);
};

async function refreshCaptureSources() {
  sysInfo = await invoke<SystemInfo>("system_info");
  const prev = ui.monitor.value;
  const groups: HTMLOptGroupElement[] = [];

  const monGroup = document.createElement("optgroup");
  monGroup.label = "Telas inteiras";
  for (const [i, m] of sysInfo.monitors.entries()) {
    monGroup.append(
      option(`monitor:${m.id}`, `Tela ${i + 1}: ${m.name} (${m.width}×${m.height})${m.primary ? " · principal" : ""}`),
    );
  }
  groups.push(monGroup);

  if (sysInfo.windows.length > 0) {
    const winGroup = document.createElement("optgroup");
    winGroup.label = "Janelas";
    for (const w of sysInfo.windows) {
      const label = w.title.length > 48 ? `${w.title.slice(0, 47)}…` : w.title;
      winGroup.append(option(`window:${w.hwnd}`, `${label} (${w.exe.replace(/\.exe$/i, "")})`));
    }
    groups.push(winGroup);
  }

  ui.monitor.replaceChildren(...groups);
  if (prev && [...ui.monitor.options].some((o) => o.value === prev)) {
    ui.monitor.value = prev;
  } else if (settings.monitorId && sysInfo.monitors.some((m) => m.id === settings.monitorId)) {
    ui.monitor.value = `monitor:${settings.monitorId}`;
  }
  renderEncoderInfo();
  renderQualityHint();
}

async function checkForUpdate(manual = false) {
  try {
    const update = await check({ timeout: 10000 });
    if (!update) {
      if (manual) toast("Você já está na versão mais recente.", true);
      return;
    }
    pendingUpdate = update;
    ui.updateTitle.textContent = `Nova versão v${update.version} disponível`;
    ui.updateSubtitle.textContent = update.body?.split("\n")[0]?.trim() || "Clique para baixar e atualizar automaticamente.";
    ui.updateBanner.hidden = false;
    if (manual && ui.settingsDialog.open) ui.settingsDialog.close("cancel");
  } catch (e) {
    if (manual) toast(`Não foi possível verificar atualizações: ${friendly(e)}`);
  }
}

ui.installUpdate.onclick = async () => {
  if (!pendingUpdate) return;
  ui.installUpdate.disabled = true;
  let downloaded = 0;
  let total = 0;
  try {
    await pendingUpdate.downloadAndInstall((ev) => {
      if (ev.event === "Started") {
        total = ev.data.contentLength ?? 0;
        ui.updateSubtitle.textContent = "Baixando atualização…";
      } else if (ev.event === "Progress") {
        downloaded += ev.data.chunkLength;
        if (total > 0) {
          const pct = Math.min(100, Math.round((downloaded / total) * 100));
          ui.updateSubtitle.textContent = `Baixando… ${pct}%`;
        }
      } else if (ev.event === "Finished") {
        ui.updateSubtitle.textContent = "Instalando… o app será reiniciado.";
      }
    });
    await relaunch();
  } catch (e) {
    ui.installUpdate.disabled = false;
    toast(`Falha ao atualizar: ${friendly(e)}`);
  }
};

ui.applyInvite.onclick = async () => {
  await ready;
  const parsed = parseInvite(ui.inviteInput.value);
  if (!parsed) {
    ui.inviteInput.focus();
    toast("Convite inválido. Cole o código csl1:... ou um endereço http(s)://");
    return;
  }
  settings.serverUrl = parsed.serverUrl;
  settings.serverKey = parsed.serverKey;
  ui.inviteInput.value = "";
  renderServerSetup();
  await persist();
  toast("Servidor configurado!", true);
  (settings.nickname ? ui.roomCode : ui.nickname).focus();
};
ui.setupManual.onclick = () => void openSettings();

ui.refreshSources.onclick = () => void refreshCaptureSources().catch((e) => toast(friendly(e)));
ui.checkUpdate.onclick = () => void checkForUpdate(true);

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
  ui.settingsOk.hidden = true;
  ui.serverUrl.value = settings.serverUrl;
  ui.serverKey.value = settings.serverKey;
  ui.serverKey.type = "password";
  ui.toggleKey.textContent = "mostrar";
  const radio = document.querySelector<HTMLInputElement>(`input[name="audio-mode"][value="${settings.audioMode}"]`);
  if (radio) radio.checked = true;
  const av1Radio = document.querySelector<HTMLInputElement>('input[name="video-codec"][value="av1"]');
  if (av1Radio) av1Radio.disabled = !sysInfo?.av1Encoder;
  const codec = settings.codec === "av1" && !sysInfo?.av1Encoder ? "auto" : settings.codec;
  const codecRadio = document.querySelector<HTMLInputElement>(`input[name="video-codec"][value="${codec}"]`);
  if (codecRadio) codecRadio.checked = true;
  ui.codecHelp.textContent = sysInfo?.av1Encoder
    ? `Neste PC: ${sysInfo.av1Encoder} disponível. Use H.264 se algum amigo estiver numa versão antiga do app.`
    : "Neste PC não há codificador AV1 por hardware; o modo Automático usará H.264.";
  void loadAudioSources();
  ui.settingsDialog.showModal();
}
ui.openSettings.onclick = () => void openSettings();
ui.changeAudio.onclick = () => void openSettings();
ui.refreshApps.onclick = () => void loadAudioSources();
ui.toggleKey.onclick = () => {
  const show = ui.serverKey.type === "password";
  ui.serverKey.type = show ? "text" : "password";
  ui.toggleKey.textContent = show ? "ocultar" : "mostrar";
};
ui.pasteInvite.onclick = async () => {
  ui.settingsError.hidden = true;
  ui.settingsOk.hidden = true;
  try {
    const text = await navigator.clipboard.readText();
    const parsed = parseInvite(text);
    if (!parsed) {
      ui.settingsError.textContent = "A área de transferência não contém um convite válido (csl1:...).";
      ui.settingsError.hidden = false;
      return;
    }
    ui.serverUrl.value = parsed.serverUrl;
    if (parsed.serverKey) {
      ui.serverKey.value = parsed.serverKey;
      ui.settingsOk.textContent = "Convite aplicado (servidor + chave). Clique em Salvar para confirmar.";
      ui.settingsOk.hidden = false;
    } else {
      ui.settingsError.textContent = "Você colou apenas a URL (sem chave de acesso). Se o servidor exigir chave, copie o código csl1:... completo.";
      ui.settingsError.hidden = false;
    }
  } catch {
    ui.settingsError.textContent = "Não foi possível ler a área de transferência.";
    ui.settingsError.hidden = false;
  }
};
ui.copyInvite.onclick = async () => {
  const url = ui.serverUrl.value.trim().replace(/\/+$/, "");
  if (!/^https?:\/\/.+/.test(url)) {
    ui.settingsError.textContent = "Preencha um endereço de servidor válido antes de copiar o convite.";
    ui.settingsError.hidden = false;
    return;
  }
  ui.settingsError.hidden = true;
  const token = encodeInvite(url, ui.serverKey.value.trim());
  try {
    await navigator.clipboard.writeText(token);
    toast("Convite copiado! Mande no privado para seus amigos.", true);
  } catch {
    ui.settingsError.textContent = `Convite: ${token}`;
    ui.settingsError.hidden = false;
  }
};
document.querySelectorAll<HTMLInputElement>('input[name="audio-mode"]').forEach((r) => (r.onchange = syncAudioInputs));
ui.audioApp.onchange = () => selectMode("app");
ui.audioExclude.onchange = () => selectMode("all-except");
ui.settingsForm.addEventListener("submit", (e) => {
  const submitter = e.submitter as HTMLButtonElement | null;
  if (submitter && submitter.value !== "save") return;
  const url = ui.serverUrl.value.trim().replace(/\/+$/, "");
  if (url && !/^https?:\/\/.+/.test(url)) {
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
  const codec = (document.querySelector<HTMLInputElement>('input[name="video-codec"]:checked')?.value ??
    settings.codec) as Settings["codec"];
  ui.settingsError.hidden = true;
  settings.serverUrl = url;
  settings.serverKey = ui.serverKey.value.trim();
  settings.audioMode = mode;
  settings.codec = codec;
  if (ui.audioExclude.value) settings.audioExclude = ui.audioExclude.value;
  if (ui.audioApp.value) settings.audioApp = ui.audioApp.value;
  renderServerSetup();
  renderAudioSummary();
  renderEncoderInfo();
  renderQualityHint();
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
  if (settings.bitrateKbps && [...ui.bitrate.options].some((o) => Number(o.value) === settings.bitrateKbps)) {
    ui.bitrate.value = String(settings.bitrateKbps);
  } else {
    ui.bitrate.value = "auto";
  }
  renderServerSetup();
  renderQualityHint();
  renderAudioSummary();
  renderWatchVolume();
  ui.appVersion.textContent = `v${await getVersion()}`;

  await refreshCaptureSources();

  (!settings.serverUrl ? ui.inviteInput : settings.nickname ? ui.roomCode : ui.nickname).focus();
  void checkForUpdate(false);
}

const ready = init().catch((e) => toast(friendly(e)));
