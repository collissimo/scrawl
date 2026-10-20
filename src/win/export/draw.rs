//! The annotations burned into each exported frame: `ExportInk` in `helper/Export.swift`,
//! drawn with tiny-skia instead of Core Graphics. Same layers, sizes, colours and timing.

use std::sync::OnceLock;

use ab_glyph::{Font, FontVec, OutlineCurve};
use tiny_skia::{
    Color, FillRule, LineCap, LineJoin, Paint, Path, PathBuilder, PixmapMut, Stroke, Transform,
};

use super::pixels::View;
use super::plan::Ink;

const RED: (f32, f32, f32) = (1.0, 59.0 / 255.0, 48.0 / 255.0);
const BLUE: (f32, f32, f32) = (0.0, 136.0 / 255.0, 1.0);
const WHITE: (f32, f32, f32) = (1.0, 1.0, 1.0);
/// How long a click ripple lasts.
pub const RIPPLE_MS: f64 = 520.0;

/// 1 until `at`, then a smoothstep down to 0 over `over` ms.
pub fn fade(t: f64, at: f64, over: f64) -> f64 {
    if t <= at {
        return 1.0;
    }
    let u = ((t - at) / over.max(1.0)).min(1.0);
    1.0 - u * u * (3.0 - 2.0 * u)
}

/// A ripple `age` ms after its click: the radius in `k * scale` units and the opacity.
pub fn ripple(age: f64) -> Option<(f64, f64)> {
    if !(0.0..RIPPLE_MS).contains(&age) {
        return None;
    }
    let p = age / RIPPLE_MS;
    let ease = 1.0 - (1.0 - p).powi(3);
    Some((9.0 + 24.0 * ease, 1.0 - p))
}

/// How far a pin has popped in, `age` ms after it was placed (0…1, ease-out cubic over 160 ms).
pub fn pin_grow(age: f64) -> f64 {
    let pop = (age / 160.0).min(1.0);
    1.0 - (1.0 - pop).powi(3)
}

/// The index of the last entry whose time is at or before `t` (entries sorted by time, the
/// first at or before `t`).
fn last_at(times: impl ExactSizeIterator<Item = f64> + Clone, t: f64) -> usize {
    let times: Vec<f64> = times.collect();
    let (mut lo, mut hi) = (0, times.len() - 1);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if times[mid] <= t {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}

fn paint(c: (f32, f32, f32), alpha: f64) -> Option<Paint<'static>> {
    let alpha = alpha.clamp(0.0, 1.0) as f32;
    if alpha <= 0.0 {
        return None;
    }
    let mut paint = Paint::default();
    paint.set_color(Color::from_rgba(c.0, c.1, c.2, alpha)?);
    paint.anti_alias = true;
    Some(paint)
}

fn rgb(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    (r / 255.0, g / 255.0, b / 255.0)
}

fn fill(pixmap: &mut PixmapMut, path: Option<Path>, c: (f32, f32, f32), alpha: f64) {
    if let (Some(path), Some(paint)) = (path, paint(c, alpha)) {
        pixmap.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
}

fn stroke(pixmap: &mut PixmapMut, path: Option<Path>, width: f64, c: (f32, f32, f32), alpha: f64) {
    if let (Some(path), Some(paint)) = (path, paint(c, alpha)) {
        let stroke = Stroke {
            width: width as f32,
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Stroke::default()
        };
        pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }
}

fn circle(c: (f32, f32), r: f64) -> Option<Path> {
    PathBuilder::from_circle(c.0, c.1, r as f32)
}

fn disc(pixmap: &mut PixmapMut, c: (f32, f32), r: f64, color: (f32, f32, f32), alpha: f64) {
    fill(pixmap, circle(c, r), color, alpha);
}

fn polyline(points: &[(f32, f32)], close: bool) -> Option<Path> {
    let (first, rest) = points.split_first()?;
    let mut pb = PathBuilder::new();
    pb.move_to(first.0, first.1);
    for p in rest {
        pb.line_to(p.0, p.1);
    }
    if close {
        pb.close();
    }
    pb.finish()
}

/// Draws everything on screen at `t` (ms, the video's clock) for one camera `view`.
pub fn draw(pixmap: &mut PixmapMut, t: f64, view: &View, ink: &Ink) {
    let s = view.scale();
    let k = ink.k;

    for stroke in &ink.strokes {
        let Some(first) = stroke.snaps.first() else {
            continue;
        };
        if t < first.t {
            continue;
        }
        let alpha = fade(t, stroke.fade_at, stroke.fade_ms);
        if alpha <= 0.002 {
            continue;
        }
        let snap = &stroke.snaps[last_at(stroke.snaps.iter().map(|s| s.t), t)];
        if snap.poly.len() < 6 {
            continue;
        }
        let points: Vec<(f32, f32)> = snap
            .poly
            .chunks_exact(2)
            .map(|p| view.point(p[0], p[1]))
            .collect();
        fill(pixmap, polyline(&points, true), RED, alpha);
    }

    for pin in ink.pins.iter().filter(|p| t >= p.t) {
        let alpha = fade(t, pin.fade_at, pin.fade_ms);
        if alpha <= 0.002 {
            continue;
        }
        let grow = pin_grow(t - pin.t);
        let scale = ink.pin_scale * s * (0.6 + 0.4 * grow);
        let c = view.point(pin.x, pin.y);
        let radius = 13.0 * scale;
        disc(pixmap, c, radius + 2.5 * scale, WHITE, alpha * grow);
        disc(pixmap, c, radius, RED, alpha * grow);
        label(
            pixmap,
            &pin.n.to_string(),
            c,
            16.0 * scale,
            radius * 1.65,
            alpha * grow,
        );
    }

    for laser in &ink.lasers {
        let (Some(first), Some(last)) = (laser.frames.first(), laser.frames.last()) else {
            continue;
        };
        if t < first.t || t > last.t {
            continue;
        }
        let frame = &laser.frames[last_at(laser.frames.iter().map(|f| f.t), t)];
        let points: Vec<((f32, f32), f64)> = frame
            .pts
            .chunks_exact(3)
            .map(|p| (view.point(p[0], p[1]), p[2]))
            .collect();
        draw_laser(pixmap, &points, k * s);
    }

    for r in &ink.ripples {
        let Some((radius, a)) = ripple(t - r.t) else {
            continue;
        };
        let c = view.point(r.x, r.y);
        let radius = radius * k * s;
        disc(pixmap, c, radius, BLUE, 0.30 * a);
        stroke(pixmap, circle(c, radius), 1.75 * k * s, WHITE, a);
        let outer = radius + 2.5 * k * s;
        stroke(pixmap, circle(c, outer), 1.75 * k * s, BLUE, 0.92 * a);
    }

    for trail in ink.trails.iter().filter(|tr| t >= tr.t) {
        let alpha = fade(t, trail.t1 + 250.0, 350.0);
        if alpha <= 0.002 {
            continue;
        }
        let progress = if trail.t1 > trail.t {
            ((t - trail.t) / (trail.t1 - trail.t)).min(1.0)
        } else {
            1.0
        };
        let points: Vec<(f32, f32)> = trail
            .pts
            .chunks_exact(2)
            .map(|p| view.point(p[0], p[1]))
            .collect();
        if points.len() < 2 {
            continue;
        }
        let upto = progress * (points.len() - 1) as f64;
        let whole = upto as usize;
        let mut shown = points[..=whole].to_vec();
        if whole + 1 < points.len() {
            let f = (upto - whole as f64) as f32;
            let (a, b) = (points[whole], points[whole + 1]);
            shown.push((a.0 + (b.0 - a.0) * f, a.1 + (b.1 - a.1) * f));
        }
        for (width, color, a) in [(4.5, WHITE, 0.6 * alpha), (2.6, BLUE, 0.9 * alpha)] {
            stroke(pixmap, polyline(&shown, false), width * k * s, color, a);
        }
        let (start, head) = (shown[0], shown[shown.len() - 1]);
        disc(pixmap, start, 3.5 * k * s, BLUE, alpha);
        disc(pixmap, head, 6.0 * k * s, BLUE, alpha);
    }
}

/// A tapered capsule from `a` (radius `r0`) to `b` (radius `r1`), as three fills like the
/// helper's (where they overlap, the colour builds up the same way).
fn capsule(
    pixmap: &mut PixmapMut,
    a: (f32, f32),
    b: (f32, f32),
    r0: f64,
    r1: f64,
    color: (f32, f32, f32),
    alpha: f64,
) {
    let (dx, dy) = ((b.0 - a.0) as f64, (b.1 - a.1) as f64);
    let length = dx.hypot(dy);
    if length > 0.001 {
        let (nx, ny) = (-dy / length, dx / length);
        let at = |p: (f32, f32), r: f64, sign: f64| {
            (
                (p.0 as f64 + sign * nx * r) as f32,
                (p.1 as f64 + sign * ny * r) as f32,
            )
        };
        let quad = [
            at(a, r0, 1.0),
            at(b, r1, 1.0),
            at(b, r1, -1.0),
            at(a, r0, -1.0),
        ];
        fill(pixmap, polyline(&quad, true), color, alpha);
    }
    disc(pixmap, a, r0, color, alpha);
    disc(pixmap, b, r1, color, alpha);
}

/// The laser trail with the keyframe renderer's four layers (`render::draw_laser`).
fn draw_laser(pixmap: &mut PixmapMut, points: &[((f32, f32), f64)], unit: f64) {
    if points.is_empty() {
        return;
    }
    let layers = [
        (7.5, 0.08, 1.1, rgb(255.0, 22.0, 38.0)),
        (4.2, 0.22, 1.2, rgb(255.0, 36.0, 48.0)),
        (2.1, 0.78, 1.3, rgb(255.0, 59.0, 48.0)),
        (0.95, 0.92, 1.5, rgb(255.0, 245.0, 245.0)),
    ];
    for (radius, alpha, power, c) in layers {
        let r = radius * unit;
        if let [(p, life)] = points {
            disc(pixmap, *p, r, c, alpha * life.powf(power));
        }
        for pair in points.windows(2) {
            let ((a, fa), (b, fb)) = (pair[0], pair[1]);
            capsule(
                pixmap,
                a,
                b,
                r * fa.powf(0.7),
                r * fb.powf(0.7),
                c,
                alpha * ((fa + fb) / 2.0).powf(power),
            );
        }
    }
    if let Some(&(tip, life)) = points.last()
        && life > 0.2
    {
        let heads = [
            (9.5, 0.14 * life, rgb(255.0, 20.0, 36.0)),
            (5.5, 0.34 * life, rgb(255.0, 36.0, 48.0)),
            (3.0, 0.88 * life, rgb(255.0, 59.0, 48.0)),
            (1.5, life, WHITE),
        ];
        for (radius, alpha, c) in heads {
            disc(pixmap, tip, radius * unit, c, alpha);
        }
    }
}

/// The bold system font: Segoe UI Bold, else Arial Bold.
fn bold_font() -> Option<&'static FontVec> {
    static FONT: OnceLock<Option<FontVec>> = OnceLock::new();
    FONT.get_or_init(|| {
        let dir = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
        ["segoeuib.ttf", "arialbd.ttf"].iter().find_map(|name| {
            let bytes = std::fs::read(std::path::Path::new(&dir).join("Fonts").join(name)).ok()?;
            FontVec::try_from_vec(bytes).ok()
        })
    })
    .as_ref()
}

/// A line of text's glyphs and advance width at `size` px per em.
fn layout(font: &FontVec, text: &str, size: f64) -> (f64, Vec<(ab_glyph::GlyphId, f64)>) {
    let em = f64::from(font.units_per_em().unwrap_or(1000.0));
    let scale = size / em;
    let mut x = 0.0;
    let mut previous = None;
    let mut glyphs = Vec::new();
    for ch in text.chars() {
        let id = font.glyph_id(ch);
        if let Some(prev) = previous {
            x += f64::from(font.kern_unscaled(prev, id)) * scale;
        }
        glyphs.push((id, x));
        x += f64::from(font.h_advance_unscaled(id)) * scale;
        previous = Some(id);
    }
    (x, glyphs)
}

/// A pin's number, white, centred on `c` at `size` px per em, shrunk to fit `max_width`.
fn label(pixmap: &mut PixmapMut, text: &str, c: (f32, f32), size: f64, max_width: f64, alpha: f64) {
    let Some(font) = bold_font() else {
        return;
    };
    let mut size = size;
    let (width, _) = layout(font, text, size);
    if width > max_width {
        size *= max_width / width;
    }
    let (width, glyphs) = layout(font, text, size);
    let em = f64::from(font.units_per_em().unwrap_or(1000.0));
    let scale = size / em;
    let ascent = f64::from(font.ascent_unscaled()) * scale;
    let height =
        f64::from(font.ascent_unscaled() - font.descent_unscaled() + font.line_gap_unscaled())
            * scale;
    let left = f64::from(c.0) - width / 2.0;
    let baseline = f64::from(c.1) - height / 2.0 + ascent;
    let mut pb = PathBuilder::new();
    for (id, x) in glyphs {
        let Some(outline) = font.outline(id) else {
            continue;
        };
        let at = |p: ab_glyph::Point| {
            (
                (left + x + f64::from(p.x) * scale) as f32,
                (baseline - f64::from(p.y) * scale) as f32,
            )
        };
        let mut last: Option<ab_glyph::Point> = None;
        for curve in &outline.curves {
            let (start, end) = match *curve {
                OutlineCurve::Line(a, b)
                | OutlineCurve::Quad(a, _, b)
                | OutlineCurve::Cubic(a, _, _, b) => (a, b),
            };
            if last != Some(start) {
                if last.is_some() {
                    pb.close();
                }
                let p = at(start);
                pb.move_to(p.0, p.1);
            }
            match *curve {
                OutlineCurve::Line(_, b) => {
                    let p = at(b);
                    pb.line_to(p.0, p.1);
                }
                OutlineCurve::Quad(_, q, b) => {
                    let (q, b) = (at(q), at(b));
                    pb.quad_to(q.0, q.1, b.0, b.1);
                }
                OutlineCurve::Cubic(_, q, r, b) => {
                    let (q, r, b) = (at(q), at(r), at(b));
                    pb.cubic_to(q.0, q.1, r.0, r.1, b.0, b.1);
                }
            }
            last = Some(end);
        }
        if last.is_some() {
            pb.close();
        }
    }
    fill(pixmap, pb.finish(), WHITE, alpha);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::export::plan::{Pin, Ripple, Snap};

    #[test]
    fn fades_hold_then_smoothstep_out() {
        assert_eq!(fade(1_000.0, 1_000.0, 500.0), 1.0);
        assert_eq!(fade(1_250.0, 1_000.0, 500.0), 0.5);
        assert!((fade(1_125.0, 1_000.0, 500.0) - 0.84375).abs() < 1e-9);
        assert_eq!(fade(1_500.0, 1_000.0, 500.0), 0.0);
        assert_eq!(fade(9_000.0, 1_000.0, 500.0), 0.0);
        // A zero-length fade still takes a millisecond.
        assert_eq!(fade(1_001.0, 1_000.0, 0.0), 0.0);
    }

    #[test]
    fn ripples_grow_from_9_to_33_units_and_fade_over_520_ms() {
        assert_eq!(ripple(-1.0), None);
        assert_eq!(ripple(0.0), Some((9.0, 1.0)));
        let (r, a) = ripple(260.0).unwrap();
        assert!((r - (9.0 + 24.0 * 0.875)).abs() < 1e-9 && (a - 0.5).abs() < 1e-9);
        assert!(ripple(519.0).unwrap().0 < 33.0);
        assert_eq!(ripple(520.0), None);
    }

    #[test]
    fn pins_pop_in_over_160_ms() {
        assert_eq!(pin_grow(0.0), 0.0);
        assert_eq!(pin_grow(80.0), 0.875);
        assert_eq!(pin_grow(160.0), 1.0);
        assert_eq!(pin_grow(1_000.0), 1.0);
    }

    #[test]
    fn snapshots_pick_the_last_one_at_or_before_the_time() {
        let times = [0.0, 25.0, 50.0, 75.0];
        assert_eq!(last_at(times.iter().copied(), 0.0), 0);
        assert_eq!(last_at(times.iter().copied(), 49.9), 1);
        assert_eq!(last_at(times.iter().copied(), 50.0), 2);
        assert_eq!(last_at(times.iter().copied(), 900.0), 3);
    }

    fn pixel(data: &[u8], w: usize, x: usize, y: usize) -> [u8; 3] {
        let i = (y * w + x) * 4;
        [data[i], data[i + 1], data[i + 2]]
    }

    #[test]
    fn ink_ripples_and_pins_land_where_the_view_puts_them() {
        let (w, h) = (200_usize, 100_usize);
        let mut data = vec![0_u8; w * h * 4];
        for p in data.chunks_exact_mut(4) {
            p.copy_from_slice(&[40, 40, 40, 255]);
        }
        // The right half of a 400×100 capture, shown 1:1.
        let view = View::new((400.0, 100.0), [0.5, 0.0, 1.0, 1.0], (200.0, 100.0));
        let ink = Ink {
            k: 1.0,
            pin_scale: 1.0,
            strokes: vec![crate::win::export::plan::Stroke {
                snaps: vec![Snap {
                    t: 0.0,
                    // A square from (220, 10) to (260, 50) in capture pixels.
                    poly: vec![0.55, 0.1, 0.65, 0.1, 0.65, 0.5, 0.55, 0.5],
                }],
                fade_at: 5_000.0,
                fade_ms: 500.0,
            }],
            pins: vec![Pin {
                x: 0.9,
                y: 0.5,
                n: 3,
                t: 0.0,
                fade_at: 5_000.0,
                fade_ms: 500.0,
            }],
            ripples: vec![Ripple {
                x: 0.75,
                y: 0.8,
                t: 0.0,
            }],
            ..Ink::default()
        };
        let mut pixmap = PixmapMut::from_bytes(&mut data, w as u32, h as u32).unwrap();
        draw(&mut pixmap, 100.0, &view, &ink);
        // The stroke: capture (240, 30) is output (40, 30), solid red.
        assert_eq!(pixel(&data, w, 40, 30), [255, 59, 48]);
        // Outside it, untouched.
        assert_eq!(pixel(&data, w, 5, 5), [40, 40, 40]);
        // The pin at output (160, 50): red disc with a white number in the middle.
        let ring = pixel(&data, w, 160 - 11, 50);
        assert!(ring[0] > 200 && ring[1] < 100, "{ring:?}");
        let middle: Vec<[u8; 3]> = (44..56)
            .flat_map(|y| (155..165).map(move |x| (x, y)))
            .map(|(x, y)| pixel(&data, w, x, y))
            .collect();
        assert!(
            middle.iter().any(|p| p.iter().all(|&c| c > 230)),
            "the label is white"
        );
        // The ripple at output (100, 80): blue tint inside, white ring around.
        let inside = pixel(&data, w, 100, 80);
        assert!(inside[2] > inside[0] + 40, "{inside:?}");
    }
}
