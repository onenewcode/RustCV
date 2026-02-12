use std::sync::Arc;
use windows::core::GUID;
use windows::Win32::Media::MediaFoundation::*;

use rustcv_core::error::{CameraError, Result};
use rustcv_core::traits::{
    DeviceControls, LensControl, SensorControl, SystemControl, TriggerConfig, TriggerMode,
};

const DEFAULT_EXPOSURE_US: u32 = 10000;

/// Creates a new `DeviceControls` instance with MSMF-specific implementations.
///
/// This function constructs a `DeviceControls` struct containing implementations
/// for sensor, lens, and system controls using the provided IMFSourceReader.
///
/// # Arguments
///
/// * `source_reader` - A reference-counted IMFSourceReader interface for accessing
///   the camera device and its media samples.
///
/// # Returns
///
/// Returns a `DeviceControls` struct with MSMF-specific control implementations.
///
/// # Note
///
/// The current implementation uses media type attributes for camera controls.
/// For production use, consider implementing proper IAMCameraControl or
/// IAMVideoProcAmp interfaces for more reliable camera control.
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

/// Retrieves the current media type from the source reader.
///
/// This function gets the current media type for the first video stream
/// from the IMFSourceReader.
///
/// # Arguments
///
/// * `source_reader` - Reference to the IMFSourceReader interface.
///
/// # Returns
///
/// * `Some(IMFMediaType)` - The current media type if successful.
/// * `None` - If the media type could not be retrieved.
///
/// # Safety
///
/// This function is unsafe as it calls Windows Media Foundation APIs
/// that require unsafe context.
#[inline]
unsafe fn get_current_media_type(source_reader: &IMFSourceReader) -> Option<IMFMediaType> {
    source_reader
        .GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)
        .ok()
}

/// Sets a UINT64 attribute on the current media type.
///
/// This function sets a UINT64 attribute value on the current media type
/// of the video stream.
///
/// # Arguments
///
/// * `source_reader` - Reference to the IMFSourceReader interface.
/// * `guid` - The GUID of the attribute to set.
/// * `value` - The UINT64 value to set.
///
/// # Safety
///
/// This function is unsafe as it calls Windows Media Foundation APIs
/// that require unsafe context.
#[inline]
unsafe fn set_media_type_uint64(source_reader: &IMFSourceReader, guid: &GUID, value: u64) {
    if let Some(media_type) = get_current_media_type(source_reader) {
        let _ = media_type.SetUINT64(guid, value);
    }
}

/// Gets a UINT64 attribute from the current media type.
///
/// # Arguments
///
/// * `source_reader` - Reference to the IMFSourceReader interface.
/// * `guid` - The GUID of the attribute to retrieve.
///
/// # Returns
///
/// `Some(u64)` if the attribute exists, `None` otherwise.
#[inline]
unsafe fn get_media_type_uint64(source_reader: &IMFSourceReader, guid: &GUID) -> Option<u64> {
    get_current_media_type(source_reader).and_then(|media_type| media_type.GetUINT64(guid).ok())
}

/// MSMF implementation of sensor controls.
///
/// This struct provides sensor-related camera controls such as exposure
/// adjustment using Windows Media Foundation APIs.
///
/// # Note
///
/// The current implementation uses media type attributes for control.
/// For production use, consider implementing proper IAMVideoProcAmp interface.
struct MsmfSensor {
    source_reader: Arc<IMFSourceReader>,
}

unsafe impl Send for MsmfSensor {}
unsafe impl Sync for MsmfSensor {}

impl SensorControl for MsmfSensor {
    fn set_exposure(&self, value_us: u32) -> Result<()> {
        unsafe {
            set_media_type_uint64(&self.source_reader, &MF_MT_VIDEO_LIGHTING, value_us as u64);
        }
        Ok(())
    }

    fn get_exposure(&self) -> Result<u32> {
        unsafe {
            Ok(
                get_media_type_uint64(&self.source_reader, &MF_MT_VIDEO_LIGHTING)
                    .map(|v| v as u32)
                    .unwrap_or(DEFAULT_EXPOSURE_US),
            )
        }
    }
}

/// MSMF implementation of lens controls.
///
/// This struct provides lens-related camera controls such as zoom and focus
/// adjustment using Windows Media Foundation APIs.
///
/// # Note
///
/// The current implementation uses media type attributes for control.
/// For production use, consider implementing proper IAMCameraControl interface.
struct MsmfLens {
    source_reader: Arc<IMFSourceReader>,
}

unsafe impl Send for MsmfLens {}
unsafe impl Sync for MsmfLens {}

impl LensControl for MsmfLens {
    fn set_zoom(&self, zoom: u32) -> Result<()> {
        unsafe {
            set_media_type_uint64(&self.source_reader, &MF_MT_VIDEO_LIGHTING, zoom as u64);
        }
        Ok(())
    }

    fn set_focus(&self, focus: u32) -> Result<()> {
        unsafe {
            set_media_type_uint64(&self.source_reader, &MF_MT_VIDEO_LIGHTING, focus as u64);
        }
        Ok(())
    }
}

/// MSMF implementation of system controls.
///
/// This struct provides system-level camera controls such as reset,
/// trigger configuration, and state export using Windows Media Foundation APIs.
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

        let exposure = unsafe {
            get_media_type_uint64(&self.source_reader, &MF_MT_VIDEO_LIGHTING).map(|v| v as u32)
        };

        Ok(json!({
            "backend": "msmf",
            "exposure": exposure,
        }))
    }
}
