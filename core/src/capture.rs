//! Video/audio sources, described as gst-launch fragments that end in raw frames
//! ready for the chosen encoder.

use anyhow::Result;
use gst::prelude::*;

use crate::encoder::{EncoderSpec, Memory};

/// Prepared capture pipeline fragment plus an optional RAII guard keeping OS
/// capture handles (such as a Linux xdg-desktop-portal session & PipeWire fd) alive.
pub struct PreparedVideo {
    pub fragment: String,
    pub guard: Option<Box<dyn std::any::Any + Send + Sync>>,
}

#[derive(Debug, Clone)]
pub enum VideoSource {
    /// A whole monitor by OS index (0 = first).
    MonitorIndex(u32),
    /// A whole monitor by the stable id from [`list_monitors`].
    Monitor(u64),
    /// A single window by its native handle (HWND on Windows, WGC capture).
    Window(u64),
    /// Synthetic moving test pattern; useful for testing without a screen.
    Test,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MonitorInfo {
    /// Pass back as `VideoSource::Monitor(id)` (HMONITOR on Windows).
    pub id: u64,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

/// Monitors that can be captured, primary first.
pub fn list_monitors() -> Vec<MonitorInfo> {
    let monitor = gst::DeviceMonitor::new();
    monitor.add_filter(Some("Source/Monitor"), None);
    let mut out: Vec<MonitorInfo> = monitor
        .devices()
        .iter()
        .filter_map(|dev| {
            let props = dev.properties()?;
            #[cfg(windows)]
            let id = props.get::<u64>("device.hmonitor").ok()?;
            #[cfg(not(windows))]
            let id = 0u64;
            let s = dev.caps()?.structure(0)?.to_owned();
            Some(MonitorInfo {
                id,
                name: dev.display_name().to_string(),
                width: s.get::<i32>("width").unwrap_or(0) as u32,
                height: s.get::<i32>("height").unwrap_or(0) as u32,
                primary: props.get::<bool>("device.primary").unwrap_or(false),
            })
        })
        .collect();
    // Several device providers (DXGI, WGC) can report the same monitor.
    let mut seen = std::collections::HashSet::new();
    out.retain(|m| seen.insert(m.id));
    out.sort_by_key(|m| !m.primary);
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioSource {
    /// Everything the computer is playing (game, music, ...).
    System,
    /// Everything except this process and its children (e.g. Discord, so
    /// friends don't hear their own voices). Windows 20348+; see `procs`.
    SystemExcept(u32),
    /// Only this process and its children (e.g. just the game).
    App(u32),
    /// Synthetic tone for testing.
    Test,
    None,
}

#[derive(Debug, Clone)]
pub struct VideoFormat {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

fn pixel_format(enc: &EncoderSpec) -> &'static str {
    if enc.factory == "openh264enc" {
        "I420"
    } else {
        "NV12"
    }
}

/// Prepares a video capture source, opening an OS screencast session on Linux when needed.
pub fn prepare_video(src: &VideoSource, enc: &EncoderSpec, fmt: &VideoFormat) -> Result<PreparedVideo> {
    #[cfg(not(windows))]
    if !matches!(src, VideoSource::Test) {
        return prepare_linux_screencast(src, enc, fmt);
    }
    Ok(PreparedVideo { fragment: video_fragment(src, enc, fmt)?, guard: None })
}

/// Returns a pipeline fragment producing raw video for `enc`, scaled with
/// letterboxing to `fmt` (so ultrawide/4K monitors keep their aspect ratio).
pub fn video_fragment(src: &VideoSource, enc: &EncoderSpec, fmt: &VideoFormat) -> Result<String> {
    let VideoFormat { width: w, height: h, fps } = *fmt;
    let pix = pixel_format(enc);
    Ok(match src {
        VideoSource::Test => format!(
            "videotestsrc is-live=true pattern=ball ! video/x-raw,framerate={fps}/1 \
             ! videoconvert ! videoscale add-borders=true \
             ! capsfilter caps=\"video/x-raw,format={pix},width={w},height={h}\""
        ),
        VideoSource::MonitorIndex(idx) => capture_fragment(&format!("monitor-index={idx}"), enc, w, h, fps)?,
        VideoSource::Monitor(id) => capture_fragment(&format!("monitor-handle={id}"), enc, w, h, fps)?,
        VideoSource::Window(hwnd) => {
            capture_fragment(&format!("capture-api=wgc window-handle={hwnd}"), enc, w, h, fps)?
        }
    })
}

#[cfg(windows)]
fn capture_fragment(select: &str, enc: &EncoderSpec, w: u32, h: u32, fps: u32) -> Result<String> {
    // Capture and scale entirely on the GPU; frames only leave VRAM if the
    // encoder can't take D3D11 textures (CPU fallback).
    let gpu = format!(
        "d3d11screencapturesrc {select} show-cursor=true \
         ! video/x-raw(memory:D3D11Memory),framerate={fps}/1 \
         ! d3d11convert add-borders=true gamma-mode=remap primaries-mode=fast \
         ! capsfilter caps=\"video/x-raw(memory:D3D11Memory),format=NV12,width={w},height={h}\""
    );
    let pix = pixel_format(enc);
    // Note: a parse description can't end in bare caps, hence the capsfilters.
    Ok(match enc.memory {
        Memory::D3D11 => gpu,
        Memory::System => {
            format!("{gpu} ! d3d11download ! videoconvert ! capsfilter caps=\"video/x-raw,format={pix}\"")
        }
    })
}

#[cfg(not(windows))]
fn capture_fragment(_select: &str, _enc: &EncoderSpec, _w: u32, _h: u32, _fps: u32) -> Result<String> {
    anyhow::bail!("use prepare_video() on Linux to open an xdg-desktop-portal screencast session")
}

#[cfg(target_os = "linux")]
fn prepare_linux_screencast(src: &VideoSource, enc: &EncoderSpec, fmt: &VideoFormat) -> Result<PreparedVideo> {
    use std::os::fd::{AsRawFd, OwnedFd};
    use anyhow::Context;
    use ashpd::desktop::screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType};
    use ashpd::desktop::{PersistMode, Session};

    struct PortalGuard {
        _fd: OwnedFd,
        _session: Session<Screencast>,
    }

    let source_types = match src {
        VideoSource::Window(_) => SourceType::WINDOW,
        _ => SourceType::MONITOR | SourceType::WINDOW,
    };

    let (fd, node_id, session) = std::thread::spawn(move || -> Result<(OwnedFd, u32, Session<Screencast>)> {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        rt.block_on(async move {
            let proxy = Screencast::new().await.context("xdg-desktop-portal ScreenCast unavailable")?;
            let session = proxy.create_session(Default::default()).await?;
            proxy
                .select_sources(
                    &session,
                    SelectSourcesOptions::default()
                        .set_cursor_mode(CursorMode::EMBEDDED)
                        .set_sources(source_types)
                        .set_multiple(false)
                        .set_persist_mode(PersistMode::DoNot),
                )
                .await?;
            let response = proxy.start(&session, None, Default::default()).await?.response()?;
            let stream = response
                .streams()
                .first()
                .context("no screen or window selected in portal")?;
            let node_id = stream.pipe_wire_node_id();
            let fd = proxy.open_pipe_wire_remote(&session, Default::default()).await?;
            Ok((fd, node_id, session))
        })
    })
    .join()
    .map_err(|_| anyhow::anyhow!("screencast portal thread panicked"))??;

    let raw_fd = fd.as_raw_fd();
    let VideoFormat { width: w, height: h, fps } = *fmt;
    let pix = pixel_format(enc);
    let fragment = format!(
        "pipewiresrc fd={raw_fd} path={node_id} do-timestamp=true keepalive-time=1000 \
         ! videorate drop-only=true \
         ! videoconvert ! videoscale add-borders=true \
         ! capsfilter caps=\"video/x-raw,format={pix},width={w},height={h},framerate={fps}/1\""
    );
    Ok(PreparedVideo { fragment, guard: Some(Box::new(PortalGuard { _fd: fd, _session: session })) })
}

#[cfg(all(not(windows), not(target_os = "linux")))]
fn prepare_linux_screencast(_src: &VideoSource, _enc: &EncoderSpec, _fmt: &VideoFormat) -> Result<PreparedVideo> {
    anyhow::bail!("screen capture is not implemented on this OS; use --source test")
}

/// Fragment that rescales the raw output of [`video_fragment`] (same source and
/// encoder) to a smaller size, staying in GPU memory when the source is.
pub fn downscale_fragment(src: &VideoSource, enc: &EncoderSpec, w: u32, h: u32) -> String {
    let on_gpu = cfg!(windows) && !matches!(src, VideoSource::Test) && enc.memory == Memory::D3D11;
    if on_gpu {
        format!(
            "d3d11convert add-borders=true gamma-mode=remap primaries-mode=fast \
             ! capsfilter caps=\"video/x-raw(memory:D3D11Memory),format=NV12,width={w},height={h}\""
        )
    } else {
        let pix = pixel_format(enc);
        format!(
            "videoscale add-borders=true ! videoconvert \
             ! capsfilter caps=\"video/x-raw,format={pix},width={w},height={h}\""
        )
    }
}

/// Returns a fragment producing Opus RTP, or None when audio is disabled.
pub fn audio_fragment(src: AudioSource) -> Option<String> {
    let raw = match src {
        AudioSource::None => return None,
        AudioSource::Test => "audiotestsrc is-live=true wave=ticks volume=0.3".to_owned(),
        #[cfg(windows)]
        AudioSource::System => "wasapi2src loopback=true low-latency=true".to_owned(),
        #[cfg(windows)]
        AudioSource::SystemExcept(pid) => format!(
            "wasapi2src loopback=true low-latency=true loopback-mode=exclude-process-tree loopback-target-pid={pid}"
        ),
        #[cfg(windows)]
        AudioSource::App(pid) => format!(
            "wasapi2src loopback=true low-latency=true loopback-mode=include-process-tree loopback-target-pid={pid}"
        ),
        // TODO(linux): per-app capture via PipeWire node targeting.
        #[cfg(not(windows))]
        AudioSource::System | AudioSource::SystemExcept(_) | AudioSource::App(_) => {
            "pulsesrc device=@DEFAULT_MONITOR@".to_owned()
        }
    };
    Some(format!(
        "{raw} ! audioconvert ! audioresample ! audio/x-raw,rate=48000,channels=2 \
         ! opusenc bitrate=128000 frame-size=10 audio-type=generic inband-fec=true packet-loss-percentage=10"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoder;

    fn fmt() -> VideoFormat {
        VideoFormat { width: 1280, height: 720, fps: 30 }
    }

    #[test]
    fn fragments_parse() {
        gst::init().unwrap();
        let spec = encoder::pick(Some("openh264enc")).expect("openh264enc available");
        let v = video_fragment(&VideoSource::Test, &spec, &fmt()).unwrap();
        gst::parse::bin_from_description(&v, true).unwrap_or_else(|e| panic!("{e}: {v}"));
        let d = downscale_fragment(&VideoSource::Test, &spec, 640, 360);
        gst::parse::bin_from_description(&d, true).unwrap_or_else(|e| panic!("{e}: {d}"));
        for src in [AudioSource::Test, AudioSource::System, AudioSource::SystemExcept(4), AudioSource::App(4)] {
            let a = audio_fragment(src).unwrap();
            gst::parse::bin_from_description(&a, true).unwrap_or_else(|e| panic!("{e}: {a}"));
        }

        #[cfg(windows)]
        for spec in encoder::available() {
            let v = video_fragment(&VideoSource::MonitorIndex(0), &spec, &fmt()).unwrap();
            gst::parse::bin_from_description(&v, true).unwrap_or_else(|e| panic!("{}: {e}: {v}", spec.factory));
            let w = video_fragment(&VideoSource::Window(1), &spec, &fmt()).unwrap();
            gst::parse::bin_from_description(&w, true).unwrap_or_else(|e| panic!("{}: {e}: {w}", spec.factory));
        }
    }
}
