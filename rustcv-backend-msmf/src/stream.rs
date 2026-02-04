use std::sync::Arc;
use std::time::Instant;
use windows::Win32::Media::MediaFoundation::*;

use async_trait::async_trait;

use rustcv_core::error::{CameraError, Result};
use rustcv_core::frame::{BackendBufferHandle, Frame, FrameMetadata, Timestamp};
use rustcv_core::time::ClockSynchronizer;
use rustcv_core::traits::Stream;

#[derive(Debug)]
pub struct MsmfBufferHandle;
impl BackendBufferHandle for MsmfBufferHandle {}

static MSMF_HANDLE_INSTANCE: MsmfBufferHandle = MsmfBufferHandle;

pub struct MsmfStream {
    source_reader: Arc<IMFSourceReader>,
    width: u32,
    height: u32,
    format: rustcv_core::pixel_format::PixelFormat,
    clock_sync: ClockSynchronizer,
    is_streaming: bool,
    sequence: u64,
    frame_data: Vec<u8>,
}

unsafe impl Send for MsmfStream {}

impl MsmfStream {
    pub fn new(
        source_reader: Arc<IMFSourceReader>,
        fmt: &super::device::NegotiatedFormat,
        _buf_count: usize,
    ) -> Result<Self> {
        Ok(Self {
            source_reader,
            width: fmt.width,
            height: fmt.height,
            format: fmt.format,
            clock_sync: ClockSynchronizer::new(30),
            is_streaming: false,
            sequence: 0,
            frame_data: Vec::new(),
        })
    }
}

#[async_trait]
impl Stream for MsmfStream {
    async fn start(&mut self) -> Result<()> {
        self.is_streaming = true;
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        self.is_streaming = false;
        Ok(())
    }

    async fn next_frame(&mut self) -> Result<Frame<'_>> {
        if !self.is_streaming {
            return Err(CameraError::Io(std::io::Error::other("Stream not started")));
        }

        let timestamp = unsafe {
            let mut stream_index = 0u32;
            let mut flags = 0u32;
            let mut timestamp = 0i64;
            let mut sample = None;

            for _ in 0..10 {
                self.source_reader
                    .ReadSample(
                        MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                        0u32,
                        Some(&mut stream_index),
                        Some(&mut flags),
                        Some(&mut timestamp),
                        Some(&mut sample),
                    )
                    .map_err(|e| {
                        CameraError::Io(std::io::Error::new(
                            std::io::ErrorKind::Other,
                            e.to_string(),
                        ))
                    })?;

                if sample.is_some() {
                    break;
                }

                std::thread::sleep(std::time::Duration::from_millis(10));
            }

            let sample = sample
                .ok_or_else(|| CameraError::Io(std::io::Error::other("No sample received")))?;

            let media_buffer = sample.GetBufferByIndex(0).map_err(|e| {
                CameraError::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    e.to_string(),
                ))
            })?;

            let mut data_ptr = std::ptr::null_mut();
            let mut current_length = 0u32;
            let mut max_length = 0u32;

            media_buffer
                .Lock(
                    &mut data_ptr,
                    Some(&mut max_length),
                    Some(&mut current_length),
                )
                .map_err(|e| {
                    CameraError::Io(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        e.to_string(),
                    ))
                })?;

            self.frame_data =
                std::slice::from_raw_parts(data_ptr as *const u8, current_length as usize).to_vec();

            media_buffer.Unlock().map_err(|e| {
                CameraError::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    e.to_string(),
                ))
            })?;

            timestamp
        };

        let arrival_time = Instant::now();
        let hw_ns = (timestamp as u64) * 100;
        let synced_time = self.clock_sync.correct(hw_ns, arrival_time);

        let metadata = FrameMetadata {
            actual_exposure_us: None,
            actual_gain_db: None,
            trigger_fired: false,
            strobe_active: false,
        };

        self.sequence += 1;

        let frame = Frame {
            data: &self.frame_data,
            width: self.width,
            height: self.height,
            stride: (self.width * self.format.bpp_estimate() / 8) as usize,
            format: self.format,
            sequence: self.sequence,
            timestamp: Timestamp {
                hw_raw_ns: hw_ns,
                system_synced: synced_time,
            },
            metadata,
            backend_handle: &MSMF_HANDLE_INSTANCE,
        };

        Ok(frame)
    }

    #[cfg(feature = "simulation")]
    async fn inject_frame(&mut self, _frame: Frame<'_>) -> Result<()> {
        Err(CameraError::SimulationError(
            "Not supported on real MSMF hardware".into(),
        ))
    }
}
