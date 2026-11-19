// Runs the Tauri CLI with GStreamer's DLLs on PATH (Windows), so `npm run dev`
// works without touching the system PATH.
import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import path from "node:path";

const env = { ...process.env };
if (process.platform === "win32") {
  const gst =
    env.GSTREAMER_1_0_ROOT_MSVC_X86_64 ?? "C:\\Program Files\\gstreamer\\1.0\\msvc_x86_64\\";
  const bin = path.join(gst, "bin");
  if (!existsSync(bin)) {
    console.error(`GStreamer not found at ${bin}. Install the MSVC x86_64 build.`);
    process.exit(1);
  }
  const key = Object.keys(env).find((k) => k.toUpperCase() === "PATH") ?? "PATH";
  env[key] = `${bin};${env[key] ?? ""}`;
}

const child = spawn("npx", ["tauri", ...process.argv.slice(2)], {
  stdio: "inherit",
  env,
  shell: process.platform === "win32",
});
child.on("exit", (code) => process.exit(code ?? 1));
