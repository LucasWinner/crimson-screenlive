# Changelog

Todas as mudanças notáveis neste projeto são documentadas neste arquivo.
O formato é baseado em [Keep a Changelog](https://keepachangelog.com/pt-BR/1.1.0/) e este projeto segue [Versionamento Semântico](https://semver.org/lang/pt-BR/).

## [0.7.1] - 2026-11-24

### Corrigido
- Tradução da mensagem de chave de acesso inválida/ausente também ao criar sala, reexibindo automaticamente o card **Conectar a um servidor** para colar um convite `csl1:...` válido.
- Feedback claro ao clicar em **Colar convite** em ⚙️ Configurações (indicando se o convite inclui servidor + chave ou apenas URL) e persistência aguardada antes de criar/entrar na sala.
- Formatação das notas do manifesto `latest.json` no workflow de release.

## [0.7.0] - 2026-11-24

### Adicionado
- **Servidor e chave configuráveis no app (BYO Server):** campo **Chave de acesso** (`x-screenlive-key`) em ⚙️ Configurações, card de configuração inicial na tela de boas-vindas e importação/exportação em 1 clique via convite `csl1:...` (`scripts/gen-invite.mjs`, **Colar convite** / **Copiar convite**).
- Flag `--key` na CLI (`screenlive-cli`) e construtor `Server::with_key` em `screenlive-core`.

### Alterado
- **Segurança da build pública:** removidos o endereço pessoal de Worker padrão e a `.app-key` embutida da release do GitHub (`.github/workflows/release.yml`, `app/src-tauri/src/settings.rs`, `core/src/bin/cli.rs`, `signaling/wrangler.toml`). Agora quem baixa pelo GitHub não acessa nenhum servidor privado sem receber um convite ou subir seu próprio Worker.

## [0.6.0] - 2026-11-24

### Adicionado
- **Suporte a AV1 por hardware**: seleção automática de `amfav1enc`, `nvd3d11av1enc`/`nvav1enc` e `qsvav1enc` quando disponível, com fallback automático para H.264 e decodificação em hardware (`d3d11av1dec`/`d3d12av1dec`) ou CPU leve (`dav1ddec`).
- **Seletor de codec e bitrate**: escolha de codec (`Automático`, `AV1`, `H.264`) em ⚙️ Configurações e seletor de bitrate (`Auto` ou manual de `2,5` a `20 Mbps`) na tela de transmissão.
- **Captura de janela específica**: além de monitores inteiros, agora é possível selecionar janelas individuais via Windows Graphics Capture (`d3d11screencapturesrc capture-api=wgc`).
- **Conversão de cores/gamma D3D11**: `d3d11convert` configurado com `gamma-mode=remap primaries-mode=fast` para melhor fidelidade em telas modernas.
- **Controle de volume e mudo no espectador**: elemento `volume` no pipeline de áudio com slider (`0%–150%`) e botão de mudo persistidos nas configurações.
- **Resiliência de áudio (Opus FEC)**: `opusenc` configurado com `inband-fec=true packet-loss-percentage=10`.
- **Telemetria de rede e bitrate adaptativo**: exibição de latência (RTT) e perda de pacotes; no modo `Auto`, redução gradual de bitrate em caso de perda sustentada (>5%) e recuperação quando a rede estabiliza.
- **Detecção tardia de Discord**: aviso na UI caso o Discord seja aberto depois do início de uma transmissão no modo "PC inteiro menos Discord".
- **Bandeja do sistema (System Tray)**: minimiza para a bandeja ao fechar no `X` enquanto estiver transmitindo ou assistindo, com menu para abrir ou sair com segurança.
- **Atualização automática (`tauri-plugin-updater`)** e workflows de **CI/Release no GitHub Actions**.
- **Testes automatizados** no servidor de sinalização (`signaling/scripts/unit.test.mjs`) e loopback WebRTC AV1 em `screenlive-core`.

### Alterado
- **Sinalização (`signaling/`)**:
  - `GET /usage` agora exige a chave do app (`x-screenlive-key`) quando `REQUIRE_APP_KEY != "false"`.
  - Adicionado rate limit por socket em `turn-usage` e redução do limite por reporte para 250 MB.
  - `Usage.fetch` atualizado para transação atômica evitando *lost updates*.
  - `Room.fetch` passa a buscar credenciais ICE/TURN antes de aceitar o WebSocket, eliminando *race conditions* antes do `welcome`.
- **Core & App**:
  - Timeouts explícitos em HTTP/WebSocket e verificação real de `Pong` no keepalive de 20 s.
  - Fallback CPU `openh264enc` compatível com as DLLs BSD empacotadas no instalador.
  - Captura por aplicativo (`AudioChoice::App`) agora resolve o processo raiz para funcionar com apps minimizados/multi-processo.
  - `Fullscreen` corrigido em `d3d11videosink`/`d3d12videosink` (`fullscreen-toggle-mode=property+alt-enter`).
  - `settings::load` não sobrescreve mais `http://127.0.0.1:8787`, permitindo desenvolvimento local normalmente.

### Removido
- Removido o lançador portátil (`portable/` e `app/scripts/make-portable.mjs`), mantendo apenas o instalador padrão NSIS.

## [0.5.0] - 2026-11-19
- Qualidade total sobre TURN, presets 30/60 FPS, lançador portátil em arquivo único.
