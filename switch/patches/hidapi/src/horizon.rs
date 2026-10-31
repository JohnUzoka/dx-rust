//! Nintendo Switch (Horizon OS) backend, added for bevy-horizon ports: applications get no HID access there, so
//! the crate builds and every call reports that. `HidApi::new` fails (it lists devices), and nothing can be opened.

use std::ffi::CStr;

use super::{DeviceInfo, HidDeviceBackendBase, HidError, HidResult};

fn unavailable() -> HidError {
    HidError::HidApiError {
        message: "HID devices aren't available to applications on Horizon (Nintendo Switch)".to_string(),
    }
}

pub struct HidApiBackend;

impl HidApiBackend {
    pub fn get_hid_device_info_vector(_vid: u16, _pid: u16) -> HidResult<Vec<DeviceInfo>> {
        Err(unavailable())
    }

    pub fn open(_vid: u16, _pid: u16) -> HidResult<HidDevice> {
        Err(unavailable())
    }

    pub fn open_serial(_vid: u16, _pid: u16, _sn: &str) -> HidResult<HidDevice> {
        Err(unavailable())
    }

    pub fn open_path(_device_path: &CStr) -> HidResult<HidDevice> {
        Err(unavailable())
    }
}

/// No device is ever opened, so no value of this type exists.
pub enum HidDevice {}

impl HidDeviceBackendBase for HidDevice {
    fn write(&self, _data: &[u8]) -> HidResult<usize> {
        match *self {}
    }

    fn read(&self, _buf: &mut [u8]) -> HidResult<usize> {
        match *self {}
    }

    fn read_timeout(&self, _buf: &mut [u8], _timeout: i32) -> HidResult<usize> {
        match *self {}
    }

    fn send_feature_report(&self, _data: &[u8]) -> HidResult<()> {
        match *self {}
    }

    fn get_feature_report(&self, _buf: &mut [u8]) -> HidResult<usize> {
        match *self {}
    }

    fn send_output_report(&self, _data: &[u8]) -> HidResult<()> {
        match *self {}
    }

    fn set_blocking_mode(&self, _blocking: bool) -> HidResult<()> {
        match *self {}
    }

    fn get_device_info(&self) -> HidResult<DeviceInfo> {
        match *self {}
    }

    fn get_manufacturer_string(&self) -> HidResult<Option<String>> {
        match *self {}
    }

    fn get_product_string(&self) -> HidResult<Option<String>> {
        match *self {}
    }

    fn get_serial_number_string(&self) -> HidResult<Option<String>> {
        match *self {}
    }

    fn get_report_descriptor(&self, _buf: &mut [u8]) -> HidResult<usize> {
        match *self {}
    }
}
