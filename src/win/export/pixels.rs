//! The export's picture on the CPU: the camera rectangle of a source frame scaled into the
//! output (what Core Image does in `helper/Export.swift`), and RGBA to NV12 for the encoder.

/// Maps the capture's normalized coordinates to output pixels (y down) for one camera
/// rectangle `[x0, y0, x1, y1]` (`ExportView` in the helper).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub source_w: f64,
    pub source_h: f64,
    pub origin_x: f64,
    pub origin_y: f64,
    pub scale_x: f64,
    pub scale_y: f64,
}

impl View {
    pub fn new(source: (f64, f64), rect: [f64; 4], out: (f64, f64)) -> Self {
        let (sw, sh) = source;
        Self {
            source_w: sw,
            source_h: sh,
            origin_x: rect[0] * sw,
            origin_y: rect[1] * sh,
            scale_x: out.0 / ((rect[2] - rect[0]) * sw).max(1.0),
            scale_y: out.1 / ((rect[3] - rect[1]) * sh).max(1.0),
        }
    }

    pub fn scale(&self) -> f64 {
        (self.scale_x + self.scale_y) / 2.0
    }

    pub fn point(&self, x: f64, y: f64) -> (f32, f32) {
        (
            ((x * self.source_w - self.origin_x) * self.scale_x) as f32,
            ((y * self.source_h - self.origin_y) * self.scale_y) as f32,
        )
    }
}

/// Where output pixel `o` samples the source along one axis: the two neighbours and the weight
/// of the second (0..=256), for each of `taps` sub-samples spread over the pixel.
fn axis(out: usize, origin: f64, scale: f64, len: usize, taps: usize) -> Vec<(u32, u32, u32)> {
    let last = len.saturating_sub(1) as f64;
    let mut v = Vec::with_capacity(out * taps);
    for o in 0..out {
        for k in 0..taps {
            let sub = o as f64 + (k as f64 + 0.5) / taps as f64;
            let s = (origin + sub / scale - 0.5).clamp(0.0, last);
            let i = s.floor();
            let i0 = i as u32;
            let i1 = (i0 + 1).min(last as u32);
            v.push((i0, i1, ((s - i) * 256.0).round() as u32));
        }
    }
    v
}

/// How many sub-samples per axis keep a downscale from aliasing (bilinear up to 2:1).
fn taps(scale: f64) -> usize {
    if scale >= 0.75 {
        1
    } else {
        ((1.0 / scale).ceil() as usize).clamp(2, 4)
    }
}

/// Scales `view`'s rectangle of a BGRA `source` (`sw × sh`, rows `sw * 4` apart) into an
/// opaque RGBA `out` of `ow × oh`, splitting rows over `threads`.
#[allow(clippy::too_many_arguments)]
pub fn sample(
    source: &[u8],
    sw: usize,
    sh: usize,
    view: &View,
    out: &mut [u8],
    ow: usize,
    oh: usize,
    threads: usize,
) {
    let (tx, ty) = (taps(view.scale_x), taps(view.scale_y));
    let xs = axis(ow, view.origin_x, view.scale_x, sw, tx);
    let ys = axis(oh, view.origin_y, view.scale_y, sh, ty);
    let rows_per = oh.div_ceil(threads.max(1)).max(1);
    let n = (tx * ty) as u32;
    std::thread::scope(|scope| {
        for (chunk, rows) in out.chunks_mut(rows_per * ow * 4).enumerate() {
            let (xs, ys) = (&xs, &ys);
            scope.spawn(move || {
                super::background_thread();
                for (r, row) in rows.chunks_exact_mut(ow * 4).enumerate() {
                    let y = chunk * rows_per + r;
                    let yt = &ys[y * ty..(y + 1) * ty];
                    for (x, px) in row.chunks_exact_mut(4).enumerate() {
                        let mut acc = [0_u32; 3];
                        for &(x0, x1, fx) in &xs[x * tx..(x + 1) * tx] {
                            for &(y0, y1, fy) in yt {
                                let at = |xx: u32, yy: u32| (yy as usize * sw + xx as usize) * 4;
                                let (a, b) = (at(x0, y0), at(x1, y0));
                                let (c, d) = (at(x0, y1), at(x1, y1));
                                for ch in 0..3 {
                                    let top = u32::from(source[a + ch]) * (256 - fx)
                                        + u32::from(source[b + ch]) * fx;
                                    let bottom = u32::from(source[c + ch]) * (256 - fx)
                                        + u32::from(source[d + ch]) * fx;
                                    acc[ch] += top * (256 - fy) + bottom * fy;
                                }
                            }
                        }
                        // BGRA in, RGBA out.
                        let div = n << 16;
                        px[0] = ((acc[2] + div / 2) / div) as u8;
                        px[1] = ((acc[1] + div / 2) / div) as u8;
                        px[2] = ((acc[0] + div / 2) / div) as u8;
                        px[3] = 255;
                    }
                }
            });
        }
    });
}

/// BT.709 limited-range luma of 8-bit RGB.
fn luma(r: i32, g: i32, b: i32) -> u8 {
    (((47 * r + 157 * g + 16 * b + 128) >> 8) + 16).clamp(16, 235) as u8
}

fn chroma(r: i32, g: i32, b: i32) -> (u8, u8) {
    let cb = ((-26 * r - 86 * g + 112 * b + 128) >> 8) + 128;
    let cr = ((112 * r - 102 * g - 10 * b + 128) >> 8) + 128;
    (cb.clamp(16, 240) as u8, cr.clamp(16, 240) as u8)
}

/// Opaque RGBA (`w × h`, both even) to NV12, BT.709 limited range: the Y plane, then
/// interleaved Cb/Cr at half size, each pixel's chroma the average of its 2×2 block.
pub fn rgba_to_nv12(rgba: &[u8], w: usize, h: usize, out: &mut Vec<u8>, threads: usize) {
    out.clear();
    out.resize(w * h + w * (h / 2), 0);
    let (luma_plane, chroma_plane) = out.split_at_mut(w * h);
    let pairs = (h / 2).div_ceil(threads.max(1)).max(1);
    std::thread::scope(|scope| {
        for (chunk, (ys, uv)) in luma_plane
            .chunks_mut(pairs * 2 * w)
            .zip(chroma_plane.chunks_mut(pairs * w))
            .enumerate()
        {
            scope.spawn(move || {
                super::background_thread();
                for (p, (ys, uv)) in ys
                    .chunks_exact_mut(2 * w)
                    .zip(uv.chunks_exact_mut(w))
                    .enumerate()
                {
                    let y = (chunk * pairs + p) * 2;
                    for x in (0..w).step_by(2) {
                        let mut sum = [0_i32; 3];
                        for (dy, dx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                            let i = ((y + dy) * w + x + dx) * 4;
                            let (r, g, b) = (
                                i32::from(rgba[i]),
                                i32::from(rgba[i + 1]),
                                i32::from(rgba[i + 2]),
                            );
                            ys[dy * w + x + dx] = luma(r, g, b);
                            sum[0] += r;
                            sum[1] += g;
                            sum[2] += b;
                        }
                        let (cb, cr) = chroma((sum[0] + 2) / 4, (sum[1] + 2) / 4, (sum[2] + 2) / 4);
                        uv[x] = cb;
                        uv[x + 1] = cr;
                    }
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_view_maps_the_camera_rectangle_onto_the_output() {
        let view = View::new((2000.0, 1000.0), [0.25, 0.5, 0.75, 1.0], (500.0, 250.0));
        assert_eq!((view.scale_x, view.scale_y), (0.5, 0.5));
        assert_eq!(view.point(0.25, 0.5), (0.0, 0.0));
        assert_eq!(view.point(0.75, 1.0), (500.0, 250.0));
        assert_eq!(view.point(0.5, 0.75), (250.0, 125.0));
        assert_eq!(view.scale(), 0.5);
    }

    #[test]
    fn nv12_is_bt709_limited_range() {
        assert_eq!(luma(0, 0, 0), 16);
        assert_eq!(luma(255, 255, 255), 235);
        assert_eq!(chroma(128, 128, 128), (128, 128));
        // Pure red: Y 63, Cb 102, Cr 240 in BT.709 limited range.
        assert!((i32::from(luma(255, 0, 0)) - 63).abs() <= 1);
        let (cb, cr) = chroma(255, 0, 0);
        assert!((i32::from(cb) - 102).abs() <= 1 && (i32::from(cr) - 240).abs() <= 1);

        let mut rgba = Vec::new();
        for _ in 0..4 * 4 {
            rgba.extend_from_slice(&[255, 0, 0, 255]);
        }
        let mut out = Vec::new();
        rgba_to_nv12(&rgba, 4, 4, &mut out, 3);
        assert_eq!(out.len(), 16 + 8);
        assert!(out[..16].iter().all(|&y| y == luma(255, 0, 0)));
        assert_eq!(&out[16..18], &[chroma(255, 0, 0).0, chroma(255, 0, 0).1]);
    }

    #[test]
    fn sampling_crops_scales_and_swaps_to_rgba() {
        // A 4×2 BGRA source: left half blue, right half red.
        let mut source = Vec::new();
        for _ in 0..2 {
            for x in 0..4 {
                source.extend_from_slice(if x < 2 {
                    &[255, 0, 0, 255]
                } else {
                    &[0, 0, 255, 255]
                });
            }
        }
        // The right half, scaled up 2×.
        let view = View::new((4.0, 2.0), [0.5, 0.0, 1.0, 1.0], (4.0, 4.0));
        let mut out = vec![0; 4 * 4 * 4];
        sample(&source, 4, 2, &view, &mut out, 4, 4, 2);
        // Past the first column, which blends in the blue edge.
        let inside = out.chunks_exact(4).enumerate().filter(|(i, _)| i % 4 > 0);
        assert!(
            inside.into_iter().all(|(_, p)| p == [255, 0, 0, 255]),
            "{out:?}"
        );
        // The whole frame, scaled down 4× (sub-sampled): an even mix.
        let view = View::new((4.0, 2.0), [0.0, 0.0, 1.0, 1.0], (1.0, 1.0));
        let mut out = vec![0; 4];
        sample(&source, 4, 2, &view, &mut out, 1, 1, 1);
        assert!((i32::from(out[0]) - 128).abs() <= 2 && (i32::from(out[2]) - 128).abs() <= 2);
    }
}
