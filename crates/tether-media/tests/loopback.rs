//! A pure Rust WebRTC viewer talks to the host over loopback. No browser involved, so this
//! proves the host's SDP, ICE, DTLS and RTP path on its own.

use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tether_core::media::MediaProvider;
use tether_core::{proto::Platform, session::PeerInfo};
use tether_media::{AudioSpec, MediaOptions, VideoHost};
use webrtc::{
    api::{
        interceptor_registry::register_default_interceptors, media_engine::MediaEngine,
        setting_engine::SettingEngine, APIBuilder,
    },
    ice::network_type::NetworkType,
    interceptor::registry::Registry,
    peer_connection::{
        configuration::RTCConfiguration, sdp::session_description::RTCSessionDescription,
    },
    rtp_transceiver::{
        rtp_codec::RTPCodecType, rtp_transceiver_direction::RTCRtpTransceiverDirection,
        RTCRtpTransceiverInit,
    },
};

#[tokio::test]
async fn a_viewer_receives_rtp_and_the_guard_ends_the_session() {
    let host = VideoHost::test_pattern(MediaOptions {
        width: 640,
        height: 360,
        fps: 30,
        bitrate_kbps: 1000,
    });
    stream_then_stop(&host, None, 40).await;
}

/// Each display has its own connection, and several run at once on one host.
#[tokio::test]
async fn every_display_streams_on_its_own() {
    let host = VideoHost::test_pattern(MediaOptions::default());
    let listed = host.displays();
    assert_eq!(listed.len(), 2);
    assert_ne!(listed[0].id, listed[1].id);
    tokio::join!(
        stream_then_stop(&host, Some(listed[0].id), 40),
        stream_then_stop(&host, Some(listed[1].id), 40),
    );

    let peer = PeerInfo {
        name: "viewer".into(),
        platform: Platform::Linux,
        public_key: [7; 32],
    };
    let err = host
        .answer_display(&peer, 99, String::new())
        .await
        .err()
        .expect("unknown display is refused");
    assert!(err.contains("99"), "{err}");
}

/// The same path with a real display as the source. Needs an X display, so run it with
/// `xvfb-run -a cargo test -p tether-media --test loopback -- --include-ignored`.
#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "needs an X display, run with xvfb-run"]
async fn a_real_screen_streams_to_a_viewer() {
    stream_then_stop(&VideoHost::screen(MediaOptions::default(), None), None, 10).await;
}

/// `min_packets` is lower for a real screen: a still picture compresses to very little.
async fn stream_then_stop(host: &VideoHost, display: Option<u32>, min_packets: usize) {
    // Viewer side, built the way a browser behaves: it offers a receive-only video line.
    let mut media = MediaEngine::default();
    media.register_default_codecs().unwrap();
    let registry = register_default_interceptors(Registry::new(), &mut media).unwrap();
    let mut settings = SettingEngine::default();
    settings.set_network_types(vec![NetworkType::Udp4]);
    settings.set_include_loopback_candidate(true);
    let api = APIBuilder::new()
        .with_media_engine(media)
        .with_interceptor_registry(registry)
        .with_setting_engine(settings)
        .build();
    let viewer = Arc::new(
        api.new_peer_connection(RTCConfiguration::default())
            .await
            .unwrap(),
    );
    viewer
        .add_transceiver_from_kind(
            RTPCodecType::Video,
            Some(RTCRtpTransceiverInit {
                direction: RTCRtpTransceiverDirection::Recvonly,
                send_encodings: vec![],
            }),
        )
        .await
        .unwrap();

    let packets = Arc::new(AtomicUsize::new(0));
    {
        let packets = packets.clone();
        viewer.on_track(Box::new(move |track, _, _| {
            let packets = packets.clone();
            tokio::spawn(async move {
                while track.read_rtp().await.is_ok() {
                    packets.fetch_add(1, Ordering::Relaxed);
                }
            });
            Box::pin(async {})
        }));
    }

    let offer = viewer.create_offer(None).await.unwrap();
    let mut gathered = viewer.gathering_complete_promise().await;
    viewer.set_local_description(offer).await.unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(5), gathered.recv()).await;
    let offer_sdp = viewer.local_description().await.unwrap().sdp;

    let peer = PeerInfo {
        name: "viewer".into(),
        platform: Platform::Linux,
        public_key: [7; 32],
    };
    let answer = match display {
        None => host.answer(&peer, offer_sdp).await,
        Some(id) => host.answer_display(&peer, id, offer_sdp).await,
    }
    .expect("host answers");
    assert!(
        answer.sdp.contains("H264"),
        "answer should negotiate H.264:\n{}",
        answer.sdp
    );
    viewer
        .set_remote_description(RTCSessionDescription::answer(answer.sdp).unwrap())
        .await
        .unwrap();

    // Wait for real media to flow.
    let mut seen = 0;
    for _ in 0..150 {
        seen = packets.load(Ordering::Relaxed);
        if seen >= min_packets {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(seen >= min_packets, "only {seen} RTP packets arrived");

    // Ending the session stops the flow.
    drop(answer.guard);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let after_stop = packets.load(Ordering::Relaxed);
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert_eq!(
        packets.load(Ordering::Relaxed),
        after_stop,
        "video kept flowing after the guard dropped"
    );
}

// ---------------------------------------------------------------------------- audio

struct Watched {
    video_packets: usize,
    audio: Vec<Vec<u8>>,
    answer_sdp: String,
}

/// Connects a viewer (asking for sound or not) and watches for about `secs` seconds.
async fn watch(host: &VideoHost, want_audio: bool, secs: u64) -> Watched {
    use std::sync::Mutex;
    use webrtc::track::track_remote::TrackRemote;

    let mut media = MediaEngine::default();
    media.register_default_codecs().unwrap();
    let registry = register_default_interceptors(Registry::new(), &mut media).unwrap();
    let mut settings = SettingEngine::default();
    settings.set_network_types(vec![NetworkType::Udp4]);
    settings.set_include_loopback_candidate(true);
    let api = APIBuilder::new()
        .with_media_engine(media)
        .with_interceptor_registry(registry)
        .with_setting_engine(settings)
        .build();
    let viewer = Arc::new(
        api.new_peer_connection(RTCConfiguration::default())
            .await
            .unwrap(),
    );
    let recv_only = || {
        Some(RTCRtpTransceiverInit {
            direction: RTCRtpTransceiverDirection::Recvonly,
            send_encodings: vec![],
        })
    };
    viewer
        .add_transceiver_from_kind(RTPCodecType::Video, recv_only())
        .await
        .unwrap();
    if want_audio {
        viewer
            .add_transceiver_from_kind(RTPCodecType::Audio, recv_only())
            .await
            .unwrap();
    }

    let video_packets = Arc::new(AtomicUsize::new(0));
    let audio = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    {
        let (video_packets, audio) = (video_packets.clone(), audio.clone());
        viewer.on_track(Box::new(move |track: Arc<TrackRemote>, _, _| {
            let (video_packets, audio) = (video_packets.clone(), audio.clone());
            tokio::spawn(async move {
                let is_audio = track.kind() == RTPCodecType::Audio;
                while let Ok((packet, _)) = track.read_rtp().await {
                    if is_audio {
                        audio.lock().unwrap().push(packet.payload.to_vec());
                    } else {
                        video_packets.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
            Box::pin(async {})
        }));
    }

    let offer = viewer.create_offer(None).await.unwrap();
    let mut gathered = viewer.gathering_complete_promise().await;
    viewer.set_local_description(offer).await.unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(5), gathered.recv()).await;
    let offer_sdp = viewer.local_description().await.unwrap().sdp;

    let peer = PeerInfo {
        name: "viewer".into(),
        platform: Platform::Linux,
        public_key: [7; 32],
    };
    let answer = host.answer(&peer, offer_sdp).await.expect("host answers");
    viewer
        .set_remote_description(RTCSessionDescription::answer(answer.sdp.clone()).unwrap())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(secs)).await;
    drop(answer.guard);
    let audio = audio.lock().unwrap().clone();
    Watched {
        video_packets: video_packets.load(Ordering::Relaxed),
        audio,
        answer_sdp: answer.sdp,
    }
}

/// Share of the signal's energy at `freq` (left channel of interleaved stereo).
fn tone_share(pcm: &[f32], freq: f32) -> f32 {
    let left: Vec<f32> = pcm.iter().step_by(2).copied().collect();
    let (mut re, mut im, mut total) = (0.0f32, 0.0f32, 0.0f32);
    for (i, &s) in left.iter().enumerate() {
        let a = 2.0 * std::f32::consts::PI * freq * i as f32 / 48_000.0;
        re += s * a.cos();
        im += s * a.sin();
        total += s * s;
    }
    let n = left.len() as f32;
    if total == 0.0 {
        return 0.0;
    }
    (4.0 * (re * re + im * im) / (n * n) / 2.0) / (total / n)
}

#[tokio::test]
async fn a_viewer_that_asks_for_sound_hears_the_tone() {
    let host = VideoHost::test_pattern(MediaOptions::default()).with_audio(AudioSpec::Tone);
    let got = watch(&host, true, 4).await;

    assert!(got.answer_sdp.contains("opus"), "{}", got.answer_sdp);
    assert!(
        got.video_packets > 20,
        "video still flows: {}",
        got.video_packets
    );
    // Four seconds at 50 packets a second, less set up time and a little loss.
    assert!(
        got.audio.len() > 80,
        "only {} audio packets",
        got.audio.len()
    );

    let mut dec = opus::Decoder::new(48_000, opus::Channels::Stereo).unwrap();
    let mut pcm = Vec::new();
    for packet in &got.audio {
        let mut out = vec![0f32; 1920];
        let n = dec.decode_float(packet, &mut out, false).unwrap();
        assert_eq!(n, 960);
        pcm.extend_from_slice(&out);
    }
    let settled = &pcm[1920 * 10..];
    assert!(
        tone_share(settled, 440.0) > 0.85,
        "the 440 Hz tone is not what arrived"
    );
}

#[tokio::test]
async fn a_video_only_viewer_gets_no_sound() {
    let host = VideoHost::test_pattern(MediaOptions::default()).with_audio(AudioSpec::Tone);
    let got = watch(&host, false, 3).await;
    assert!(got.video_packets > 20, "video: {}", got.video_packets);
    assert!(got.audio.is_empty());
    assert!(!got.answer_sdp.contains("opus"), "{}", got.answer_sdp);
}

#[tokio::test]
async fn a_host_without_sound_ignores_a_request_for_it() {
    let host = VideoHost::test_pattern(MediaOptions::default());
    let got = watch(&host, true, 3).await;
    assert!(got.video_packets > 20, "video: {}", got.video_packets);
    assert!(got.audio.is_empty());
}
