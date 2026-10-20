//! The plan `src/export` writes, as `helper/Export.swift`'s `ExportPlan` reads it.

use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub video: PathBuf,
    pub out: PathBuf,
    pub audio: Option<PathBuf>,
    pub audio_start_ms: u64,
    pub audio_length_ms: u64,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub codec: String,
    pub duration_ms: u64,
    pub crop: Vec<f64>,
    /// `[t, x0, y0, x1, y1]` rows, interpolated linearly.
    pub camera: Option<Vec<[f64; 5]>>,
    pub ink: Ink,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ink {
    pub k: f64,
    pub pin_scale: f64,
    pub strokes: Vec<Stroke>,
    pub pins: Vec<Pin>,
    pub lasers: Vec<Laser>,
    pub ripples: Vec<Ripple>,
    pub trails: Vec<Trail>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stroke {
    pub snaps: Vec<Snap>,
    pub fade_at: f64,
    pub fade_ms: f64,
}

#[derive(Debug, Deserialize)]
pub struct Snap {
    pub t: f64,
    /// The outline drawn so far, flat `x, y` pairs.
    pub poly: Vec<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pin {
    pub x: f64,
    pub y: f64,
    pub n: u32,
    pub t: f64,
    pub fade_at: f64,
    pub fade_ms: f64,
}

#[derive(Debug, Deserialize)]
pub struct Laser {
    pub frames: Vec<LaserFrame>,
}

#[derive(Debug, Deserialize)]
pub struct LaserFrame {
    pub t: f64,
    /// Flat `x, y, life` triples.
    pub pts: Vec<f64>,
}

#[derive(Debug, Deserialize)]
pub struct Ripple {
    pub x: f64,
    pub y: f64,
    pub t: f64,
}

#[derive(Debug, Deserialize)]
pub struct Trail {
    /// Flat `x, y` pairs.
    pub pts: Vec<f64>,
    pub t: f64,
    pub t1: f64,
}

impl Plan {
    pub fn parse(json: &[u8]) -> Result<Self> {
        let plan: Self = serde_json::from_slice(json).context("Could not read the export plan")?;
        ensure!(plan.crop.len() == 4, "The plan has no crop");
        ensure!(
            plan.width >= 2 && plan.height >= 2 && plan.width % 2 == 0 && plan.height % 2 == 0,
            "The plan's size {}×{} is not even",
            plan.width,
            plan.height
        );
        ensure!(plan.fps > 0, "The plan has no frame rate");
        Ok(plan)
    }

    pub fn crop(&self) -> [f64; 4] {
        [self.crop[0], self.crop[1], self.crop[2], self.crop[3]]
    }

    /// Frames in the output: the duration at the plan's rate, rounded up.
    pub fn frames(&self) -> u64 {
        (self.duration_ms * u64::from(self.fps)).div_ceil(1000)
    }

    /// The camera rectangle at `ms`: the crop without a camera, else the keys interpolated
    /// linearly and held at both ends (`ExportFrames.cameraRect`).
    pub fn camera_rect(&self, ms: f64) -> [f64; 4] {
        let Some(keys) = self.camera.as_deref().filter(|k| !k.is_empty()) else {
            return self.crop();
        };
        let rect = |k: &[f64; 5]| [k[1], k[2], k[3], k[4]];
        let (mut lo, mut hi) = (0, keys.len() - 1);
        if ms <= keys[0][0] {
            return rect(&keys[0]);
        }
        if ms >= keys[hi][0] {
            return rect(&keys[hi]);
        }
        while lo < hi - 1 {
            let mid = (lo + hi) / 2;
            if keys[mid][0] <= ms {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let (a, b) = (&keys[lo], &keys[hi]);
        let f = (ms - a[0]) / (b[0] - a[0]).max(1e-6);
        std::array::from_fn(|i| a[i + 1] + (b[i + 1] - a[i + 1]) * f)
    }
}
