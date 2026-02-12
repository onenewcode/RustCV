//! This module implements the video stream for Windows using Media Foundation.
//! It defines the `MsmfStream` which captures frames from a camera device
//! asynchronously.

use async_trait::async_trait;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Instant;
use tokio::sync::Notify;

use rustcv_core::{
    frame::{Frame, FrameMetadata},
    pixel_format::PixelFormat,
    time::{ClockSynchronizer, Timestamp},
    traits::{BackendHandle, Stream},
};

use windows::core::{implement, IUnknown, Interface, Result, HRESULT};
use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer2, IMFMediaEvent, IMFMediaSource, IMFSample, IMFSourceReader,
    IMFSourceReaderCallback, IMFSourceReaderCallback2, IMFSourceReaderCallback2_Impl,
    IMFSourceReaderCallback_Impl, MF2DBuffer_LockFlags_Read, MFCreateAttributes,
    MFCreateSourceReaderFromMediaSource, MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS,
    MF_SOURCE_READER_ASYNC_CALLBACK, MF_SOURCE_READER_FIRST_VIDEO_STREAM,
};

/// `SharedState` facilitates communication between the asynchronous `MsmfCallback`
/// and the main `MsmfStream`. It holds the most recent sample, its timestamp,
/// and synchronization primitives.
struct SharedState {
    /// A mutex-protected slot to hold the latest `IMFSample` from the camera.
    sample_slot: Mutex<Option<IMFSample>>,
    /// The timestamp of the latest sample, in 100-nanosecond units.
    timestamp: AtomicI64,
    /// A `Notify` object to signal the `MsmfStream` that a new sample is available.
    notifier: Notify,
    /// An atomic boolean to control the running state of the callback loop.
    is_running: AtomicBool,
}

/// `MsmfCallback` is the implementation of the `IMFSourceReaderCallback` COM interface.
/// An instance of this struct is passed to the Media Foundation source reader to receive
/// samples and events asynchronously.
#[implement(IMFSourceReaderCallback, IMFSourceReaderCallback2)]
struct MsmfCallback {
    /// A weak reference to the `IMFSourceReader` to avoid a reference cycle.
    /// The source reader owns the callback, and the callback needs to call methods
    /// on the source reader to request the next sample.
    reader: OnceLock<Weak<IMFSourceReader>>,
    /// The shared state for communicating with the `MsmfStream`.
    shared: Arc<SharedState>,
}

impl MsmfCallback {
    fn new(shared: Arc<SharedState>) -> Self {
        Self {
            reader: OnceLock::new(),
            shared,
        }
    }
}

impl IMFSourceReaderCallback_Impl for MsmfCallback {
    fn OnReadSample(
        &self,
        hr: HRESULT,
        _stream_index: u32,
        _stream_flags: u32,
        timestamp: i64,
        sample: Option<&IMFSample>,
    ) -> Result<()> {
        // Stop processing if the stream is no longer running or if there was an error.
        if !self.shared.is_running.load(Ordering::Acquire) || hr.is_err() {
            return Ok(());
        }

        if let Some(s) = sample {
            {
                let mut slot = self.shared.sample_slot.lock().unwrap();
                *slot = Some(s.clone());
            }
            self.shared.timestamp.store(timestamp, Ordering::Release);
            self.shared.notifier.notify_waiters();
        }

        // Request the next sample to keep the stream flowing.
        if let Some(reader) = self.reader.get().and_then(|w| w.upgrade()) {
            unsafe {
                let _ = reader.ReadSample(
                    MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                    0,
                    None,
                    None,
                    None,
                    None,
                );
            }
        }
        Ok(())
    }

    /// Called when the source reader has completed a flush operation.
    fn OnFlush(&self, _: u32) -> Result<()> {
        Ok(())
    }

    /// Called when an event occurs in the media source.
    fn OnEvent(&self, _: u32, _: &IMFMediaEvent) -> Result<()> {
        Ok(())
    }
}

impl IMFSourceReaderCallback2_Impl for MsmfCallback {
    /// Called when the source reader's transform chain is modified.
    fn OnTransformChange(&self) -> Result<()> {
        Ok(())
    }

    /// Called when a stream error occurs.
    fn OnStreamError(&self, _: u32, _: HRESULT) -> Result<()> {
        Ok(())
    }
}

pub static MSMF_HANDLE_INSTANCE: MsmfHandle = MsmfHandle;
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MsmfHandle;
impl BackendHandle for MsmfHandle {}

/// `MsmfStream` is a video stream implementation using Windows Media Foundation (MSMF).
/// It asynchronously receives frames from a media source (like a webcam) and makes them
/// available as `Frame` objects.
pub struct MsmfStream {
    source_reader: IMFSourceReader,
    shared: Arc<SharedState>,
    /// A buffer to hold the linearized frame data, copied from `IMFSample`.
    linear_buffer: Vec<u8>,
    width: u32,
    height: u32,
    format: PixelFormat,
    /// The number of bytes in one row of the image.
    line_width_bytes: usize,
    sequence: u64,
    clock_sync: ClockSynchronizer,
}

impl MsmfStream {
    /// Creates a new `MsmfStream` from a given `IMFMediaSource` and negotiated format.
    pub fn new(
        media_source: &IMFMediaSource,
        fmt: &super::device::NegotiatedFormat,
    ) -> Result<Self> {
        // Calculate buffer properties based on the negotiated format.
        let bpp = fmt.format.bpp_estimate();
        let line_width_bytes = (fmt.width * bpp / 8) as usize;
        let total_size = line_width_bytes * fmt.height as usize;

        // Initialize the shared state for communication with the callback.
        let shared = Arc::new(SharedState {
            sample_slot: Mutex::new(None),
            timestamp: AtomicI64::new(0),
            notifier: Notify::new(),
            is_running: AtomicBool::new(false),
        });

        // Create the callback implementation and get its COM interface.
        let callback_impl = MsmfCallback::new(shared.clone());
        let callback_interface: IMFSourceReaderCallback = callback_impl.clone().into();

        // Configure the source reader attributes for asynchronous mode.
        let attributes = unsafe {
            let attr = MFCreateAttributes(2)?;
            // Set the callback interface.
            attr.SetUnknown(
                &MF_SOURCE_READER_ASYNC_CALLBACK,
                &callback_interface.cast::<IUnknown>()?,
            )?;
            // Enable hardware transforms for better performance.
            // attr.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
            attr
        };

        // Create the source reader from the media source and attributes.
        let source_reader =
            unsafe { MFCreateSourceReaderFromMediaSource(media_source, Some(&attributes))? };

        // Provide the callback with a weak reference to the source reader to break the retain cycle.
        // We temporarily create an Arc to get a Weak pointer.
        let _ = callback_impl
            .reader
            .set(Arc::downgrade(&Arc::new(source_reader.clone())));

        Ok(Self {
            source_reader,
            shared,
            linear_buffer: vec![0u8; total_size],
            width: fmt.width,
            height: fmt.height,
            format: fmt.format,
            line_width_bytes,
            sequence: 0,
            clock_sync: ClockSynchronizer::new(30),
        })
    }

    /// Copies the image data from an `IMFSample` into the stream's `linear_buffer`.
    /// This function handles both 2D and contiguous buffers and deals with stride differences.
    fn copy_sample_to_linear_buffer(&mut self, sample: &IMFSample) -> Result<()> {
        // Unsafe block is required for various FFI calls to Media Foundation.
        unsafe {
            let buffer = sample.GetBufferByIndex(0)?;
            if let Ok(buffer2d) = buffer.cast::<IMF2DBuffer2>() {
                let (mut scanline_ptr, mut pitch) = (std::ptr::null_mut(), 0);
                let (mut buffer_start_ptr, mut buffer_size) = (std::ptr::null_mut(), 0);

                buffer2d.Lock2DSize(
                    MF2DBuffer_LockFlags_Read,
                    &mut scanline_ptr,
                    &mut pitch,
                    &mut buffer_start_ptr,
                    &mut buffer_size,
                )?;

                let src_stride = pitch as usize;
                if src_stride == self.line_width_bytes {
                    std::ptr::copy_nonoverlapping(
                        scanline_ptr,
                        self.linear_buffer.as_mut_ptr(),
                        self.linear_buffer.len(),
                    );
                } else {
                    for y in 0..self.height as usize {
                        std::ptr::copy_nonoverlapping(
                            scanline_ptr.add(y * src_stride),
                            self.linear_buffer
                                .as_mut_ptr()
                                .add(y * self.line_width_bytes),
                            self.line_width_bytes,
                        );
                    }
                }
                let _ = buffer2d.Unlock2D();
            } else {
                let (mut ptr, mut len) = (std::ptr::null_mut(), 0);
                buffer.Lock(&mut ptr, None, Some(&mut len))?;
                std::ptr::copy_nonoverlapping(
                    ptr,
                    self.linear_buffer.as_mut_ptr(),
                    (len as usize).min(self.linear_buffer.len()),
                );
                let _ = buffer.Unlock();
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Stream for MsmfStream {
    async fn start(&mut self) -> Result<()> {
        self.shared.is_running.store(true, Ordering::Release);
        unsafe {
            self.source_reader.ReadSample(
                MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                0,
                None,
                None,
                None,
                None,
            )?;
        }
        Ok(())
    }

    /// Stops the video stream.
    async fn stop(&mut self) -> Result<()> {
        self.shared.is_running.store(false, Ordering::Release);
        unsafe {
            let _ = self
                .source_reader
                .Flush(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32);
        }
        Ok(())
    }

    async fn next_frame(&mut self) -> Result<Frame<'_>> {
        // Wait for the callback to notify us that a new sample is available.
        let sample = loop {
            self.shared.notifier.notified().await;

            // Try to take the sample from the shared slot.
            let mut slot = self.shared.sample_slot.lock().unwrap();
            if let Some(s) = slot.take() {
                break s;
            }
            // If the slot was empty, loop and wait for the next notification.
        };

        let ts_raw = self.shared.timestamp.load(Ordering::Acquire);

        // Copy the sample data into our linear buffer.
        self.copy_sample_to_linear_buffer(&sample)?;

        let hw_ns = (ts_raw as u64) * 100;
        self.sequence += 1;

        // Construct and return the frame.
        Ok(Frame {
            data: &self.linear_buffer,
            width: self.width,
            height: self.height,
            stride: self.line_width_bytes,
            format: self.format,
            sequence: self.sequence,
            timestamp: Timestamp {
                hw_raw_ns: hw_ns,
                system_synced: self.clock_sync.correct(hw_ns, Instant::now()),
            },
            metadata: FrameMetadata::default(),
            backend_handle: &MSMF_HANDLE_INSTANCE,
        })
    }
}
