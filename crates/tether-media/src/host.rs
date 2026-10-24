use crate::audio::{AudioSource, AudioSpec, OpusEncoder, FRAME_DURATION};
use crate::{
    encoder::H264Encoder,
    source::{Frame, FrameSource, TestPattern},
};
use bytes::Bytes;
use std::{
    fmt::Display,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tether_core::{
    media::{MediaAnswer, MediaFuture, MediaProvider},
    proto::DisplayDesc,
    session::PeerInfo,
};
use tokio::{sync::mpsc, time::timeout};
use webrtc::{
    api::{
        interceptor_registry::register_default_interceptors,
        media_engine::{MediaEngine, MIME_TYPE_H264, MIME_TYPE_OPUS},
        setting_engine::SettingEngine,
        APIBuilder,
    },
    ice::network_type::NetworkType,
    interceptor::registry::Registry,
    media::Sample,
    peer_connection::{
        configuration::RTCConfiguration, peer_connection_state::RTCPeerConnectionState,
        sdp::session_description::RTCSessionDescription, RTCPeerConnection,
    },
    rtcp::payload_feedbacks::{
        full_intra_request::FullIntraRequest, picture_loss_indication::PictureLossIndication,
    },
    rtp_transceiver::{
        rtp_codec::{RTCRtpCodecCapability, RTCRtpCodecParameters, RTPCodecType},
        rtp_transceiver_direction::RTCRtpTransceiverDirection,
        RTCPFeedback, RTCRtpTransceiverInit,
    },
    track::track_local::{track_local_static_sample::TrackLocalStaticSample, TrackLocal},
};

const GATHER_TIMEOUT: Duration = Duration::from_secs(5);
const H264_FMTP: &str = "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f";

/// Opus is always 48 kHz and two channels on the wire.
fn opus_capability() -> RTCRtpCodecCapability {
    RTCRtpCodecCapability {
        mime_type: MIME_TYPE_OPUS.to_owned(),
        clock_rate: 48_000,
        channels: 2,
        sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
        rtcp_feedback: vec![],
    }
}

/// Plenty for stereo system sound.
const AUDIO_BITRATE_BPS: i32 = 96_000;

fn h264_capability() -> RTCRtpCodecCapability {
    RTCRtpCodecCapability {
        mime_type: MIME_TYPE_H264.to_owned(),
        clock_rate: 90_000,
        channels: 0,
        sdp_fmtp_line: H264_FMTP.to_owned(),
        rtcp_feedback: vec![
            RTCPFeedback {
                typ: "nack".to_owned(),
                parameter: String::new(),
            },
            RTCPFeedback {
                typ: "nack".to_owned(),
                parameter: "pli".to_owned(),
            },
            RTCPFeedback {
                typ: "ccm".to_owned(),
                parameter: "fir".to_owned(),
            },
        ],
    }
}

fn msg(e: impl Display) -> String {
    e.to_string()
}

#[derive(Debug, Clone)]
pub struct MediaOptions {
    pub fps: u32,
    pub bitrate_kbps: u32,
    /// Only used by the test pattern; screen capture sizes itself.
    pub width: usize,
    pub height: usize,
}

impl Default for MediaOptions {
    fn default() -> Self {
        Self {
            fps: 30,
            bitrate_kbps: 2000,
            width: 960,
            height: 540,
        }
    }
}

/// Which picture source to stream, as typed on the command line or chosen in the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoSpec {
    TestPattern,
    /// A display id from [`crate::list_displays`], or the primary display.
    Screen(Option<u32>),
}

impl std::str::FromStr for VideoSpec {
    type Err = String;

    /// `test-pattern`, `screen` (the primary display) or `screen:<id>`.
    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim() {
            "test-pattern" => Ok(Self::TestPattern),
            "screen" => Ok(Self::Screen(None)),
            other => match other.strip_prefix("screen:") {
                Some(id) => id
                    .parse::<u32>()
                    .map(|id| Self::Screen(Some(id)))
                    .map_err(|_| {
                        format!("bad display id {id:?}; run `tether displays` to list them")
                    }),
                None => Err(format!(
                    "unknown video source {other:?}; use test-pattern, screen or screen:<id>"
                )),
            },
        }
    }
}

type AudioFactory = Arc<dyn Fn() -> Result<Box<dyn AudioSource>, String> + Send + Sync>;
type DisplaySourceFactory = Arc<dyn Fn(u32) -> Result<Box<dyn FrameSource>, String> + Send + Sync>;
type SourceFactory = Arc<dyn Fn() -> Result<Box<dyn FrameSource>, String> + Send + Sync>;

/// The test pattern pretends to be two screens side by side, so multi window viewing and
/// pointer routing can be tried without two monitors.
fn test_display_size(id: u32) -> Option<(usize, usize)> {
    match id {
        1 => Some((640, 360)),
        2 => Some((480, 270)),
        _ => None,
    }
}

fn test_displays() -> Vec<DisplayDesc> {
    [(1u32, 0i32, true), (2, 640, false)]
        .into_iter()
        .map(|(id, x, primary)| {
            let (w, h) = test_display_size(id).unwrap_or((0, 0));
            DisplayDesc {
                id,
                name: format!("Test pattern {id}"),
                width: w as u32,
                height: h as u32,
                x,
                y: 0,
                primary,
            }
        })
        .collect()
}

/// Streams video from a [`FrameSource`] to whoever offers to watch.
pub struct VideoHost {
    make_source: SourceFactory,
    opts: MediaOptions,
    /// The displays a viewer can ask for one by one, and how to open one.
    list_displays: Arc<dyn Fn() -> Vec<DisplayDesc> + Send + Sync>,
    display_source: DisplaySourceFactory,
    /// Where system sound comes from. `None` means this host sends no audio.
    audio: Option<AudioFactory>,
}

impl VideoHost {
    /// Streams the built in test pattern.
    pub fn test_pattern(opts: MediaOptions) -> Self {
        let (w, h) = (opts.width, opts.height);
        Self::with_source(opts, move || Ok(Box::new(TestPattern::new(w, h)?))).with_displays(
            test_displays,
            |id| {
                let (w, h) = test_display_size(id).ok_or_else(|| format!("no display {id}"))?;
                Ok(Box::new(TestPattern::new(w, h)?))
            },
        )
    }

    /// Streams whatever `spec` names.
    pub fn from_spec(spec: VideoSpec, opts: MediaOptions) -> Self {
        match spec {
            VideoSpec::TestPattern => Self::test_pattern(opts),
            VideoSpec::Screen(display) => Self::screen(opts, display),
        }
    }

    /// Streams a real display (an id from [`crate::list_displays`], or `None` for the
    /// primary one). The source is opened per viewer, so a bad choice is reported when
    /// someone tries to watch.
    pub fn screen(opts: MediaOptions, display: Option<u32>) -> Self {
        Self::with_source(opts, move || {
            Ok(Box::new(crate::capture::ScreenSource::new(
                display,
                crate::capture::DEFAULT_MAX_SIZE,
            )?))
        })
        .with_displays(
            || {
                crate::capture::list_displays()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|d| DisplayDesc {
                        id: d.id,
                        name: d.name,
                        width: d.width,
                        height: d.height,
                        x: d.x,
                        y: d.y,
                        primary: d.primary,
                    })
                    .collect()
            },
            |id| {
                Ok(Box::new(crate::capture::ScreenSource::new(
                    Some(id),
                    crate::capture::DEFAULT_MAX_SIZE,
                )?))
            },
        )
    }

    /// Streams any source. `make_source` runs once per viewer.
    pub fn with_source(
        opts: MediaOptions,
        make_source: impl Fn() -> Result<Box<dyn FrameSource>, String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            make_source: Arc::new(make_source),
            opts,
            list_displays: Arc::new(Vec::new),
            display_source: Arc::new(|_| Err("this host cannot share displays one by one".into())),
            audio: None,
        }
    }

    /// Adds sound to the stream. It is only sent to viewers that ask for it (their offer has
    /// an audio line), so existing video only viewers are unaffected.
    pub fn with_audio(mut self, spec: AudioSpec) -> Self {
        self.audio = Some(Arc::new(move || spec.open()));
        self
    }

    /// Like [`VideoHost::with_audio`], with any source. Used by tests.
    pub fn with_audio_source(
        mut self,
        open: impl Fn() -> Result<Box<dyn AudioSource>, String> + Send + Sync + 'static,
    ) -> Self {
        self.audio = Some(Arc::new(open));
        self
    }

    /// Lets viewers ask for each display separately: `list` says which exist, `open` makes
    /// the source for one of them.
    pub fn with_displays(
        mut self,
        list: impl Fn() -> Vec<DisplayDesc> + Send + Sync + 'static,
        open: impl Fn(u32) -> Result<Box<dyn FrameSource>, String> + Send + Sync + 'static,
    ) -> Self {
        self.list_displays = Arc::new(list);
        self.display_source = Arc::new(open);
        self
    }

    async fn start(
        &self,
        offer: String,
        source: Box<dyn FrameSource>,
    ) -> Result<(String, ViewerGuard), String> {
        let encoder = H264Encoder::new(self.opts.fps, self.opts.bitrate_kbps)?;
        // Sound only if this host has it and the viewer asked for it.
        let audio = match &self.audio {
            Some(open) if offer.lines().any(|l| l.starts_with("m=audio")) => {
                Some((open()?, OpusEncoder::new(AUDIO_BITRATE_BPS)?))
            }
            _ => None,
        };

        let mut media = MediaEngine::default();
        // H.264 only: the answer then offers exactly the codec we encode.
        media
            .register_codec(
                RTCRtpCodecParameters {
                    capability: h264_capability(),
                    payload_type: 102,
                    ..Default::default()
                },
                RTPCodecType::Video,
            )
            .map_err(msg)?;
        if audio.is_some() {
            media
                .register_codec(
                    RTCRtpCodecParameters {
                        capability: opus_capability(),
                        payload_type: 111,
                        ..Default::default()
                    },
                    RTPCodecType::Audio,
                )
                .map_err(msg)?;
        }
        let registry = register_default_interceptors(Registry::new(), &mut media).map_err(msg)?;
        let mut settings = SettingEngine::default();
        settings.set_network_types(vec![NetworkType::Udp4]);
        settings.set_include_loopback_candidate(true);
        let api = APIBuilder::new()
            .with_media_engine(media)
            .with_interceptor_registry(registry)
            .with_setting_engine(settings)
            .build();

        let pc = Arc::new(
            api.new_peer_connection(RTCConfiguration::default())
                .await
                .map_err(msg)?,
        );
        let stop = Arc::new(AtomicBool::new(false));
        let guard = ViewerGuard {
            pc: pc.clone(),
            stop: stop.clone(),
        };

        let track = Arc::new(TrackLocalStaticSample::new(
            h264_capability(),
            "video".to_owned(),
            "tether".to_owned(),
        ));

        // Create the sending transceiver first. When the viewer's offer arrives, its video line
        // binds to this one. (`add_track` after the fact would make a second, unnegotiated
        // transceiver, and the viewer would never hear about the stream.)
        let transceiver = pc
            .add_transceiver_from_track(
                Arc::clone(&track) as Arc<dyn TrackLocal + Send + Sync>,
                Some(RTCRtpTransceiverInit {
                    direction: RTCRtpTransceiverDirection::Sendonly,
                    send_encodings: vec![],
                }),
            )
            .await
            .map_err(msg)?;

        // The audio sender is created first for the same reason as the video one.
        let audio_track = Arc::new(TrackLocalStaticSample::new(
            opus_capability(),
            "audio".to_owned(),
            "tether".to_owned(),
        ));
        let audio_transceiver = if audio.is_some() {
            Some(
                pc.add_transceiver_from_track(
                    Arc::clone(&audio_track) as Arc<dyn TrackLocal + Send + Sync>,
                    Some(RTCRtpTransceiverInit {
                        direction: RTCRtpTransceiverDirection::Sendonly,
                        send_encodings: vec![],
                    }),
                )
                .await
                .map_err(msg)?,
            )
        } else {
            None
        };

        pc.set_remote_description(
            RTCSessionDescription::offer(offer).map_err(|e| format!("bad offer: {e}"))?,
        )
        .await
        .map_err(|e| format!("could not use the offer: {e}"))?;

        let sender = transceiver.sender().await;

        // Viewers ask for a fresh keyframe after packet loss. Reading RTCP is also what
        // lets the interceptors (retransmission, reports) run.
        let want_key = Arc::new(AtomicBool::new(false));
        {
            let want_key = want_key.clone();
            let stop = stop.clone();
            tokio::spawn(async move {
                while !stop.load(Ordering::Relaxed) {
                    let Ok((packets, _)) = sender.read_rtcp().await else {
                        break;
                    };
                    for p in packets {
                        let any = p.as_any();
                        if any.is::<PictureLossIndication>() || any.is::<FullIntraRequest>() {
                            want_key.store(true, Ordering::Relaxed);
                        }
                    }
                }
            });
        }

        // The audio sender's RTCP has to be read too, or its reports pile up unread.
        if let Some(t) = audio_transceiver {
            let sender = t.sender().await;
            let stop = stop.clone();
            tokio::spawn(async move {
                while !stop.load(Ordering::Relaxed) {
                    if sender.read_rtcp().await.is_err() {
                        break;
                    }
                }
            });
        }

        // Start sending once the connection is up, stop when it is gone.
        {
            let stop = stop.clone();
            let started = Arc::new(AtomicBool::new(false));
            let pump = Mutex1::new(Some((source, encoder)));
            let audio_pump = Mutex1::new(audio);
            let opts = self.opts.clone();
            pc.on_peer_connection_state_change(Box::new(move |state| {
                match state {
                    RTCPeerConnectionState::Connected => {
                        if !started.swap(true, Ordering::SeqCst) {
                            if let Some((source, encoder)) = audio_pump.take() {
                                start_audio_pump(
                                    source,
                                    encoder,
                                    audio_track.clone(),
                                    stop.clone(),
                                );
                            }
                            if let Some((source, encoder)) = pump.take() {
                                start_pump(
                                    source,
                                    encoder,
                                    opts.fps,
                                    track.clone(),
                                    stop.clone(),
                                    want_key.clone(),
                                );
                            }
                        }
                    }
                    RTCPeerConnectionState::Disconnected
                    | RTCPeerConnectionState::Failed
                    | RTCPeerConnectionState::Closed => stop.store(true, Ordering::SeqCst),
                    _ => {}
                }
                Box::pin(async {})
            }));
        }

        let answer = pc.create_answer(None).await.map_err(msg)?;
        let mut gathered = pc.gathering_complete_promise().await;
        pc.set_local_description(answer).await.map_err(msg)?;
        let _ = timeout(GATHER_TIMEOUT, gathered.recv()).await;
        let local = pc
            .local_description()
            .await
            .ok_or_else(|| "no local description".to_owned())?;
        Ok((local.sdp, guard))
    }
}

impl MediaProvider for VideoHost {
    fn answer<'a>(&'a self, peer: &'a PeerInfo, offer: String) -> MediaFuture<'a> {
        Box::pin(async move {
            tracing::info!(viewer = %peer.name, "starting video");
            let source = (self.make_source)()?;
            let (sdp, guard) = self
                .start(offer, source)
                .await
                .map_err(|e| format!("could not start video: {e}"))?;
            Ok(MediaAnswer {
                sdp,
                guard: Box::new(guard),
            })
        })
    }

    fn displays(&self) -> Vec<DisplayDesc> {
        (self.list_displays)()
    }

    fn answer_display<'a>(&'a self, peer: &'a PeerInfo, id: u32, offer: String) -> MediaFuture<'a> {
        Box::pin(async move {
            tracing::info!(viewer = %peer.name, display_id = id, "starting video for one display");
            let source = (self.display_source)(id)?;
            let (sdp, guard) = self
                .start(offer, source)
                .await
                .map_err(|e| format!("could not start video: {e}"))?;
            Ok(MediaAnswer {
                sdp,
                guard: Box::new(guard),
            })
        })
    }
}

/// Ends the video session when dropped.
struct ViewerGuard {
    pc: Arc<RTCPeerConnection>,
    stop: Arc<AtomicBool>,
}

impl Drop for ViewerGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let pc = self.pc.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = pc.close().await;
            });
        }
    }
}

/// Holds a value until the first caller takes it. The state callback is `FnMut + Sync`, so
/// the one-shot hand off lives behind a lock.
struct Mutex1<T>(std::sync::Mutex<Option<T>>);

impl<T> Mutex1<T> {
    fn new(value: Option<T>) -> Self {
        Self(std::sync::Mutex::new(value))
    }

    fn take(&self) -> Option<T> {
        self.0.lock().expect("lock").take()
    }
}

/// Capture and encoding run on their own thread, paced to the frame rate. Finished samples
/// go to the async side over a small channel, which also gives backpressure.
fn start_pump(
    mut source: Box<dyn FrameSource>,
    mut encoder: H264Encoder,
    fps: u32,
    track: Arc<TrackLocalStaticSample>,
    stop: Arc<AtomicBool>,
    want_key: Arc<AtomicBool>,
) {
    let period = Duration::from_secs_f64(1.0 / f64::from(fps.max(1)));
    let (tx, mut rx) = mpsc::channel::<Bytes>(4);

    let thread_stop = stop.clone();
    let spawned = std::thread::Builder::new()
        .name("tether-video".into())
        .spawn(move || {
            let mut next = Instant::now();
            let mut first = true;
            while !thread_stop.load(Ordering::Relaxed) {
                let frame: Frame = match source.next_frame() {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::warn!("video source stopped: {e}");
                        break;
                    }
                };
                let key = first || want_key.swap(false, Ordering::Relaxed);
                first = false;
                match encoder.encode(&frame, key) {
                    Ok(data) if !data.is_empty() => {
                        if tx.blocking_send(Bytes::from(data)).is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!("{e}");
                        break;
                    }
                }
                next += period;
                let now = Instant::now();
                if next > now {
                    std::thread::sleep(next - now);
                } else {
                    next = now;
                }
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("could not start the video thread: {e}");
        return;
    }

    tokio::spawn(async move {
        while let Some(data) = rx.recv().await {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let sample = Sample {
                data,
                duration: period,
                ..Default::default()
            };
            if track.write_sample(&sample).await.is_err() {
                break;
            }
        }
        stop.store(true, Ordering::SeqCst);
    });
}

fn start_audio_pump(
    mut source: Box<dyn AudioSource>,
    mut encoder: OpusEncoder,
    track: Arc<TrackLocalStaticSample>,
    stop: Arc<AtomicBool>,
) {
    let (tx, mut rx) = mpsc::channel::<Bytes>(8);
    let thread_stop = stop.clone();
    let spawned = std::thread::Builder::new()
        .name("tether-audio".into())
        .spawn(move || {
            while !thread_stop.load(Ordering::Relaxed) {
                let frame = match source.next_frame() {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::warn!("audio source stopped: {e}");
                        break;
                    }
                };
                match encoder.encode(&frame) {
                    // Behind means the network is slower than the sound; skip rather than lag.
                    Ok(data) => {
                        if matches!(
                            tx.try_send(Bytes::from(data)),
                            Err(mpsc::error::TrySendError::Closed(_))
                        ) {
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::warn!("{e}");
                        break;
                    }
                }
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("could not start the audio thread: {e}");
        return;
    }
    tokio::spawn(async move {
        while let Some(data) = rx.recv().await {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let sample = Sample {
                data,
                duration: FRAME_DURATION,
                ..Default::default()
            };
            if track.write_sample(&sample).await.is_err() {
                break;
            }
        }
    });
}

#[cfg(test)]
mod spec_tests {
    use super::VideoSpec;

    #[test]
    fn parses_video_specs() {
        assert_eq!("test-pattern".parse(), Ok(VideoSpec::TestPattern));
        assert_eq!("screen".parse(), Ok(VideoSpec::Screen(None)));
        assert_eq!("screen:7".parse(), Ok(VideoSpec::Screen(Some(7))));
        assert!("screen:x".parse::<VideoSpec>().is_err());
        assert!("webcam".parse::<VideoSpec>().is_err());
    }
}
