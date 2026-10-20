//! The export's Media Foundation sink writer: H.264 video from NV12 frames at a constant frame
//! rate, and AAC audio from 48 kHz mono PCM, into one `.mp4` (the twin of AVAssetWriter in
//! `helper/Export.swift`).

use std::path::Path;

use anyhow::{Context, Result};
use windows::Win32::Media::MediaFoundation::{
    CODECAPI_AVEncMPVGOPSize, IMFAttributes, IMFMediaType, IMFSinkWriter, MF_MT_AAC_PAYLOAD_TYPE,
    MF_MT_ALL_SAMPLES_INDEPENDENT, MF_MT_AUDIO_AVG_BYTES_PER_SECOND, MF_MT_AUDIO_BITS_PER_SAMPLE,
    MF_MT_AUDIO_BLOCK_ALIGNMENT, MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND,
    MF_MT_AVG_BITRATE, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE,
    MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_MPEG2_PROFILE, MF_MT_PIXEL_ASPECT_RATIO,
    MF_MT_SUBTYPE, MF_MT_TRANSFER_FUNCTION, MF_MT_VIDEO_NOMINAL_RANGE, MF_MT_VIDEO_PRIMARIES,
    MF_MT_YUV_MATRIX, MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, MF_TRANSCODE_CONTAINERTYPE,
    MFAudioFormat_AAC, MFAudioFormat_PCM, MFCreateAttributes, MFCreateMediaType,
    MFCreateMemoryBuffer, MFCreateSample, MFCreateSinkWriterFromURL, MFMediaType_Audio,
    MFMediaType_Video, MFNominalRange_16_235, MFTranscodeContainerType_MPEG4, MFVideoFormat_H264,
    MFVideoFormat_NV12, MFVideoInterlace_Progressive, MFVideoPrimaries_BT709, MFVideoTransFunc_709,
    MFVideoTransferMatrix_BT709, eAVEncH264VProfile_High,
};
use windows::core::{GUID, HSTRING};

/// AAC in the export is 48 kHz mono at 96 kbit/s, like the macOS helper's.
pub const AUDIO_RATE: u32 = 48_000;
const AAC_BYTES_PER_SECOND: u32 = 12_000;

fn pack(hi: u32, lo: u32) -> u64 {
    (u64::from(hi) << 32) | u64::from(lo)
}

/// The helper's bit rate: 0.065 bit per pixel and frame, between 1.5 and 18 Mbit/s.
pub fn bit_rate(width: u32, height: u32, fps: u32) -> u32 {
    let pixels = f64::from(width) * f64::from(height) * f64::from(fps);
    (pixels * 0.065).clamp(1_500_000.0, 18_000_000.0) as u32
}

/// The 100 ns presentation time of frame `tick`.
pub fn tick_time(tick: u64, fps: u32) -> i64 {
    (u128::from(tick) * 10_000_000 / u128::from(fps.max(1))) as i64
}

fn video_type(subtype: &GUID, width: u32, height: u32, fps: u32) -> Result<IMFMediaType> {
    let media = unsafe { MFCreateMediaType() }?;
    unsafe {
        media.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        media.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        media.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        media.SetUINT64(&MF_MT_FRAME_SIZE, pack(width, height))?;
        media.SetUINT64(&MF_MT_FRAME_RATE, pack(fps, 1))?;
        media.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        // BT.709, limited range: what `pixels::rgba_to_nv12` writes.
        media.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
        media.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32)?;
        media.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32)?;
        media.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
    }
    Ok(media)
}

fn audio_type(subtype: &GUID, bytes_per_second: u32) -> Result<IMFMediaType> {
    let media = unsafe { MFCreateMediaType() }?;
    unsafe {
        media.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        media.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        media.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, AUDIO_RATE)?;
        media.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 1)?;
        media.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        media.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, bytes_per_second)?;
    }
    Ok(media)
}

pub struct Writer {
    writer: IMFSinkWriter,
    video: u32,
    audio: Option<u32>,
    fps: u32,
    /// Audio samples written so far (48 kHz).
    pub audio_written: u64,
}

impl Writer {
    /// `hardware` lets Media Foundation pick the GPU's encoder.
    pub fn new(
        path: &Path,
        width: u32,
        height: u32,
        fps: u32,
        audio: bool,
        hardware: bool,
    ) -> Result<Self> {
        let mut attributes: Option<IMFAttributes> = None;
        unsafe { MFCreateAttributes(&mut attributes, 3) }?;
        let attributes = attributes.context("attributes")?;
        unsafe {
            attributes.SetUINT32(
                &MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS,
                u32::from(hardware),
            )?;
            // Not `MF_MPEG4SINK_MOOV_BEFORE_MDAT`: on Windows 10 it writes a movie Media
            // Foundation can't read back.
            attributes.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)?;
        }
        let url = HSTRING::from(path.as_os_str());
        let writer = unsafe { MFCreateSinkWriterFromURL(&url, None, &attributes) }
            .with_context(|| format!("Could not create {}", path.display()))?;

        let output = video_type(&MFVideoFormat_H264, width, height, fps)?;
        unsafe {
            output.SetUINT32(&MF_MT_AVG_BITRATE, bit_rate(width, height, fps))?;
            output.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
        }
        let video = unsafe { writer.AddStream(&output) }.context("No H.264 encoder")?;
        let input = video_type(&MFVideoFormat_NV12, width, height, fps)?;
        let mut params: Option<IMFAttributes> = None;
        unsafe { MFCreateAttributes(&mut params, 1) }?;
        let params = params.context("attributes")?;
        unsafe {
            input.SetUINT32(&MF_MT_DEFAULT_STRIDE, width)?;
            input.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
            // A key frame every 2 s.
            params.SetUINT32(&CODECAPI_AVEncMPVGOPSize, fps * 2)?;
            writer
                .SetInputMediaType(video, &input, &params)
                .context("The H.264 encoder refused the frames")?;
        }

        let audio = if audio {
            let output = audio_type(&MFAudioFormat_AAC, AAC_BYTES_PER_SECOND)?;
            unsafe { output.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0) }?;
            let stream = unsafe { writer.AddStream(&output) }.context("No AAC encoder")?;
            let input = audio_type(&MFAudioFormat_PCM, AUDIO_RATE * 2)?;
            unsafe {
                input.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 2)?;
                input.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
                writer
                    .SetInputMediaType(stream, &input, None)
                    .context("The AAC encoder refused the audio")?;
            }
            Some(stream)
        } else {
            None
        };
        unsafe { writer.BeginWriting() }.context("Could not start writing the movie")?;
        Ok(Self {
            writer,
            video,
            audio,
            fps,
            audio_written: 0,
        })
    }

    fn write(&self, stream: u32, bytes: &[u8], time: i64, duration: i64) -> Result<()> {
        let buffer = unsafe { MFCreateMemoryBuffer(bytes.len() as u32) }?;
        unsafe {
            let mut data = std::ptr::null_mut();
            buffer.Lock(&mut data, None, None)?;
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(bytes.len() as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(time)?;
            sample.SetSampleDuration(duration)?;
            self.writer.WriteSample(stream, &sample)?;
        }
        Ok(())
    }

    /// Writes NV12 frame number `tick`.
    pub fn video(&self, nv12: &[u8], tick: u64) -> Result<()> {
        let time = tick_time(tick, self.fps);
        let duration = tick_time(tick + 1, self.fps) - time;
        self.write(self.video, nv12, time, duration)
            .context("Encoding a frame failed")
    }

    /// Appends 48 kHz mono samples after the ones already written.
    pub fn audio(&mut self, pcm: &[i16]) -> Result<()> {
        let Some(stream) = self.audio else {
            return Ok(());
        };
        if pcm.is_empty() {
            return Ok(());
        }
        let start = self.audio_written;
        let end = start + pcm.len() as u64;
        let time = (u128::from(start) * 10_000_000 / u128::from(AUDIO_RATE)) as i64;
        let next = (u128::from(end) * 10_000_000 / u128::from(AUDIO_RATE)) as i64;
        let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        self.write(stream, &bytes, time, next - time)
            .context("Encoding the audio failed")?;
        self.audio_written = end;
        Ok(())
    }

    pub fn finish(self) -> Result<()> {
        unsafe { self.writer.Finalize() }.context("Could not finish the movie")
    }
}
