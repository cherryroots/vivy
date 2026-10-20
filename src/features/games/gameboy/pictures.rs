//! Screens as pictures: one as a PNG, several as a GIF. Pure, no emulator.
//!
//! A Game Boy screen is tiny (160×144), so both are scaled up by whole pixels, which keeps
//! the edges sharp. A Game Boy shows 4 shades, and a Game Boy Color at most 56 colours at
//! once, so a GIF's 256-colour palette holds every colour exactly, with no dithering.

use std::collections::HashMap;

use anyhow::{Context as _, Result};
use image::{ImageFormat, RgbImage, imageops::FilterType};

use super::emulator::{FPS, HEIGHT, WIDTH};

/// How many times bigger pictures are than the screen: 3 makes 480×432, easy to read on a
/// phone.
pub const SCALE: u32 = 3;

/// A screen (3 bytes a pixel, from `Emulator::screen`) as a PNG, `SCALE` times bigger.
pub fn png(screen: &[u8]) -> Result<Vec<u8>> {
    png_at(screen, SCALE)
}

/// A screen as a PNG, `scale` times bigger. Models get small ones: at 2×, a screen costs
/// Claude about 120 tokens.
pub fn png_at(screen: &[u8], scale: u32) -> Result<Vec<u8>> {
    let image = RgbImage::from_raw(WIDTH as u32, HEIGHT as u32, screen.to_vec())
        .context("a screen of the wrong size")?;
    let big = image::imageops::resize(
        &image,
        WIDTH as u32 * scale,
        HEIGHT as u32 * scale,
        FilterType::Nearest,
    );
    let mut out = std::io::Cursor::new(Vec::new());
    big.write_to(&mut out, ImageFormat::Png)?;
    Ok(out.into_inner())
}

/// Screens as an animated GIF, `scale` times bigger, playing at the game's speed when they
/// were taken every `every` frames. A screen the same as the one before it isn't repeated;
/// the one before just stays up longer. Plays once (Discord loops GIFs anyway).
pub fn gif(screens: &[Vec<u8>], every: u32, scale: u32) -> Result<Vec<u8>> {
    gif_held(screens, every, scale, 0)
}

/// [`gif`], with the last screen kept up `hold` frames longer, so a short GIF Discord
/// loops stays on its last screen instead of starting over.
pub fn gif_held(screens: &[Vec<u8>], every: u32, scale: u32, hold: u32) -> Result<Vec<u8>> {
    anyhow::ensure!(!screens.is_empty(), "no screens for a GIF");
    let (width, height) = (WIDTH as u32 * scale, HEIGHT as u32 * scale);

    // One palette for the whole GIF: every colour, in the order they turn up.
    let mut palette: Vec<[u8; 3]> = Vec::new();
    let mut index: HashMap<[u8; 3], u8> = HashMap::new();
    let mut frames: Vec<(Vec<u8>, u32)> = Vec::new(); // (colour indexes, frames shown)
    for screen in screens {
        anyhow::ensure!(
            screen.len() == WIDTH * HEIGHT * 3,
            "a screen of the wrong size"
        );
        let mut pixels = Vec::with_capacity(WIDTH * HEIGHT);
        for rgb in screen.chunks_exact(3) {
            let colour = [rgb[0], rgb[1], rgb[2]];
            let i = match index.get(&colour) {
                Some(&i) => i,
                None => {
                    // The hardware can't show more than 256, but a GIF's palette can't hold
                    // more either, so this is a check, not a limit anyone reaches.
                    anyhow::ensure!(palette.len() < 256, "more than 256 colours");
                    let i = palette.len() as u8;
                    palette.push(colour);
                    index.insert(colour, i);
                    i
                }
            };
            pixels.push(i);
        }
        match frames.last_mut() {
            Some((last, shown)) if *last == pixels => *shown += every,
            _ => frames.push((pixels, every)),
        }
    }
    if let Some((_, shown)) = frames.last_mut() {
        *shown += hold;
    }

    let flat: Vec<u8> = palette.iter().flatten().copied().collect();
    let mut out = Vec::new();
    {
        let mut encoder = gif::Encoder::new(&mut out, width as u16, height as u16, &flat)?;
        // Hundredths of a second, counted from the start, so rounding never adds up.
        let mut shown_frames: u64 = 0;
        let mut shown_cs: u64 = 0;
        for (pixels, frames_shown) in &frames {
            shown_frames += u64::from(*frames_shown);
            let until = (shown_frames as f64 / FPS * 100.0).round() as u64;
            let delay = (until - shown_cs).max(2);
            shown_cs += delay;
            let mut frame = gif::Frame {
                width: width as u16,
                height: height as u16,
                delay: delay.min(u64::from(u16::MAX)) as u16,
                buffer: scale_up(pixels, scale).into(),
                ..gif::Frame::default()
            };
            frame.dispose = gif::DisposalMethod::Keep;
            encoder.write_frame(&frame)?;
        }
    }
    Ok(out)
}

/// Each pixel as a `scale`×`scale` square.
fn scale_up(pixels: &[u8], scale: u32) -> Vec<u8> {
    let scale = scale as usize;
    let mut out = Vec::with_capacity(pixels.len() * scale * scale);
    for row in pixels.chunks_exact(WIDTH) {
        let wide: Vec<u8> = row
            .iter()
            .flat_map(|&p| std::iter::repeat_n(p, scale))
            .collect();
        for _ in 0..scale {
            out.extend_from_slice(&wide);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A screen of one colour with a black pixel at `(x, y)`.
    fn screen(colour: [u8; 3], dot: (usize, usize)) -> Vec<u8> {
        let mut s: Vec<u8> = std::iter::repeat_n(colour, WIDTH * HEIGHT)
            .flatten()
            .collect();
        let i = (dot.1 * WIDTH + dot.0) * 3;
        s[i..i + 3].copy_from_slice(&[0, 0, 0]);
        s
    }

    #[test]
    fn a_png_is_the_screen_scaled_up() {
        let png = png(&screen([255, 255, 255], (1, 0))).unwrap();
        let image = image::load_from_memory(&png).unwrap().to_rgb8();
        assert_eq!(image.dimensions(), (480, 432));
        assert_eq!(image.get_pixel(0, 0).0, [255, 255, 255]);
        // Pixel (1, 0) covers x 3 to 5.
        assert_eq!(image.get_pixel(3, 0).0, [0, 0, 0]);
        assert_eq!(image.get_pixel(5, 2).0, [0, 0, 0]);
        assert_eq!(image.get_pixel(6, 0).0, [255, 255, 255]);
        assert!(super::png(&[0; 10]).is_err());
    }

    #[test]
    fn a_gif_keeps_colours_exact_and_folds_repeats() {
        let white = screen([255, 255, 255], (0, 0));
        let moved = screen([255, 255, 255], (5, 5));
        let green = screen([155, 188, 15], (0, 0));
        let screens = vec![white.clone(), white.clone(), white, moved, green];
        let bytes = gif(&screens, 3, 2).unwrap();

        let mut options = gif::DecodeOptions::new();
        options.set_color_output(gif::ColorOutput::RGBA);
        let mut decoder = options.read_info(bytes.as_slice()).unwrap();
        assert_eq!((decoder.width(), decoder.height()), (320, 288));
        let mut seen = Vec::new();
        while let Some(frame) = decoder.read_next_frame().unwrap() {
            seen.push((frame.delay, frame.buffer.to_vec()));
        }
        // The three white screens became one frame shown three times as long.
        assert_eq!(seen.len(), 3);
        // 9 frames at 59.73 a second is 15 hundredths, then 3 more is 5.
        assert_eq!(seen[0].0, 15);
        assert_eq!(seen[1].0, 5);
        // The green screen's corner pixel is still exactly black, and the rest green.
        let last = &seen[2].1;
        assert_eq!(&last[0..4], &[0, 0, 0, 255]);
        assert_eq!(&last[8..12], &[155, 188, 15, 255]);
    }

    #[test]
    fn a_held_gif_stays_on_its_last_screen() {
        let screens = vec![screen([255, 255, 255], (0, 0)), screen([0, 0, 0], (1, 1))];
        let bytes = gif_held(&screens, 3, 1, 60 * 60).unwrap();
        let mut decoder = gif::DecodeOptions::new()
            .read_info(bytes.as_slice())
            .unwrap();
        let mut delays = Vec::new();
        while let Some(frame) = decoder.read_next_frame().unwrap() {
            delays.push(frame.delay);
        }
        // 3 frames is 5 hundredths; the last also has a minute (3603 frames) more.
        assert_eq!(delays, vec![5, 6032]);
    }

    #[test]
    fn gif_needs_screens() {
        assert!(gif(&[], 1, 1).is_err());
    }
}
