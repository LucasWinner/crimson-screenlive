//! Video/audio sources, described as gst-launch fragments that end in raw frames
//! ready for the chosen encoder.

#[cfg(not(windows))]
use anyhow::bail;
use anyhow::Result;
use gst::prelude::*;

use crate::encoder::EncoderSpec;
#[cfg(windows)]
use crate::encoder::Memory;

#[derive(Debug, Clone)]
pub enum VideoSource {
    /// A whole monitor by OS index (0 = first).
    MonitorIndex(u32),
    /// A whole monitor by the stable id from [`list_monitors`].
    Monitor(u64),
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

/// Returns a pipeline fragment producing raw video for `enc`, scaled with
/// letterboxing to `fmt` (so ultrawide/4K monitors keep their aspect ratio).
pub fn video_fragment(src: &VideoSource, enc: &EncoderSpec, fmt: &VideoFormat) -> Result<String> {
    let VideoFormat { width: w, height: h, fps } = *fmt;
    Ok(match src {
        VideoSource::Test => format!(
            "videotestsrc is-live=true pattern=ball ! video/x-raw,framerate={fps}/1 \
             ! videoconvert ! videoscale add-borders=true \
             ! capsfilter caps=\"video/x-raw,format=NV12,width={w},height={h}\""
        ),
        VideoSource::MonitorIndex(idx) => monitor_fragment(&format!("monitor-index={idx}"), enc, w, h, fps)?,
        VideoSource::Monitor(id) => monitor_fragment(&format!("monitor-handle={id}"), enc, w, h, fps)?,
    })
}

#[cfg(windows)]
fn monitor_fragment(select: &str, enc: &EncoderSpec, w: u32, h: u32, fps: u32) -> Result<String> {
    // Capture and scale entirely on the GPU; frames only leave VRAM if the
    // encoder can't take D3D11 textures (CPU x264 fallback).
    let gpu = format!(
        "d3d11screencapturesrc {select} show-cursor=true \
         ! video/x-raw(memory:D3D11Memory),framerate={fps}/1 \
         ! d3d11convert add-borders=true \
         ! capsfilter caps=\"video/x-raw(memory:D3D11Memory),format=NV12,width={w},height={h}\""
    );
    // Note: a parse description can't end in bare caps, hence the capsfilters.
    Ok(match enc.memory {
        Memory::D3D11 => gpu,
        Memory::System => format!("{gpu} ! d3d11download ! capsfilter caps=\"video/x-raw,format=NV12\""),
    })
}

#[cfg(not(windows))]
fn monitor_fragment(_select: &str, _enc: &EncoderSpec, _w: u32, _h: u32, _fps: u32) -> Result<String> {
    // TODO(linux): request a ScreenCast session through xdg-desktop-portal
    // (ashpd) and capture with `pipewiresrc fd=.. path=..`.
    bail!("screen capture on Linux is not implemented yet; use --source test")
}

/// Returns a fragment producing Opus RTP, or None when audio is disabled.
pub fn audio_fragment(src: AudioSource) -> Option<String> {
    let raw = match src {
        AudioSource::None => return None,
        AudioSource::Test => "audiotestsrc is-live=true wave=ticks volume=0.3".to_owned(),
        #[cfg(windows)]
        AudioSource::System => "wasapi2src loopback=true low-latency=true".to_owned(),
        #[cfg(not(windows))]
        AudioSource::System => "pulsesrc device=@DEFAULT_MONITOR@".to_owned(),
    };
    Some(format!(
        "{raw} ! audioconvert ! audioresample ! audio/x-raw,rate=48000,channels=2 \
         ! opusenc bitrate=128000 frame-size=10 audio-type=generic"
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
        let spec = encoder::pick(Some("x264enc")).expect("x264enc available");
        let v = video_fragment(&VideoSource::Test, &spec, &fmt()).unwrap();
        gst::parse::bin_from_description(&v, true).unwrap_or_else(|e| panic!("{e}: {v}"));
        let a = audio_fragment(AudioSource::Test).unwrap();
        gst::parse::bin_from_description(&a, true).unwrap_or_else(|e| panic!("{e}: {a}"));

        #[cfg(windows)]
        for spec in encoder::available() {
            let v = video_fragment(&VideoSource::MonitorIndex(0), &spec, &fmt()).unwrap();
            gst::parse::bin_from_description(&v, true).unwrap_or_else(|e| panic!("{}: {e}: {v}", spec.factory));
        }
    }
}
