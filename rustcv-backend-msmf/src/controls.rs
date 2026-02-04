use std::sync::Arc;
use windows::Win32::Media::MediaFoundation::*;

use rustcv_core::error::{CameraError, Result};
use rustcv_core::traits::{
    DeviceControls, LensControl, SensorControl, SystemControl, TriggerConfig, TriggerMode,
};

pub fn create_controls(source_reader: Arc<IMFSourceReader>) -> DeviceControls {
    DeviceControls {
        sensor: Box::new(MsmfSensor {
            source_reader: source_reader.clone(),
        }),
        lens: Box::new(MsmfLens {
            source_reader: source_reader.clone(),
        }),
        system: Box::new(MsmfSystem { source_reader }),
    }
}

struct MsmfSensor {
    source_reader: Arc<IMFSourceReader>,
}

unsafe impl Send for MsmfSensor {}
unsafe impl Sync for MsmfSensor {}

impl SensorControl for MsmfSensor {
    fn set_exposure(&self, value_us: u32) -> Result<()> {
        unsafe {
            if let Ok(media_type) = self.source_reader.GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32) {
                let _ = media_type.SetUINT64(&MF_MT_VIDEO_LIGHTING, value_us as u64);
            }
        }
        Ok(())
    }

    fn get_exposure(&self) -> Result<u32> {
        unsafe {
            if let Ok(media_type) = self.source_reader.GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32) {
                if let Ok(value) = media_type.GetUINT64(&MF_MT_VIDEO_LIGHTING) {
                    return Ok(value as u32);
                }
            }
        }
        Ok(0)
    }
}

struct MsmfLens {
    source_reader: Arc<IMFSourceReader>,
}

unsafe impl Send for MsmfLens {}
unsafe impl Sync for MsmfLens {}

impl LensControl for MsmfLens {
    fn set_zoom(&self, zoom: u32) -> Result<()> {
        unsafe {
            if let Ok(media_type) = self.source_reader.GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32) {
                let _ = media_type.SetUINT64(&MF_MT_VIDEO_LIGHTING, zoom as u64);
            }
        }
        Ok(())
    }

    fn set_focus(&self, focus: u32) -> Result<()> {
        unsafe {
            if let Ok(media_type) = self.source_reader.GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32) {
                let _ = media_type.SetUINT64(&MF_MT_VIDEO_LIGHTING, focus as u64);
            }
        }
        Ok(())
    }
}

struct MsmfSystem {
    source_reader: Arc<IMFSourceReader>,
}

unsafe impl Send for MsmfSystem {}
unsafe impl Sync for MsmfSystem {}

impl SystemControl for MsmfSystem {
    unsafe fn force_reset(&self) -> Result<()> {
        Ok(())
    }

    fn set_trigger(&self, config: TriggerConfig) -> Result<()> {
        if config.mode == TriggerMode::Off {
            return Ok(());
        }
        Err(CameraError::FormatNotSupported)
    }

    fn export_state(&self) -> Result<serde_json::Value> {
        use serde_json::json;

        let mut exposure = None;

        unsafe {
            if let Ok(media_type) = self.source_reader.GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32) {
                if let Ok(value) = media_type.GetUINT64(&MF_MT_VIDEO_LIGHTING) {
                    exposure = Some(value as u32);
                }
            }
        }

        Ok(json!({
            "backend": "msmf",
            "exposure": exposure,
        }))
    }
}