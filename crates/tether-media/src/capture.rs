//! Real screen capture as a [`FrameSource`], through the `xcap` crate: Windows Graphics
//! Capture / GDI on Windows, CoreGraphics on macOS, X11 or Wayland on Linux.
//!
//! This is the software path: pictures are grabbed as RGBA, scaled down if they are bigger
//! than the encoder can keep up with, and handed to OpenH264. Hardware capture and encode
//! (ScreenCaptureKit with VideoToolbox, Media Foundation with NVENC) sit behind the same
//! trait later.

use crate::source::{Frame, FrameSource};
use xcap::Monitor;

/// Biggest picture sent. Software H.264 cannot keep up with 4K at 30 fps, and a 1080p
/// stream is plenty for controlling a computer. Larger displays are scaled down, keeping
/// their shape.
pub const DEFAULT_MAX_SIZE: (usize, usize) = (1920, 1080);

/// One physical display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayInfo {
    pub id: u32,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

/// The displays that can be shared, primary first.
pub fn list_displays() -> Result<Vec<DisplayInfo>, String> {
    let mut out = Vec::new();
    for m in Monitor::all().map_err(|e| format!("could not list displays: {e}"))? {
        out.push(DisplayInfo {
            id: m.id().map_err(|e| e.to_string())?,
            name: m
                .friendly_name()
                .or_else(|_| m.name())
                .unwrap_or_else(|_| "display".to_owned()),
            width: m.width().map_err(|e| e.to_string())?,
            height: m.height().map_err(|e| e.to_string())?,
            primary: m.is_primary().unwrap_or(false),
        });
    }
    out.sort_by_key(|d| (!d.primary, d.id));
    Ok(out)
}

/// The size sent for a display of `src` pixels: scaled down to fit `max` if needed, never
/// scaled up, and always even in both directions (H.264 needs that).
pub fn target_size(src: (usize, usize), max: (usize, usize)) -> (usize, usize) {
    let (sw, sh) = (src.0.max(2), src.1.max(2));
    let scale = (max.0 as f64 / sw as f64)
        .min(max.1 as f64 / sh as f64)
        .min(1.0);
    let even = |v: f64| ((v.round() as usize) & !1).max(2);
    (even(sw as f64 * scale), even(sh as f64 * scale))
}

/// Converts RGBA to BGRA while scaling to `to` with nearest neighbour sampling. Fast and
/// good enough for text at 1080p; a nicer filter can come with the hardware path.
fn convert(rgba: &[u8], from: (usize, usize), to: (usize, usize)) -> Vec<u8> {
    let (fw, fh) = from;
    let (tw, th) = to;
    let mut out = vec![255u8; tw * th * 4];
    for y in 0..th {
        let sy = (y * fh / th).min(fh - 1);
        let src_row = &rgba[sy * fw * 4..(sy + 1) * fw * 4];
        let dst_row = &mut out[y * tw * 4..(y + 1) * tw * 4];
        for x in 0..tw {
            let sx = (x * fw / tw).min(fw - 1) * 4;
            let d = x * 4;
            dst_row[d] = src_row[sx + 2];
            dst_row[d + 1] = src_row[sx + 1];
            dst_row[d + 2] = src_row[sx];
        }
    }
    out
}

/// Captures one display. The size is fixed when the source is created; if the display
/// changes resolution mid stream, pictures are fitted to the original size.
pub struct ScreenSource {
    monitor: Monitor,
    size: (usize, usize),
}

impl ScreenSource {
    /// `display` is an id from [`list_displays`]; `None` means the primary display.
    pub fn new(display: Option<u32>, max: (usize, usize)) -> Result<Self, String> {
        let monitors = Monitor::all().map_err(|e| format!("could not list displays: {e}"))?;
        let chosen = match display {
            Some(id) => monitors
                .into_iter()
                .find(|m| m.id().ok() == Some(id))
                .ok_or_else(|| format!("no display with id {id}"))?,
            None => {
                let mut primary = None;
                let mut first = None;
                for m in monitors {
                    if m.is_primary().unwrap_or(false) && primary.is_none() {
                        primary = Some(m);
                    } else if first.is_none() {
                        first = Some(m);
                    }
                }
                primary
                    .or(first)
                    .ok_or_else(|| "no display found to share".to_owned())?
            }
        };
        // Capture once to learn the real size in pixels, which can differ from the
        // reported size on scaled displays.
        let probe = chosen.capture_image().map_err(capture_error)?;
        let size = target_size((probe.width() as usize, probe.height() as usize), max);
        Ok(Self {
            monitor: chosen,
            size,
        })
    }
}

fn capture_error(e: xcap::XCapError) -> String {
    let text = e.to_string();
    if cfg!(target_os = "macos") {
        format!("could not capture the screen: {text}. Allow Tether under System Settings, Privacy & Security, Screen Recording, then restart it")
    } else {
        format!("could not capture the screen: {text}")
    }
}

impl FrameSource for ScreenSource {
    fn size(&self) -> (usize, usize) {
        self.size
    }

    fn next_frame(&mut self) -> Result<Frame, String> {
        let img = self.monitor.capture_image().map_err(capture_error)?;
        let from = (img.width() as usize, img.height() as usize);
        Ok(Frame {
            width: self.size.0,
            height: self.size.1,
            bgra: convert(img.as_raw(), from, self.size),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_displays_are_not_scaled_up() {
        assert_eq!(target_size((1280, 720), DEFAULT_MAX_SIZE), (1280, 720));
    }

    #[test]
    fn big_displays_shrink_keeping_their_shape() {
        assert_eq!(target_size((3840, 2160), DEFAULT_MAX_SIZE), (1920, 1080));
        // 16:10 laptop panel
        assert_eq!(target_size((2880, 1800), DEFAULT_MAX_SIZE), (1728, 1080));
        // ultrawide is limited by width
        assert_eq!(target_size((5120, 1440), DEFAULT_MAX_SIZE), (1920, 540));
    }

    #[test]
    fn sizes_are_always_even() {
        for w in [1023usize, 1365, 2561] {
            for h in [767usize, 1001] {
                let (tw, th) = target_size((w, h), DEFAULT_MAX_SIZE);
                assert!(tw % 2 == 0 && th % 2 == 0, "{w}x{h} -> {tw}x{th}");
            }
        }
    }

    #[test]
    fn convert_swaps_channels_and_scales() {
        // 2x2: red, green / blue, white (RGBA)
        let rgba = [
            255, 0, 0, 255, 0, 255, 0, 255, //
            0, 0, 255, 255, 255, 255, 255, 255,
        ];
        let same = convert(&rgba, (2, 2), (2, 2));
        assert_eq!(&same[0..4], &[0, 0, 255, 255]); // red as BGRA
        assert_eq!(&same[8..12], &[255, 0, 0, 255]); // blue as BGRA
        let big = convert(&rgba, (2, 2), (4, 4));
        assert_eq!(&big[0..4], &big[4..8]); // each source pixel covers 2x2
        assert_eq!(big.len(), 4 * 4 * 4);
    }
}

/// Needs a real display, so it only runs with `--include-ignored` (CI uses xvfb-run on
/// Linux). It opens a red window over the whole screen and checks that red comes back
/// through capture, scaling and channel order.
#[cfg(all(test, target_os = "linux"))]
mod live_tests {
    use super::*;
    use xcb::{x, Xid};

    fn paint_screen_red() -> xcb::Connection {
        let (conn, n) = xcb::Connection::connect(None).expect("connect to the X display");
        let screen = conn.get_setup().roots().nth(n as usize).expect("screen");
        let win: x::Window = conn.generate_id();
        conn.send_request(&x::CreateWindow {
            depth: x::COPY_FROM_PARENT as u8,
            wid: win,
            parent: screen.root(),
            x: 0,
            y: 0,
            width: screen.width_in_pixels(),
            height: screen.height_in_pixels(),
            border_width: 0,
            class: x::WindowClass::InputOutput,
            visual: screen.root_visual(),
            value_list: &[x::Cw::BackPixel(0x00ff_0000), x::Cw::OverrideRedirect(true)],
        });
        conn.send_request(&x::MapWindow { window: win });
        conn.flush().expect("flush");
        // Round trip so the server has drawn it before we capture.
        conn.wait_for_reply(conn.send_request(&x::GetInputFocus {}))
            .expect("sync");
        assert_ne!(win.resource_id(), 0);
        conn
    }

    #[test]
    #[ignore = "needs an X display, run with xvfb-run"]
    fn captures_a_real_display() {
        let _window = paint_screen_red();

        let displays = list_displays().expect("list displays");
        assert!(!displays.is_empty());
        assert!(displays[0].width > 0);

        // Full size, then forced small to exercise the scaling path.
        for max in [(100_000, 100_000), (320, 200)] {
            let mut src = ScreenSource::new(None, max).expect("open display");
            let (w, h) = src.size();
            assert!(w % 2 == 0 && h % 2 == 0 && w <= max.0 && h <= max.1);
            let f = src.next_frame().expect("frame");
            assert_eq!((f.width, f.height), (w, h));
            assert_eq!(f.bgra.len(), w * h * 4);
            let i = ((h / 2) * w + w / 2) * 4;
            assert_eq!(
                &f.bgra[i..i + 3],
                &[0, 0, 255],
                "centre pixel should be red (BGR)"
            );
        }
        assert!(ScreenSource::new(Some(u32::MAX), DEFAULT_MAX_SIZE).is_err());
    }
}
