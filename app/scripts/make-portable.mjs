// Builds portable, no-install builds next to the installer:
//   target/release/bundle/portable/CrimsonScreenLive_<ver>_x64-portable.zip  (folder version)
//   target/release/bundle/portable/CrimsonScreenLive_<ver>_x64-portable.exe  (single file)
// Layout mirrors the installed app: exe + GStreamer DLLs side by side, plugins in
// gstreamer/plugins (main.rs detects that folder and uses it).
// Run after `tauri build` (npm run build does both).
import { execFileSync } from "node:child_process";
import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

if (process.platform !== "win32") {
  console.log("make-portable: Windows only");
  process.exit(0);
}

const appDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const repo = path.resolve(appDir, "..");
const { version } = JSON.parse(readFileSync(path.join(appDir, "src-tauri", "tauri.conf.json"), "utf8"));
const exe = path.join(repo, "target", "release", "crimson-screenlive.exe");
const gst = path.join(appDir, "src-tauri", "gstreamer");
if (!existsSync(exe) || !existsSync(gst)) {
  console.error("run `npm run build` first (needs the release exe and the bundled GStreamer)");
  process.exit(1);
}

const outRoot = path.join(repo, "target", "release", "bundle", "portable");
const name = `CrimsonScreenLive_${version}_x64-portable`;
const dir = path.join(outRoot, "Crimson ScreenLive");
rmSync(outRoot, { recursive: true, force: true });
mkdirSync(path.join(dir, "gstreamer"), { recursive: true });

cpSync(exe, path.join(dir, "Crimson ScreenLive.exe"));
for (const f of readdirSync(path.join(gst, "bin"))) cpSync(path.join(gst, "bin", f), path.join(dir, f));
cpSync(path.join(gst, "plugins"), path.join(dir, "gstreamer", "plugins"), { recursive: true });
cpSync(path.join(repo, "LICENSE"), path.join(dir, "LICENSE.txt"));
writeFileSync(
  path.join(dir, "LEIA-ME.txt"),
  [
    `Crimson ScreenLive ${version} — versão portátil (não precisa instalar)`,
    "",
    "1. Extraia esta pasta inteira para qualquer lugar (Área de Trabalho, Documentos, pendrive...).",
    "2. Abra \"Crimson ScreenLive.exe\".",
    "   Se o Windows mostrar \"O Windows protegeu o computador\", clique em",
    "   \"Mais informações\" -> \"Executar assim mesmo\".",
    "",
    "Não apague nem mova os outros arquivos da pasta: o app precisa deles.",
    "Para remover, é só apagar a pasta.",
    "",
    "Requer Windows 10 ou 11 (64 bits).",
    "",
  ].join("\r\n"),
);

const zip = path.join(outRoot, `${name}.zip`);
// Windows 10+ ships bsdtar, which writes standard ZIPs with -a. Use it by full
// path: Git for Windows puts a GNU tar on PATH that treats "D:" as a host.
const tar = path.join(process.env.SystemRoot ?? "C:\\Windows", "System32", "tar.exe");
execFileSync(tar, ["-a", "-c", "-f", path.basename(zip), "Crimson ScreenLive"], { cwd: outRoot });
const mb = (p) => (statSync(p).size / 1024 / 1024).toFixed(1);
console.log(`portable zip: ${zip} (${mb(zip)} MB)`);

// Single-file portable .exe: the launcher crate embeds the ZIP and unpacks it
// to %LOCALAPPDATA% on first run (see portable/src/main.rs).
execFileSync("cargo", ["build", "--release", "-p", "crimson-screenlive-portable"], {
  cwd: repo,
  stdio: "inherit",
  env: { ...process.env, SCREENLIVE_PORTABLE_PAYLOAD: zip },
});
const single = path.join(outRoot, `CrimsonScreenLive_${version}_x64-portable.exe`);
cpSync(path.join(repo, "target", "release", "CrimsonScreenLive.exe"), single);
console.log(`portable exe: ${single} (${mb(single)} MB)`);
