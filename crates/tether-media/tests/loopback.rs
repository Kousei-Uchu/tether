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
use tether_media::{MediaOptions, VideoHost};
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
    stream_then_stop(host).await;
}

/// The same path with a real display as the source. Needs an X display, so run it with
/// `xvfb-run -a cargo test -p tether-media --test loopback -- --include-ignored`.
#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "needs an X display, run with xvfb-run"]
async fn a_real_screen_streams_to_a_viewer() {
    stream_then_stop(VideoHost::screen(MediaOptions::default(), None)).await;
}

async fn stream_then_stop(host: VideoHost) {
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
    let answer = host.answer(&peer, offer_sdp).await.expect("host answers");
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
        if seen >= 40 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(seen >= 40, "only {seen} RTP packets arrived");

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
