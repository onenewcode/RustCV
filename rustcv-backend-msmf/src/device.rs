use std::sync::Arc;
use windows::core::{GUID, HSTRING};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::*;

use rustcv_core::builder::CameraConfig;
use rustcv_core::error::{CameraError, Result};
use rustcv_core::pixel_format::PixelFormat;
use rustcv_core::traits::{DeviceControls, DeviceInfo, Stream};

use crate::controls::create_controls;
use crate::pixel_map;
use crate::stream::MsmfStream;

pub fn list_devices() -> Result<Vec<DeviceInfo>> {
    let mut devices = Vec::new();

    unsafe {
        let mut attributes = None;
        MFCreateAttributes(&mut attributes, 1).map_err(|e| CameraError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;
        
        let attributes = attributes.ok_or_else(|| CameraError::Io(std::io::Error::other("Failed to create attributes")))?;
        
        attributes.SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID).map_err(|e| CameraError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;

        let mut pp_devices: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count: u32 = 0;
        MFEnumDeviceSources(&attributes, &mut pp_devices, &mut count).map_err(|e| CameraError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;
        
        if count > 0 && !pp_devices.is_null() {
            let devices_slice = std::slice::from_raw_parts_mut(pp_devices, count as usize);
            
            for i in 0..count as usize {
                if let Some(attr) = devices_slice[i].as_mut().as_ref() {
                    let name = get_attribute_string(attr, MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME);
                    let device_id = get_attribute_string(attr, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK);
                    
                    if !name.is_empty() {
                        devices.push(DeviceInfo {
                            name,
                            id: device_id,
                            backend: "MSMF".to_string(),
                            bus_info: None,
                        });
                    }
                }
            }
            
            CoTaskMemFree(Some(pp_devices as *const std::ffi::c_void));
        }
    }

    Ok(devices)
}

unsafe fn get_attribute_string(attr: &IMFActivate, guid: GUID) -> String {
    let mut pwsz_value: windows::core::PWSTR = windows::core::PWSTR::null();
    let mut pcch_length: u32 = 0;
    
    if attr.GetAllocatedString(&guid, &mut pwsz_value, &mut pcch_length).is_ok() && !pwsz_value.is_null() {
        let slice = std::slice::from_raw_parts(pwsz_value.as_ptr(), pcch_length as usize);
        let result = String::from_utf16_lossy(slice);
        CoTaskMemFree(Some(pwsz_value.as_ptr() as *const std::ffi::c_void));
        result
    } else {
        String::new()
    }
}

pub fn open(id: &str, config: CameraConfig) -> Result<(Box<dyn Stream>, DeviceControls)> {
    unsafe {
        let mut attributes = None;
        MFCreateAttributes(&mut attributes, 2).map_err(|e| CameraError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;
        
        let attributes = attributes.ok_or_else(|| CameraError::Io(std::io::Error::other("Failed to create attributes")))?;
        
        attributes.SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID).map_err(|e| CameraError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;
        
        let id_hstring = HSTRING::from(id);
        attributes.SetString(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK, &id_hstring).map_err(|e| CameraError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;

        let source = MFCreateDeviceSource(&attributes).map_err(|e| CameraError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;
        let source_reader = MFCreateSourceReaderFromMediaSource(&source, None).map_err(|e| CameraError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;
        
        source_reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false).map_err(|e| CameraError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;
        source_reader.SetStreamSelection(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, true).map_err(|e| CameraError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;

        let negotiated_fmt = negotiate_format(&source_reader, &config)?;

        let media_type = MFCreateMediaType().map_err(|e| CameraError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;
        
        media_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(|e| CameraError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;
        
        let mf_guid = pixel_map::to_mf_guid(negotiated_fmt.format).ok_or(CameraError::FormatNotSupported)?;
        media_type.SetGUID(&MF_MT_SUBTYPE, &mf_guid).map_err(|e| CameraError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;
        
        media_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).map_err(|e| CameraError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;
        media_type.SetUINT64(&MF_MT_FRAME_SIZE, ((negotiated_fmt.height as u64) << 32) | (negotiated_fmt.width as u64)).map_err(|e| CameraError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;

        source_reader.SetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, None, &media_type).map_err(|e| CameraError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;
        
        source_reader.Flush(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32).map_err(|e| CameraError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;

        tracing::info!(
            "Camera opened: {}x{} @ {:?}",
            negotiated_fmt.width,
            negotiated_fmt.height,
            negotiated_fmt.format
        );

        let source_reader_arc = Arc::new(source_reader);

        let stream = MsmfStream::new(source_reader_arc.clone(), &negotiated_fmt, config.buffer_count)?;
        let controls = create_controls(source_reader_arc);

        Ok((Box::new(stream), controls))
    }
}

pub struct NegotiatedFormat {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub fps: u32,
}

fn negotiate_format(source_reader: &IMFSourceReader, config: &CameraConfig) -> Result<NegotiatedFormat> {
    let mut best_score = -1;
    let mut best_fmt = None;

    unsafe {
        let mut index = 0u32;

        loop {
            match source_reader.GetNativeMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, index) {
                Ok(mt) => {
                    if let Ok(major_type) = mt.GetGUID(&MF_MT_MAJOR_TYPE) {
                        if major_type == MFMediaType_Video {
                            if let Ok(subtype) = mt.GetGUID(&MF_MT_SUBTYPE) {
                                let core_fmt = pixel_map::from_mf_guid(&subtype);
                                
                                let mut width = 0u32;
                                let mut height = 0u32;
                                
                                if let Ok(frame_size) = mt.GetUINT64(&MF_MT_FRAME_SIZE) {
                                    width = (frame_size & 0xFFFFFFFF) as u32;
                                    height = ((frame_size >> 32) & 0xFFFFFFFF) as u32;
                                }

                                let current_score = calculate_score(config, width, height, core_fmt);

                                if current_score > best_score {
                                    best_score = current_score;
                                    best_fmt = Some(NegotiatedFormat {
                                        width,
                                        height,
                                        format: core_fmt,
                                        fps: 30,
                                    });
                                }
                            }
                        }
                    }
                    index += 1;
                }
                Err(_) => break,
            }
        }
    }

    best_fmt.ok_or(CameraError::FormatNotSupported)
}

fn calculate_score(config: &CameraConfig, w: u32, h: u32, fmt: PixelFormat) -> i32 {
    let mut score = 0;

    for (req_w, req_h, prio) in &config.resolution_req {
        if w == *req_w && h == *req_h {
            score += *prio as i32 * 10;
        }
    }

    for (req_fmt, prio) in &config.format_req {
        if fmt == *req_fmt {
            score += *prio as i32 * 10;
        }
    }

    score += (w / 100) as i32;

    score
}