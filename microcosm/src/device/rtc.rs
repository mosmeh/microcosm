use super::{PortIoDevice, PortRange};
use crate::Result;
use chrono::{Datelike, Timelike, Utc};

const RTC_PORT_INDEX: u16 = 0x70;
const RTC_PORT_DATA: u16 = 0x71;

const RTC_SECONDS: u8 = 0x00;
const RTC_MINUTES: u8 = 0x02;
const RTC_HOURS: u8 = 0x04;
const RTC_DAY_OF_WEEK: u8 = 0x06;
const RTC_DAY_OF_MONTH: u8 = 0x07;
const RTC_MONTH: u8 = 0x08;
const RTC_YEAR: u8 = 0x09;
const RTC_CENTURY: u8 = 0x32;

const RTC_STATUS_B: u8 = 0x0b;
const RTC_STATUS_B_24H: u8 = 0x02;

#[derive(Default)]
pub struct Rtc {
    cmos_index: u8,
}

impl Rtc {
    pub fn new() -> Self {
        Self::default()
    }
}

impl PortIoDevice for Rtc {
    fn port_range(&self) -> PortRange {
        (RTC_PORT_INDEX..=RTC_PORT_DATA).into()
    }

    fn read(&mut self, port: u16, data: &mut [u8]) -> Result<()> {
        let Some(data) = data.first_mut() else {
            return Ok(());
        };
        if port == RTC_PORT_DATA {
            let now = Utc::now();
            *data = match self.cmos_index {
                RTC_SECONDS => bin_to_bcd(now.second() as u8),
                RTC_MINUTES => bin_to_bcd(now.minute() as u8),
                RTC_HOURS => bin_to_bcd(now.hour() as u8),
                RTC_DAY_OF_WEEK => bin_to_bcd(now.weekday().num_days_from_sunday() as u8 + 1),
                RTC_DAY_OF_MONTH => bin_to_bcd(now.day() as u8),
                RTC_MONTH => bin_to_bcd(now.month() as u8),
                RTC_YEAR => bin_to_bcd((now.year() % 100) as u8),
                RTC_CENTURY => bin_to_bcd((now.year() / 100) as u8),
                RTC_STATUS_B => RTC_STATUS_B_24H,
                _ => return Ok(()),
            };
        }
        Ok(())
    }

    fn write(&mut self, port: u16, data: &[u8]) -> Result<()> {
        let Some(&data) = data.first() else {
            return Ok(());
        };
        if port == RTC_PORT_INDEX {
            self.cmos_index = data & !(1 << 7);
        }
        Ok(())
    }
}

const fn bin_to_bcd(bin: u8) -> u8 {
    ((bin / 10) << 4) | (bin % 10)
}
