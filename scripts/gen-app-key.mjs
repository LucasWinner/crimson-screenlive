// Creates the app access key (if missing) and prints its SHA-256.
//
// The key is compiled into the app/CLI (see core/build.rs) and sent to the
// signaling server, which only stores the hash (APP_KEY_SHA256 in
// signaling/wrangler.toml). `.app-key` is gitignored — keep a backup of it:
// builds made with a different key can't use your server.
//
//   node scripts/gen-app-key.mjs           # create if missing, print hash
//   node scripts/gen-app-key.mjs --rotate  # replace with a new key
import { createHash, randomBytes } from "node:crypto";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const file = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", ".app-key");
const rotate = process.argv.includes("--rotate");

if (rotate || !existsSync(file)) {
  writeFileSync(file, randomBytes(32).toString("base64url") + "\n", { mode: 0o600 });
  console.log(rotate ? "rotated .app-key" : "created .app-key");
}
const key = readFileSync(file, "utf8").trim();
console.log("APP_KEY_SHA256 =", createHash("sha256").update(key).digest("hex"));
