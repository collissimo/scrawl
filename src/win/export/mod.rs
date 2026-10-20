//! `scrawl export` on Windows, in process: the twin of `helper/Export.swift`, rendering the
//! same plan (`src/export`).
//!
//!   Media Foundation source reader (BGRA)  →  CPU crop / camera rectangle (`pixels`)
//!   →  tiny-skia (ink, laser, ripples, pins, drawn into the same frame, `draw`)
//!   →  NV12  →  Media Foundation sink writer (H.264) + AAC from the plan's audio (`encode`).
//!
//! Output is a constant frame rate: each tick shows the latest source frame, so the camera and
//! the ink move while the screen holds still. Every time in the plan is on the video's clock.

mod draw;
mod encode;
mod pixels;
pub mod plan;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail, ensure};
use tiny_skia::PixmapMut;
use windows::Win32::Media::MediaFoundation::{IMF2DBuffer, IMFSample};
use windows::Win32::System::Threading::{
    GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN, THREAD_MODE_BACKGROUND_END,
};
use windows::core::Interface;

use crate::export::Done;
use crate::win::media::VideoReader;

use self::encode::{AUDIO_RATE, Writer};
use self::pixels::View;
use self::plan::Plan;

/// Audio is handed to the encoder this far ahead of the frames, so the two stay interleaved.
const AUDIO_LEAD_MS: f64 = 500.0;
/// Progress is reported every this many frames.
const PROGRESS_EVERY: u64 = 30;

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

/// Background mode for the export's own thread while it runs (`Options::background`, the app's
/// menu export), the twin of the helper's `taskpolicy -c utility`. Media Foundation's encoder
/// threads keep their normal priority.
struct Priority(bool);

impl Priority {
    fn enter(background: bool) -> Self {
        BACKGROUND.store(background, Ordering::Relaxed);
        background_thread();
        Self(background)
    }
}

impl Drop for Priority {
    fn drop(&mut self) {
        if self.0 {
            let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_END) };
        }
        BACKGROUND.store(false, Ordering::Relaxed);
    }
}

fn threads() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get().min(8))
}

/// Renders the plan at `plan_path` into its `out`, reporting progress `0.0..=1.0`. The
/// counterpart of `scrawl-helper export PLAN` on macOS.
pub fn run(plan_path: &Path, background: bool, progress: &mut dyn FnMut(f32)) -> Result<Done> {
    let json = std::fs::read(plan_path)
        .with_context(|| format!("Could not read {}", plan_path.display()))?;
    let plan = Plan::parse(&json)?;
    ensure!(
        plan.codec != "hevc",
        "HEVC export needs macOS; leave out --hevc for H.264"
    );
    let _priority = Priority::enter(background);
    crate::win::media::media_startup();
    let _ = std::fs::remove_file(&plan.out);
    let result = render(&plan, progress);
    if result.is_err() {
        let _ = std::fs::remove_file(&plan.out);
    }
    result
}

/// The plan's audio as 48 kHz mono PCM, or `None` without any.
fn audio_track(plan: &Plan) -> Result<Option<Vec<i16>>> {
    let Some(path) = plan.audio.as_deref().filter(|_| plan.audio_length_ms > 0) else {
        return Ok(None);
    };
    // A file without an audio track exports silent, like the helper.
    if crate::win::media::probe(path)?.audio_tracks == 0 {
        return Ok(None);
    }
    let (rate, mono) = crate::win::media::audio_mono(path)
        .with_context(|| format!("Could not read the audio in {}", path.display()))?;
    Ok(Some(resample(
        &mono,
        rate,
        plan.audio_start_ms,
        plan.audio_length_ms,
    )))
}

/// `length_ms` of `mono` (at `rate`) from `start_ms`, at 48 kHz, linearly interpolated. Past the
/// end of the source it is silent.
fn resample(mono: &[f32], rate: u32, start_ms: u64, length_ms: u64) -> Vec<i16> {
    let out = (length_ms * u64::from(AUDIO_RATE) / 1000) as usize;
    let step = f64::from(rate) / f64::from(AUDIO_RATE);
    let start = start_ms as f64 * f64::from(rate) / 1000.0;
    (0..out)
        .map(|i| {
            let s = start + i as f64 * step;
            let i0 = s.floor() as usize;
            let f = (s - i0 as f64) as f32;
            let at = |i: usize| mono.get(i).copied().unwrap_or(0.0);
            let v = at(i0) + (at(i0 + 1) - at(i0)) * f;
            (v.clamp(-1.0, 1.0) * 32_767.0).round() as i16
        })
        .collect()
}

fn render(plan: &Plan, progress: &mut dyn FnMut(f32)) -> Result<Done> {
    let video = VideoReader::open(&plan.video)?;
    let (sw, sh) = (video.aperture.2 as usize, video.aperture.3 as usize);
    ensure!(sw > 0 && sh > 0, "The recording has no video");
    let audio = audio_track(plan)?;
    let (w, h) = (plan.width as usize, plan.height as usize);
    // The GPU's encoder when Media Foundation has one, else its software encoder.
    let with_audio = audio.is_some();
    let mut writer = Writer::new(
        &plan.out,
        plan.width,
        plan.height,
        plan.fps,
        with_audio,
        true,
    )
    .or_else(|_| {
        Writer::new(
            &plan.out,
            plan.width,
            plan.height,
            plan.fps,
            with_audio,
            false,
        )
    })?;
    let threads = threads();
    let total = plan.frames();
    let mut pending = video.next().context("Reading the recording failed")?;
    let mut source: Vec<u8> = Vec::new();
    let mut rgba = vec![0_u8; w * h * 4];
    let mut nv12 = Vec::new();
    let mut audio_at = 0_usize;
    for tick in 0..total {
        let ms = tick as f64 * 1000.0 / f64::from(plan.fps);
        // The latest source frame at this tick (or the first one, before it starts).
        let mut shown = None;
        while pending.as_ref().is_some_and(|(t, _)| *t as f64 <= ms + 0.5) {
            shown = pending.take();
            pending = video.next().context("Reading the recording failed")?;
        }
        if source.is_empty() && shown.is_none() {
            shown = pending.clone();
        }
        if let Some((_, sample)) = shown {
            bgra(&video, &sample, &mut source)?;
        }
        if source.is_empty() {
            bail!("The recording has no frames");
        }
        ensure!(
            source.len() == sw * sh * 4,
            "A frame of the recording changed size"
        );

        let view = View::new(
            (sw as f64, sh as f64),
            plan.camera_rect(ms),
            (w as f64, h as f64),
        );
        pixels::sample(&source, sw, sh, &view, &mut rgba, w, h, threads);
        let mut pixmap =
            PixmapMut::from_bytes(&mut rgba, plan.width, plan.height).context("frame size")?;
        draw::draw(&mut pixmap, ms, &view, &plan.ink);
        pixels::rgba_to_nv12(&rgba, w, h, &mut nv12, threads);

        if let Some(pcm) = &audio {
            let until =
                (((ms + AUDIO_LEAD_MS) * f64::from(AUDIO_RATE) / 1000.0) as usize).min(pcm.len());
            if until > audio_at {
                writer.audio(&pcm[audio_at..until])?;
                audio_at = until;
            }
        }
        writer.video(&nv12, tick)?;
        if (tick + 1) % PROGRESS_EVERY == 0 {
            progress((tick + 1) as f32 / total as f32);
        }
    }
    if let Some(pcm) = &audio
        && audio_at < pcm.len()
    {
        writer.audio(&pcm[audio_at..])?;
    }
    writer.finish()?;

    let written = crate::win::media::probe(&plan.out)?.duration_ms;
    if written.abs_diff(plan.duration_ms) >= 250 {
        bail!(
            "The export is {:.2} s long, the recording {:.2} s",
            written as f64 / 1000.0,
            plan.duration_ms as f64 / 1000.0
        );
    }
    let bytes = std::fs::metadata(&plan.out).map_or(0, |m| m.len());
    Ok(Done {
        duration_ms: written,
        bytes,
    })
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
        let pitch = crate::win::media::packed_pitch(len as usize, video.width, video.height);
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
        assert_eq!(crate::win::media::packed_pitch(1000 * 562 * 4, 1000, 562), 4000);
        assert_eq!(crate::win::media::packed_pitch(1008 * 576 * 4, 1000, 562), 4032);
        assert_eq!(crate::win::media::packed_pitch(1920 * 1088 * 4, 1920, 1080), 7680);
        assert_eq!(crate::win::media::packed_pitch(1920 * 1072 * 4, 1920, 1072), 7680);
    }

    #[test]
    fn audio_is_windowed_and_resampled_to_48_khz() {
        // 16 kHz: a ramp of 0.0, 0.1, 0.2, ... per sample.
        let mono: Vec<f32> = (0..16_000).map(|i| (i % 10) as f32 / 10.0).collect();
        let pcm = resample(&mono, 16_000, 500, 100);
        assert_eq!(pcm.len(), 4_800);
        // 500 ms in is sample 8000 (0.0); each source sample spans three output ones.
        assert_eq!(pcm[0], 0);
        assert_eq!(pcm[3], (0.1_f32 * 32_767.0).round() as i16);
        assert_eq!(pcm[1], (0.1_f32 / 3.0 * 32_767.0).round() as i16);
        // Past the source's end the tail is silent.
        let tail = resample(&mono, 16_000, 900, 200);
        assert_eq!(tail.len(), 9_600);
        assert!(tail[4_800..].iter().all(|&s| s == 0));
    }

    /// `scrawl export` end to end on a synthetic bundle: a grey 3 s recording, a tone in
    /// `audio.wav`, a red stroke and a click. The movie comes back the right length and size,
    /// with an AAC track, and the stroke burned in where it was drawn.
    #[test]
    #[ignore = "encodes and decodes through Media Foundation"]
    fn a_synthetic_bundle_exports_with_its_ink_and_audio() {
        crate::win::media::media_startup();
        let dir = std::env::temp_dir().join(format!("scrawl-export-{}.scrawl", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (w, h) = (960_u32, 540_u32);
        let grey = vec![128_u8; (w * h * 4) as usize];
        let mut nv12 = Vec::new();
        pixels::rgba_to_nv12(&grey, w as usize, h as usize, &mut nv12, threads());
        let writer = Writer::new(&dir.join("recording.mp4"), w, h, 30, false, true).unwrap();
        for tick in 0..90 {
            writer.video(&nv12, tick).unwrap();
        }
        writer.finish().unwrap();
        let tone: Vec<f32> = (0..16_000 * 3)
            .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / 16_000.0).sin() * 0.3)
            .collect();
        std::fs::write(
            dir.join("audio.wav"),
            crate::audio::encode_wav_16k(&tone).unwrap(),
        )
        .unwrap();
        // A horizontal stroke across the middle, drawn from 0.5 s to 0.9 s.
        let pts: Vec<i32> = (0..=20)
            .flat_map(|i| [3_000 + i * 200, 5_000, i * 20])
            .collect();
        let bundle: crate::model::ScrawlBundle = serde_json::from_value(serde_json::json!({
            "version": 1, "id": "synthetic", "createdAtMs": 0, "durationMs": 3_000,
            "target": {"mode": "display", "displayId": 0,
                "frame": {"x": 0.0, "y": 0.0, "width": 960.0, "height": 540.0, "scale": 1.0}},
            "transcript": {"source": "test", "text": "", "segments": []},
            "strokes": [{"t0": 500, "t1": 900, "fw": 960, "fh": 540, "pts": pts}],
            "marks": [], "keyframes": [], "contactSheet": "c.png",
            "video": "recording.mp4", "audio": "audio.wav", "videoOffsetMs": 0,
            "events": [{"kind": "click", "t": 1_500, "x": 0.8, "y": 0.2}],
            "cursor": []
        }))
        .unwrap();
        let out = dir.join("out.mp4");
        let options = crate::export::Options {
            out: Some(out.clone()),
            crop: crate::export::crop::CropArg::Full,
            ..Default::default()
        };
        let mut reported = Vec::new();
        let exported =
            crate::export::export(&dir, &bundle, &options, &mut |f| reported.push(f)).unwrap();
        eprintln!("{}", crate::export::describe(&exported));
        assert_eq!((exported.width, exported.height, exported.fps), (w, h, 30));
        assert!(exported.audio);
        assert_eq!(reported.last(), Some(&1.0));
        assert!(reported.windows(2).all(|p| p[0] <= p[1]), "{reported:?}");

        let probe = crate::win::media::probe(&out).unwrap();
        assert_eq!((probe.width, probe.height, probe.audio_tracks), (w, h, 1));
        assert!(probe.duration_ms.abs_diff(3_000) < 250, "{probe:?}");
        let (rate, audio) = crate::win::media::audio_mono(&out).unwrap();
        let rms = (audio.iter().map(|s| s * s).sum::<f32>() / audio.len() as f32).sqrt();
        assert_eq!(rate, AUDIO_RATE);
        assert!(
            (rms - 0.3 / 2_f32.sqrt()).abs() < 0.05,
            "the tone survives, rms {rms}"
        );

        // At 1.2 s the stroke is drawn and holding: red along y = 270 from x = 288 to 672.
        let video = VideoReader::open(&out).unwrap();
        let mut frame = Vec::new();
        while let Some((t, sample)) = video.next().unwrap() {
            if t >= 1_200 {
                bgra(&video, &sample, &mut frame).unwrap();
                break;
            }
        }
        let px = |x: usize, y: usize| {
            let i = (y * w as usize + x) * 4;
            [frame[i + 2], frame[i + 1], frame[i]]
        };
        for x in [360, 480, 600] {
            let ink = px(x, 270);
            // A 4 px line loses some saturation to 4:2:0 chroma; it stays clearly red.
            let red = ink[0] > 160 && ink[0] > ink[1] + 60 && ink[0] > ink[2] + 60;
            assert!(red, "ink at {x}: {ink:?}");
        }
        let clear = px(480, 200);
        assert!(clear.iter().all(|&c| c.abs_diff(128) < 16), "{clear:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Probes an exported movie and saves frames from it as PNGs, for looking at a real export:
    /// `SCRAWL_EXPORT_PROBE=movie.mp4 SCRAWL_EXPORT_FRAMES=1900,2800 cargo test
    /// win::export::tests::probe_an_export -- --ignored --nocapture` writes `movie-1900.png`, ….
    #[test]
    #[ignore = "reads SCRAWL_EXPORT_PROBE"]
    fn probe_an_export() {
        let Some(movie) = std::env::var_os("SCRAWL_EXPORT_PROBE") else {
            return;
        };
        let movie = std::path::PathBuf::from(movie);
        let probe = crate::win::media::probe(&movie).unwrap();
        let bytes = std::fs::metadata(&movie).unwrap().len();
        eprintln!("{}: {probe:?}, {bytes} bytes", movie.display());
        if probe.audio_tracks > 0 {
            let (rate, audio) = crate::win::media::audio_mono(&movie).unwrap();
            let rms = (audio.iter().map(|s| s * s).sum::<f32>() / audio.len().max(1) as f32).sqrt();
            let ms = audio.len() as u64 * 1000 / u64::from(rate);
            eprintln!("audio: {rate} Hz, {ms} ms, rms {rms:.4}");
        }
        let times: Vec<u64> = std::env::var("SCRAWL_EXPORT_FRAMES")
            .unwrap_or_default()
            .split(',')
            .filter_map(|t| t.trim().parse().ok())
            .collect();
        if times.is_empty() {
            return;
        }
        let video = VideoReader::open(&movie).unwrap();
        let (w, h) = (video.aperture.2, video.aperture.3);
        let mut frame = Vec::new();
        let mut times = times.into_iter().peekable();
        while let Some((t, sample)) = video.next().unwrap() {
            let Some(&want) = times.peek() else {
                break;
            };
            if t + 17 < want {
                continue;
            }
            times.next();
            bgra(&video, &sample, &mut frame).unwrap();
            let rgba = frame
                .chunks_exact(4)
                .flat_map(|p| [p[2], p[1], p[0], 255])
                .collect();
            let png = movie.with_file_name(format!(
                "{}-{want}.png",
                movie.file_stem().unwrap().to_string_lossy()
            ));
            crate::render::save_png(&crate::model::RgbaFrame::new(t, w, h, rgba), &png).unwrap();
            eprintln!("frame at {t} ms -> {}", png.display());
        }
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
