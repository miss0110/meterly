//! Render a short value string (e.g. "81.1M", "$186.8") to an RGBA icon for
//! the Windows tray, which has no text title. Uses a tiny built-in 3×5 pixel
//! font (no deps, no bundled font file). White glyphs with a 1px dark outline
//! so they read on both light and dark taskbars; the OS downscales the result
//! to the tray size.

/// 3×5 glyphs, top→bottom rows, 3 bits per row (MSB = left pixel).
fn glyph(c: char) -> Option<[u8; 5]> {
    Some(match c {
        '0' => [0b111, 0b101, 0b101, 0b101, 0b111],
        '1' => [0b010, 0b110, 0b010, 0b010, 0b111],
        '2' => [0b111, 0b001, 0b111, 0b100, 0b111],
        '3' => [0b111, 0b001, 0b111, 0b001, 0b111],
        '4' => [0b101, 0b101, 0b111, 0b001, 0b001],
        '5' => [0b111, 0b100, 0b111, 0b001, 0b111],
        '6' => [0b111, 0b100, 0b111, 0b101, 0b111],
        '7' => [0b111, 0b001, 0b010, 0b010, 0b010],
        '8' => [0b111, 0b101, 0b111, 0b101, 0b111],
        '9' => [0b111, 0b101, 0b111, 0b001, 0b111],
        '.' => [0b000, 0b000, 0b000, 0b000, 0b010],
        '$' => [0b011, 0b110, 0b010, 0b011, 0b110],
        'M' => [0b101, 0b111, 0b111, 0b101, 0b101],
        'K' => [0b101, 0b110, 0b100, 0b110, 0b101],
        'B' => [0b110, 0b101, 0b110, 0b101, 0b110],
        '-' => [0b000, 0b000, 0b111, 0b000, 0b000],
        ' ' => [0b000, 0b000, 0b000, 0b000, 0b000],
        _ => return None,
    })
}

const GW: usize = 3;
const GH: usize = 5;

/// (rgba, width, height) for `text`, or `None` if it has no drawable glyphs.
/// `scale` sizes the pixel font; the canvas is square (max of w/h) so the OS
/// keeps the aspect ratio when it scales the icon down.
pub fn render(text: &str, scale: usize) -> Option<(Vec<u8>, u32, u32)> {
    let glyphs: Vec<[u8; 5]> = text.chars().filter_map(glyph).collect();
    if glyphs.is_empty() {
        return None;
    }
    let gap = scale; // 1 font-pixel gap between glyphs
    let margin = scale + 1; // room for the outline + breathing space
    let content_w = glyphs.len() * GW * scale + (glyphs.len() - 1) * gap;
    let content_h = GH * scale;
    let canvas = (content_w.max(content_h)) + margin * 2;
    let (w, h) = (canvas, canvas);
    let ox = (w - content_w) / 2;
    let oy = (h - content_h) / 2;

    // Coverage grid (glyph pixels), then dilate by 1 for the outline.
    let mut cover = vec![false; w * h];
    let mut x0 = ox;
    for g in &glyphs {
        for (ry, row) in g.iter().enumerate() {
            for cx in 0..GW {
                if row & (1 << (GW - 1 - cx)) != 0 {
                    for sy in 0..scale {
                        for sx in 0..scale {
                            let px = x0 + cx * scale + sx;
                            let py = oy + ry * scale + sy;
                            cover[py * w + px] = true;
                        }
                    }
                }
            }
        }
        x0 += GW * scale + gap;
    }

    let mut rgba = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 4;
            if cover[y * w + x] {
                rgba[i..i + 4].copy_from_slice(&[255, 255, 255, 255]); // white fill
            } else if is_neighbor_covered(&cover, w, h, x, y) {
                rgba[i..i + 4].copy_from_slice(&[0, 0, 0, 220]); // dark outline
            }
        }
    }
    Some((rgba, w as u32, h as u32))
}

fn is_neighbor_covered(cover: &[bool], w: usize, h: usize, x: usize, y: usize) -> bool {
    for dy in -1i32..=1 {
        for dx in -1i32..=1 {
            let (nx, ny) = (x as i32 + dx, y as i32 + dy);
            if nx >= 0 && ny >= 0 && (nx as usize) < w && (ny as usize) < h && cover[ny as usize * w + nx as usize] {
                return true;
            }
        }
    }
    false
}

/// The value token to render — the trailing whitespace-separated part of a
/// tray state ("이 기기 81.1M" → "81.1M", "$186.8" → "$186.8").
pub fn value_token(state: &str) -> &str {
    state.rsplit(' ').next().unwrap_or(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_token_takes_trailing_part() {
        assert_eq!(value_token("이 기기 81.1M"), "81.1M");
        assert_eq!(value_token("$186.8"), "$186.8");
        assert_eq!(value_token("81.1M"), "81.1M");
    }

    #[test]
    fn render_produces_square_rgba() {
        let (rgba, w, h) = render("81.1M", 3).expect("some glyphs");
        assert_eq!(w, h); // square canvas
        assert_eq!(rgba.len(), (w * h * 4) as usize);
        assert!(rgba.iter().skip(3).step_by(4).any(|&a| a > 0)); // not blank
        assert!(render("한글", 3).is_none()); // no drawable glyphs
    }

    /// Dump a sample to $METERLY_ICON_DUMP as `WxH` header + raw RGBA so it can
    /// be viewed. Ignored in normal runs.
    #[test]
    #[ignore]
    fn dump_sample() {
        let Ok(path) = std::env::var("METERLY_ICON_DUMP") else {
            return;
        };
        let (rgba, w, h) = render("81.1M", 6).unwrap();
        let mut out = format!("{w} {h}\n").into_bytes();
        out.extend_from_slice(&rgba);
        std::fs::write(path, out).unwrap();
    }
}
