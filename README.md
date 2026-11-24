# Crimson ScreenLive

Transmissão de tela entre amigos — leve, gratuita e feita para gameplay.
Uma alternativa ao "Go Live" do Discord para quando ele não funciona ou limita a qualidade.

## Principais recursos

- **Não pesa no jogo:** captura e codificação acontecem direto na placa de vídeo (**AV1** ou **H.264** via AMD AMF, NVIDIA NVENC, Intel Quick Sync). Em 1080p60 consome cerca de ~0,2% de CPU.
- **Dual-codec simultâneo (`AV1` + `H.264`):** usa AV1 por hardware para quem suporta e, se entrar um espectador que só aceita H.264, sobe um encoder H.264 paralelo sob demanda sem prejudicar quem está no AV1.
- **Direto entre os PCs (P2P):** o vídeo vai de você para até **4 espectadores** sem passar por servidor de mídia, com baixa latência.
- **Tela inteira ou janela específica:** compartilhe um monitor completo ou apenas a janela do jogo (Windows Graphics Capture / Linux `xdg-desktop-portal`).
- **Áudio inteligente com Opus FEC:** por padrão transmite o áudio do PC **sem o Discord**, para seus amigos não ouvirem a própria voz de volta. Se o Discord for aberto depois que a transmissão começou, ele é excluído automaticamente ao vivo sem reiniciar a stream. Em ⚙️ Configurações também dá para escolher: PC inteiro, excluir outro programa, só um programa, ou sem áudio.
- **Player integrado:** janela própria com vídeo em aceleração de hardware e barra inferior com **mudo**, **volume (0–150%)**, **FPS/latência/perda**, **tela cheia** (`F11` / `Alt+Enter`) e botão **Parar**.
- **Resiliência de rede:** bitrate adaptativo no modo `Auto` (reduz temporariamente quando há perda de pacotes e recupera sozinho), reconexão automática se o Wi-Fi oscilar e medidor mensal da cota TURN direto nas Configurações.
- **Atalhos globais e bandeja:** `Ctrl+Shift+M` (silenciar espectador) e `Ctrl+Shift+S` (parar transmissão) configuráveis e ativos mesmo dentro de jogos em tela cheia. Fechar no `X` durante uma live minimiza para a bandeja do sistema.
- **Atualização automática assinada:** verifica novas versões no GitHub Releases e atualiza dentro do próprio app mantendo suas configurações.

---

## Baixar e usar (Windows 10/11)

1. Baixe o instalador em [Releases](../../releases/latest) (`CrimsonScreenLive_x.y.z_x64-setup.exe`) e execute.
   > O Windows pode exibir *"O Windows protegeu o computador"* porque o executável não possui certificado pago de Code Signing. Clique em **Mais informações → Executar assim mesmo**.
2. Na primeira abertura, cole o **convite do servidor (`csl1:...`)** enviado por quem hospeda a sala (ou configure seu próprio servidor em ⚙️ Configurações). Isso só precisa ser feito uma vez.
3. Escolha um apelido.
4. **Criar sala** → clique no código de 6 letras para copiar e envie para seus amigos; ou cole o código de alguém e clique **Entrar**.
5. Para transmitir: escolha **Tela ou janela**, resolução (`720p`/`1080p`/`1440p`), `30` ou `60 FPS` e bitrate (`Auto` ou manual) → clique **Transmitir**.
6. Para assistir: clique **Assistir** ao lado de quem está **AO VIVO**.

### Qualidade × internet

- No modo **Auto** com **AV1** por hardware: ~`3,8 Mbps` (`720p60`), ~`6 Mbps` (`1080p60`) e ~`12 Mbps` (`1440p60` para máxima nitidez).
- Em **H.264**: ~`4,5 Mbps` (`720p60`), ~`7 Mbps` (`1080p60`) e ~`12 Mbps` (`1440p60`).
- Em `30 FPS`, usa cerca de `2/3` desses valores (ótimo para vídeos e filmes).
- Cada espectador recebe um fluxo próprio em P2P, então multiplique o bitrate pelo número de espectadores ao estimar seu *upload*.

### Conexão via servidor (TURN)

Quando a conexão direta P2P falha (4G, CGNAT, redes restritas), o vídeo passa pelo relay **TURN** da Cloudflare mantendo a qualidade configurada. O app exibe o selo **VIA SERVIDOR** para quem transmite e para quem assiste, porque esse tráfego conta na cota mensal configurada no servidor.

---

## Tutorial completo: como criar seu próprio servidor gratuito (Cloudflare)

A build pública do GitHub **não vem ligada a nenhum servidor** — assim ninguém estranho consome a sua cota. Qualquer pessoa pode subir o servidor de sinalização gratuitamente na Cloudflare em ~5 minutos e mandar o convite (`csl1:...`) para os amigos.

### Pré-requisitos
- Conta gratuita na [Cloudflare](https://dash.cloudflare.com/)
- [Node.js 22+](https://nodejs.org/) instalado

### Passo 1 — Clonar o repositório e gerar a chave de acesso
No terminal (PowerShell ou Bash), na pasta do projeto:

```powershell
node scripts/gen-app-key.mjs
```

Esse comando cria o arquivo `.app-key` na raiz (ignorado pelo Git) e mostra na tela um código `APP_KEY_SHA256 = ...`. **Guarde o `.app-key` em segurança** — é a chave secreta que só você e seus amigos (via convite) vão usar.

### Passo 2 — Fazer login e publicar o Worker de sinalização
```powershell
cd signaling
npm install
npx wrangler login        # abre o navegador para autorizar sua conta Cloudflare
npx wrangler deploy       # cria e publica o Worker e os Durable Objects (Room e Usage)
```

Ao final do `wrangler deploy`, anote a URL exibida no terminal, algo como:
`https://crimson-screenlive-signaling.<seu-subdominio>.workers.dev`

> **Dica:** se o nome `crimson-screenlive-signaling` já existir na sua conta ou você quiser personalizar a URL, altere `name = "..."` na primeira linha de `signaling/wrangler.toml` antes de rodar `npx wrangler deploy`.

### Passo 3 — Salvar o hash da chave no Worker
Ainda dentro de `signaling/`, configure o secret `APP_KEY_SHA256` colando o hash hexadecimal exibido no Passo 1:

```powershell
npx wrangler secret put APP_KEY_SHA256
```

*(O servidor nunca armazena a sua `.app-key`, apenas o SHA-256 dela.)*

### Passo 4 — Ativar TURN gratuito da Cloudflare *(opcional, recomendado para 4G/CGNAT)*
Sem TURN, o servidor usa STUN público e funciona na maioria das redes residenciais. Para garantir conexão mesmo em CGNAT/4G:

1. No painel da Cloudflare, acesse **Realtime → TURN Server** e crie uma chave TURN (a Cloudflare oferece **1.000 GB/mês gratuitos**).
2. Copie o **TURN Key ID** e o **API Token** e registre como secrets no Worker:
   ```powershell
   npx wrangler secret put TURN_KEY_ID
   npx wrangler secret put TURN_KEY_API_TOKEN
   ```
3. Por segurança, `signaling/wrangler.toml` define `TURN_MONTHLY_CAP_GB = "800"` (padrão). Ao atingir 800 GB no mês UTC, o servidor para de entregar credenciais TURN até o mês virar — você pode acompanhar o uso dentro do próprio app em **⚙️ Configurações**.

### Passo 5 — Gerar e enviar o convite para os amigos
Volte para a raiz do projeto e gere a string de convite de 1 linha (`csl1:...`):

```powershell
cd ..
node scripts/gen-invite.mjs https://seu-worker.workers.dev
```

Envie essa linha `csl1:...` **no privado** para seus amigos (ela contém o endereço do servidor + a chave de acesso).
- Quem abre o app pela primeira vez cola no quadro **Conectar a um servidor** → **Aplicar**.
- Quem já usa o app pode abrir **⚙️ Configurações → Colar convite → Salvar** (ou clicar em **Copiar convite** para repassar a outro amigo).

### Como trocar a chave se o convite vazar
```powershell
node scripts/gen-app-key.mjs --rotate   # gera nova chave em .app-key e mostra o novo hash
cd signaling
npx wrangler secret put APP_KEY_SHA256  # cole o novo hash
cd ..
node scripts/gen-invite.mjs https://seu-worker.workers.dev
```
Na mesma hora a chave antiga para de funcionar e só quem receber o novo `csl1:...` entra nas salas.

### Servidor local (para desenvolvimento/testes na rede local)
```powershell
cd signaling
npm install
npm run dev    # sobe em http://127.0.0.1:8787
```
No app, abra **⚙️ Configurações**, coloque `http://127.0.0.1:8787` (e sua chave, ou deixe `REQUIRE_APP_KEY = "false"` em `.dev.vars` local) e salve.

---

## Estrutura do projeto

```
├── core/                   # Biblioteca Rust (screenlive-core) + CLI (screenlive-cli)
│   └── src/
│       ├── capture.rs      # Captura D3D11 monitor/janela (Win), Portal+PipeWire (Linux) e Opus FEC
│       ├── encoder.rs      # Seleção/configuração AV1 & H.264 (AMF/NVENC/QSV/MF/VAAPI/x264/OpenH264)
│       ├── procs.rs        # Enumeração de processos/janelas e root PID (exclusão de áudio do Discord)
│       ├── streamer.rs     # Pipeline de transmissão, dual-codec paralelo, hot-swap áudio, bitrate adaptativo
│       ├── viewer.rs       # Pipeline de recepção, decodebin, controle de volume e VideoOverlay
│       ├── webrtc.rs       # Helpers webrtcbin, stats (bytes_sent, is_relayed, RTT, perda)
│       ├── ice.rs          # Conversão W3C ICE -> URI GStreamer
│       ├── protocol.rs     # Tipos JSON compartilhados com o servidor
│       └── signaling.rs    # Cliente WebSocket/HTTP com keepalive Pong e endpoint /usage
├── app/                    # Desktop app Tauri 2 (index.html + player.html)
│   ├── src/                # UI TypeScript (main.ts, player.ts, style.css)
│   ├── src-tauri/          # Backend Tauri (main.rs, room.rs, settings.rs, player_win.rs)
│   └── scripts/            # bundle-gstreamer.mjs e wrapper tauri.mjs
├── signaling/              # Cloudflare Worker + Durable Objects (Room, Usage)
├── scripts/                # gen-app-key.mjs, gen-invite.mjs, e2e.sh, cli.ps1
├── ARCHITECTURE.md         # Detalhes técnicos dos pipelines, protocolo e player
└── CHANGELOG.md            # Histórico de versões
```

---

## Desenvolvimento e testes

### Windows
Pré-requisitos: Rust (toolchain **MSVC**), Node 22+, Visual Studio Build Tools (C++), [GStreamer 1.28+ MSVC x86_64](https://gstreamer.freedesktop.org/download/) (instalação completa).

```powershell
rustup override set stable-x86_64-pc-windows-msvc
cd app; npm install; npm run dev        # roda o app em modo dev
cd app; npm run build                  # gera o instalador NSIS em target\release\bundle\nsis\
```

### Linux (Ubuntu/Debian/Wayland/X11)
Dependências de sistema:
```bash
sudo apt-get install -y \
  libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev patchelf libxdo-dev \
  libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev libgstreamer-plugins-bad1.0-dev \
  gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
  gstreamer1.0-libav gstreamer1.0-pipewire libx264-dev libssl-dev pkg-config
```
Captura de tela/janela no Linux usa `xdg-desktop-portal` (`ashpd`) + `pipewiresrc` e áudio usa `pulsesrc`.

### Comandos de verificação
```powershell
# Rust (unitários + clippy estrito)
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# Frontend Tauri
npm --prefix app run vite:build

# Servidor de sinalização
npm --prefix signaling run typecheck
npm --prefix signaling test
npm --prefix signaling run smoke -- https://seu-worker.workers.dev

# Ponta a ponta (com servidor local rodando)
bash scripts/e2e.sh
```

### CLI de desenvolvimento (`screenlive-cli`)
```powershell
.\scripts\cli.ps1 encoders
.\scripts\cli.ps1 monitors
.\scripts\cli.ps1 windows
.\scripts\cli.ps1 stream --server https://seu-worker.workers.dev --key SUA_CHAVE --name Voce --codec auto
.\scripts\cli.ps1 watch  --server https://seu-worker.workers.dev --key SUA_CHAVE --room ABCDEF --name Amigo
```

### Releases automáticas (GitHub Actions)
Configure em **Settings → Secrets and variables → Actions → Repository secrets**:
- `TAURI_SIGNING_PRIVATE_KEY`: chave privada gerada via `npm --prefix app run tauri -- signer generate`.
- `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`: senha da chave (se tiver).

Ao dar push em uma tag `vX.Y.Z` (`git tag v1.0.0 && git push origin refs/tags/v1.0.0`), o workflow `.github/workflows/release.yml` compila o instalador, assina para o updater, extrai as notas do `CHANGELOG.md`, gera `latest.json` e publica na aba **Releases**.

---

## Solução de problemas rápida

- **"Chave de acesso inválida ou ausente":** peça um novo convite `csl1:...` para quem hospeda a sala e cole via **⚙️ Configurações → Colar convite → Salvar**.
- **Erro de conexão / sala não abre:** confira se o servidor Cloudflare está no ar (`GET /health` retorna `ok`) e se o endereço começa com `https://`.
- **Áudio do Discord vazou na live:** verifique se em ⚙️ Configurações está selecionado *"PC inteiro, menos um programa → Discord"*. Se o Discord abrir depois que a live já começou, o app tenta excluir automaticamente; se falhar em alguma build do Windows, basta reiniciar a transmissão.
- **Atalho global não funcionou:** outro programa (ex.: GeForce Experience / Discord) já pode estar usando a mesma combinação — altere o atalho em ⚙️ Configurações.

---

## Licença

Copyright © 2026 LucasWinner. **Todos os direitos reservados** — veja [LICENSE](LICENSE).
O instalador inclui bibliotecas de terceiros com suas licenças próprias (GStreamer/LGPL, gst-plugins-rs/rsrtp/MPL-2.0, dav1d/BSD-2-Clause, OpenH264/BSD-2-Clause, Opus/BSD-3-Clause, libnice/LGPL-2.1/MPL-1.1, OpenSSL/Apache-2.0, Tauri/MIT/Apache-2.0).
