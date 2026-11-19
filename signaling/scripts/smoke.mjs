// Smoke test against a running signaling server (default: local `npm run dev`).
// Usage: node scripts/smoke.mjs [baseUrl]
import assert from "node:assert/strict";

const base = process.argv[2] ?? "http://127.0.0.1:8787";
const wsBase = base.replace(/^http/, "ws");

function connect(code, name) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(`${wsBase}/rooms/${code}/ws?name=${encodeURIComponent(name)}`);
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

const { code } = await (await fetch(`${base}/rooms`, { method: "POST" })).json();
assert.match(code, /^[A-Z2-9]{6}$/);

const ice = await (await fetch(`${base}/ice`)).json();
assert.ok(Array.isArray(ice.iceServers) && ice.iceServers.length > 0);

const alice = await connect(code, "Alice");
const aWelcome = await alice.next("welcome");
assert.equal(aWelcome.peers.length, 0);

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
const ice2 = await (await fetch(`${base}/ice`)).json();
assert.equal(typeof ice2.turnCapped, "boolean");
if (process.env.EXPECT_CAPPED) {
  assert.equal(ice2.turnCapped, true, "cap should be reached");
  assert.ok(!JSON.stringify(ice2.iceServers).includes("turn:"), "no TURN when capped");
}

bob.ws.close();
const left = await alice.next("peer-left");
assert.equal(left.id, bWelcome.id);
alice.ws.close();

console.log(`smoke OK (room ${code})`);
