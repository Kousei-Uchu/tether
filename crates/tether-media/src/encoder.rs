use crate::source::Frame;
use openh264::{
    encoder::{
        BitRate, Encoder, EncoderConfig, FrameRate, IntraFramePeriod, Profile, RateControlMode,
        UsageType,
    },
    formats::{BgraSliceU8, YUVBuffer},
    OpenH264API,
};

/// Software H.264 (Constrained Baseline, Annex B) using OpenH264. This is the fallback that
/// works everywhere. Hardware encoders for each platform plug in behind the same shape later.
pub struct H264Encoder {
    inner: Encoder,
}

impl H264Encoder {
    pub fn new(fps: u32, bitrate_kbps: u32) -> Result<Self, String> {
        let config = EncoderConfig::new()
            .bitrate(BitRate::from_bps(bitrate_kbps.saturating_mul(1000)))
            .max_frame_rate(FrameRate::from_hz(fps as f32))
            .usage_type(UsageType::ScreenContentRealTime)
            .rate_control_mode(RateControlMode::Bitrate)
            .profile(Profile::Baseline)
            .skip_frames(true)
            // Both only apply to camera style content and OpenH264 warns when they are on
            // together with screen content mode.
            .adaptive_quantization(false)
            .background_detection(false)
            .intra_frame_period(IntraFramePeriod::from_num_frames(fps.saturating_mul(2)));
        let inner = Encoder::with_api_config(OpenH264API::from_source(), config)
            .map_err(|e| format!("could not start the H.264 encoder: {e}"))?;
        Ok(Self { inner })
    }

    /// Returns one access unit in Annex B form (start codes included). An empty result means
    /// the encoder chose to skip the frame.
    pub fn encode(&mut self, frame: &Frame, force_keyframe: bool) -> Result<Vec<u8>, String> {
        if force_keyframe {
            self.inner.force_intra_frame();
        }
        let yuv =
            YUVBuffer::from_rgb_source(BgraSliceU8::new(&frame.bgra, (frame.width, frame.height)));
        let stream = self
            .inner
            .encode(&yuv)
            .map_err(|e| format!("encode failed: {e}"))?;
        Ok(stream.to_vec())
    }
}

/// NAL unit types found in an Annex B buffer, in order. Handy for tests and diagnostics.
pub fn nal_types(annex_b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 3 < annex_b.len() {
        if annex_b[i] == 0 && annex_b[i + 1] == 0 && annex_b[i + 2] == 1 {
            out.push(annex_b[i + 3] & 0x1f);
            i += 3;
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{FrameSource, TestPattern};

    #[test]
    fn first_frame_is_a_keyframe_with_parameter_sets_then_deltas_until_forced() {
        let mut src = TestPattern::new(640, 360).unwrap();
        let mut enc = H264Encoder::new(30, 1500).unwrap();

        let first = enc.encode(&src.next_frame().unwrap(), false).unwrap();
        let kinds = nal_types(&first);
        assert!(
            kinds.contains(&7) && kinds.contains(&8) && kinds.contains(&5),
            "{kinds:?}"
        );

        let second = enc.encode(&src.next_frame().unwrap(), false).unwrap();
        assert_eq!(nal_types(&second), vec![1]);

        let forced = enc.encode(&src.next_frame().unwrap(), true).unwrap();
        assert!(nal_types(&forced).contains(&5));
    }

    /// Guards against a catastrophically slow (for example unoptimised) encoder, not against
    /// a slow machine: the bound is deliberately loose.
    #[test]
    fn encodes_720p_at_a_usable_speed() {
        let mut src = TestPattern::new(1280, 720).unwrap();
        let mut enc = H264Encoder::new(30, 2500).unwrap();
        let start = std::time::Instant::now();
        let mut bytes = 0;
        for _ in 0..60 {
            bytes += enc.encode(&src.next_frame().unwrap(), false).unwrap().len();
        }
        let per_frame = start.elapsed().as_secs_f64() / 60.0;
        assert!(
            per_frame < 0.25,
            "{:.1} ms per frame is too slow",
            per_frame * 1000.0
        );
        assert!(bytes > 1000);
    }
}
