// Copies the GStreamer plugins the app needs, plus every DLL they (and the app
// exe) transitively depend on, into src-tauri/gstreamer/ for the installer.
// Windows only. Uses dumpbin from the VS Build Tools to read PE imports.
//
//   node scripts/bundle-gstreamer.mjs
import { execFileSync } from "node:child_process";
import { cpSync, existsSync, mkdirSync, readdirSync, rmSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

if (process.platform !== "win32") {
  console.log("bundle-gstreamer: not Windows, nothing to do (Linux uses system GStreamer).");
  process.exit(0);
}

const here = path.dirname(fileURLToPath(import.meta.url));
const appDir = path.resolve(here, "..");
const repo = path.resolve(appDir, "..");
const out = path.join(appDir, "src-tauri", "gstreamer");
const GST = process.env.GSTREAMER_1_0_ROOT_MSVC_X86_64 ?? "C:\\Program Files\\gstreamer\\1.0\\msvc_x86_64\\";
const gstBin = path.join(GST, "bin");
const gstPlugins = path.join(GST, "lib", "gstreamer-1.0");

// Keep in sync with what core/ builds in its pipelines.
const PLUGINS = [
  "coreelements", "typefindfunctions", "playback", "autodetect",
  // capture / GPU / encoders / decoders
  "d3d11", "d3d12", "amfcodec", "nvcodec", "qsv", "mediafoundation", "openh264", "dav1d",
  "videoconvertscale", "videoparsersbad", "videotestsrc",
  // audio
  "wasapi2", "audioconvert", "audioresample", "opus", "audiotestsrc",
  // webrtc
  "rtp", "rtpmanager", "rsrtp", "srtp", "dtls", "nice", "webrtc",
];

function dumpbin() {
  const vswhere = "C:\\Program Files (x86)\\Microsoft Visual Studio\\Installer\\vswhere.exe";
  const vs = execFileSync(vswhere, ["-latest", "-products", "*", "-property", "installationPath"]).toString().trim();
  const msvc = path.join(vs, "VC", "Tools", "MSVC");
  const ver = readdirSync(msvc).sort((a, b) => a.localeCompare(b, undefined, { numeric: true })).at(-1);
  return path.join(msvc, ver, "bin", "Hostx64", "x64", "dumpbin.exe");
}
const DUMPBIN = dumpbin();

function imports(file) {
  const txt = execFileSync(DUMPBIN, ["/nologo", "/dependents", file]).toString();
  return [...txt.matchAll(/^\s+(\S+\.dll)\s*$/gim)].map((m) => m[1]);
}

const exe = path.join(repo, "target", "release", "crimson-screenlive.exe");
const exeDebug = path.join(repo, "target", "debug", "crimson-screenlive.exe");
const roots = [existsSync(exe) ? exe : exeDebug];
if (!existsSync(roots[0])) {
  console.error("build the app first (cargo build -p crimson-screenlive)");
  process.exit(1);
}

rmSync(out, { recursive: true, force: true });
mkdirSync(path.join(out, "bin"), { recursive: true });
mkdirSync(path.join(out, "plugins"), { recursive: true });

for (const p of PLUGINS) {
  const src = path.join(gstPlugins, `gst${p}.dll`);
  if (!existsSync(src)) throw new Error(`missing plugin ${src}`);
  cpSync(src, path.join(out, "plugins", `gst${p}.dll`));
  roots.push(src);
}

// Breadth-first over imports; only DLLs that live in GStreamer's bin/ are
// bundled (system DLLs like KERNEL32/d3d11 come with Windows).
const available = new Map(readdirSync(gstBin).filter((f) => f.endsWith(".dll")).map((f) => [f.toLowerCase(), f]));
const bundled = new Set();
const queue = [...roots];
while (queue.length) {
  for (const dep of imports(queue.shift())) {
    const real = available.get(dep.toLowerCase());
    if (!real || bundled.has(real)) continue;
    bundled.add(real);
    const src = path.join(gstBin, real);
    cpSync(src, path.join(out, "bin", real));
    queue.push(src);
  }
}

const size = (dir) => readdirSync(dir).reduce((n, f) => n + statSync(path.join(dir, f)).size, 0);
const mb = (b) => (b / 1024 / 1024).toFixed(1);
console.log(`bundled ${PLUGINS.length} plugins (${mb(size(path.join(out, "plugins")))} MB) and ${bundled.size} DLLs (${mb(size(path.join(out, "bin")))} MB) into ${out}`);
