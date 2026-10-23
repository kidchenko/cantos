//! The macOS menu bar glyph: the app's mark (a rounded tile with one corner
//! lit) as a template image.
//!
//! The menu bar wants black and alpha, which AppKit tints for light and dark
//! menus and for the highlighted state; the coloured tile used on Windows and
//! Linux would be flattened to a solid square there. So it is drawn here, from
//! signed distances with 4x4 supersampling, rather than shipped as a second
//! bitmap.
//!
//! Sized to the keyline Cantos shares with Cota, Sua Vez, Café and Porta: 26
//! px across in a 32 px canvas, with a 2.4 px stroke, so the five sit the same
//! size side by side.

/// The @2x of a 16 pt menu bar glyph.
pub const EDGE: u32 = 32;

const SS: u32 = 4;
const SIDE: f32 = 26.0;
const RADIUS: f32 = 6.5;
const STROKE: f32 = 2.4;
/// The lit quarter-disc, in proportion to the tile exactly as in icon.svg
/// (radius 40 on a 64 tile).
const LIT: f32 = SIDE * 40.0 / 64.0;

/// RGBA8, `EDGE` x `EDGE`. `lit` fills the corner; without it the glyph is
/// the empty outline.
pub fn template(lit: bool) -> Vec<u8> {
    let mut out = vec![0u8; (EDGE * EDGE * 4) as usize];
    let half = SIDE / 2.0;
    let centre = EDGE as f32 / 2.0;
    let corner = centre - half; // the tile's top-left, where the disc sits

    for y in 0..EDGE {
        for x in 0..EDGE {
            let mut hits = 0u32;
            for sy in 0..SS {
                for sx in 0..SS {
                    let px = x as f32 + (sx as f32 + 0.5) / SS as f32;
                    let py = y as f32 + (sy as f32 + 0.5) / SS as f32;
                    let d = rounded_square(px - centre, py - centre, half, RADIUS);
                    // The stroke sits inside the edge, so the outline and the
                    // lit corner share one silhouette.
                    let outline = (-STROKE..=0.0).contains(&d);
                    let disc = lit && d <= 0.0 && {
                        let (dx, dy) = (px - corner, py - corner);
                        (dx * dx + dy * dy).sqrt() <= LIT
                    };
                    hits += (outline || disc) as u32;
                }
            }
            let i = ((y * EDGE + x) * 4) as usize;
            out[i + 3] = (hits * 255 / (SS * SS)) as u8;
        }
    }
    out
}

/// Signed distance to a square of half-side `half` with corners rounded to
/// `r`, centred on the origin: negative inside.
fn rounded_square(x: f32, y: f32, half: f32, r: f32) -> f32 {
    let qx = x.abs() - (half - r);
    let qy = y.abs() - (half - r);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha(px: &[u8], x: u32, y: u32) -> u8 {
        px[((y * EDGE + x) * 4 + 3) as usize]
    }

    #[test]
    fn the_glyph_is_black_with_alpha() {
        let px = template(true);
        assert_eq!(px.len() as u32, EDGE * EDGE * 4);
        assert!(px.chunks(4).all(|p| p[..3] == [0, 0, 0]));
    }

    #[test]
    fn the_lit_corner_is_filled_and_the_rest_is_hollow() {
        let px = template(true);
        assert_eq!(alpha(&px, 8, 8), 255, "top-left is lit");
        assert_eq!(alpha(&px, 22, 22), 0, "bottom-right is hollow");
        assert_eq!(alpha(&px, 0, 0), 0, "outside the tile is clear");
    }

    #[test]
    fn paused_is_the_outline_alone() {
        let px = template(false);
        assert_eq!(alpha(&px, 8, 8), 0);
        assert_eq!(alpha(&px, 16, 4), 255, "the top edge is still drawn");
    }
}
