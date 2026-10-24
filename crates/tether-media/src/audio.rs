//! System audio: capture, and the Opus encoder.
//!
//! Audio travels as 20 ms frames of 48 kHz stereo. Capture hands over whatever the sound
//! device gives (any sample format, channel count and rate) and this module turns it into
//! those frames.
//!
//! Capturing what a computer is playing ("loopback") works through `cpal`:
//! - Windows: WASAPI loopback, by opening the default output device as an input.
//! - macOS: a CoreAudio process tap. That needs macOS 14.2 or later and the "System Audio
//!   Recording" permission (not Screen Recording). macOS 13 is not covered; that needs
//!   ScreenCaptureKit.
//! - Linux: a PulseAudio or PipeWire "monitor" input, if the system lists one.

use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig,
};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender},
        Arc,
    },
    time::{Duration, Instant},
};

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
/// Samples per channel in one 20 ms frame.
pub const FRAME_SAMPLES: usize = 960;
/// Interleaved values in one frame.
pub const FRAME_LEN: usize = FRAME_SAMPLES * CHANNELS;
pub const FRAME_DURATION: Duration = Duration::from_millis(20);

/// Hands out one frame at a time (`FRAME_LEN` interleaved f32 values, -1.0 to 1.0). It blocks
/// until the next frame is due and returns an error when capture has ended for good.
pub trait AudioSource: Send {
    fn next_frame(&mut self) -> Result<Vec<f32>, String>;
}

/// Which audio to stream, as typed on the command line or chosen in the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioSpec {
    /// What the computer is playing.
    System,
    /// A steady 440 Hz tone, for trying the path without a sound device.
    Tone,
}

impl std::str::FromStr for AudioSpec {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim() {
            "system" => Ok(Self::System),
            "tone" => Ok(Self::Tone),
            other => Err(format!(
                "unknown audio source {other:?}; use system or tone"
            )),
        }
    }
}

impl AudioSpec {
    pub fn open(self) -> Result<Box<dyn AudioSource>, String> {
        match self {
            Self::System => Ok(Box::new(LoopbackSource::new()?)),
            Self::Tone => Ok(Box::new(ToneSource::new(440.0))),
        }
    }
}

// ---------------------------------------------------------------- tone

/// A sine wave at real time speed.
pub struct ToneSource {
    freq: f32,
    frames: u64,
    started: Option<Instant>,
}

impl ToneSource {
    pub fn new(freq: f32) -> Self {
        Self {
            freq,
            frames: 0,
            started: None,
        }
    }
}

impl AudioSource for ToneSource {
    fn next_frame(&mut self) -> Result<Vec<f32>, String> {
        let started = *self.started.get_or_insert_with(Instant::now);
        let due = started + FRAME_DURATION * (self.frames as u32);
        let now = Instant::now();
        if due > now {
            std::thread::sleep(due - now);
        }
        let first = self.frames * FRAME_SAMPLES as u64;
        self.frames += 1;
        let mut out = Vec::with_capacity(FRAME_LEN);
        for i in 0..FRAME_SAMPLES as u64 {
            let t = (first + i) as f32 / SAMPLE_RATE as f32;
            let v = (2.0 * std::f32::consts::PI * self.freq * t).sin() * 0.5;
            out.push(v);
            out.push(v);
        }
        Ok(out)
    }
}

// ------------------------------------------------------- pure conversion

/// Appends `input` (`channels` interleaved) to `out` as stereo. Mono is doubled, more than
/// two channels keep the first two.
pub fn to_stereo(input: &[f32], channels: usize, out: &mut Vec<f32>) {
    match channels {
        0 => {}
        1 => {
            for &s in input {
                out.push(s);
                out.push(s);
            }
        }
        2 => out.extend_from_slice(input),
        n => {
            for frame in input.chunks_exact(n) {
                out.push(frame[0]);
                out.push(frame[1]);
            }
        }
    }
}

/// Streaming linear resampler for interleaved stereo. Good enough for system sound; it
/// keeps its position between calls so block boundaries do not click.
pub struct Resampler {
    step: f64,
    pos: f64,
    last: [f32; 2],
    passthrough: bool,
}

impl Resampler {
    pub fn new(from: u32, to: u32) -> Self {
        Self {
            step: f64::from(from) / f64::from(to),
            pos: 0.0,
            last: [0.0; 2],
            passthrough: from == to,
        }
    }

    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if self.passthrough {
            out.extend_from_slice(input);
            return;
        }
        let frames = input.len() / 2;
        if frames == 0 {
            return;
        }
        let at = |i: i64, c: usize| -> f32 {
            if i < 0 {
                self.last[c]
            } else {
                input[i as usize * 2 + c]
            }
        };
        let mut produced = Vec::new();
        while self.pos < (frames - 1) as f64 {
            let i0 = self.pos.floor() as i64;
            let frac = (self.pos - i0 as f64) as f32;
            for c in 0..2 {
                let a = at(i0, c);
                let b = at(i0 + 1, c);
                produced.push(a + (b - a) * frac);
            }
            self.pos += self.step;
        }
        out.extend_from_slice(&produced);
        self.pos -= frames as f64;
        self.last = [input[(frames - 1) * 2], input[(frames - 1) * 2 + 1]];
    }
}

/// Collects samples and hands them back in whole frames.
#[derive(Default)]
pub struct Chunker {
    buf: Vec<f32>,
}

impl Chunker {
    pub fn push(&mut self, samples: &[f32]) {
        self.buf.extend_from_slice(samples);
    }

    pub fn pop(&mut self) -> Option<Vec<f32>> {
        if self.buf.len() < FRAME_LEN {
            return None;
        }
        Some(self.buf.drain(..FRAME_LEN).collect())
    }
}

// ----------------------------------------------------------------- opus

/// Opus at 48 kHz stereo, tuned for music and system sound.
pub struct OpusEncoder {
    inner: opus::Encoder,
}

impl OpusEncoder {
    pub fn new(bitrate_bps: i32) -> Result<Self, String> {
        let mut inner = opus::Encoder::new(
            SAMPLE_RATE,
            opus::Channels::Stereo,
            opus::Application::Audio,
        )
        .map_err(|e| format!("could not start the Opus encoder: {e}"))?;
        inner
            .set_bitrate(opus::Bitrate::Bits(bitrate_bps))
            .map_err(|e| format!("could not set the Opus bitrate: {e}"))?;
        Ok(Self { inner })
    }

    /// One frame of `FRAME_LEN` interleaved samples in, one Opus packet out.
    pub fn encode(&mut self, frame: &[f32]) -> Result<Vec<u8>, String> {
        if frame.len() != FRAME_LEN {
            return Err(format!("expected {FRAME_LEN} samples, got {}", frame.len()));
        }
        let mut out = vec![0u8; 1500];
        let n = self
            .inner
            .encode_float(frame, &mut out)
            .map_err(|e| format!("Opus encode failed: {e}"))?;
        out.truncate(n);
        Ok(out)
    }
}

// ------------------------------------------------------------- loopback

/// Captures what the computer is playing. The sound device is opened on its own thread,
/// because on some platforms the stream object cannot move between threads.
pub struct LoopbackSource {
    rx: Receiver<Vec<f32>>,
    stop: Arc<AtomicBool>,
    idle: bool,
}

impl LoopbackSource {
    pub fn new() -> Result<Self, String> {
        let (tx, rx) = sync_channel::<Vec<f32>>(50); // about a second of slack
        let (ready_tx, ready_rx) = sync_channel::<Result<(), String>>(1);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        std::thread::Builder::new()
            .name("tether-audio-capture".into())
            .spawn(move || match open_loopback(tx) {
                Ok(stream) => {
                    let _ = ready_tx.send(Ok(()));
                    while !thread_stop.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    drop(stream);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            })
            .map_err(|e| format!("could not start audio capture: {e}"))?;
        ready_rx
            .recv_timeout(Duration::from_secs(10))
            .map_err(|_| "audio capture took too long to start".to_owned())??;
        Ok(Self {
            rx,
            stop,
            idle: false,
        })
    }
}

impl Drop for LoopbackSource {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

impl AudioSource for LoopbackSource {
    fn next_frame(&mut self) -> Result<Vec<f32>, String> {
        // Some systems deliver nothing at all while nothing is playing. Keep the stream's
        // timeline going with silence instead of stalling.
        let wait = if self.idle {
            FRAME_DURATION
        } else {
            FRAME_DURATION * 3
        };
        match self.rx.recv_timeout(wait) {
            Ok(frame) => {
                self.idle = false;
                Ok(frame)
            }
            Err(RecvTimeoutError::Timeout) => {
                self.idle = true;
                Ok(vec![0.0; FRAME_LEN])
            }
            Err(RecvTimeoutError::Disconnected) => Err("audio capture stopped".to_owned()),
        }
    }
}

fn pick_device() -> Result<cpal::Device, String> {
    let host = cpal::default_host();
    #[cfg(target_os = "linux")]
    {
        // PulseAudio and PipeWire expose what is playing as a "monitor" input.
        let found = host
            .input_devices()
            .map_err(|e| format!("could not list sound devices: {e}"))?
            .find(|d| {
                d.description()
                    .map(|n| n.name().to_lowercase().contains("monitor"))
                    .unwrap_or(false)
            });
        found
            .ok_or_else(|| "no \"monitor\" sound input found to capture what is playing".to_owned())
    }
    #[cfg(not(target_os = "linux"))]
    {
        host.default_output_device()
            .ok_or_else(|| "no sound output device to capture".to_owned())
    }
}

fn open_loopback(tx: SyncSender<Vec<f32>>) -> Result<Stream, String> {
    let device = pick_device()?;
    // On Windows and macOS, asking an output device for an input stream is how loopback is
    // requested, so the output configuration is the one to use there.
    #[cfg(target_os = "linux")]
    let supported = device
        .default_input_config()
        .map_err(|e| format!("could not read the sound device settings: {e}"))?;
    #[cfg(not(target_os = "linux"))]
    let supported = device
        .default_output_config()
        .map_err(|e| format!("could not read the sound device settings: {e}"))?;
    let format = supported.sample_format();
    let config: StreamConfig = supported.config();
    let stream = match format {
        SampleFormat::F32 => build::<f32>(&device, &config, tx),
        SampleFormat::I16 => build::<i16>(&device, &config, tx),
        SampleFormat::U16 => build::<u16>(&device, &config, tx),
        SampleFormat::I32 => build::<i32>(&device, &config, tx),
        other => Err(format!("unsupported sound format {other:?}")),
    }?;
    stream
        .play()
        .map_err(|e| format!("could not start capturing sound: {e}"))?;
    Ok(stream)
}

fn build<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    tx: SyncSender<Vec<f32>>,
) -> Result<Stream, String>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels);
    let mut resampler = Resampler::new(config.sample_rate, SAMPLE_RATE);
    let mut chunker = Chunker::default();
    let (mut converted, mut stereo, mut resampled) = (Vec::new(), Vec::new(), Vec::new());
    device
        .build_input_stream(
            config,
            move |data: &[T], _| {
                converted.clear();
                converted.extend(data.iter().map(|s| f32::from_sample(*s)));
                stereo.clear();
                to_stereo(&converted, channels, &mut stereo);
                resampled.clear();
                resampler.process(&stereo, &mut resampled);
                chunker.push(&resampled);
                while let Some(frame) = chunker.pop() {
                    // Full means the encoder is behind; dropping a frame beats growing a lag.
                    let _ = tx.try_send(frame);
                }
            },
            |e| tracing::warn!("audio capture error: {e}"),
            None,
        )
        .map_err(|e| format!("could not capture system sound: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How strongly `freq` is present in interleaved stereo `pcm` (left channel), relative
    /// to the signal's overall energy. Near 1.0 for a pure tone, near 0 for anything else.
    pub(crate) fn tone_share(pcm: &[f32], freq: f32, rate: f32) -> f32 {
        let left: Vec<f32> = pcm.iter().step_by(2).copied().collect();
        let (mut re, mut im, mut total) = (0.0f32, 0.0f32, 0.0f32);
        for (i, &s) in left.iter().enumerate() {
            let a = 2.0 * std::f32::consts::PI * freq * i as f32 / rate;
            re += s * a.cos();
            im += s * a.sin();
            total += s * s;
        }
        if total == 0.0 {
            return 0.0;
        }
        let n = left.len() as f32;
        let amp_sq = 4.0 * (re * re + im * im) / (n * n); // amplitude squared of that tone
        (amp_sq / 2.0) / (total / n)
    }

    fn sine(freq: f32, rate: u32, frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let v = (2.0 * std::f32::consts::PI * freq * i as f32 / rate as f32).sin() * 0.5;
                [v, v]
            })
            .collect()
    }

    #[test]
    fn stereo_conversion() {
        let mut out = Vec::new();
        to_stereo(&[1.0, 2.0], 1, &mut out);
        assert_eq!(out, [1.0, 1.0, 2.0, 2.0]);
        out.clear();
        to_stereo(&[1.0, 2.0, 3.0, 4.0], 2, &mut out);
        assert_eq!(out, [1.0, 2.0, 3.0, 4.0]);
        out.clear();
        to_stereo(&[1.0, 2.0, 9.0, 3.0, 4.0, 9.0], 3, &mut out);
        assert_eq!(out, [1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn chunker_hands_back_whole_frames() {
        let mut c = Chunker::default();
        c.push(&vec![0.5; FRAME_LEN - 1]);
        assert!(c.pop().is_none());
        c.push(&[0.5; 4]); // now one frame and three samples
        assert_eq!(c.pop().unwrap().len(), FRAME_LEN);
        assert!(c.pop().is_none());
        c.push(&vec![0.5; FRAME_LEN - 3]);
        assert!(c.pop().is_some());
        assert!(c.pop().is_none());
    }

    #[test]
    fn resampling_keeps_the_pitch_and_the_length() {
        // 44.1 kHz to 48 kHz, fed in awkward block sizes.
        let input = sine(1000.0, 44_100, 44_100);
        let mut r = Resampler::new(44_100, 48_000);
        let mut out = Vec::new();
        for block in input.chunks(2 * 441 + 6) {
            r.process(block, &mut out);
        }
        let frames = out.len() / 2;
        assert!((47_900..=48_000).contains(&frames), "{frames} frames");
        assert!(tone_share(&out, 1000.0, 48_000.0) > 0.95);
        assert!(tone_share(&out, 3000.0, 48_000.0) < 0.01);
        // Passing through at the same rate changes nothing.
        let mut same = Vec::new();
        Resampler::new(48_000, 48_000).process(&[0.1, 0.2, 0.3, 0.4], &mut same);
        assert_eq!(same, [0.1, 0.2, 0.3, 0.4]);
    }

    #[test]
    fn opus_round_trip_keeps_the_tone() {
        let mut enc = OpusEncoder::new(96_000).unwrap();
        let mut dec = opus::Decoder::new(SAMPLE_RATE, opus::Channels::Stereo).unwrap();
        let input = sine(1000.0, SAMPLE_RATE, FRAME_SAMPLES * 50); // one second
        let mut pcm = Vec::new();
        let mut bytes = 0;
        for frame in input.chunks(FRAME_LEN) {
            let packet = enc.encode(frame).unwrap();
            bytes += packet.len();
            let mut out = vec![0f32; FRAME_LEN];
            let n = dec.decode_float(&packet, &mut out, false).unwrap();
            assert_eq!(n, FRAME_SAMPLES);
            pcm.extend_from_slice(&out);
        }
        // Skip the first frames while the codec settles.
        let settled = &pcm[FRAME_LEN * 5..];
        assert!(tone_share(settled, 1000.0, 48_000.0) > 0.9);
        assert!(bytes < 96_000 / 8 * 2, "{bytes} bytes for one second");
        assert!(enc.encode(&[0.0; 10]).is_err());
    }

    #[test]
    fn the_tone_source_runs_in_real_time() {
        let mut t = ToneSource::new(440.0);
        let start = Instant::now();
        let mut pcm = Vec::new();
        for _ in 0..10 {
            pcm.extend(t.next_frame().unwrap());
        }
        let took = start.elapsed();
        assert!(took >= Duration::from_millis(170), "{took:?}");
        assert!(took < Duration::from_millis(400), "{took:?}");
        assert!(tone_share(&pcm, 440.0, 48_000.0) > 0.95);
    }

    #[test]
    fn parses_audio_specs() {
        assert_eq!("system".parse(), Ok(AudioSpec::System));
        assert_eq!("tone".parse(), Ok(AudioSpec::Tone));
        assert!("mic".parse::<AudioSpec>().is_err());
    }
}
