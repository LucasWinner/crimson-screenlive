# Arquitetura — Crimson ScreenLive

Este documento descreve a arquitetura interna, os fluxos de mídia e sinalização e as convenções de manutenção do projeto.

## Visão geral dos componentes

```
┌─────────────────────────────────────────────────────────────────────────┐
│                         App Desktop (Tauri 2)                           │
│  app/src/main.ts  ◄──IPC/Events──►  app/src-tauri/src/                  │
│   (UI TypeScript)                     ├── main.rs (tray, updater, init) │
│                                       ├── room.rs (sessão, stats loop)  │
│                                       └── settings.rs (JSON persistido) │
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
Vídeo:
  [d3d11screencapturesrc (monitor ou WGC janela)]
    ──► d3d11convert (letterbox + gamma/primaries remap, NV12 em VRAM)
    ──► raw_tee
          └──► queue (leaky) ──► encoder (AV1 ou H.264 hardware/CPU)
                 ──► parser (av1parse ou h264parse)
                 ──► high.tee
                       └── [por espectador, até 4]
                           queue (leaky) ──► rtpav1pay / rtph264pay ──► webrtcbin

Áudio:
  [wasapi2src (sistema / exclui processo / inclui processo)]
    ──► audioconvert ──► audioresample (48kHz stereo)
    ──► opusenc (128 kbps, 10 ms, inband-fec=true)
    ──► rtpopuspay ──► atee
          └── [por espectador] queue (leaky) ──► webrtcbin
```

- **Zero-copy na GPU:** quando o encoder suporta `memory:D3D11Memory` (`amfav1enc`, `amfh264enc`, `nvd3d11*`, `qsv*`, `mfh264enc`), os frames capturados e redimensionados não saem da VRAM até serem codificados.
- **Bitrate adaptativo:** no modo `Auto`, o `Streamer::tick()` lê `remote-inbound-rtp` a cada segundo. Se a maior perda de pacotes entre os espectadores passar de `5%`, reduz o bitrate em `15%` (piso de 50% do alvo, mínimo `1500 kbps`); após 5 segundos com perda `< 1%`, recupera gradualmente até o bitrate alvo.

### Viewer (`core/src/viewer.rs`)

```
webrtcbin ──► decodebin
                ├── vídeo ──► queue ──► autovideosink (d3d11/d3d12, fullscreen habilitado)
                └── áudio ──► queue ──► volume ──► audioconvert ──► audioresample ──► autoaudiosink
```

- Decodificadores suportados para AV1: `d3d11av1dec` / `d3d12av1dec` (hardware) e `dav1ddec` (CPU, empacotado no instalador).
- Decodificadores suportados para H.264: `d3d11h264dec` / `d3d12h264dec` (hardware) e `openh264dec` (CPU).

## Protocolo de sinalização (`core/src/protocol.rs` ↔ `signaling/src/index.ts`)

1. `POST /rooms` (exige `x-screenlive-key`) → gera código de 6 caracteres (`ABCDEFGHJKLMNPQRSTUVWXYZ23456789`).
2. `GET /rooms/:code/ws?name=<nick>` (exige `x-screenlive-key`) → o Durable Object `Room`:
   - Obtém STUN + credenciais TURN temporárias via API Cloudflare (se abaixo do teto mensal `TURN_MONTHLY_CAP_GB`).
   - Aceita o WebSocket e envia imediatamente `{ type: "welcome", id, peers, iceServers, turnCapped }`.
   - Notifica os demais membros com `{ type: "peer-joined", peer }`.
3. Mensagens em tempo real:
   - `stream`: anuncia `streaming: true/false` para a sala.
   - `signal`: encaminha `watch`, `offer`, `answer`, `ice` e `bye` entre pares.
   - `turn-usage`: reporta bytes enviados via TURN (com rate-limit de 5 s e teto de 250 MB por reporte) para o Durable Object `Usage`.
   - `ping` / `pong`: keepalive a cada 20 s; o cliente encerra a conexão se não receber resposta até o próximo ciclo.

## Segurança e chaves

- **Chave de acesso (`x-screenlive-key`):** a build pública do GitHub não embute servidor padrão nem chave; o usuário configura URL do servidor e chave via convite (`csl1:...`) ou em ⚙️ Configurações (`settings.json`). No servidor, apenas o SHA-256 fica armazenado em `APP_KEY_SHA256` (secret da Cloudflare). Para builds locais/CLI, `core/build.rs` ainda suporta `.app-key` ou `SCREENLIVE_APP_KEY`. Rotacionável via `node scripts/gen-app-key.mjs --rotate`.
- **Chave do Tauri Updater (`~/.tauri/crimson-screenlive.key`):** assinatura Ed25519/minisign dos instaladores para atualização automática segura. A chave pública fica em `app/src-tauri/tauri.conf.json` e a privada no secret `TAURI_SIGNING_PRIVATE_KEY` do GitHub Actions.
