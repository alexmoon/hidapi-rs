// **************************************************************************
// Copyright (c) 2018 Roland Ruckerbauer All Rights Reserved.
//
// This file is part of hidapi-rs, based on hidapi-rs by Osspial
// **************************************************************************

mod ffi;

use failure::Fail;
use std::ffi::CStr;
use std::io::{Read, Write};

use backend::{ApiBackend, ApiDevice, ApiDeviceInfo};

pub struct HidapiBackend {
    api: HidApi,
}

impl ApiBackend for HidapiBackend {
    type Device = HidapiDevice;
    type DeviceInfo = HidapiDeviceInfo;
    type DeviceInfoIter = std::vec::IntoIter<HidapiDeviceInfo>;

    fn create() -> HidResult<Self> {
        Ok(HidapiBackend {
            api: HidApi::new()?,
        })
    }

    fn open_device(&self, vid: u16, pid: u16) -> HidResult<HidapiDevice> {
        Ok(HidapiDevice {
            device: self.api.open(vid, pid)?,
        })
    }

    fn enumerate(&mut self) -> HidResult<Self::DeviceInfoIter> {
        self.api.refresh_devices()?;
        let devices: Vec<_> = self
            .api
            .devices()
            .iter()
            .map(|d| HidapiDeviceInfo(d.clone()))
            .collect();
        Ok(devices.into_iter())
    }
}

pub struct HidapiDevice {
    device: HidDevice,
}

impl ApiDevice for HidapiDevice {}

impl Write for HidapiDevice {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.device
            .write(data)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.compat()))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        panic!("Not usable")
    }
}

// impl Read for HidapiDevice {

// }

pub struct HidapiDeviceInfo(HidDeviceInfo);

impl ApiDeviceInfo for HidapiDeviceInfo {
    fn path(&self) -> Option<String> {
        self.0.path.to_str().map(|s| s.to_owned()).ok()
    }
    fn vendor_id(&self) -> u16 {
        self.0.vendor_id
    }
    fn product_id(&self) -> u16 {
        self.0.product_id
    }
    fn serial_number(&self) -> Option<String> {
        self.0.serial_number.clone()
    }
    fn release_number(&self) -> u16 {
        self.0.release_number
    }
    fn manufacturer_string(&self) -> Option<String> {
        self.0.manufacturer_string.clone()
    }
    fn product_string(&self) -> Option<String> {
        self.0.product_string.clone()
    }
    fn usage_page(&self) -> Option<u16> {
        if cfg!(target_os = "linux") {
            None
        } else {
            Some(self.0.usage_page)
        }
    }
    fn usage(&self) -> u16 {
        self.0.usage
    }
    fn interface_number(&self) -> i32 {
        self.0.interface_number
    }
}

use failure::Error;
use libc::{c_int, size_t, wchar_t};
use std::ffi::CString;
use std::mem::ManuallyDrop;
use std::rc::Rc;

use error::{HidError, HidResult};
const STRING_BUF_LEN: usize = 128;

/// Hidapi context and device member, which ensures deinitialization
/// of the C library happens, when, and only when all devices and the api instance is dropped.
struct HidApiLock;

impl HidApiLock {
    fn acquire() -> HidResult<HidApiLock> {
        if unsafe { !HID_API_LOCK } {
            // Initialize the HID and prevent other HIDs from being created
            unsafe {
                if ffi::hid_init() == -1 {
                    return Err(HidError::InitializationError);
                }
                HID_API_LOCK = true;
                Ok(HidApiLock)
            }
        } else {
            Err(HidError::InitializationError)
        }
    }
}

impl Drop for HidApiLock {
    fn drop(&mut self) {
        unsafe {
            ffi::hid_exit();
            HID_API_LOCK = false;
        }
    }
}

/// Object for handling hidapi context and implementing RAII for it.
/// Only one instance can exist at a time.
pub struct HidApi {
    devices: Vec<HidDeviceInfo>,
    _lock: Rc<HidApiLock>,
}

static mut HID_API_LOCK: bool = false;

impl HidApi {
    /// Initializes the hidapi.
    ///
    /// Will also initialize the currently available device list.
    pub fn new() -> HidResult<Self> {
        let lock = HidApiLock::acquire()?;

        Ok(HidApi {
            devices: unsafe { HidApi::get_hid_device_info_vector()? },
            _lock: Rc::new(lock),
        })
    }

    /// Refresh devices list and information about them (to access them use
    /// `devices()` method)
    pub fn refresh_devices(&mut self) -> HidResult<()> {
        self.devices = unsafe { HidApi::get_hid_device_info_vector()? };
        Ok(())
    }

    unsafe fn get_hid_device_info_vector() -> HidResult<Vec<HidDeviceInfo>> {
        let mut device_vector = Vec::with_capacity(8);

        let enumeration = ffi::hid_enumerate(0, 0);
        {
            let mut current_device = enumeration;

            while !current_device.is_null() {
                device_vector.push(conv_hid_device_info(current_device)?);
                current_device = (*current_device).next;
            }
        }

        if !enumeration.is_null() {
            ffi::hid_free_enumeration(enumeration);
        }

        Ok(device_vector)
    }

    /// Returns list of objects containing information about connected devices
    pub fn devices(&self) -> &Vec<HidDeviceInfo> {
        &self.devices
    }

    /// Open a HID device using a Vendor ID (VID) and Product ID (PID).
    ///
    /// When multiple devices with the same vid and pid are available, then the
    /// first one found in the internal device list will be used. There are however
    /// no grantees, which device this will be.
    pub fn open(&self, vid: u16, pid: u16) -> HidResult<HidDevice> {
        let device = unsafe { ffi::hid_open(vid, pid, std::ptr::null()) };

        if device.is_null() {
            Err(HidError::OpenHidDeviceError)
        } else {
            Ok(HidDevice {
                _hid_device: device,
                _lock: ManuallyDrop::new(self._lock.clone()),
            })
        }
    }

    /// Open a HID device using a Vendor ID (VID), Product ID (PID) and
    /// a serial number.
    pub fn open_serial(&self, vid: u16, pid: u16, sn: &str) -> HidResult<HidDevice> {
        let mut chars = sn.chars().map(|c| c as wchar_t).collect::<Vec<_>>();
        chars.push(0 as wchar_t);
        let device = unsafe { ffi::hid_open(vid, pid, chars.as_ptr()) };
        if device.is_null() {
            Err(HidError::OpenHidDeviceError)
        } else {
            Ok(HidDevice {
                _hid_device: device,
                _lock: ManuallyDrop::new(self._lock.clone()),
            })
        }
    }

    /// The path name be determined by inspecting the device list available with [HidApi::devices()](struct.HidApi.html#method.devices)
    ///
    /// Alternatively a platform-specific path name can be used (eg: /dev/hidraw0 on Linux).
    pub fn open_path(&self, device_path: &CStr) -> HidResult<HidDevice> {
        let device = unsafe { ffi::hid_open_path(device_path.as_ptr()) };

        if device.is_null() {
            Err(HidError::OpenHidDeviceError)
        } else {
            Ok(HidDevice {
                _hid_device: device,
                _lock: ManuallyDrop::new(self._lock.clone()),
            })
        }
    }
}

/// Converts a pointer to a `wchar_t` to a optional string
unsafe fn wchar_to_string(wstr: *const wchar_t) -> HidResult<Option<String>> {
    if wstr.is_null() {
        return Ok(None);
    }

    let mut char_vector: Vec<char> = Vec::with_capacity(8);
    let mut index: isize = 0;

    let o = |i| *wstr.offset(i);

    while o(index) != 0 {
        use std::char;
        char_vector.push(match char::from_u32(o(index) as u32) {
            Some(ch) => ch,
            None => Err(HidError::FromWideCharError {
                wide_char: o(index),
            })?,
        });

        index += 1;
    }

    Ok(Some(char_vector.into_iter().collect()))
}

/// Convert the CFFI `HidDeviceInfo` struct to a native `HidDeviceInfo` struct
unsafe fn conv_hid_device_info(src: *mut ffi::HidDeviceInfo) -> HidResult<HidDeviceInfo> {
    Ok(HidDeviceInfo {
        path: CStr::from_ptr((*src).path).to_owned(),
        vendor_id: (*src).vendor_id,
        product_id: (*src).product_id,
        serial_number: wchar_to_string((*src).serial_number)?,
        release_number: (*src).release_number,
        manufacturer_string: wchar_to_string((*src).manufacturer_string)?,
        product_string: wchar_to_string((*src).product_string)?,
        usage_page: (*src).usage_page,
        usage: (*src).usage,
        interface_number: (*src).interface_number,
    })
}

#[derive(Debug, Clone)]
/// Storage for device related information
pub struct HidDeviceInfo {
    pub path: CString,
    pub vendor_id: u16,
    pub product_id: u16,
    pub serial_number: Option<String>,
    pub release_number: u16,
    pub manufacturer_string: Option<String>,
    pub product_string: Option<String>,
    pub usage_page: u16,
    pub usage: u16,
    pub interface_number: i32,
}

impl HidDeviceInfo {
    /// Use the information contained in `HidDeviceInfo` to open
    /// and return a handle to a [HidDevice](struct.HidDevice.html).
    ///
    /// By default the device path is used to open the device.
    /// When no path is available, then vid, pid and serial number are used.
    /// If both path and serial number are not available, then this function will
    /// fail with [HidError::OpenHidDeviceWithDeviceInfoError](enum.HidError.html#variant.OpenHidDeviceWithDeviceInfoError).
    ///
    /// Note, that opening a device could still be done using [HidApi::open()](struct.HidApi.html#method.open) directly.
    pub fn open_device(&self, hidapi: &HidApi) -> HidResult<HidDevice> {
        if self.path.as_bytes().len() != 0 {
            hidapi.open_path(self.path.as_c_str())
        } else if let Some(ref sn) = self.serial_number {
            hidapi.open_serial(self.vendor_id, self.product_id, sn)
        } else {
            Err(HidError::OpenHidDeviceError)
        }
    }
}

/// Object for accessing HID device
pub struct HidDevice {
    _hid_device: *mut ffi::HidDevice,
    /// Prevents this from outliving the api instance that created it
    _lock: ManuallyDrop<Rc<HidApiLock>>,
}

impl Drop for HidDevice {
    fn drop(&mut self) {
        unsafe {
            ffi::hid_close(self._hid_device);
            ManuallyDrop::drop(&mut self._lock);
        };
    }
}

impl HidDevice {
    /// Check size returned by other methods, if it's equal to -1 check for
    /// error and return Error, otherwise return size as unsigned number
    fn check_size(&self, res: i32) -> HidResult<usize> {
        if res == -1 {
            match self.check_error() {
                Ok(err) => Err(err),
                Err(e) => Err(e),
            }
        } else {
            Ok(res as usize)
        }
    }

    /// Get the last error, which happened in the underlying hidapi C library.
    ///
    /// The `Ok()` variant of the result will contain a [HidError::HidApiError](enum.HidError.html).
    ///
    /// When `Err()` is returned, then acquiring the error string from the hidapi C
    /// library failed. The contained [HidError](enum.HidError.html) is the cause, why no error could
    /// be fetched.
    pub fn check_error(&self) -> HidResult<HidError> {
        Ok(HidError::HidApiError {
            message: unsafe {
                wchar_to_string(ffi::hid_error(self._hid_device))
                    .map_err(|e| HidError::HidApiErrorEmptyWithCause {
                        cause: Error::from(e).compat(),
                    })?
                    .ok_or(HidError::HidApiErrorEmpty)?
            },
        })
    }

    /// The first byte of `data` must contain the Report ID. For
    /// devices which only support a single report, this must be set
    /// to 0x0. The remaining bytes contain the report data. Since
    /// the Report ID is mandatory, calls to `write()` will always
    /// contain one more byte than the report contains. For example,
    /// if a hid report is 16 bytes long, 17 bytes must be passed to
    /// `write()`, the Report ID (or 0x0, for devices with a
    /// single report), followed by the report data (16 bytes). In
    /// this example, the length passed in would be 17.
    /// `write()` will send the data on the first OUT endpoint, if
    /// one exists. If it does not, it will send the data through
    /// the Control Endpoint (Endpoint 0).
    pub fn write(&self, data: &[u8]) -> HidResult<usize> {
        if data.len() == 0 {
            return Err(HidError::InvalidZeroSizeData);
        }
        let res = unsafe { ffi::hid_write(self._hid_device, data.as_ptr(), data.len() as size_t) };
        self.check_size(res)
    }

    /// Input reports are returned to the host through the 'INTERRUPT IN'
    /// endpoint. The first byte will contain the Report number if the device
    /// uses numbered reports.
    pub fn read(&self, buf: &mut [u8]) -> HidResult<usize> {
        let res = unsafe { ffi::hid_read(self._hid_device, buf.as_mut_ptr(), buf.len() as size_t) };
        self.check_size(res)
    }

    /// Input reports are returned to the host through the 'INTERRUPT IN'
    /// endpoint. The first byte will contain the Report number if the device
    /// uses numbered reports. Timeout measured in milliseconds, set -1 for
    /// blocking wait.
    pub fn read_timeout(&self, buf: &mut [u8], timeout: i32) -> HidResult<usize> {
        let res = unsafe {
            ffi::hid_read_timeout(
                self._hid_device,
                buf.as_mut_ptr(),
                buf.len() as size_t,
                timeout,
            )
        };
        self.check_size(res)
    }

    /// Send a Feature report to the device.
    /// Feature reports are sent over the Control endpoint as a
    /// Set_Report transfer.  The first byte of `data` must contain the
    /// 'Report ID'. For devices which only support a single report, this must
    /// be set to 0x0. The remaining bytes contain the report data. Since the
    /// 'Report ID' is mandatory, calls to `send_feature_report()` will always
    /// contain one more byte than the report contains. For example, if a hid
    /// report is 16 bytes long, 17 bytes must be passed to
    /// `send_feature_report()`: 'the Report ID' (or 0x0, for devices which
    /// do not use numbered reports), followed by the report data (16 bytes).
    /// In this example, the length passed in would be 17.
    pub fn send_feature_report(&self, data: &[u8]) -> HidResult<()> {
        if data.len() == 0 {
            return Err(HidError::InvalidZeroSizeData);
        }
        let res = unsafe {
            ffi::hid_send_feature_report(self._hid_device, data.as_ptr(), data.len() as size_t)
        };
        let res = self.check_size(res)?;
        if res != data.len() {
            Err(HidError::IncompleteSendError {
                sent: res,
                all: data.len(),
            })
        } else {
            Ok(())
        }
    }

    /// Set the first byte of `buf` to the 'Report ID' of the report to be read.
    /// Upon return, the first byte will still contain the Report ID, and the
    /// report data will start in buf[1].
    pub fn get_feature_report(&self, buf: &mut [u8]) -> HidResult<usize> {
        let res = unsafe {
            ffi::hid_get_feature_report(self._hid_device, buf.as_mut_ptr(), buf.len() as size_t)
        };
        self.check_size(res)
    }

    /// Set the device handle to be in blocking or in non-blocking mode. In
    /// non-blocking mode calls to `read()` will return immediately with an empty
    /// slice if there is no data to be read. In blocking mode, `read()` will
    /// wait (block) until there is data to read before returning.
    /// Modes can be changed at any time.
    pub fn set_blocking_mode(&self, blocking: bool) -> HidResult<()> {
        let res = unsafe {
            ffi::hid_set_nonblocking(self._hid_device, if blocking { 0i32 } else { 1i32 })
        };
        if res == -1 {
            Err(HidError::SetBlockingModeError {
                mode: match blocking {
                    true => "blocking",
                    false => "not blocking",
                },
            })
        } else {
            Ok(())
        }
    }

    /// Get The Manufacturer String from a HID device.
    pub fn get_manufacturer_string(&self) -> HidResult<Option<String>> {
        let mut buf = [0 as wchar_t; STRING_BUF_LEN];
        let res = unsafe {
            ffi::hid_get_manufacturer_string(
                self._hid_device,
                buf.as_mut_ptr(),
                STRING_BUF_LEN as size_t,
            )
        };
        let res = self.check_size(res)?;
        unsafe { wchar_to_string(buf[..res].as_ptr()) }
    }

    /// Get The Manufacturer String from a HID device.
    pub fn get_product_string(&self) -> HidResult<Option<String>> {
        let mut buf = [0 as wchar_t; STRING_BUF_LEN];
        let res = unsafe {
            ffi::hid_get_product_string(
                self._hid_device,
                buf.as_mut_ptr(),
                STRING_BUF_LEN as size_t,
            )
        };
        let res = self.check_size(res)?;
        unsafe { wchar_to_string(buf[..res].as_ptr()) }
    }

    /// Get The Serial Number String from a HID device.
    pub fn get_serial_number_string(&self) -> HidResult<Option<String>> {
        let mut buf = [0 as wchar_t; STRING_BUF_LEN];
        let res = unsafe {
            ffi::hid_get_serial_number_string(
                self._hid_device,
                buf.as_mut_ptr(),
                STRING_BUF_LEN as size_t,
            )
        };
        let res = self.check_size(res)?;
        unsafe { wchar_to_string(buf[..res].as_ptr()) }
    }

    /// Get a string from a HID device, based on its string index.
    pub fn get_indexed_string(&self, index: i32) -> HidResult<Option<String>> {
        let mut buf = [0 as wchar_t; STRING_BUF_LEN];
        let res = unsafe {
            ffi::hid_get_indexed_string(
                self._hid_device,
                index as c_int,
                buf.as_mut_ptr(),
                STRING_BUF_LEN,
            )
        };
        let res = self.check_size(res)?;
        unsafe { wchar_to_string(buf[..res].as_ptr()) }
    }
}
