//! Picks the best available H.264 encoder (hardware first) and configures it
//! for low-latency constant-bitrate streaming.

use anyhow::{bail, Result};
use gst::glib;
use gst::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Memory {
    /// Encoder accepts frames that stay on the GPU (D3D11 textures).
    D3D11,
    /// Encoder wants frames in system memory.
    System,
}

#[derive(Debug, Clone, Copy)]
pub struct EncoderSpec {
    pub factory: &'static str,
    pub label: &'static str,
    pub hardware: bool,
    pub memory: Memory,
}

/// Ordered by preference.
#[cfg(windows)]
const CANDIDATES: &[EncoderSpec] = &[
    EncoderSpec { factory: "amfh264enc", label: "AMD AMF", hardware: true, memory: Memory::D3D11 },
    EncoderSpec { factory: "nvd3d11h264enc", label: "NVIDIA NVENC", hardware: true, memory: Memory::D3D11 },
    EncoderSpec { factory: "nvh264enc", label: "NVIDIA NVENC", hardware: true, memory: Memory::System },
    EncoderSpec { factory: "qsvh264enc", label: "Intel Quick Sync", hardware: true, memory: Memory::D3D11 },
    EncoderSpec { factory: "mfh264enc", label: "Media Foundation", hardware: true, memory: Memory::D3D11 },
    EncoderSpec { factory: "x264enc", label: "x264 (CPU)", hardware: false, memory: Memory::System },
    EncoderSpec { factory: "openh264enc", label: "OpenH264 (CPU)", hardware: false, memory: Memory::System },
];

#[cfg(not(windows))]
const CANDIDATES: &[EncoderSpec] = &[
    EncoderSpec { factory: "vah264lpenc", label: "VA-API (low power)", hardware: true, memory: Memory::System },
    EncoderSpec { factory: "vah264enc", label: "VA-API", hardware: true, memory: Memory::System },
    EncoderSpec { factory: "nvh264enc", label: "NVIDIA NVENC", hardware: true, memory: Memory::System },
    EncoderSpec { factory: "qsvh264enc", label: "Intel Quick Sync", hardware: true, memory: Memory::System },
    EncoderSpec { factory: "x264enc", label: "x264 (CPU)", hardware: false, memory: Memory::System },
    EncoderSpec { factory: "openh264enc", label: "OpenH264 (CPU)", hardware: false, memory: Memory::System },
];

/// Hardware encoder plugins only register factories when a usable device exists,
/// but we also try to instantiate to be sure.
fn usable(spec: &EncoderSpec) -> bool {
    gst::ElementFactory::find(spec.factory).is_some() && gst::ElementFactory::make(spec.factory).build().is_ok()
}

pub fn available() -> Vec<EncoderSpec> {
    CANDIDATES.iter().copied().filter(usable).collect()
}

pub fn pick(preferred: Option<&str>) -> Result<EncoderSpec> {
    if let Some(name) = preferred {
        let Some(spec) = CANDIDATES.iter().find(|c| c.factory == name) else {
            bail!("unknown encoder '{name}'");
        };
        if !usable(spec) {
            bail!("encoder '{name}' is not available on this machine");
        }
        return Ok(*spec);
    }
    match available().first() {
        Some(spec) => Ok(*spec),
        None => bail!("no H.264 encoder found (install GStreamer plugins: bad/ugly)"),
    }
}

/// Sets a property from its string form, skipping it if this encoder version
/// doesn't have it or rejects the value (property names differ across versions).
pub(crate) fn set_if(elem: &gst::Element, name: &str, value: &str) {
    let Some(pspec) = elem.find_property(name) else {
        log::debug!("{}: no property '{name}'", elem.name());
        return;
    };
    match <glib::Value as gst::prelude::GstValueExt>::deserialize(value, pspec.value_type()) {
        Ok(v) => elem.set_property_from_value(name, &v),
        Err(_) => log::warn!("{}: invalid value '{value}' for '{name}'", elem.name()),
    }
}

pub struct EncoderSettings {
    pub bitrate_kbps: u32,
    pub fps: u32,
    /// Keyframe interval in seconds.
    pub keyframe_secs: u32,
}

pub fn build(spec: &EncoderSpec, s: &EncoderSettings) -> Result<gst::Element> {
    let enc = gst::ElementFactory::make(spec.factory).build()?;
    let br = s.bitrate_kbps.to_string();
    let gop = (s.fps * s.keyframe_secs).to_string();

    match spec.factory {
        "amfh264enc" => {
            set_if(&enc, "usage", "ultra-low-latency");
            set_if(&enc, "preset", "speed");
            set_if(&enc, "rate-control", "cbr");
            set_if(&enc, "bitrate", &br);
            set_if(&enc, "max-bitrate", &br);
            set_if(&enc, "gop-size", &gop);
            set_if(&enc, "b-frames", "0");
        }
        "nvd3d11h264enc" | "nvh264enc" => {
            set_if(&enc, "preset", "p1");
            set_if(&enc, "tune", "ultra-low-latency");
            set_if(&enc, "rate-control", "cbr");
            set_if(&enc, "rc-mode", "cbr");
            set_if(&enc, "zerolatency", "true");
            set_if(&enc, "zero-reorder-delay", "true");
            set_if(&enc, "bitrate", &br);
            set_if(&enc, "gop-size", &gop);
            set_if(&enc, "bframes", "0");
        }
        "qsvh264enc" => {
            set_if(&enc, "target-usage", "7");
            set_if(&enc, "rate-control", "cbr");
            set_if(&enc, "low-latency", "true");
            set_if(&enc, "bitrate", &br);
            set_if(&enc, "gop-size", &gop);
            set_if(&enc, "b-frames", "0");
        }
        "mfh264enc" => {
            set_if(&enc, "rc-mode", "cbr");
            set_if(&enc, "low-latency", "true");
            set_if(&enc, "quality-vs-speed", "0");
            set_if(&enc, "bitrate", &br);
            set_if(&enc, "gop-size", &gop);
            set_if(&enc, "bframes", "0");
        }
        "vah264lpenc" | "vah264enc" => {
            set_if(&enc, "rate-control", "cbr");
            set_if(&enc, "target-usage", "7");
            set_if(&enc, "bitrate", &br);
            set_if(&enc, "key-int-max", &gop);
            set_if(&enc, "b-frames", "0");
        }
        "x264enc" => {
            set_if(&enc, "tune", "zerolatency");
            set_if(&enc, "speed-preset", "ultrafast");
            set_if(&enc, "bitrate", &br);
            set_if(&enc, "key-int-max", &gop);
            set_if(&enc, "bframes", "0");
        }
        "openh264enc" => {
            let bps = (s.bitrate_kbps as u64 * 1000).min(u32::MAX as u64).to_string();
            set_if(&enc, "usage-type", "screen");
            set_if(&enc, "rate-control", "bitrate");
            set_if(&enc, "complexity", "low");
            set_if(&enc, "bitrate", &bps);
            set_if(&enc, "max-bitrate", &bps);
            set_if(&enc, "gop-size", &gop);
            set_if(&enc, "enable-frame-skip", "false");
        }
        _ => {}
    }
    Ok(enc)
}

/// Asks the encoder for an immediate keyframe (with SPS/PPS), e.g. when a new viewer joins.
pub fn request_keyframe(encoder: &gst::Element) {
    let event = gst_video::UpstreamForceKeyUnitEvent::builder().all_headers(true).build();
    if let Some(pad) = encoder.static_pad("src") {
        pad.send_event(event);
    }
}
