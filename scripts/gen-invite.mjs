// Generates a single-line server invite string (`csl1:...`) that friends can
// paste directly into Crimson ScreenLive to configure both Server URL and Key.
//
// Usage:
//   node scripts/gen-invite.mjs https://your-worker.workers.dev [key]
//
// If [key] is omitted, reads `.app-key` in the repo root.
import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const url = (process.argv[2] ?? "").trim().replace(/\/+$/, "");
if (!/^https?:\/\/.+/.test(url)) {
  console.error("Usage: node scripts/gen-invite.mjs <https://your-worker.workers.dev> [key]");
  process.exit(1);
}

const keyFile = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", ".app-key");
const key = (process.argv[3] ?? (existsSync(keyFile) ? readFileSync(keyFile, "utf8") : "")).trim();

const payload = key ? { s: url, k: key } : { s: url };
const token = "csl1:" + Buffer.from(JSON.stringify(payload), "utf8").toString("base64url");
console.log(token);
