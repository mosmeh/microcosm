mod i8042;
mod rtc;
mod serial;

pub use i8042::I8042;
pub use rtc::Rtc;
pub use serial::Serial;

use crate::{Error, Result};
use std::{
    ops::{Range, RangeInclusive},
    sync::{Arc, Mutex},
};

pub trait PortIoDevice {
    fn port_range(&self) -> PortRange;
    fn read(&mut self, port: u16, data: &mut [u8]) -> Result<()>;
    fn write(&mut self, port: u16, data: &[u8]) -> Result<()>;
}

impl<T: PortIoDevice + ?Sized> PortIoDevice for &mut T {
    fn port_range(&self) -> PortRange {
        (**self).port_range()
    }

    fn read(&mut self, port: u16, data: &mut [u8]) -> Result<()> {
        (**self).read(port, data)
    }

    fn write(&mut self, port: u16, data: &[u8]) -> Result<()> {
        (*self).write(port, data)
    }
}

impl<T: PortIoDevice + ?Sized> PortIoDevice for Box<T> {
    fn port_range(&self) -> PortRange {
        (**self).port_range()
    }

    fn read(&mut self, port: u16, data: &mut [u8]) -> Result<()> {
        (**self).read(port, data)
    }

    fn write(&mut self, port: u16, data: &[u8]) -> Result<()> {
        (**self).write(port, data)
    }
}

impl<T: PortIoDevice + ?Sized> PortIoDevice for Mutex<T> {
    fn port_range(&self) -> PortRange {
        self.lock().unwrap().port_range()
    }

    fn read(&mut self, port: u16, data: &mut [u8]) -> Result<()> {
        self.get_mut().unwrap().read(port, data)
    }

    fn write(&mut self, port: u16, data: &[u8]) -> Result<()> {
        self.get_mut().unwrap().write(port, data)
    }
}

impl<T: PortIoDevice + ?Sized> PortIoDevice for Arc<Mutex<T>> {
    fn port_range(&self) -> PortRange {
        self.lock().unwrap().port_range()
    }

    fn read(&mut self, port: u16, data: &mut [u8]) -> Result<()> {
        self.lock().unwrap().read(port, data)
    }

    fn write(&mut self, port: u16, data: &[u8]) -> Result<()> {
        self.lock().unwrap().write(port, data)
    }
}

pub(crate) struct PortIoHub<T> {
    devices: Vec<T>,
}

impl<T> Default for PortIoHub<T> {
    fn default() -> Self {
        Self {
            devices: Vec::new(),
        }
    }
}

impl<T: PortIoDevice> PortIoHub<T> {
    pub fn add_device(&mut self, device: T) -> Result<()> {
        let range = device.port_range();
        for d in &self.devices {
            if range.overlaps(d.port_range()) {
                return Err(Error::DeviceRangeOverlap);
            }
        }
        self.devices.push(device);
        Ok(())
    }
}

impl<T: PortIoDevice> PortIoDevice for PortIoHub<T> {
    fn port_range(&self) -> PortRange {
        (0..=u16::MAX).into()
    }

    fn read(&mut self, port: u16, data: &mut [u8]) -> Result<()> {
        for device in &mut self.devices {
            if device.port_range().contains(port) {
                return device.read(port, data);
            }
        }
        Ok(())
    }

    fn write(&mut self, port: u16, data: &[u8]) -> Result<()> {
        for device in &mut self.devices {
            if device.port_range().contains(port) {
                return device.write(port, data);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PortRange {
    base: u16,
    len: u16,
}

impl From<Range<u16>> for PortRange {
    fn from(range: Range<u16>) -> Self {
        Self {
            base: range.start,
            len: range.end - range.start,
        }
    }
}

impl From<RangeInclusive<u16>> for PortRange {
    fn from(range: RangeInclusive<u16>) -> Self {
        Self {
            base: *range.start(),
            len: *range.end() - *range.start() + 1,
        }
    }
}

impl Ord for PortRange {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.base
            .cmp(&other.base)
            .then_with(|| self.len.cmp(&other.len))
    }
}

impl PartialOrd for PortRange {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl PortRange {
    fn contains(self, port: u16) -> bool {
        self.base <= port && port < self.base + self.len
    }

    fn overlaps(self, other: Self) -> bool {
        self.base < other.base + other.len && other.base < self.base + self.len
    }
}
