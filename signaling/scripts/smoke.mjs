// Smoke test against a running signaling server (default: local `npm run dev`).
// Usage: node scripts/smoke.mjs [baseUrl]
import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";

// App key (see scripts/gen-app-key.mjs); servers with REQUIRE_APP_KEY="false" ignore it.
const keyFile = new URL("../../.app-key", import.meta.url);
const KEY = process.env.SCREENLIVE_APP_KEY ?? (existsSync(keyFile) ? readFileSync(keyFile, "utf8").trim() : "");
const auth = KEY ? { "x-screenlive-key": KEY } : {};

const base = process.argv[2] ?? "http://127.0.0.1:8787";
const wsBase = base.replace(/^http/, "ws");

function connect(code, name) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(`${wsBase}/rooms/${code}/ws?name=${encodeURIComponent(name)}`, { headers: auth });
    const inbox = [];
    const waiters = [];
    ws.onmessage = (e) => {
      const msg = JSON.parse(e.data);
      const i = waiters.findIndex((w) => w.type === msg.type);
      if (i >= 0) waiters.splice(i, 1)[0].resolve(msg);
      else inbox.push(msg);
    };
    ws.onerror = () => reject(new Error(`ws error for ${name}`));
    const next = (type, ms = 3000) => {
      const i = inbox.findIndex((m) => m.type === type);
      if (i >= 0) return Promise.resolve(inbox.splice(i, 1)[0]);
      return new Promise((res, rej) => {
        const t = setTimeout(() => rej(new Error(`${name}: timeout waiting for ${type}`)), ms);
        waiters.push({ type, resolve: (m) => { clearTimeout(t); res(m); } });
      });
    };
    const send = (obj) => ws.send(JSON.stringify(obj));
    ws.onopen = () => resolve({ ws, next, send });
  });
}

const health = await fetch(`${base}/health`);
assert.equal(await health.text(), "ok");

// Without the key, room creation must be refused.
const noKey = await fetch(`${base}/rooms`, { method: "POST" });
assert.equal(noKey.status, 401, "POST /rooms without key must be 401");

const { code } = await (await fetch(`${base}/rooms`, { method: "POST", headers: auth })).json();
assert.match(code, /^[A-Z2-9]{6}$/);

const ice = await (await fetch(`${base}/ice`)).json();
assert.ok(Array.isArray(ice.iceServers) && ice.iceServers.length > 0);
assert.ok(!JSON.stringify(ice).includes("turn:"), "public /ice must not expose TURN");

const alice = await connect(code, "Alice");
const aWelcome = await alice.next("welcome");
assert.equal(aWelcome.peers.length, 0);
assert.ok(Array.isArray(aWelcome.iceServers) && aWelcome.iceServers.length > 0, "welcome carries ICE");
assert.equal(typeof aWelcome.turnCapped, "boolean");

// Join using a lowercase, dashed version of the code: must land in the same room.
const fuzzy = `${code.slice(0, 3)}-${code.slice(3)}`.toLowerCase();
const bob = await connect(fuzzy, "Bob");
const bWelcome = await bob.next("welcome");
assert.deepEqual(bWelcome.peers.map((p) => p.name), ["Alice"]);
const joined = await alice.next("peer-joined");
assert.equal(joined.peer.name, "Bob");

alice.send({ type: "stream", streaming: true });
const upd = await bob.next("peer-updated");
assert.equal(upd.peer.streaming, true);

bob.send({ type: "signal", to: aWelcome.id, payload: { sdp: "offer" } });
const sig = await alice.next("signal");
assert.equal(sig.from, bWelcome.id);
assert.deepEqual(sig.payload, { sdp: "offer" });

bob.send({ type: "signal", to: "nobody", payload: {} });
assert.match((await bob.next("error")).message, /not in room/);

bob.send({ type: "ping" });
await bob.next("pong");

// TURN usage accounting (tiny amount so running against prod is harmless).
const before = await (await fetch(`${base}/usage`)).json();
bob.send({ type: "turn-usage", bytes: 1000 });
bob.send({ type: "turn-usage", bytes: -5 });
assert.match((await bob.next("error")).message, /invalid turn-usage/);
const after = await (await fetch(`${base}/usage`)).json();
assert.ok(after.usedGb >= before.usedGb && typeof after.capGb === "number", "usage endpoint");
if (process.env.EXPECT_CAPPED) {
  const carol = await connect(code, "Carol");
  const w = await carol.next("welcome");
  assert.equal(w.turnCapped, true, "cap should be reached");
  assert.ok(!JSON.stringify(w.iceServers).includes("turn:"), "no TURN when capped");
  carol.ws.close();
}
if (process.env.EXPECT_TURN) {
  assert.ok(JSON.stringify(aWelcome.iceServers).includes("turn:"), "members get TURN");
}

// A WebSocket without the key must be refused.
await new Promise((resolve, reject) => {
  const ws = new WebSocket(`${wsBase}/rooms/${code}/ws?name=Mallory`);
  ws.onopen = () => reject(new Error("WebSocket without key was accepted"));
  ws.onerror = () => resolve();
});

bob.ws.close();
const left = await alice.next("peer-left");
assert.equal(left.id, bWelcome.id);
alice.ws.close();

console.log(`smoke OK (room ${code})`);
