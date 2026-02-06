use futures::Stream;
use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll, Waker};
use windows::{
    core::*, Win32::Foundation::*, Win32::Media::MediaFoundation::*, Win32::System::Com::*,
};

// --- 全局初始化 ---
static MF_STARTUP: std::sync::Once = std::sync::Once::new();

fn ensure_mf_initialized() {
    MF_STARTUP.call_once(|| unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_FULL).expect("MFStartup Failed");
    });
}

// --- 高性能 Frame 包装 ---

pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub stride: i32,
    pub timestamp: i64,
    data: IMFSample,
    buffer: IMFMediaBuffer,
    ptr: *mut u8,
    pub length: u32,
}

impl VideoFrame {
    /// 获取原始切片。注意：由于 Stride 存在，每一行末尾可能有填充字节
    pub fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.length as usize) }
    }
}

unsafe impl Send for VideoFrame {}

impl Drop for VideoFrame {
    fn drop(&mut self) {
        unsafe {
            let _ = self.buffer.Unlock();
        }
    }
}

// --- 核心状态管理 ---

struct StreamState {
    queue: VecDeque<VideoFrame>,
    waker: Option<Waker>,
    closed: bool,
    // 限制队列长度防止 OOM (Backpressure)
    max_capacity: usize,
}

#[implement(IMFSourceReaderCallback)]
struct ReaderCallback {
    state: Arc<Mutex<StreamState>>,
    // 使用原子变量快速检查状态，减少锁请求
    is_active: Arc<AtomicBool>,
}

impl IMFSourceReaderCallback_Impl for ReaderCallback {
    fn OnReadSample(
        &self,
        hr_status: HRESULT,
        _stream_index: u32,
        _stream_flags: u32,
        timestamp: i64,
        sample: Option<&IMFSample>,
    ) -> Result<()> {
        if hr_status.is_err() || !self.is_active.load(Ordering::Relaxed) {
            return Ok(());
        }

        if let Some(s) = sample {
            unsafe {
                // 1. 提取缓冲区
                let buffer = s.ConvertToContiguousBuffer()?;
                let mut ptr = std::ptr::null_mut();
                let mut length = 0;
                buffer.Lock(&mut ptr, None, Some(&mut length))?;

                // 2. 构造 Frame (此处可根据需要解析具体分辨率/Stride)
                let frame = VideoFrame {
                    width: 0, // 建议从媒体类型中预先获取
                    height: 0,
                    stride: 0,
                    timestamp,
                    data: s.clone(),
                    buffer,
                    ptr,
                    length,
                };

                // 3. 入队并唤醒异步任务
                let mut state = self.state.lock().unwrap();
                if state.queue.len() < state.max_capacity {
                    state.queue.push_back(frame);
                    if let Some(waker) = state.waker.take() {
                        waker.wake();
                    }
                }
                // 如果队列满了，我们选择丢弃老帧或停止请求（取决于业务需求）
            }
        }
        Ok(())
    }

    fn OnFlush(&self, _: u32) -> Result<()> {
        Ok(())
    }
    fn OnEvent(&self, _: u32, _: Option<&IMFMediaEvent>) -> Result<()> {
        Ok(())
    }
}

// --- 异步流对象 ---

pub struct VisionStream {
    reader: IMFSourceReader,
    state: Arc<Mutex<StreamState>>,
    is_active: Arc<AtomicBool>,
}

impl VisionStream {
    pub fn new(index: u32, max_buffer: usize) -> Result<Self> {
        ensure_mf_initialized();

        unsafe {
            // 设备枚举
            let mut attributes: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attributes, 1)?;
            let attributes = attributes.unwrap();
            attributes.SetGUID(
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
            )?;

            let mut devices_ptr = std::ptr::null_mut();
            let mut count = 0;
            MFEnumDeviceSources(&attributes, &mut devices_ptr, &mut count)?;
            let devices = std::slice::from_raw_parts(devices_ptr, count as usize);
            let activate = devices
                .get(index as usize)
                .ok_or(Error::from(E_FAIL))?
                .as_ref()
                .unwrap();

            // 状态初始化
            let state = Arc::new(Mutex::new(StreamState {
                queue: VecDeque::with_capacity(max_buffer),
                waker: None,
                closed: false,
                max_capacity: max_buffer,
            }));
            let is_active = Arc::new(AtomicBool::new(true));

            // 回调配置
            let callback_impl = ReaderCallback {
                state: state.clone(),
                is_active: is_active.clone(),
            };
            let callback: IMFSourceReaderCallback = callback_impl.into();

            let mut reader_attrs: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut reader_attrs, 2)?;
            let reader_attrs = reader_attrs.unwrap();
            reader_attrs.SetUnknown(&MF_SOURCE_READER_ASYNC_CALLBACK, &callback)?;
            // 允许硬件转换，提高性能
            reader_attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;

            let source: IMFMediaSource = activate.ActivateObject()?;
            let mut reader: Option<IMFSourceReader> = None;
            MFCreateSourceReaderFromMediaSource(&source, Some(&reader_attrs), &mut reader)?;
            let reader = reader.unwrap();

            // 强制设置输出格式为 RGB32 或 NV12 以保证兼容性
            let mut media_type: Option<IMFMediaType> = None;
            MFCreateMediaType(&mut media_type)?;
            let media_type = media_type.unwrap();
            media_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            media_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
            reader.SetCurrentMediaType(
                MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                None,
                &media_type,
            )?;

            // 启动首个采样
            reader.ReadSample(
                MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                0,
                None,
                None,
                None,
                None,
            )?;

            Ok(Self {
                reader,
                state,
                is_active,
            })
        }
    }
}

impl Stream for VisionStream {
    type Item = VideoFrame;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut state = self.state.lock().unwrap();

        if let Some(frame) = state.queue.pop_front() {
            // 立即发出下一个请求以维持流水线
            unsafe {
                let _ = self.reader.ReadSample(
                    MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                    0,
                    None,
                    None,
                    None,
                    None,
                );
            }
            Poll::Ready(Some(frame))
        } else {
            if state.closed {
                Poll::Ready(None)
            } else {
                state.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}

impl Drop for VisionStream {
    fn drop(&mut self) {
        self.is_active.store(false, Ordering::SeqCst);
        // 实际清理逻辑通常由 MF 的异步机制处理
    }
}
