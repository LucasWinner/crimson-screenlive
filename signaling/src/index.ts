/**
 * Crimson ScreenLive — signaling server.
 *
 * Routes:
 *   GET /health                         -> "ok"
 *   POST /rooms                         -> { code } (random room code)
 *   GET /ice                            -> { iceServers } (STUN, + TURN if configured)
 *   GET /rooms/:code/ws?name=<nick>     -> WebSocket upgrade into the room
 *
 * Room protocol (JSON text frames):
 *   server -> client
 *     { type: "welcome", id, peers: Peer[] }
 *     { type: "peer-joined", peer: Peer }
 *     { type: "peer-left", id }
 *     { type: "peer-updated", peer: Peer }
 *     { type: "signal", from, payload }
 *     { type: "error", message }
 *     { type: "pong" }
 *   client -> server
 *     { type: "signal", to, payload }      // SDP / ICE, relayed as-is
 *     { type: "stream", streaming: bool }  // announce start/stop of a stream
 *     { type: "ping" }
 */

export interface Env {
  ROOMS: DurableObjectNamespace;
  TURN_KEY_ID?: string;
  TURN_KEY_API_TOKEN?: string;
}

export interface Peer {
  id: string;
  name: string;
  streaming: boolean;
}

const MAX_PEERS = 8;
const MAX_MESSAGE_BYTES = 64 * 1024;
const CODE_ALPHABET = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789"; // no 0/O/1/I
const CODE_RE = /^[A-HJ-NP-Z2-9]{6}$/;
const PUBLIC_STUN = [{ urls: ["stun:stun.cloudflare.com:3478", "stun:stun.l.google.com:19302"] }];

const CORS = {
  "access-control-allow-origin": "*",
  "access-control-allow-methods": "GET, POST, OPTIONS",
  "access-control-allow-headers": "content-type",
};

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json", ...CORS },
  });
}

function randomString(len: number): string {
  const bytes = crypto.getRandomValues(new Uint8Array(len));
  let out = "";
  for (const b of bytes) out += CODE_ALPHABET[b % CODE_ALPHABET.length];
  return out;
}

/** Normalizes user-typed codes like "kx7-p2q" -> "KX7P2Q". */
export function normalizeCode(raw: string): string | null {
  const code = decodeURIComponent(raw).toUpperCase().replace(/[\s-]/g, "");
  return CODE_RE.test(code) ? code : null;
}

function cleanName(raw: string | null): string | null {
  const name = (raw ?? "").trim().replace(/\s+/g, " ").slice(0, 32);
  return name.length > 0 ? name : null;
}

async function iceServers(env: Env): Promise<unknown[]> {
  if (!env.TURN_KEY_ID || !env.TURN_KEY_API_TOKEN) return PUBLIC_STUN;
  const base = `https://rtc.live.cloudflare.com/v1/turn/keys/${env.TURN_KEY_ID.trim()}/credentials`;
  try {
    // Newer endpoint first, then the original one.
    for (const path of ["generate-ice-servers", "generate"]) {
      const res = await fetch(`${base}/${path}`, {
        method: "POST",
        headers: {
          authorization: `Bearer ${env.TURN_KEY_API_TOKEN.trim()}`,
          "content-type": "application/json",
        },
        body: JSON.stringify({ ttl: 86400 }),
      });
      if (!res.ok) {
        console.error(`TURN API ${path}: HTTP ${res.status}`);
        continue;
      }
      const data = (await res.json()) as { iceServers: unknown };
      const servers = Array.isArray(data.iceServers) ? data.iceServers : [data.iceServers];
      return [...PUBLIC_STUN, ...servers];
    }
    // Shape only (never the values) to diagnose swapped/mistyped secrets.
    const shape = (s: string) => `${s.trim().length} chars, ${/^[0-9a-f]+$/i.test(s.trim()) ? "hex" : "non-hex"}`;
    console.error(`TURN secrets: KEY_ID ${shape(env.TURN_KEY_ID)}; API_TOKEN ${shape(env.TURN_KEY_API_TOKEN)}`);
    throw new Error("all TURN endpoints failed");
  } catch (err) {
    console.error("TURN credentials failed, falling back to STUN:", err);
    return PUBLIC_STUN;
  }
}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    const parts = url.pathname.split("/").filter(Boolean);

    if (request.method === "OPTIONS") return new Response(null, { headers: CORS });

    if (url.pathname === "/health") return new Response("ok", { headers: CORS });

    if (url.pathname === "/rooms" && request.method === "POST") {
      return json({ code: randomString(6) });
    }

    if (url.pathname === "/ice" && request.method === "GET") {
      return json({ iceServers: await iceServers(env) });
    }

    if (parts.length === 3 && parts[0] === "rooms" && parts[2] === "ws") {
      if (request.headers.get("upgrade")?.toLowerCase() !== "websocket") {
        return json({ error: "expected websocket upgrade" }, 426);
      }
      const code = normalizeCode(parts[1]);
      if (!code) return json({ error: "invalid room code" }, 400);
      if (!cleanName(url.searchParams.get("name"))) return json({ error: "missing name" }, 400);
      const stub = env.ROOMS.get(env.ROOMS.idFromName(code));
      return stub.fetch(request);
    }

    return json({ error: "not found" }, 404);
  },
};

/**
 * One Durable Object per room. Uses the WebSocket Hibernation API so an idle
 * room costs nothing; per-socket state lives in the socket attachment.
 */
export class Room {
  constructor(
    private readonly ctx: DurableObjectState,
    _env: Env,
  ) {}

  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    const name = cleanName(url.searchParams.get("name"))!;

    const sockets = this.ctx.getWebSockets();
    if (sockets.length >= MAX_PEERS) return json({ error: "room is full" }, 409);

    const pair = new WebSocketPair();
    const [client, server] = [pair[0], pair[1]];
    const peer: Peer = { id: randomString(10), name, streaming: false };

    this.ctx.acceptWebSocket(server);
    server.serializeAttachment(peer);

    const others = sockets.map((ws) => ws.deserializeAttachment() as Peer);
    server.send(JSON.stringify({ type: "welcome", id: peer.id, peers: others }));
    this.broadcast({ type: "peer-joined", peer }, peer.id);

    return new Response(null, { status: 101, webSocket: client });
  }

  async webSocketMessage(ws: WebSocket, raw: string | ArrayBuffer): Promise<void> {
    const me = ws.deserializeAttachment() as Peer;
    const size = typeof raw === "string" ? raw.length : raw.byteLength;
    if (typeof raw !== "string" || size > MAX_MESSAGE_BYTES) {
      return this.sendError(ws, "message must be JSON text under 64KB");
    }

    let msg: { type?: string; to?: string; payload?: unknown; streaming?: unknown };
    try {
      msg = JSON.parse(raw);
    } catch {
      return this.sendError(ws, "invalid JSON");
    }

    switch (msg.type) {
      case "signal": {
        const target = this.findSocket(msg.to);
        if (!target) return this.sendError(ws, `peer ${msg.to} not in room`);
        target.send(JSON.stringify({ type: "signal", from: me.id, payload: msg.payload }));
        return;
      }
      case "stream": {
        me.streaming = msg.streaming === true;
        ws.serializeAttachment(me);
        this.broadcast({ type: "peer-updated", peer: me }, me.id);
        return;
      }
      case "ping":
        ws.send(JSON.stringify({ type: "pong" }));
        return;
      default:
        return this.sendError(ws, `unknown message type: ${msg.type}`);
    }
  }

  async webSocketClose(ws: WebSocket, code: number): Promise<void> {
    this.leave(ws);
    try {
      ws.close(code === 1005 ? 1000 : code, "bye");
    } catch {
      /* already closed */
    }
  }

  async webSocketError(ws: WebSocket): Promise<void> {
    this.leave(ws);
  }

  private leave(ws: WebSocket): void {
    const me = ws.deserializeAttachment() as Peer | null;
    if (me) this.broadcast({ type: "peer-left", id: me.id }, me.id);
  }

  private findSocket(id: unknown): WebSocket | undefined {
    if (typeof id !== "string") return undefined;
    return this.ctx.getWebSockets().find((ws) => (ws.deserializeAttachment() as Peer).id === id);
  }

  private broadcast(msg: unknown, exceptId: string): void {
    const data = JSON.stringify(msg);
    for (const ws of this.ctx.getWebSockets()) {
      if ((ws.deserializeAttachment() as Peer).id === exceptId) continue;
      try {
        ws.send(data);
      } catch {
        /* socket closing */
      }
    }
  }

  private sendError(ws: WebSocket, message: string): void {
    ws.send(JSON.stringify({ type: "error", message }));
  }
}
