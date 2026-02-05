#![allow(unexpected_cfgs)]

#[cfg(all(windows, not(feature = "docs-only")))]
pub mod controls;
pub mod device;
pub mod pixel_map;
pub mod stream;

use rustcv_core::error::Result;
use rustcv_core::traits::{DeviceControls, DeviceInfo, Driver, Stream};
use std::sync::Arc;

/// Represents the MSMF driver, which acts as the entry point for camera operations.
///
/// This struct implements the `Driver` trait, providing methods to list available
/// devices and open a specific camera by its ID.
#[derive(Debug, Clone)]
pub struct MsmfDriver;

impl Default for MsmfDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl MsmfDriver {
    /// Creates a new `MsmfDriver` instance.
    ///
    /// This is the main entry point for using the MSMF backend.
    pub fn new() -> Self {
        Self
    }
}

impl Driver for MsmfDriver {
    /// Lists all available video capture devices recognized by MSMF.
    ///
    /// # Returns
    ///
    /// A `Result` containing a `Vec` of `DeviceInfo` structs, each representing a
    /// camera, or a `CameraError` if the device enumeration fails.
    fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        device::list_devices()
    }

    /// Opens a camera device with the specified ID and configuration.
    ///
    /// # Arguments
    ///
    /// * `id` - The unique identifier of the camera to open.
    /// * `config` - The desired camera configuration (resolution, frame rate, etc.).
    ///
    /// # Returns
    ///
    /// A `Result` containing a tuple with a `Stream` and `DeviceControls` handle,
    /// or a `CameraError` if the device fails to open.
    fn open(
        &self,
        id: &str,
        config: rustcv_core::builder::CameraConfig,
    ) -> Result<(Box<dyn Stream>, DeviceControls)> {
        device::open(id, config)
    }
}

/// Returns a default MSMF driver instance wrapped in an `Arc`.
///
/// This is a convenience function to easily create a shareable driver object.
pub fn default_driver() -> Arc<dyn Driver> {
    Arc::new(MsmfDriver::new())
}
