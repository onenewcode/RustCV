#![cfg(target_os = "windows")]

pub mod controls;
pub mod device;
pub mod pixel_map;
pub mod stream;

use rustcv_core::error::Result;
use rustcv_core::traits::{DeviceControls, DeviceInfo, Driver, Stream};
use std::sync::Arc;

/// MSMF driver implementation for Windows camera devices.
///
/// This struct implements the `Driver` trait to provide camera device
/// enumeration and opening functionality using Windows Media Foundation.
#[derive(Debug, Clone)]
pub struct MsmfDriver;

impl Default for MsmfDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl MsmfDriver {
    pub fn new() -> Self {
        Self
    }
}

impl Driver for MsmfDriver {
    fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        device::list_devices()
    }

    fn open(
        &self,
        id: &str,
        config: rustcv_core::builder::CameraConfig,
    ) -> Result<(Box<dyn Stream>, DeviceControls)> {
        device::open(id, config)
    }
}

/// Creates a default MSMF driver instance.
///
/// This is a convenience function that creates a new `MsmfDriver` and wraps
/// it in an `Arc<dyn Driver>` for easy use with the RustCV trait system.
///
/// # Returns
///
/// Returns an `Arc<dyn Driver>` containing a new `MsmfDriver` instance.
///
/// # Example
///
/// ```rust,no_run
/// use rustcv_backend_msmf::default_driver;
///
/// let driver = default_driver();
/// let devices = driver.list_devices().unwrap();
/// ```
pub fn default_driver() -> Arc<dyn Driver> {
    Arc::new(MsmfDriver::new())
}
