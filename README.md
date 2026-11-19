# Crimson ScreenLive

Transmissão de tela entre amigos — leve, gratuita e feita para gameplay.
Uma alternativa ao "Go Live" do Discord para quando ele não funciona.

- **Não pesa no jogo:** captura e codificação acontecem na placa de vídeo (AMD AMF, NVIDIA NVENC, Intel Quick Sync). Em teste, 1080p60 usou ~0,2% de CPU.
- **Direto entre os PCs (P2P):** o vídeo vai de você para seus amigos sem passar por servidor, com baixa latência. Até 4 espectadores por transmissão.
- **Simples:** crie uma sala, mande o código de 6 letras no Discord, pronto.
- **Áudio do PC/jogo** junto com a tela — por padrão **sem o Discord**, para seus amigos não ouvirem a própria voz de volta. Em ⚙️ Configurações dá para escolher: PC inteiro, PC inteiro menos um programa, ou só um programa (ex.: só o jogo). Requer Windows 11 ou Windows 10 build 20348+; em versões antigas, transmite o PC inteiro.

## Baixar e usar (Windows 10/11)

Duas opções, mesmo app:
- **Portátil (`CrimsonScreenLive_x.y.z_x64-portable.exe`)** — um único arquivo, não instala nada: é só abrir. Na primeira vez ele descompacta o app em `%LOCALAPPDATA%CrimsonScreenLiveportable` (~3 s); depois abre direto.
- **Instalador (`..._x64-setup.exe`)** — instala com atalho no Menu Iniciar e desinstalador.


1. Baixe o instalador em [Releases](../../releases/latest) (`CrimsonScreenLive_x.y.z_x64-setup.exe`) e instale.
   > O Windows pode mostrar "O Windows protegeu o computador" porque o app não é assinado. Clique em **Mais informações → Executar assim mesmo**.
2. Abra o **Crimson ScreenLive**, escolha um apelido.
3. **Criar sala** → clique no código para copiar e mande para os amigos. Ou cole o código de alguém e clique **Entrar**.
4. Para transmitir: escolha a tela, a qualidade e clique **Transmitir**.
5. Para assistir: clique **Assistir** ao lado de quem está **AO VIVO**. O vídeo abre numa janela separada (botão **Tela cheia** no app).

**Qualidade × internet:** escolha resolução (720p/1080p/1440p) e 30 ou 60 FPS. Cada espectador recebe um fluxo próprio, então o seu *upload* precisa de aproximadamente, **por espectador**:
720p ≈ 3 / 4,5 Mbps · 1080p ≈ 4,7 / 7 Mbps · 1440p ≈ 8 / 12 Mbps (30 / 60 FPS). 30 FPS é ótimo para vídeos e filmes; 60 FPS para jogos.

**Conexão via servidor (TURN):** quando a conexão direta não é possível (4G, CGNAT, redes restritas), o vídeo passa pelo TURN da Cloudflare. A qualidade é a mesma da conexão direta; o app mostra um aviso **VIA SERVIDOR** (em amarelo) para quem assiste e para quem transmite, porque esse tráfego conta na cota do TURN.

## Como funciona

```
App (Tauri + Rust) ──captura GPU──► encoder H.264 por hardware ──► WebRTC ──P2P──► App do amigo (decodifica na GPU)
          │                                                                    ▲
          └──────── sinalização (salas, SDP/ICE) ── Cloudflare Worker ─────────┘
                    TURN da Cloudflare só quando a conexão direta falha (4G/CGNAT)
```

| Pasta | O que é |
|---|---|
| `core/` | Biblioteca Rust: captura, escolha de encoder, pipelines GStreamer/WebRTC, cliente de sinalização. Inclui `screenlive-cli` para testes. |
| `app/` | App desktop Tauri 2 (UI em TypeScript, backend em `app/src-tauri`). |
| `signaling/` | Servidor de salas: Cloudflare Worker + Durable Objects (plano gratuito). |
| `scripts/` | Teste ponta a ponta (`e2e.sh`) e atalho da CLI para PowerShell. |

## Desenvolvimento (Windows)

Pré-requisitos: Rust (toolchain **MSVC**), Node 20+, Visual Studio Build Tools (C++), [GStreamer 1.26+ MSVC x86_64](https://gstreamer.freedesktop.org/download/) (instalação completa).

```powershell
rustup override set stable-x86_64-pc-windows-msvc   # se seu Rust padrão for -gnu
cd app; npm install; npm run dev                    # roda o app
```

```powershell
cd app; npm run build    # gera o instalador em target\release\bundle\nsis\
```

O `npm run build` copia só os plugins/DLLs do GStreamer que o app usa (~36 MB) para dentro do instalador.

Servidor de salas local: `cd signaling; npm install; npm run dev` e aponte o app para `http://127.0.0.1:8787` em ⚙️ Configurações.

Testes: `cargo test --workspace` · `bash scripts/e2e.sh` (com o servidor local rodando).

### Chave do app

O servidor só aceita apps compilados com a chave do projeto. Ela fica em `.app-key` na raiz (fora do git) e é embutida no app durante a compilação; o servidor guarda só o hash (`APP_KEY_SHA256` em `signaling/wrangler.toml`).

```powershell
node scripts/gen-app-key.mjs            # cria a chave (se não existir) e mostra o hash
node scripts/gen-app-key.mjs --rotate   # troca a chave: atualize o hash e faça deploy; versões antigas param de funcionar
```

**Guarde um backup do `.app-key`** — sem ele, novas versões não conseguem usar o seu servidor (aí é preciso rotacionar e redistribuir o app).

### Servidor de salas (Cloudflare)

```powershell
cd signaling; npx wrangler deploy
npx wrangler secret put TURN_KEY_ID          # chave em Realtime → TURN Server
npx wrangler secret put TURN_KEY_API_TOKEN
```

O app de quem transmite informa ao servidor quantos bytes enviou pelo TURN; o servidor soma por mês (UTC) e, ao atingir `TURN_MONTHLY_CAP_GB` (padrão **800 GB**, em `wrangler.toml`), para de entregar credenciais TURN até o mês virar — a cota gratuita da Cloudflare é 1000 GB. Consumo estimado: `GET /usage`. Credenciais TURN só são entregues a quem entra numa sala com a chave do app, e valem por `TURN_TTL_SECONDS` (6 h).

## Status

- ✅ Windows: transmitir e assistir, até 4 espectadores, áudio do sistema, TURN com 720p automático e teto mensal.
- 🚧 Linux: o app compila, mas a captura de tela (PipeWire/portal) ainda não foi implementada.

## Licença

Copyright © 2026 LucasWinner. **Todos os direitos reservados** — veja [LICENSE](LICENSE).
O instalador inclui bibliotecas de terceiros com licenças próprias (GStreamer/LGPL, OpenH264/BSD, entre outras).
