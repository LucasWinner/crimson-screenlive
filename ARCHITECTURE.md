# Arquitetura — Crimson ScreenLive

Este documento descreve a arquitetura interna, os fluxos de mídia e sinalização, a janela de player integrada e as convenções de segurança do projeto.

## Visão geral dos componentes

```
┌─────────────────────────────────────────────────────────────────────────┐
│                         App Desktop (Tauri 2)                           │
│  app/src/main.ts + player.ts  ◄──IPC/Events──►  app/src-tauri/src/      │
│   ├── index.html (sala/config)                  ├── main.rs (tray/keys) │
│   └── player.html (barra do player)             ├── player_win.rs (HWND)│
│                                                 ├── room.rs (sessão)    │
│                                                 └── settings.rs (JSON)  │
└───────────────────────────────────────────────┬─────────────────────────┘
                                                │
                                                ▼
┌─────────────────────────────────────────────────────────────────────────┐
│                       screenlive-core (core/src/)                       │
│  capture.rs   encoder.rs   procs.rs   ice.rs   signaling.rs   webrtc.rs │
│  streamer.rs  viewer.rs    protocol.rs                                  │
└───────────────┬─────────────────────────────────────────┬───────────────┘
                │ WebSocket / HTTPS (x-screenlive-key)    │ WebRTC (RTP/SRTP)
                ▼                                         ▼
┌───────────────────────────────────┐     ┌───────────────────────────────┐
│  Sinalização (Cloudflare Worker)  │     │  P2P direto ou TURN Cloudflare│
│  signaling/src/index.ts           │     │  (usado apenas se P2P falhar) │
│   ├── Room (Durable Object)       │     └───────────────────────────────┘
│   └── Usage (Durable Object/mês)  │
└───────────────────────────────────┘
```

## Pipelines GStreamer

### Streamer (`core/src/streamer.rs`)

```
Vídeo (Windows D3D11 / Linux PipeWire):
  [d3d11screencapturesrc (monitor ou WGC janela) / pipewiresrc (portal)]
    ──► d3d11convert / videoconvert (letterbox + gamma/primaries remap)
    ──► raw_tee
          ├──► queue (leaky) ──► encoder principal (AV1 ou H.264) ──► parser ──► high.tee
          │                                                                          └──► [espectadores AV1]
          └──► [sob demanda] queue (leaky) ──► encoder H.264 paralelo ──► h264parse ──► compat.tee
                                                                                         └──► [espectadores só H.264]

Áudio (Windows WASAPI2 / Linux Pulse/PipeWire):
  [wasapi2src (sistema / exclui processo / inclui processo) — hot-swap via IDLE probe]
    ──► audioconvert ──► audioresample (48kHz stereo)
    ──► opusenc (128 kbps, 10 ms, inband-fec=true, packet-loss-percentage=10)
    ──► rtpopuspay ──► atee
          └── [por espectador] queue (leaky) ──► webrtcbin
```

- **Zero-copy na GPU:** quando o encoder suporta `memory:D3D11Memory` (`amfav1enc`, `amfh264enc`, `nvd3d11*`, `qsv*`, `mfh264enc`), captura, escala e codificação permanecem em VRAM.
- **Dual-codec simultâneo:** se o streamer estiver em AV1 e entrar um espectador que só anuncia `["h264"]`, um segundo encoder H.264 compatível é ligado no `raw_tee` apenas enquanto houver espectadores H.264.
- **Hot-swap de áudio:** se o Discord abrir após o início da transmissão no modo sem Discord, `Streamer::set_audio_source()` substitui a fonte de áudio com `PadProbeType::IDLE` sem interromper o vídeo.
- **Bitrate adaptativo:** no modo `Auto`, `Streamer::tick()` lê `remote-inbound-rtp`; com perda `> 5%` reduz o bitrate em `15%` (piso de 50%, mínimo `1500 kbps`) nos encoders ativos, e recupera após 5 segundos com perda `< 1%`.

### Viewer e Player Integrado (`core/src/viewer.rs`, `app/src-tauri/src/player_win.rs`)

```
webrtcbin ──► decodebin
                ├── vídeo ──► queue ──► autovideosink (GstVideoOverlay -> child HWND no Windows)
                └── áudio ──► queue ──► volume ──► audioconvert ──► audioresample ──► autoaudiosink
```

- No Windows, `PlayerSurface` cria uma janela filha Win32 (`WS_CHILD | WS_CLIPSIBLINGS`, `HWND_TOP`) dentro da janela Tauri `"player"` ocupando toda a área acima da barra inferior de `48 px` (`player.html`), permitindo controles nativos em WebView2 sem sobrepor ou causar flicker no vídeo DXGI.
- Decodificadores AV1: `d3d11av1dec` / `d3d12av1dec` (hardware) e `dav1ddec` (CPU). H.264: `d3d11h264dec` / `d3d12h264dec` e `openh264dec`.

## Protocolo de sinalização (`core/src/protocol.rs` ↔ `signaling/src/index.ts`)

1. `POST /rooms` (exige `x-screenlive-key`) → código de 6 caracteres (`ABCDEFGHJKLMNPQRSTUVWXYZ23456789`).
2. `GET /rooms/:code/ws?name=<nick>` (exige `x-screenlive-key`) → `Room` busca STUN/TURN antes de aceitar o WebSocket e envia `welcome` imediatamente antes de qualquer broadcast.
3. `GET /usage` (exige `x-screenlive-key`) → retorna consumo estimado do mês UTC (`{ month, usedGb, capGb, turnCapped }`).
4. `Payload::Watch { codecs }` → o espectador anuncia seus codecs suportados (`["av1", "h264"]`).
5. **Reconexão automática:** em caso de queda inesperada do WebSocket, o cliente tenta reconectar até 4 vezes (`1s → 2s → 4s → 8s`) mantendo o stream de captura ativo.

## Segurança e chaves

- **Build pública sem segredos:** o código e os instaladores gerados no GitHub Actions não possuem servidor padrão nem chave embutida. Cada host sobe seu Worker e compartilha um convite `csl1:<base64url>` contendo `{ s: url, k: key }`.
- **Chave do servidor (`APP_KEY_SHA256`):** armazenada como secret na Cloudflare (apenas SHA-256). Rotacionável via `node scripts/gen-app-key.mjs --rotate`.
- **Chave do Tauri Updater (`~/.tauri/crimson-screenlive.key`):** assinatura minisign/Ed25519 para atualizações automáticas seguras.
