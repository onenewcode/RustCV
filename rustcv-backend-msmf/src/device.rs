use std::sync::Arc;
use windows::core::{Error as HResultError, GUID, HSTRING};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::*;

use rustcv_core::builder::CameraConfig;
use rustcv_core::error::{CameraError, Result};
use rustcv_core::pixel_format::PixelFormat;
use rustcv_core::traits::{DeviceControls, DeviceInfo, Stream};

use crate::controls::create_controls;
use crate::pixel_map;
use crate::stream::MsmfStream;

/// Converts a Windows HRESULT error into a `CameraError`.
/// This simplifies error handling when interacting with the Windows API.
fn hresult_to_camera_error(e: HResultError) -> CameraError {
    CameraError::Io(std::io::Error::new(
        std::io::ErrorKind::Other,
        e.to_string(),
    ))
}

/// Lists all available video capture devices.
pub fn list_devices() -> Result<Vec<DeviceInfo>> {
    // This function interacts with COM objects, which requires `unsafe` blocks.
    // The resources are carefully managed to prevent leaks.
    unsafe {
        // Create an attribute store to specify the type of devices we want to enumerate.
        let attributes = create_video_capture_attributes()?;

        let mut pp_devices: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count: u32 = 0;
        // Enumerate devices of the specified type.
        MFEnumDeviceSources(&attributes, &mut pp_devices, &mut count)
            .map_err(hresult_to_camera_error)?;

        if count == 0 || pp_devices.is_null() {
            return Ok(Vec::new());
        }

        // Convert the raw C-style array of COM pointers into a Rust slice.
        let devices_slice = std::slice::from_raw_parts_mut(pp_devices, count as usize);

        // Iterate over the devices, extract their information, and collect them into a Vec.
        let devices = devices_slice
            .iter_mut()
            // `take()` transfers ownership of the `IMFActivate` object, preventing double-freeing.
            .filter_map(|opt| opt.take())
            .filter_map(|device| {
                let name = get_attribute_string(&device, MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME);
                if name.is_empty() {
                    None
                } else {
                    let id = get_attribute_string(
                        &device,
                        MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
                    );
                    Some(DeviceInfo {
                        name,
                        id,
                        backend: "MSMF".to_string(),
                        bus_info: None,
                    })
                }
            })
            .collect();

        // Free the memory allocated by `MFEnumDeviceSources`.
        CoTaskMemFree(Some(pp_devices as *const std::ffi::c_void));

        Ok(devices)
    }
}

/// Creates an `IMFAttributes` object configured to search for video capture devices.
unsafe fn create_video_capture_attributes() -> Result<IMFAttributes> {
    let mut attributes = None;
    MFCreateAttributes(&mut attributes, 1).map_err(hresult_to_camera_error)?;
    let attributes = attributes
        .ok_or_else(|| CameraError::Io(std::io::Error::other("Failed to create attributes")))?;

    // Set the attribute to filter for video capture devices.
    attributes
        .SetGUID(
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
        )
        .map_err(hresult_to_camera_error)?;
    Ok(attributes)
}

/// Retrieves a string attribute from a device's `IMFActivate` interface.
unsafe fn get_attribute_string(attr: &IMFActivate, guid: GUID) -> String {
    let mut value = windows::core::PWSTR::null();
    let mut length = 0;

    // The `GetAllocatedString` function allocates memory that we must free later.
    if attr
        .GetAllocatedString(&guid, &mut value, &mut length)
        .is_ok()
        && !value.is_null()
    {
        let slice = std::slice::from_raw_parts(value.as_ptr(), length as usize);
        let result = String::from_utf16_lossy(slice);
        // Free the memory allocated by the Windows API.
        CoTaskMemFree(Some(value.as_ptr() as *const std::ffi::c_void));
        result
    } else {
        String::new()
    }
}

/// Opens a camera device by its ID and applies the given configuration.
pub fn open(id: &str, config: CameraConfig) -> Result<(Box<dyn Stream>, DeviceControls)> {
    // 1. Create a source reader for the specified device.
    let source_reader = unsafe { create_source_reader(id)? };
    // 2. Negotiate the best format based on the user's configuration.
    let negotiated_fmt = negotiate_format(&source_reader, &config)?;
    // 3. Set the chosen format as the output type for the source reader.
    unsafe { set_output_media_type(&source_reader, &negotiated_fmt)? };

    tracing::info!(
        "Camera opened: {}x{} @ {:?}",
        negotiated_fmt.width,
        negotiated_fmt.height,
        negotiated_fmt.format
    );

    // 4. Create the stream and controls, wrapping the source reader in an Arc for shared ownership.
    let source_reader_arc = Arc::new(source_reader);
    let stream = MsmfStream::new(
        source_reader_arc.clone(),
        &negotiated_fmt,
        config.buffer_count,
    )?;
    let controls = create_controls(source_reader_arc);

    Ok((Box::new(stream), controls))
}

/// Creates and configures an `IMFSourceReader` for the camera device with the given ID.
unsafe fn create_source_reader(id: &str) -> Result<IMFSourceReader> {
    let mut attributes = None;
    MFCreateAttributes(&mut attributes, 2).map_err(hresult_to_camera_error)?;
    let attributes = attributes
        .ok_or_else(|| CameraError::Io(std::io::Error::other("Failed to create attributes")))?;

    // Specify that we want a video capture device.
    attributes
        .SetGUID(
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
        )
        .map_err(hresult_to_camera_error)?;

    // Specify the device's symbolic link (its unique ID).
    let id_hstring = HSTRING::from(id);
    attributes
        .SetString(
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
            &id_hstring,
        )
        .map_err(hresult_to_camera_error)?;

    // Create the media source and then a source reader from it.
    let source = MFCreateDeviceSource(&attributes).map_err(hresult_to_camera_error)?;
    let source_reader =
        MFCreateSourceReaderFromMediaSource(&source, None).map_err(hresult_to_camera_error)?;

    // Deselect all streams, then select only the first video stream.
    source_reader
        .SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)
        .map_err(hresult_to_camera_error)?;
    source_reader
        .SetStreamSelection(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, true)
        .map_err(hresult_to_camera_error)?;

    Ok(source_reader)
}

/// Sets the output media type on the source reader based on the negotiated format.
unsafe fn set_output_media_type(
    source_reader: &IMFSourceReader,
    negotiated_fmt: &NegotiatedFormat,
) -> Result<()> {
    // Create a new media type object.
    let media_type = MFCreateMediaType().map_err(hresult_to_camera_error)?;

    // Set the major type to Video.
    media_type
        .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
        .map_err(hresult_to_camera_error)?;

    // Set the subtype to the negotiated pixel format.
    let mf_guid =
        pixel_map::to_mf_guid(negotiated_fmt.format).ok_or(CameraError::FormatNotSupported)?;
    media_type
        .SetGUID(&MF_MT_SUBTYPE, &mf_guid)
        .map_err(hresult_to_camera_error)?;

    // Set other necessary attributes like interlace mode and frame size.
    media_type
        .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
        .map_err(hresult_to_camera_error)?;
    media_type
        .SetUINT64(
            &MF_MT_FRAME_SIZE,
            ((negotiated_fmt.height as u64) << 32) | (negotiated_fmt.width as u64),
        )
        .map_err(hresult_to_camera_error)?;

    // Apply the configured media type to the source reader.
    source_reader
        .SetCurrentMediaType(
            MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
            None,
            &media_type,
        )
        .map_err(hresult_to_camera_error)?;

    // Flush the source reader to apply the changes.
    source_reader
        .Flush(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)
        .map_err(hresult_to_camera_error)?;

    Ok(())
}

/// Represents a video format that has been successfully negotiated with the device.
pub struct NegotiatedFormat {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub fps: u32,
}

/// Iterates through the device's available formats and selects the best one based on the user's config.
fn negotiate_format(
    source_reader: &IMFSourceReader,
    config: &CameraConfig,
) -> Result<NegotiatedFormat> {
    (0..)
        .map(|index| unsafe {
            source_reader.GetNativeMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, index)
        })
        .take_while(|result| result.is_ok())
        .filter_map(|result| result.ok())
        .filter_map(|media_type| unsafe { parse_media_type(&media_type, config) })
        .max_by_key(|(score, _)| *score)
        .map(|(_, fmt)| fmt)
        .ok_or(CameraError::FormatNotSupported)
}

/// Parses an `IMFMediaType` to extract format information and calculate a preference score.
unsafe fn parse_media_type(
    media_type: &IMFMediaType,
    config: &CameraConfig,
) -> Option<(i32, NegotiatedFormat)> {
    if media_type.GetGUID(&MF_MT_MAJOR_TYPE).ok()? != MFMediaType_Video {
        return None;
    }

    let subtype = media_type.GetGUID(&MF_MT_SUBTYPE).ok()?;
    let core_fmt = pixel_map::from_mf_guid(&subtype);

    let frame_size = media_type.GetUINT64(&MF_MT_FRAME_SIZE).ok()?;
    let width = (frame_size & 0xFFFFFFFF) as u32;
    let height = ((frame_size >> 32) & 0xFFFFFFFF) as u32;

    let score = calculate_score(config, width, height, core_fmt);

    Some((
        score,
        NegotiatedFormat {
            width,
            height,
            format: core_fmt,
            fps: 30, // FPS is hardcoded as in original, but can be extracted from media_type
        },
    ))
}

/// Calculates a score for a given format based on the user's preferences.
/// Higher scores are better.
fn calculate_score(config: &CameraConfig, w: u32, h: u32, fmt: PixelFormat) -> i32 {
    // Score based on matching the requested resolution.
    let resolution_score = config
        .resolution_req
        .iter()
        .find(|(req_w, req_h, _)| w == *req_w && h == *req_h)
        .map_or(0, |(_, _, prio)| *prio as i32 * 10);

    // Score based on matching the requested pixel format.
    let format_score = config
        .format_req
        .iter()
        .find(|(req_fmt, _)| fmt == *req_fmt)
        .map_or(0, |(_, prio)| *prio as i32 * 10);

    // Combine scores and use width as a tie-breaker, preferring higher resolutions.
    resolution_score + format_score + (w / 100) as i32
}
