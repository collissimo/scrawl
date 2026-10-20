//! `scrawl export` on Windows, in process: the twin of `helper/Export.swift`, rendering the
//! same plan (`src/export`).
//!
//!   Media Foundation source reader (BGRA)  →  CPU crop / camera rectangle (`pixels`)
//!   →  tiny-skia (ink, laser, ripples, pins, drawn into the same frame, `draw`)
//!   →  NV12  →  Media Foundation sink writer (H.264) + AAC from the plan's audio (`encode`).
//!
//! Output is a constant frame rate: each tick shows the latest source frame, so the camera and
//! the ink move while the screen holds still. Every time in the plan is on the video's clock.

mod encode;
mod pixels;

use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Result, ensure};
use windows::Win32::Media::MediaFoundation::{IMF2DBuffer, IMFSample};
use windows::Win32::System::Threading::{
    GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN,
};
use windows::core::Interface;

use crate::win::media::VideoReader;

/// Set while a background (`Options::background`) export runs, so its worker threads lower
/// their priority too.
static BACKGROUND: AtomicBool = AtomicBool::new(false);

/// Puts the calling worker thread in background mode (low CPU and I/O priority) during a
/// background export. Threads end with the frame, so nothing needs undoing.
fn background_thread() {
    if BACKGROUND.load(Ordering::Relaxed) {
        let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) };
    }
}

/// The row pitch of a packed RGB32 frame `len` bytes long. The H.264 decoder pads frames to
/// 16 px macroblocks and the converted buffer keeps that padding (a 1000×562 frame arrives as
/// 1008×576), so a size off the grid read at `width * 4` comes back sheared.
fn packed_pitch(len: usize, width: u32, height: u32) -> usize {
    let row = width as usize * 4;
    let (h, padded) = (height as usize, (height as usize).div_ceil(16) * 16);
    if len == row * h || padded == 0 || len % padded != 0 || len / padded < row {
        row
    } else {
        len / padded
    }
}

/// A decoded frame's visible area as tightly packed BGRA.
fn bgra(video: &VideoReader, sample: &IMFSample, out: &mut Vec<u8>) -> Result<()> {
    let buffer = unsafe { sample.ConvertToContiguousBuffer() }?;
    let (ax, ay, aw, ah) = video.aperture;
    let row = aw as usize * 4;
    out.clear();
    out.reserve(row * ah as usize);
    let mut copy = |base: *const u8, pitch: isize| {
        for y in 0..ah as isize {
            let start = unsafe { base.offset((y + ay as isize) * pitch + ax as isize * 4) };
            out.extend_from_slice(unsafe { std::slice::from_raw_parts(start, row) });
        }
    };
    if let Ok(two_d) = buffer.cast::<IMF2DBuffer>() {
        let mut scan0 = std::ptr::null_mut();
        let mut pitch = 0_i32;
        unsafe { two_d.Lock2D(&mut scan0, &mut pitch) }?;
        copy(scan0, pitch as isize);
        unsafe { two_d.Unlock2D() }?;
    } else {
        let mut data = std::ptr::null_mut();
        let mut len = 0_u32;
        unsafe { buffer.Lock(&mut data, None, Some(&mut len)) }?;
        let pitch = packed_pitch(len as usize, video.width, video.height);
        let enough = len as usize >= pitch * video.height as usize;
        if enough {
            copy(data, pitch as isize);
        }
        unsafe { buffer.Unlock() }?;
        ensure!(enough, "Short video frame");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn packed_frames_keep_the_decoders_macroblock_padding() {
        assert_eq!(packed_pitch(1000 * 562 * 4, 1000, 562), 4000);
        assert_eq!(packed_pitch(1008 * 576 * 4, 1000, 562), 4032);
        assert_eq!(packed_pitch(1920 * 1088 * 4, 1920, 1080), 7680);
        assert_eq!(packed_pitch(1920 * 1072 * 4, 1920, 1072), 7680);
    }

    fn threads() -> usize {
        std::thread::available_parallelism().map_or(4, |n| n.get())
    }

    /// Encodes `seconds` of a grey frame with a red square near the bottom right, plus a tone,
    /// and returns the square's rectangle.
    fn synthetic_movie(path: &Path, w: u32, h: u32, hardware: bool) -> [usize; 4] {
        crate::win::media::media_startup();
        let fps = 30;
        let square = [w as usize - 120, h as usize - 90, 60, 40];
        let mut rgba = vec![128_u8; w as usize * h as usize * 4];
        for y in square[1]..square[1] + square[3] {
            for x in square[0]..square[0] + square[2] {
                let i = (y * w as usize + x) * 4;
                rgba[i..i + 4].copy_from_slice(&[255, 0, 0, 255]);
            }
        }
        let mut nv12 = Vec::new();
        pixels::rgba_to_nv12(&rgba, w as usize, h as usize, &mut nv12, threads());
        let mut writer = encode::Writer::new(path, w, h, fps, true, hardware).unwrap();
        for tick in 0..60 {
            let tone: Vec<i16> = (0..1600)
                .map(|i| {
                    let n = (tick * 1600 + i) as f32;
                    ((n * 440.0 * std::f32::consts::TAU / 48_000.0).sin() * 8000.0) as i16
                })
                .collect();
            writer.audio(&tone).unwrap();
            writer.video(&nv12, tick as u64).unwrap();
        }
        writer.finish().unwrap();
        square
    }

    /// The encoder writes a movie Media Foundation reads back: duration, size, an AAC track,
    /// and the square where it was drawn (a size off the 16 px grid must not shear).
    #[test]
    #[ignore = "encodes and decodes through Media Foundation"]
    fn the_encoder_round_trips_through_media_foundation() {
        for (w, h, hardware) in [(1000, 562, true), (1000, 562, false), (1920, 1080, true)] {
            let path =
                std::env::temp_dir().join(format!("scrawl-export-encoder-{w}x{h}-{hardware}.mp4"));
            let _ = std::fs::remove_file(&path);
            let square = synthetic_movie(&path, w, h, hardware);
            let probe = crate::win::media::probe(&path).unwrap();
            let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            eprintln!("{w}x{h} hw={hardware}: {bytes} bytes, {probe:?}");
            assert_eq!((probe.width, probe.height), (w, h));
            assert!(probe.duration_ms.abs_diff(2_000) < 250, "{probe:?}");
            assert_eq!(probe.audio_tracks, 1);
            let video = VideoReader::open(&path).unwrap();
            let (_, sample) = video.next().unwrap().unwrap();
            let mut frame = Vec::new();
            bgra(&video, &sample, &mut frame).unwrap();
            let (aw, ah) = (video.aperture.2 as usize, video.aperture.3 as usize);
            assert_eq!((aw, ah), (w as usize, h as usize));
            let px = |x: usize, y: usize| {
                let i = (y * aw + x) * 4;
                [frame[i + 2], frame[i + 1], frame[i]]
            };
            let inside = px(square[0] + 30, square[1] + 20);
            assert!(
                inside[0] > 220 && inside[1] < 40 && inside[2] < 40,
                "{inside:?}"
            );
            let corner = px(square[0] + 3, square[1] + 3);
            assert!(
                corner[0] > 200 && corner[1] < 60,
                "square's corner {corner:?}"
            );
            let outside = px(square[0] - 6, square[1] - 6);
            assert!(outside.iter().all(|&c| c.abs_diff(128) < 16), "{outside:?}");
            let _ = std::fs::remove_file(&path);
        }
    }
}
