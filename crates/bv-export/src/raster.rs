//! A small software rasteriser for the static graph snapshot.
//!
//! Go renders PNG snapshots with `git.sr.ht/~sbinet/gg`, a general 2D library
//! with an anti-aliased scanline filler. This module is the subset that
//! `pkg/export/graph_snapshot.go` actually exercises — rounded rectangles
//! filled and stroked, straight lines, one small polygon, and bitmap text —
//! expressed as signed-distance tests instead.
//!
//! **The resulting PNG is not byte-identical to Go's.** gg anti-aliases every
//! edge; these primitives do not. Geometry, canvas size, colours, layout and
//! glyphs all match, and the text uses the very same 6x13 face
//! ([`crate::face7x13`]) at the same pixel positions, so the two images are
//! visually the same picture — but a byte diff will not be empty and should
//! not be expected to be. Nothing in the golden corpus covers this path, and
//! the golden corpus is the byte-parity gate; this is the "make the flag
//! produce a picture at all" half of the work.

use crate::face7x13;

/// An opaque 8-bit-per-channel colour, matching Go's `color.RGBA` with a full
/// alpha. Go's `css` helper drops the alpha channel, so every colour in the
/// snapshot is opaque.
pub type Color = [u8; 3];

/// An RGB image buffer.
pub struct Canvas {
    width: i32,
    height: i32,
    pixels: Vec<u8>,
}

impl Canvas {
    /// A canvas of `width` x `height` filled with `background`.
    pub fn new(width: i32, height: i32, background: Color) -> Self {
        let mut pixels = Vec::with_capacity((width * height * 3) as usize);
        for _ in 0..(width * height) {
            pixels.extend_from_slice(&background);
        }
        Self {
            width,
            height,
            pixels,
        }
    }

    pub fn width(&self) -> i32 {
        self.width
    }

    pub fn height(&self) -> i32 {
        self.height
    }

    /// Set one pixel. Out-of-bounds writes are dropped, which is how Go's
    /// `image.RGBA` behaves for a point outside the rectangle.
    pub fn set(&mut self, x: i32, y: i32, color: Color) {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return;
        }
        let offset = ((y * self.width + x) * 3) as usize;
        self.pixels[offset..offset + 3].copy_from_slice(&color);
    }

    /// Fill the rounded rectangle with corner radius `r`.
    pub fn fill_rounded_rect(&mut self, rect: Rect, r: f64, color: Color) {
        for (px, py) in bounding_pixels(rect, 0) {
            if rounded_rect_distance(rect, r, px, py) <= 0.0 {
                self.set(px, py, color);
            }
        }
    }

    /// Stroke the outline of the rounded rectangle, centred on the boundary
    /// so half the `width` falls inside and half outside — the same
    /// straddling `gg`'s `Stroke()` produces.
    pub fn stroke_rounded_rect(&mut self, rect: Rect, r: f64, color: Color, width: f64) {
        let half = width / 2.0;
        // A stroke straddles the boundary, so its bounding box reaches
        // half a stroke width outside the shape on every side.
        for (px, py) in bounding_pixels(rect, half.ceil() as i32) {
            if rounded_rect_distance(rect, r, px, py).abs() <= half {
                self.set(px, py, color);
            }
        }
    }

    /// A straight line of the given width, again straddling the path.
    pub fn stroke_line(&mut self, x1: f64, y1: f64, x2: f64, y2: f64, color: Color, width: f64) {
        let half = width / 2.0;
        let min_x = x1.min(x2).floor() as i32 - width.ceil() as i32;
        let max_x = x1.max(x2).ceil() as i32 + width.ceil() as i32;
        let min_y = y1.min(y2).floor() as i32 - width.ceil() as i32;
        let max_y = y1.max(y2).ceil() as i32 + width.ceil() as i32;
        for py in min_y..=max_y {
            for px in min_x..=max_x {
                let d = point_segment_distance(px as f64 + 0.5, py as f64 + 0.5, x1, y1, x2, y2);
                if d <= half {
                    self.set(px, py, color);
                }
            }
        }
    }

    /// Fill a simple polygon with an even-odd scanline.
    ///
    /// The only polygon in the snapshot is the three-point arrow head, so the
    /// general case never sees a self-intersecting outline.
    pub fn fill_polygon(&mut self, points: &[(i32, i32)], color: Color) {
        if points.len() < 3 {
            return;
        }
        let min_y = points.iter().map(|p| p.1).min().unwrap();
        let max_y = points.iter().map(|p| p.1).max().unwrap();
        for y in min_y..=max_y {
            // Sample at the pixel centre.
            let sample = y as f64 + 0.5;
            let mut crossings: Vec<f64> = Vec::new();
            for i in 0..points.len() {
                let (x1, y1) = points[i];
                let (x2, y2) = points[(i + 1) % points.len()];
                let (lo, hi) = if y1 <= y2 { (y1, y2) } else { (y2, y1) };
                if sample < lo as f64 || sample >= hi as f64 {
                    continue;
                }
                if y1 == y2 {
                    continue;
                }
                let t = (sample - y1 as f64) / (y2 - y1) as f64;
                crossings.push(x1 as f64 + t * (x2 - x1) as f64);
            }
            crossings.sort_by(|a, b| a.partial_cmp(b).unwrap());
            // Even-odd: fill between the 1st/2nd, 3rd/4th, ... crossings.
            // `as_chunks`, not `chunks_exact`: the chunk size is the
            // constant 2, and clippy asks for the array-chunk API there.
            // `as_chunks` yields (chunks, remainder); the remainder is the odd
            // crossing out, and the even-odd rule only consumes pairs.
            let (pair, _odd) = crossings.as_chunks::<2>();
            for span in pair {
                let from = span[0].ceil() as i32;
                let to = (span[1] - f64::EPSILON).floor() as i32;
                for x in from..=to {
                    self.set(x, y, color);
                }
            }
        }
    }

    /// Draw `text` with gg's `(ax=0, ay=0.5)` anchor: the point is the left
    /// edge and the vertical middle of the line box, which is the only
    /// anchor `pkg/export/graph_snapshot.go` uses.
    ///
    /// The position arithmetic follows gg exactly
    /// (`context.go:791` then `basicfont.go:Glyph`): the anchor is offset down
    /// by half the 13px line height, rounded to a pixel, and each glyph's
    /// ink starts [`face7x13::ASCENT`] pixels above that.
    pub fn draw_text_anchored(&mut self, text: &str, x: f64, y_middle: f64, color: Color) {
        let origin_y = y_middle + f64::from(face7x13::HEIGHT) / 2.0;
        // `int(dot+32) >> 6` in gg rounds a 26.6 fixed-point value to the
        // nearest pixel, halves up.
        let baseline = (origin_y + 0.5).floor() as i32;
        for (i, ch) in text.chars().enumerate() {
            let pen_x = (x + f64::from(face7x13::ADVANCE) * i as f64 + 0.5).floor() as i32;
            let glyph = glyph_index(ch);
            let top = baseline - face7x13::ASCENT;
            for row in 0..face7x13::HEIGHT {
                for col in 0..face7x13::WIDTH {
                    let offset =
                        (glyph * face7x13::HEIGHT as usize + row as usize) * 6 + col as usize;
                    if face7x13::MASK[offset] != 0 {
                        self.set(pen_x + col, top + row, color);
                    }
                }
            }
        }
    }

    /// Encode as a PNG.
    pub fn to_png(&self) -> Result<Vec<u8>, png::EncodingError> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, self.width as u32, self.height as u32);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            // Go's `dc.SavePNG` uses the encoder's default compression, so
            // there is nothing to tune here.
            let mut writer = encoder.write_header()?;
            writer.write_image_data(&self.pixels)?;
            writer.finish()?;
        }
        Ok(out)
    }
}

/// An axis-aligned rectangle in device pixels.
#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }
}

/// Every pixel index whose centre could lie inside the rectangle's bounding
/// box, grown by `margin` on each side. Returned as a plain `Vec` rather than
/// visited through a closure parameter because the callers also need
/// `&mut self` to write the pixel.
fn bounding_pixels(rect: Rect, margin: i32) -> Vec<(i32, i32)> {
    let min_x = rect.x.floor() as i32 - margin;
    let min_y = rect.y.floor() as i32 - margin;
    let max_x = (rect.x + rect.w).ceil() as i32 + margin;
    let max_y = (rect.y + rect.h).ceil() as i32 + margin;
    let mut out = Vec::with_capacity(((max_x - min_x).max(0) * (max_y - min_y).max(0)) as usize);
    for py in min_y..max_y {
        for px in min_x..max_x {
            out.push((px, py));
        }
    }
    out
}

/// Go maps a rune to a glyph through two ranges: printable ASCII, and
/// U+FFFD. Anything else falls into the second range, which renders as the
/// replacement character — the same substitution `basicfont.find` performs.
fn glyph_index(ch: char) -> usize {
    let code = ch as u32;
    if (face7x13::FIRST_RUNE..0x7f).contains(&code) {
        (code - face7x13::FIRST_RUNE) as usize
    } else {
        face7x13::REPLACEMENT_INDEX
    }
}

/// Signed distance from the pixel centre of `(px, py)` to the boundary of a
/// rounded rectangle: zero on the boundary, negative inside by the distance to
/// the nearest edge, positive outside.
///
/// The interior magnitude matters. `fill_rounded_rect` only needs the sign, but
/// `stroke_rounded_rect` tests `|d| <= width/2`, which would paint the whole
/// interior if interior points reported zero.
fn rounded_rect_distance(rect: Rect, r: f64, px: i32, py: i32) -> f64 {
    let cx = rect.x + rect.w / 2.0;
    let cy = rect.y + rect.h / 2.0;
    let r = r.min(rect.w / 2.0).min(rect.h / 2.0);
    // Offset from the "core" rectangle the corner circles sit outside of.
    let qx = (px as f64 + 0.5 - cx).abs() - (rect.w / 2.0 - r);
    let qy = (py as f64 + 0.5 - cy).abs() - (rect.h / 2.0 - r);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - r
}

/// Distance from a point to a line segment.
fn point_segment_distance(px: f64, py: f64, x1: f64, y1: f64, x2: f64, y2: f64) -> f64 {
    let dx = x2 - x1;
    let dy = y2 - y1;
    let length_sq = dx * dx + dy * dy;
    if length_sq == 0.0 {
        return ((px - x1).powi(2) + (py - y1).powi(2)).sqrt();
    }
    let t = (((px - x1) * dx + (py - y1) * dy) / length_sq).clamp(0.0, 1.0);
    let cx = x1 + t * dx;
    let cy = y1 + t * dy;
    ((px - cx).powi(2) + (py - cy).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Color = [255, 0, 0];
    const BLUE: Color = [0, 0, 255];

    fn pixel(canvas: &Canvas, x: i32, y: i32) -> Color {
        let offset = ((y * canvas.width + x) * 3) as usize;
        [
            canvas.pixels[offset],
            canvas.pixels[offset + 1],
            canvas.pixels[offset + 2],
        ]
    }

    #[test]
    fn a_filled_rect_covers_its_interior() {
        let mut canvas = Canvas::new(20, 20, [0, 0, 0]);
        canvas.fill_rounded_rect(Rect::new(2.0, 2.0, 10.0, 10.0), 0.0, RED);
        assert_eq!(pixel(&canvas, 5, 5), RED);
        // The border pixels whose centre is outside stay background.
        assert_eq!(pixel(&canvas, 1, 1), [0, 0, 0]);
        assert_eq!(pixel(&canvas, 13, 13), [0, 0, 0]);
    }

    #[test]
    fn a_rounded_corner_is_cut_away() {
        let mut canvas = Canvas::new(20, 20, [0, 0, 0]);
        canvas.fill_rounded_rect(Rect::new(2.0, 2.0, 16.0, 16.0), 6.0, RED);
        // The extreme corner of the bounding box is outside the rounded shape.
        assert_eq!(pixel(&canvas, 2, 2), [0, 0, 0]);
        // The middle of an edge is not.
        assert_eq!(pixel(&canvas, 10, 3), RED);
    }

    #[test]
    fn a_stroke_straddles_the_boundary() {
        let mut canvas = Canvas::new(40, 40, [0, 0, 0]);
        canvas.stroke_rounded_rect(Rect::new(10.0, 10.0, 20.0, 20.0), 0.0, BLUE, 2.0);
        // A 2px stroke centred on x=10 covers the span [9, 11], whose pixel
        // centres are 9.5 and 10.5 — that is, pixels 9 and 10.
        assert_eq!(pixel(&canvas, 9, 20), BLUE);
        assert_eq!(pixel(&canvas, 10, 20), BLUE);
        assert_eq!(pixel(&canvas, 8, 20), [0, 0, 0]);
        assert_eq!(pixel(&canvas, 11, 20), [0, 0, 0]);
        // Deep interior is not boundary, so it stays unpainted.
        assert_eq!(pixel(&canvas, 20, 20), [0, 0, 0]);
    }

    #[test]
    fn a_horizontal_line_is_the_requested_thickness() {
        let mut canvas = Canvas::new(40, 40, [0, 0, 0]);
        canvas.stroke_line(5.0, 20.0, 35.0, 20.0, RED, 2.0);
        for y in 19..=20 {
            assert_eq!(pixel(&canvas, 20, y), RED, "row {y} should be inked");
        }
        assert_eq!(pixel(&canvas, 20, 18), [0, 0, 0]);
        assert_eq!(pixel(&canvas, 20, 21), [0, 0, 0]);
    }

    #[test]
    fn a_triangle_is_filled() {
        let mut canvas = Canvas::new(20, 20, [0, 0, 0]);
        canvas.fill_polygon(&[(4, 4), (15, 4), (4, 15)], RED);
        assert_eq!(pixel(&canvas, 5, 5), RED);
        assert_eq!(pixel(&canvas, 12, 6), RED);
        assert_eq!(pixel(&canvas, 15, 15), [0, 0, 0]);
    }

    #[test]
    fn text_puts_ink_in_the_expected_band() {
        // gg offsets the anchor down by half the 13px line height and back up
        // by the 11px ascent, so ink lands on rows y-4 ..= y+8.
        let mut canvas = Canvas::new(80, 40, [0, 0, 0]);
        canvas.draw_text_anchored("I", 2.0, 20.0, RED);
        let inked: Vec<i32> = (0..40)
            .filter(|y| (0..12).any(|x| pixel(&canvas, 2 + x, *y) == RED))
            .collect();
        assert!(!inked.is_empty(), "expected some ink");
        assert!(inked.iter().all(|y| (16..=28).contains(y)), "{inked:?}");
        // 'I' is a serif capital: it occupies most, but not all, of the band.
        assert!(inked.len() >= 9, "{inked:?}");
        assert!(inked.len() <= 13, "{inked:?}");
    }

    #[test]
    fn unmapped_runes_use_the_replacement_glyph() {
        let mut a = Canvas::new(60, 30, [0, 0, 0]);
        let mut b = Canvas::new(60, 30, [0, 0, 0]);
        a.draw_text_anchored("\u{FFFD}", 0.0, 15.0, RED);
        b.draw_text_anchored("\u{263A}", 0.0, 15.0, RED);
        assert_eq!(a.pixels, b.pixels);

        // A printable rune must *not* render as the replacement glyph.
        let mut c = Canvas::new(60, 30, [0, 0, 0]);
        c.draw_text_anchored("A", 0.0, 15.0, RED);
        assert_ne!(a.pixels, c.pixels);
    }

    #[test]
    fn text_advances_by_the_font_advance() {
        let mut canvas = Canvas::new(120, 40, [0, 0, 0]);
        canvas.draw_text_anchored("AA", 0.0, 20.0, RED);
        // The first glyph's ink is at x=0..6; the second starts 7px later.
        let columns_with_ink = |canvas: &Canvas| -> Vec<i32> {
            (0..120)
                .filter(|x| (0..40).any(|y| pixel(canvas, *x, y) == RED))
                .collect()
        };
        let inked = columns_with_ink(&canvas);
        let first_gap = inked.iter().position(|x| *x > 5).unwrap();
        assert_eq!(inked[first_gap], 7, "second glyph should start at x=7");
    }

    #[test]
    fn png_encoding_produces_a_readable_image() {
        let mut canvas = Canvas::new(8, 4, [0x12, 0x34, 0x56]);
        canvas.fill_rounded_rect(Rect::new(1.0, 1.0, 6.0, 2.0), 0.0, RED);
        let bytes = canvas.to_png().expect("encode");
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");

        let decoded = png::Decoder::new(std::io::Cursor::new(&bytes));
        let mut reader = decoded.read_info().expect("decode header");
        let mut buffer = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buffer).expect("decode frame");
        assert_eq!((info.width, info.height), (8, 4));
        assert_eq!(&buffer[0..3], &[0x12, 0x34, 0x56]);
        // The filled band starts one pixel in.
        let at = |x: usize, y: usize| &buffer[(y * 8 + x) * 3..(y * 8 + x) * 3 + 3];
        assert_eq!(at(2, 1), RED);
    }

    #[test]
    fn out_of_bounds_writes_are_dropped() {
        let mut canvas = Canvas::new(4, 4, [0, 0, 0]);
        canvas.set(-1, 0, RED);
        canvas.set(0, -1, RED);
        canvas.set(4, 0, RED);
        canvas.set(0, 4, RED);
        assert!(canvas.pixels.iter().all(|b| *b == 0));
    }
}
