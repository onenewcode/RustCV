use async_trait::async_trait;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Instant;
use tokio::sync::Notify;

use windows::core::{implement, IUnknown, Interface, Result, HRESULT};
use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer2, IMFMediaEvent, IMFMediaSource, IMFSample, IMFSourceReader,
    IMFSourceReaderCallback, IMFSourceReaderCallback2, IMFSourceReaderCallback2_Impl,
    IMFSourceReaderCallback_Impl, MF2DBuffer_LockFlags_Read, MFCreateAttributes,
    MFCreateSourceReaderFromMediaSource, MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS,
    MF_SOURCE_READER_ASYNC_CALLBACK, MF_SOURCE_READER_FIRST_VIDEO_STREAM,
};

// =============================================================================
// 1. 共享状态
// =============================================================================
struct SharedState {
    sample_slot: Mutex<Option<IMFSample>>,
    timestamp: AtomicI64,
    notifier: Notify,
    is_running: AtomicBool,
}

// =============================================================================
// 2. 生产者：MsmfCallback (修正弱引用和逻辑)
// =============================================================================
#[implement(IMFSourceReaderCallback, IMFSourceReaderCallback2)]
struct MsmfCallback {
    // 使用 OnceLock 存储 Reader 的弱引用，防止循环引用导致内存泄漏
    reader: OnceLock<Weak<IMFSourceReader>>,
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
        _: u32,
        _: u32,
        ts: i64,
        sample: Option<&IMFSample>,
    ) -> Result<()> {
        // 1. 检查运行状态和错误码
        if !self.shared.is_running.load(Ordering::Acquire) || hr.is_err() {
            return Ok(());
        }

        // 2. 更新 Sample 槽位并通知消费者
        if let Some(s) = sample {
            {
                let mut slot = self.shared.sample_slot.lock().unwrap();
                *slot = Some(s.clone());
            }
            self.shared.timestamp.store(ts, Ordering::Release);
            self.shared.notifier.notify_waiters();
        }

        // 3. 异步请求下一帧（极致转发的关键）
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

    fn OnFlush(&self, _: u32) -> Result<()> {
        Ok(())
    }
    fn OnEvent(&self, _: u32, _: &IMFMediaEvent) -> Result<()> {
        Ok(())
    }
}

impl IMFSourceReaderCallback2_Impl for MsmfCallback {
    fn OnTransformChange(&self) -> Result<()> {
        Ok(())
    }
    fn OnStreamError(&self, _: u32, _: HRESULT) -> Result<()> {
        Ok(())
    }
}

// =============================================================================
// 3. 消费者：MsmfStream (修正初始化顺序)
// =============================================================================
pub struct MsmfStream {
    source_reader: IMFSourceReader,
    shared: Arc<SharedState>,
    linear_buffer: Vec<u8>,
    width: u32,
    height: u32,
    format: PixelFormat,
    line_width_bytes: usize,
    sequence: u64,
    clock_sync: ClockSynchronizer,
}

impl MsmfStream {
    pub fn new(
        media_source: &IMFMediaSource, // 传入 MediaSource 而不是 Reader
        fmt: &super::device::NegotiatedFormat,
    ) -> Result<Self> {
        let bpp = fmt.format.bpp_estimate();
        let line_width_bytes = (fmt.width * bpp / 8) as usize;
        let total_size = line_width_bytes * fmt.height as usize;

        // A. 初始化共享状态
        let shared = Arc::new(SharedState {
            sample_slot: Mutex::new(None),
            timestamp: AtomicI64::new(0),
            notifier: Notify::new(),
            is_running: AtomicBool::new(false),
        });

        // B. 先创建回调对象
        let callback_impl = MsmfCallback::new(shared.clone());
        let callback_interface: IMFSourceReaderCallback = callback_impl.clone().into();

        // C. 配置属性，绑定回调以开启异步模式
        let attributes = unsafe {
            let attr = MFCreateAttributes(2)?;
            attr.SetUnknown(
                &MF_SOURCE_READER_ASYNC_CALLBACK,
                &callback_interface.cast::<IUnknown>()?,
            )?;
            attr.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
            attr
        };

        // D. 创建 SourceReader
        let source_reader =
            unsafe { MFCreateSourceReaderFromMediaSource(media_source, Some(&attributes))? };

        // E. 注入 Reader 的弱引用到回调中，完成闭环
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
        let sample = loop {
            // 1. 等待信号
            self.shared.notifier.notified().await;

            // 2. 尝试从槽位取出采样
            let mut slot = self.shared.sample_slot.lock().unwrap();
            if let Some(s) = slot.take() {
                // take() 可以减少对旧 Sample 的持有时间
                break s;
            }
            // 如果被唤醒但没拿到 Sample，继续循环（应对虚假唤醒）
        };

        let ts_raw = self.shared.timestamp.load(Ordering::Acquire);

        // 3. 数据平铺拷贝逻辑
        unsafe {
            let buffer = sample.GetBufferByIndex(0)?;
            if let Ok(buffer2d) = buffer.cast::<IMF2DBuffer2>() {
                let (mut pb_scanline, mut l_pitch) = (std::ptr::null_mut(), 0);
                let (mut pb_start, mut cb_buf) = (std::ptr::null_mut(), 0);
                buffer2d.Lock2DSize(
                    MF2DBuffer_LockFlags_Read,
                    &mut pb_scanline,
                    &mut l_pitch,
                    &mut pb_start,
                    &mut cb_buf,
                )?;

                let src_stride = l_pitch as usize;
                if src_stride == self.line_width_bytes {
                    std::ptr::copy_nonoverlapping(
                        pb_scanline,
                        self.linear_buffer.as_mut_ptr(),
                        self.linear_buffer.len(),
                    );
                } else {
                    for y in 0..self.height as usize {
                        std::ptr::copy_nonoverlapping(
                            pb_scanline.add(y * src_stride),
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

        let hw_ns = (ts_raw as u64) * 100;
        self.sequence += 1;

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
