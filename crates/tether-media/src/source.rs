/// One picture, 4 bytes per pixel in B, G, R, A order (what screen capture APIs produce).
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub bgra: Vec<u8>,
}

/// Anything that can produce pictures: a test pattern now, screen capture next.
pub trait FrameSource: Send + 'static {
    /// Constant for the life of the source. Both must be even (H.264 needs it).
    fn size(&self) -> (usize, usize);
    /// Blocks until the next picture is ready, then returns it.
    fn next_frame(&mut self) -> Result<Frame, String>;
}

/// Number of blocks in the frame counter bar along the bottom of the test pattern.
pub const COUNTER_BITS: usize = 16;
const BAR_HEIGHT: usize = 32;
const BOX: usize = 64;

/// A gradient with a bouncing square and a 16 bit frame counter drawn as black and white
/// blocks along the bottom edge. The counter lets a test read back, after the picture has
/// been encoded, streamed and decoded, that frames really are arriving and advancing.
pub struct TestPattern {
    width: usize,
    height: usize,
    background: Vec<u8>,
    n: u64,
}

impl TestPattern {
    pub fn new(width: usize, height: usize) -> Result<Self, String> {
        if width % 2 != 0 || height % 2 != 0 || width < 4 * COUNTER_BITS || height < 4 * BAR_HEIGHT
        {
            return Err(format!("unsupported test pattern size {width}x{height}"));
        }
        let mut background = vec![255u8; width * height * 4];
        for y in 0..height {
            for x in 0..width {
                let i = (y * width + x) * 4;
                background[i] = 128; // blue
                background[i + 1] = (y * 200 / height) as u8 + 20; // green
                background[i + 2] = (x * 200 / width) as u8 + 20; // red
            }
        }
        Ok(Self {
            width,
            height,
            background,
            n: 0,
        })
    }
}

impl FrameSource for TestPattern {
    fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    fn next_frame(&mut self) -> Result<Frame, String> {
        let (w, h) = (self.width, self.height);
        let mut bgra = self.background.clone();

        // Bouncing white square.
        let span = w - BOX;
        let t = (self.n as usize * 6) % (2 * span);
        let x0 = if t < span { t } else { 2 * span - t };
        let y0 = h / 3;
        for y in y0..y0 + BOX {
            for x in x0..x0 + BOX {
                let i = (y * w + x) * 4;
                bgra[i..i + 3].fill(255);
            }
        }

        // Counter bar, most significant bit on the left.
        let counter = (self.n & 0xffff) as u16;
        let block = w / COUNTER_BITS;
        for bit in 0..COUNTER_BITS {
            let on = counter >> (COUNTER_BITS - 1 - bit) & 1 == 1;
            let value = if on { 255 } else { 0 };
            for y in h - BAR_HEIGHT..h {
                for x in bit * block..(bit + 1) * block {
                    let i = (y * w + x) * 4;
                    bgra[i..i + 3].fill(value);
                }
            }
        }

        self.n += 1;
        Ok(Frame {
            width: w,
            height: h,
            bgra,
        })
    }
}

/// Reads the frame counter back out of a decoded picture given as RGBA or BGRA bytes, by
/// sampling the middle of each block. Used by tests, including the Electron end to end test.
pub fn read_counter(pixels: &[u8], width: usize, height: usize) -> u16 {
    let block = width / COUNTER_BITS;
    let y = height - BAR_HEIGHT / 2;
    let mut value = 0u16;
    for bit in 0..COUNTER_BITS {
        let x = bit * block + block / 2;
        let i = (y * width + x) * 4;
        let brightness =
            (u32::from(pixels[i]) + u32::from(pixels[i + 1]) + u32::from(pixels[i + 2])) / 3;
        value = value << 1 | u16::from(brightness > 127);
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_survives_a_round_trip_through_the_pixels() {
        let mut src = TestPattern::new(640, 360).unwrap();
        for expected in 0..300u16 {
            let f = src.next_frame().unwrap();
            assert_eq!(read_counter(&f.bgra, f.width, f.height), expected);
        }
    }

    #[test]
    fn rejects_bad_sizes() {
        assert!(TestPattern::new(641, 360).is_err());
        assert!(TestPattern::new(32, 360).is_err());
    }
}
