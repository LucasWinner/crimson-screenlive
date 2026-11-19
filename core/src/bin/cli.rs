//! Developer CLI for testing streams before the Tauri app exists.
//!
//!   screenlive-cli encoders
//!   screenlive-cli stream [--room CODE] --name Alice
//!   screenlive-cli watch  --room CODE  --name Bob

use std::time::Duration;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand, ValueEnum};
use screenlive_core::capture::{AudioSource, VideoFormat, VideoSource};
use screenlive_core::protocol::ServerMsg;
use screenlive_core::signaling::Server;
use screenlive_core::streamer::{StreamConfig, Streamer};
use screenlive_core::viewer::{self, ViewConfig, ViewEnd, Viewer};
use screenlive_core::{encoder, ice};

#[derive(Parser)]
#[command(about = "Crimson ScreenLive developer CLI")]
struct Cli {
    /// Signaling server base URL.
    #[arg(long, global = true, env = "SCREENLIVE_SERVER", default_value = "https://seu-worker.workers.dev")]
    server: String,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum)]
enum Source {
    Screen,
    Test,
}

#[derive(Clone, Copy, ValueEnum)]
enum Audio {
    System,
    Test,
    None,
}

#[derive(Subcommand)]
enum Cmd {
    /// List H.264 encoders usable on this machine (best first).
    Encoders,
    /// List monitors that can be captured.
    Monitors,
    /// Share your screen in a room (creates one if --room is omitted).
    Stream {
        #[arg(long)]
        room: Option<String>,
        #[arg(long)]
        name: String,
        #[arg(long, value_enum, default_value = "screen")]
        source: Source,
        #[arg(long, default_value_t = 0)]
        monitor: u32,
        #[arg(long, value_enum, default_value = "system")]
        audio: Audio,
        #[arg(long, default_value_t = 1920)]
        width: u32,
        #[arg(long, default_value_t = 1080)]
        height: u32,
        #[arg(long, default_value_t = 60)]
        fps: u32,
        /// Video bitrate in kbps.
        #[arg(long, default_value_t = 7000)]
        bitrate: u32,
        /// Force an encoder (see `encoders`).
        #[arg(long)]
        encoder: Option<String>,
    },
    /// Watch a stream in a room.
    Watch {
        #[arg(long)]
        room: String,
        #[arg(long)]
        name: String,
        /// Nickname of the streamer to watch (default: whoever is streaming).
        #[arg(long)]
        target: Option<String>,
        /// No window/sound; just count frames (for testing).
        #[arg(long)]
        headless: bool,
        /// Stop after N seconds; exits with an error if no video arrived.
        #[arg(long)]
        seconds: Option<u64>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info,screenlive_core=info")).init();
    screenlive_core::init()?;
    let cli = Cli::parse();

    match cli.cmd {
        Cmd::Monitors => {
            for m in screenlive_core::capture::list_monitors() {
                let p = if m.primary { "  [primary]" } else { "" };
                println!("{:<12} {}x{}  {}{p}", m.id, m.width, m.height, m.name);
            }
            Ok(())
        }
        Cmd::Encoders => {
            for e in encoder::available() {
                println!("{:<16} {}{}", e.factory, e.label, if e.hardware { "" } else { "  [software]" });
            }
            Ok(())
        }
        Cmd::Stream { room, name, source, monitor, audio, width, height, fps, bitrate, encoder } => {
            let server = Server::new(&cli.server)?;
            let room = match room {
                Some(r) => r,
                None => server.create_room().await?,
            };
            let cfg = StreamConfig {
                video: match source {
                    Source::Screen => VideoSource::MonitorIndex(monitor),
                    Source::Test => VideoSource::Test,
                },
                audio: match audio {
                    Audio::System => AudioSource::System,
                    Audio::Test => AudioSource::Test,
                    Audio::None => AudioSource::None,
                },
                format: VideoFormat { width, height, fps },
                bitrate_kbps: bitrate,
                encoder,
                relay_saver: true,
            };
            let ice = ice::fetch(&server).await;
            let (sig, events) = server.join(&room, &name).await?;
            let streamer = Streamer::new(&cfg, sig, ice)?;
            log::info!("encoder: {} ({})", streamer.encoder.label, streamer.encoder.factory);
            streamer.start()?;
            println!("Streaming in room {room}. Share this code with your friends. Ctrl+C to stop.");
            streamer.run(events).await
        }
        Cmd::Watch { room, name, target, headless, seconds } => {
            let server = Server::new(&cli.server)?;
            let cfg = ViewConfig { target, headless, duration: seconds.map(Duration::from_secs) };
            let ice = ice::fetch(&server).await;
            let (sig, mut events) = server.join(&room, &name).await?;
            let peers = match events.recv().await {
                Some(ServerMsg::Welcome { peers, .. }) => peers,
                other => bail!("unexpected first message from server: {other:?}"),
            };
            let find = viewer::find_streamer(&peers, &mut events, &cfg);
            let streamer = match cfg.duration {
                Some(d) => tokio::time::timeout(d, find).await.map_err(|_| anyhow::anyhow!("nobody is streaming"))??,
                None => find.await?,
            };
            let v = Viewer::new(streamer, sig, &ice, cfg.headless)?;
            let end = v.run(events, cfg.duration).await?;
            let stats = v.stats();
            println!("video frames: {}, audio buffers: {}", stats.video_frames, stats.audio_buffers);
            if let ViewEnd::StreamEnded = end {
                println!("{} stopped streaming.", v.streamer().name);
            }
            if seconds.is_some() && stats.video_frames == 0 {
                bail!("no video received");
            }
            Ok(())
        }
    }
}
