//! Picks the best available video encoder (hardware AV1, then hardware H.264,
//! then CPU fallback) and configures it for low-latency streaming.

use anyhow::{bail, Result};
use gst::glib;
use gst::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Codec {
    /// Prefer hardware AV1 when available, otherwise H.264.
    #[default]
    Auto,
    Av1,
    H264,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoCodec {
    Av1,
    H264,
}

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
    pub codec: VideoCodec,
    pub hardware: bool,
    pub memory: Memory,
}

/// Ordered by preference (hardware AV1 first, then hardware H.264, then CPU H.264).
#[cfg(windows)]
const CANDIDATES: &[EncoderSpec] = &[
    EncoderSpec { factory: "amfav1enc", label: "AMD AMF AV1", codec: VideoCodec::Av1, hardware: true, memory: Memory::D3D11 },
    EncoderSpec { factory: "nvd3d11av1enc", label: "NVIDIA NVENC AV1", codec: VideoCodec::Av1, hardware: true, memory: Memory::D3D11 },
    EncoderSpec { factory: "nvav1enc", label: "NVIDIA NVENC AV1", codec: VideoCodec::Av1, hardware: true, memory: Memory::System },
    EncoderSpec { factory: "qsvav1enc", label: "Intel Quick Sync AV1", codec: VideoCodec::Av1, hardware: true, memory: Memory::D3D11 },
    EncoderSpec { factory: "amfh264enc", label: "AMD AMF H.264", codec: VideoCodec::H264, hardware: true, memory: Memory::D3D11 },
    EncoderSpec { factory: "nvd3d11h264enc", label: "NVIDIA NVENC H.264", codec: VideoCodec::H264, hardware: true, memory: Memory::D3D11 },
    EncoderSpec { factory: "nvh264enc", label: "NVIDIA NVENC H.264", codec: VideoCodec::H264, hardware: true, memory: Memory::System },
    EncoderSpec { factory: "qsvh264enc", label: "Intel Quick Sync H.264", codec: VideoCodec::H264, hardware: true, memory: Memory::D3D11 },
    EncoderSpec { factory: "mfh264enc", label: "Media Foundation H.264", codec: VideoCodec::H264, hardware: true, memory: Memory::D3D11 },
    EncoderSpec { factory: "x264enc", label: "x264 (CPU)", codec: VideoCodec::H264, hardware: false, memory: Memory::System },
    EncoderSpec { factory: "openh264enc", label: "OpenH264 (CPU)", codec: VideoCodec::H264, hardware: false, memory: Memory::System },
];

#[cfg(not(windows))]
const CANDIDATES: &[EncoderSpec] = &[
    EncoderSpec { factory: "vaav1lpenc", label: "VA-API AV1 (low power)", codec: VideoCodec::Av1, hardware: true, memory: Memory::System },
    EncoderSpec { factory: "vaav1enc", label: "VA-API AV1", codec: VideoCodec::Av1, hardware: true, memory: Memory::System },
    EncoderSpec { factory: "nvav1enc", label: "NVIDIA NVENC AV1", codec: VideoCodec::Av1, hardware: true, memory: Memory::System },
    EncoderSpec { factory: "qsvav1enc", label: "Intel Quick Sync AV1", codec: VideoCodec::Av1, hardware: true, memory: Memory::System },
    EncoderSpec { factory: "vah264lpenc", label: "VA-API H.264 (low power)", codec: VideoCodec::H264, hardware: true, memory: Memory::System },
    EncoderSpec { factory: "vah264enc", label: "VA-API H.264", codec: VideoCodec::H264, hardware: true, memory: Memory::System },
    EncoderSpec { factory: "nvh264enc", label: "NVIDIA NVENC H.264", codec: VideoCodec::H264, hardware: true, memory: Memory::System },
    EncoderSpec { factory: "qsvh264enc", label: "Intel Quick Sync H.264", codec: VideoCodec::H264, hardware: true, memory: Memory::System },
    EncoderSpec { factory: "x264enc", label: "x264 (CPU)", codec: VideoCodec::H264, hardware: false, memory: Memory::System },
    EncoderSpec { factory: "openh264enc", label: "OpenH264 (CPU)", codec: VideoCodec::H264, hardware: false, memory: Memory::System },
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
    pick_for_codec(preferred, Codec::Auto)
}

pub fn pick_for_codec(preferred: Option<&str>, codec: Codec) -> Result<EncoderSpec> {
    if let Some(name) = preferred {
        let Some(spec) = CANDIDATES.iter().find(|c| c.factory == name) else {
            bail!("unknown encoder '{name}'");
        };
        if !usable(spec) {
            bail!("encoder '{name}' is not available on this machine");
        }
        return Ok(*spec);
    }
    let avail = available();
    let found = match codec {
        Codec::Auto => avail.into_iter().next(),
        Codec::Av1 => avail.into_iter().find(|c| c.codec == VideoCodec::Av1),
        Codec::H264 => avail.into_iter().find(|c| c.codec == VideoCodec::H264),
    };
    match found {
        Some(spec) => Ok(spec),
        None => match codec {
            Codec::Av1 => bail!("no hardware AV1 encoder found on this machine"),
            _ => bail!("no video encoder found (install GStreamer plugins: bad/ugly)"),
        },
    }
}

/// Picks an H.264 encoder compatible with `memory` (used when spinning up a
/// parallel H.264 tier alongside a primary AV1 stream on the same `raw_tee`).
pub fn pick_h264_for_memory(memory: Memory) -> Result<EncoderSpec> {
    let avail = available();
    avail
        .iter()
        .copied()
        .find(|c| c.codec == VideoCodec::H264 && c.memory == memory)
        .or_else(|| avail.into_iter().find(|c| c.codec == VideoCodec::H264))
        .ok_or_else(|| anyhow::anyhow!("no compatible H.264 fallback encoder found"))
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
        "amfav1enc" => {
            set_if(&enc, "usage", "low-latency");
            set_if(&enc, "preset", "speed");
            set_if(&enc, "rate-control", "cbr");
            set_if(&enc, "bitrate", &br);
            set_if(&enc, "max-bitrate", &br);
            set_if(&enc, "gop-size", &gop);
        }
        "amfh264enc" => {
            set_if(&enc, "usage", "ultra-low-latency");
            set_if(&enc, "preset", "speed");
            set_if(&enc, "rate-control", "cbr");
            set_if(&enc, "bitrate", &br);
            set_if(&enc, "max-bitrate", &br);
            set_if(&enc, "gop-size", &gop);
            set_if(&enc, "b-frames", "0");
        }
        "nvd3d11av1enc" | "nvav1enc" | "nvd3d11h264enc" | "nvh264enc" => {
            set_if(&enc, "preset", "p1");
            set_if(&enc, "tune", "ultra-low-latency");
            set_if(&enc, "rate-control", "cbr");
            set_if(&enc, "rc-mode", "cbr");
            set_if(&enc, "zerolatency", "true");
            set_if(&enc, "zero-reorder-delay", "true");
            set_if(&enc, "bitrate", &br);
            set_if(&enc, "gop-size", &gop);
            set_if(&enc, "bframes", "0");
            set_if(&enc, "b-frames", "0");
        }
        "qsvav1enc" | "qsvh264enc" => {
            set_if(&enc, "target-usage", "7");
            set_if(&enc, "rate-control", "cbr");
            set_if(&enc, "low-latency", "true");
            set_if(&enc, "bitrate", &br);
            set_if(&enc, "gop-size", &gop);
            set_if(&enc, "b-frames", "0");
            set_if(&enc, "bframes", "0");
        }
        "mfh264enc" => {
            set_if(&enc, "rc-mode", "cbr");
            set_if(&enc, "low-latency", "true");
            set_if(&enc, "quality-vs-speed", "0");
            set_if(&enc, "bitrate", &br);
            set_if(&enc, "gop-size", &gop);
            set_if(&enc, "bframes", "0");
        }
        "vaav1lpenc" | "vaav1enc" | "vah264lpenc" | "vah264enc" => {
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

/// Adjusts target bitrate dynamically while the pipeline is running.
pub fn set_bitrate(encoder: &gst::Element, spec: &EncoderSpec, bitrate_kbps: u32) {
    if spec.factory == "openh264enc" {
        let bps = (bitrate_kbps as u64 * 1000).min(u32::MAX as u64).to_string();
        set_if(encoder, "bitrate", &bps);
        set_if(encoder, "max-bitrate", &bps);
    } else {
        let br = bitrate_kbps.to_string();
        set_if(encoder, "bitrate", &br);
        if spec.factory.starts_with("amf") {
            set_if(encoder, "max-bitrate", &br);
        }
    }
}

/// Asks the encoder for an immediate keyframe (with SPS/PPS), e.g. when a new viewer joins.
pub fn request_keyframe(encoder: &gst::Element) {
    let event = gst_video::UpstreamForceKeyUnitEvent::builder().all_headers(true).build();
    if let Some(pad) = encoder.static_pad("src") {
        pad.send_event(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_requested_codec() {
        gst::init().unwrap();
        let h264 = pick_for_codec(None, Codec::H264).expect("H.264 encoder available");
        assert_eq!(h264.codec, VideoCodec::H264);

        let auto = pick_for_codec(None, Codec::Auto).expect("auto encoder available");
        if let Ok(av1) = pick_for_codec(None, Codec::Av1) {
            assert_eq!(av1.codec, VideoCodec::Av1);
            assert!(av1.hardware);
            assert_eq!(auto.codec, VideoCodec::Av1);
            let elem = build(&av1, &EncoderSettings { bitrate_kbps: 6000, fps: 60, keyframe_secs: 2 }).unwrap();
            assert_eq!(elem.factory().unwrap().name(), av1.factory);
            set_bitrate(&elem, &av1, 4500);
        } else {
            assert_eq!(auto.codec, VideoCodec::H264);
        }
        let elem_h264 = build(&h264, &EncoderSettings { bitrate_kbps: 6000, fps: 60, keyframe_secs: 2 }).unwrap();
        set_bitrate(&elem_h264, &h264, 4000);
    }
}
