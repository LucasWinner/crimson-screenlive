# Crimson ScreenLive

Transmissão de tela entre amigos — leve, gratuita e feita para gameplay.
Uma alternativa ao "Go Live" do Discord para quando ele não funciona.

- **Não pesa no jogo:** captura e codificação acontecem na placa de vídeo (**AV1** ou **H.264** via AMD AMF, NVIDIA NVENC, Intel Quick Sync). Em teste, 1080p60 usou ~0,2% de CPU.
- **Direto entre os PCs (P2P):** o vídeo vai de você para seus amigos sem passar por servidor, com baixa latência. Até 4 espectadores por transmissão.
- **Simples:** crie uma sala, mande o código de 6 letras no Discord, pronto.
- **Tela inteira ou janela:** escolha transmitir um monitor completo ou apenas uma janela específica (ex.: só o jogo).
- **Áudio do PC/jogo** com proteção contra perda de pacotes (Opus FEC) — por padrão **sem o Discord**, para seus amigos não ouvirem a própria voz de volta. Em ⚙️ Configurações dá para escolher: PC inteiro, PC inteiro menos um programa, ou só um programa. Requer Windows 11 ou Windows 10 build 20348+; em versões antigas, transmite o PC inteiro.
- **Controle de volume e bandeja:** ajuste o volume/mudo de quem você assiste direto no app. Se fechar no `X` durante uma transmissão/visualização, o app minimiza para a bandeja do sistema para não derrubar a live.
- **Atualizações automáticas:** verifica novas versões no GitHub Releases e atualiza com um clique.

## Baixar e usar (Windows 10/11)

1. Baixe o instalador em [Releases](../../releases/latest) (`CrimsonScreenLive_x.y.z_x64-setup.exe`) e instale.
   > O Windows pode mostrar "O Windows protegeu o computador" porque o app não tem certificado pago de Code Signing. Clique em **Mais informações → Executar assim mesmo**.
2. Abra o **Crimson ScreenLive**, escolha um apelido.
3. **Criar sala** → clique no código para copiar e mande para os amigos. Ou cole o código de alguém e clique **Entrar**.
4. Para transmitir: escolha a tela ou janela, a resolução/FPS/bitrate e clique **Transmitir**.
5. Para assistir: clique **Assistir** ao lado de quem está **AO VIVO**. O vídeo abre numa janela separada (botão **Tela cheia** no app ou `Alt+Enter`).

**Qualidade × internet:** escolha resolução (`720p`/`1080p`/`1440p`), `30` ou `60 FPS` e bitrate (**Auto** ou manual de `2,5` a `20 Mbps`).
- No modo **Auto** com **AV1** por hardware: ~3,8 Mbps (`720p60`), ~6 Mbps (`1080p60`) e ~12 Mbps (`1440p60` para máxima nitidez). Se houver perda de pacotes na rede de algum amigo, o app reduz temporariamente o bitrate e recupera sozinho quando estabilizar.
- Em **H.264**: ~4,5 / 7 / 12 Mbps (`60 FPS`). Em `30 FPS`, usa cerca de 2/3 desses valores.
- Em ⚙️ Configurações é possível escolher o codec (**Automático**, **AV1** ou **H.264**).

**Conexão via servidor (TURN):** quando a conexão direta não é possível (4G, CGNAT, redes restritas), o vídeo passa pelo TURN da Cloudflare com a mesma qualidade da conexão direta. O app mostra **VIA SERVIDOR** para quem assiste e para quem transmite, porque esse tráfego conta na cota mensal do TURN.

## Como funciona

```
App (Tauri + Rust) ──captura GPU──► encoder AV1/H.264 por hardware ──► WebRTC ──P2P──► App do amigo (decodifica na GPU)
          │                                                                        ▲
          └──────── sinalização (salas, SDP/ICE) ── Cloudflare Worker ─────────────┘
                    TURN da Cloudflare só quando a conexão direta falha (4G/CGNAT)
```

| Pasta / Arquivo | O que é |
|---|---|
| `core/` | Biblioteca Rust: captura (tela/janela), escolha de encoder, pipelines GStreamer/WebRTC, cliente de sinalização. Inclui `screenlive-cli`. |
| `app/` | App desktop Tauri 2 (UI em TypeScript, backend em `app/src-tauri`). |
| `signaling/` | Servidor de salas: Cloudflare Worker + Durable Objects (plano gratuito). |
| `scripts/` | Teste ponta a ponta (`e2e.sh`), atalho da CLI (`cli.ps1`) e geração de `.app-key`. |
| `ARCHITECTURE.md` | Detalhes técnicos dos pipelines GStreamer, protocolo WebSocket e segurança. |
| `CHANGELOG.md` | Histórico de versões e alterações. |

## Desenvolvimento (Windows)

Pré-requisitos: Rust (toolchain **MSVC**), Node 22+, Visual Studio Build Tools (C++), [GStreamer 1.28+ MSVC x86_64](https://gstreamer.freedesktop.org/download/) (instalação completa).

```powershell
rustup override set stable-x86_64-pc-windows-msvc   # se seu Rust padrão for -gnu
cd app; npm install; npm run dev                    # roda o app
```

```powershell
cd app; npm run build    # gera o instalador em target\release\bundle\nsis\
```

O `npm run build` compila o frontend e o binário release, copia os plugins/DLLs necessários do GStreamer (~42 MB) para dentro do pacote e gera o instalador NSIS (mais a assinatura `.sig` caso `TAURI_SIGNING_PRIVATE_KEY` esteja definida).

Servidor de salas local: `cd signaling; npm install; npm run dev` e aponte o app para `http://127.0.0.1:8787` em ⚙️ Configurações.

Testes e checks:
- Rust: `cargo test --workspace` · `cargo clippy --workspace --all-targets -- -D warnings`
- Sinalização: `cd signaling; npm run typecheck; npm test` · `npm run smoke` (com `wrangler dev` rodando)
- Ponta a ponta: `bash scripts/e2e.sh` (com servidor local rodando)

### Chave do app e secrets de Release (GitHub Actions)

1. **Chave do app (`.app-key`):** o servidor só aceita apps compilados com a chave do projeto. Ela fica em `.app-key` na raiz (fora do git) e o servidor guarda só o hash (`APP_KEY_SHA256` em `signaling/wrangler.toml`).
   ```powershell
   node scripts/gen-app-key.mjs            # cria a chave (se não existir) e mostra o hash
   node scripts/gen-app-key.mjs --rotate   # troca a chave: atualize o hash e faça deploy; versões antigas param de funcionar
   ```
   **Guarde um backup do `.app-key`** — sem ele, novas builds não conseguem usar o seu servidor.

2. **Chave do Tauri Updater:** usada para assinar atualizações automáticas. A chave pública fica em `app/src-tauri/tauri.conf.json` (`plugins.updater.pubkey`) e a privada deve ficar segura fora do repositório (ex.: `%USERPROFILE%\.tauri\crimson-screenlive.key`).

3. **Secrets no GitHub (Settings → Secrets and variables → Actions):**
   - `SCREENLIVE_APP_KEY`: conteúdo do arquivo `.app-key`.
   - `TAURI_SIGNING_PRIVATE_KEY`: conteúdo da chave privada do updater.
   - `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`: senha da chave (se houver).
   Ao criar uma tag `vX.Y.Z` (ex.: `git tag v0.6.0 && git push origin v0.6.0`), o workflow `.github/workflows/release.yml` compila, assina e publica o instalador e o `latest.json` automaticamente no GitHub Releases.

### Servidor de salas (Cloudflare)

```powershell
cd signaling; npx wrangler deploy
npx wrangler secret put TURN_KEY_ID          # chave em Realtime → TURN Server
npx wrangler secret put TURN_KEY_API_TOKEN
```

O app de quem transmite informa periodicamente quantos bytes enviou pelo TURN; o servidor soma por mês (UTC) e, ao atingir `TURN_MONTHLY_CAP_GB` (padrão **800 GB**, em `wrangler.toml`), para de entregar credenciais TURN até o mês virar — a cota gratuita da Cloudflare é 1000 GB. Consumo estimado: `GET /usage` (requer header `x-screenlive-key`). Credenciais TURN só são entregues a membros autenticados e valem por `TURN_TTL_SECONDS` (6 h).

## Status

- ✅ Windows: transmitir tela/janela e assistir (AV1/H.264 por hardware), até 4 espectadores, áudio do PC/por app com Opus FEC, controle de volume, bitrate adaptativo, bandeja do sistema, auto-updater e TURN com teto mensal.
- 🚧 Linux: o app compila, mas a captura de tela (PipeWire/portal) ainda não foi implementada.

## Licença

Copyright © 2026 LucasWinner. **Todos os direitos reservados** — veja [LICENSE](LICENSE).
O instalador inclui bibliotecas de terceiros com licenças próprias (GStreamer/LGPL, rsrtp/MPL-2.0, dav1d/BSD-2-Clause, OpenH264/BSD-2-Clause, entre outras).
