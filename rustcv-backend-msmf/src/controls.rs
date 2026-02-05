//! This module implements the device control traits for the MSMF backend.
//!
//! It provides mechanisms to control camera sensor, lens, and system settings.
//! Note that many of these controls are placeholders and may not be fully
//! implemented by all MSMF devices.

use std::sync::Arc;
use windows::Win32::Media::MediaFoundation::*;

use rustcv_core::error::{CameraError, Result};
use rustcv_core::traits::{
    DeviceControls, LensControl, SensorControl, SystemControl, TriggerConfig, TriggerMode,
};

/// Creates a `DeviceControls` structure for the MSMF backend.
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

/// A struct for controlling sensor-related properties of an MSMF camera.
struct MsmfSensor {
    source_reader: Arc<IMFSourceReader>,
}

unsafe impl Send for MsmfSensor {}
unsafe impl Sync for MsmfSensor {}

impl SensorControl for MsmfSensor {
    /// Sets the exposure time of the camera sensor.
    ///
    /// Note: `MF_MT_VIDEO_LIGHTING` is used here as a placeholder for exposure
    /// control, which might not be the correct attribute for all devices.
    fn set_exposure(&self, value_us: u32) -> Result<()> {
        unsafe {
            if let Ok(media_type) = self
                .source_reader
                .GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)
            {
                let _ = media_type.SetUINT64(&MF_MT_VIDEO_LIGHTING, value_us as u64);
            }
        }
        Ok(())
    }

    /// Gets the current exposure time of the camera sensor.
    fn get_exposure(&self) -> Result<u32> {
        unsafe {
            if let Ok(media_type) = self
                .source_reader
                .GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)
            {
                if let Ok(value) = media_type.GetUINT64(&MF_MT_VIDEO_LIGHTING) {
                    return Ok(value as u32);
                }
            }
        }
        Ok(0)
    }
}

/// A struct for controlling lens-related properties of an MSMF camera.
struct MsmfLens {
    source_reader: Arc<IMFSourceReader>,
}

unsafe impl Send for MsmfLens {}
unsafe impl Sync for MsmfLens {}

impl LensControl for MsmfLens {
    /// Sets the zoom level of the camera lens.
    ///
    /// Note: This is a placeholder and uses a lighting attribute, which is likely incorrect.
    fn set_zoom(&self, zoom: u32) -> Result<()> {
        unsafe {
            if let Ok(media_type) = self
                .source_reader
                .GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)
            {
                let _ = media_type.SetUINT64(&MF_MT_VIDEO_LIGHTING, zoom as u64);
            }
        }
        Ok(())
    }

    /// Sets the focus of the camera lens.
    ///
    /// Note: This is a placeholder and uses a lighting attribute, which is likely incorrect.
    fn set_focus(&self, focus: u32) -> Result<()> {
        unsafe {
            if let Ok(media_type) = self
                .source_reader
                .GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)
            {
                let _ = media_type.SetUINT64(&MF_MT_VIDEO_LIGHTING, focus as u64);
            }
        }
        Ok(())
    }
}

/// A struct for controlling system-level properties of an MSMF camera.
struct MsmfSystem {
    source_reader: Arc<IMFSourceReader>,
}

unsafe impl Send for MsmfSystem {}
unsafe impl Sync for MsmfSystem {}

impl SystemControl for MsmfSystem {
    /// Resets the camera device. (Not implemented)
    unsafe fn force_reset(&self) -> Result<()> {
        Ok(())
    }

    /// Configures the trigger mode of the camera. (Not supported)
    fn set_trigger(&self, config: TriggerConfig) -> Result<()> {
        if config.mode == TriggerMode::Off {
            return Ok(());
        }
        Err(CameraError::FormatNotSupported)
    }

    /// Exports the current state of the camera.
    fn export_state(&self) -> Result<serde_json::Value> {
        use serde_json::json;

        let mut exposure = None;

        unsafe {
            if let Ok(media_type) = self
                .source_reader
                .GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)
            {
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
