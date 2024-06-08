use super::{PortIoDevice, PortRange};
use crate::Result;

const I8042_DATA_REG: u16 = 0x60;
const I8042_COMMAND_REG: u16 = 0x64;
const I8042_CMD_SYSTEM_RESET: u8 = 0xfe;

#[derive(Default)]
pub struct I8042 {
    _private: (),
}

impl I8042 {
    pub fn new() -> Self {
        Self::default()
    }
}

impl PortIoDevice for I8042 {
    fn port_range(&self) -> PortRange {
        (I8042_DATA_REG..=I8042_COMMAND_REG).into()
    }

    fn read(&mut self, _port: u16, _data: &mut [u8]) -> Result<()> {
        Ok(())
    }

    fn write(&mut self, port: u16, data: &[u8]) -> Result<()> {
        if port == I8042_COMMAND_REG && data.first() == Some(&I8042_CMD_SYSTEM_RESET) {
            std::process::exit(0);
        }
        Ok(())
    }
}
