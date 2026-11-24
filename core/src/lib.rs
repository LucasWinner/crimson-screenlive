//! Crimson ScreenLive core: screen capture, hardware H.264/AV1 encoding and
//! WebRTC transport (P2P, up to 4 viewers) on top of GStreamer.

pub mod capture;
pub mod encoder;
pub mod ice;
pub mod procs;
pub mod protocol;
pub mod signaling;
pub mod streamer;
pub mod viewer;
mod webrtc;

pub fn init() -> anyhow::Result<()> {
    gst::init()?;
    // rtpsession logs a harmless "generated empty RTCP messages" at ERROR level
    // every few seconds. Hide it unless the user asked for GStreamer logs.
    if std::env::var_os("GST_DEBUG").is_none() {
        gst::log::set_threshold_for_name("rtpsession", gst::DebugLevel::None);
    }
    Ok(())
}
