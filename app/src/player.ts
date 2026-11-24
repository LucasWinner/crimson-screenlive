import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

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

interface WatchVolumeState {
  volume: number;
  muted: boolean;
}

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const watchName = $("watch-name");
const watchStats = $("watch-stats");
const watchRelay = $("watch-relay");
const muteBtn = $<HTMLButtonElement>("mute-btn");
const volumeSlider = $<HTMLInputElement>("volume-slider");
const volumeLabel = $("volume-label");
const fullscreenBtn = $<HTMLButtonElement>("fullscreen-btn");
const stopBtn = $<HTMLButtonElement>("stop-btn");

let volume = 1.0;
let muted = false;

function formatHealth(rttMs: number | null, lossPct: number | null): string {
  const parts: string[] = [];
  if (rttMs !== null && rttMs > 0) parts.push(`${Math.round(rttMs)} ms`);
  if (lossPct !== null && lossPct >= 1) parts.push(`${lossPct.toFixed(0)}% perda`);
  return parts.join(" · ");
}

function renderWatch(w: WatchStatus) {
  if (!w.active) return;
  watchName.textContent = w.streamerName ?? "";
  watchRelay.hidden = !w.relayed;
  if (w.fps > 0) {
    const health = formatHealth(w.rttMs, w.lossPct);
    const path = w.relayed ? "" : " · conexão direta";
    watchStats.textContent = `${w.fps} fps${health ? ` · ${health}` : ""}${path}`;
  } else {
    watchStats.textContent = "Conectando…";
  }
}

function renderVolume() {
  const pct = Math.round(volume * 100);
  volumeSlider.value = String(pct);
  volumeLabel.textContent = muted ? "Mudo" : `${pct}%`;
  muteBtn.textContent = muted ? "Ativar som" : "Mudo";
}

let volTimer: number | undefined;
function pushVolume(immediate = false) {
  renderVolume();
  clearTimeout(volTimer);
  const send = () => void invoke("set_watch_volume", { volume, muted });
  if (immediate) send();
  else volTimer = window.setTimeout(send, 120);
}

volumeSlider.oninput = () => {
  volume = Number(volumeSlider.value) / 100;
  if (volume > 0 && muted) muted = false;
  pushVolume(false);
};
volumeSlider.onchange = () => pushVolume(true);
muteBtn.onclick = () => {
  muted = !muted;
  pushVolume(true);
};
fullscreenBtn.onclick = () => void invoke("toggle_fullscreen");
stopBtn.onclick = () => void invoke("stop_watch");

window.addEventListener("keydown", async (e) => {
  if (e.key === "F11" || (e.key === "Enter" && e.altKey)) {
    e.preventDefault();
    await invoke("toggle_fullscreen");
  } else if (e.key === "Escape") {
    const win = getCurrentWindow();
    if (await win.isFullscreen()) {
      await win.setFullscreen(false);
    }
  } else if (e.key.toLowerCase() === "m") {
    muted = !muted;
    pushVolume(true);
  }
});

void listen<WatchStatus>("watch-status", (e) => renderWatch(e.payload));
void listen<WatchVolumeState>("watch-volume-changed", (e) => {
  volume = e.payload.volume;
  muted = e.payload.muted;
  renderVolume();
});

async function init() {
  const [w, v] = await Promise.all([
    invoke<WatchStatus>("get_watch_state"),
    invoke<WatchVolumeState>("get_watch_volume"),
  ]);
  volume = v.volume;
  muted = v.muted;
  renderVolume();
  renderWatch(w);
}

void init();
