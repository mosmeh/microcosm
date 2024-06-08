use super::{PortIoDevice, PortRange};
use crate::{guest::Irq, Result};
use std::{collections::VecDeque, io::Write};
use sys::serial_reg::{
    UART_FCR, UART_FCR_CLEAR_RCVR, UART_FCR_CLEAR_XMIT, UART_IER, UART_IER_RDI, UART_IER_THRI,
    UART_IIR, UART_IIR_FIFO_ENABLED_16550A, UART_IIR_NO_INT, UART_IIR_RDI, UART_IIR_THRI, UART_LCR,
    UART_LCR_DLAB, UART_LSR, UART_LSR_BI, UART_LSR_DR, UART_LSR_TEMT, UART_LSR_THRE, UART_MCR,
    UART_MCR_LOOP, UART_MCR_OUT2, UART_MSR, UART_MSR_CTS, UART_MSR_DCD, UART_MSR_DSR, UART_RX,
    UART_SCR, UART_TX,
};

const FIFO_LEN: usize = 64;

pub struct Serial {
    base_port: u16,
    irq: Irq,
    irq_number: u8,
    irq_state: u8,
    dll: u8,
    dlm: u8,
    iir: u8,
    ier: u8,
    fcr: u8,
    lcr: u8,
    mcr: u8,
    lsr: u8,
    msr: u8,
    scr: u8,
    rx_buf: VecDeque<u8>,
    tx_buf: VecDeque<u8>,
}

impl Serial {
    pub fn new(n: u8, irq: Irq) -> Self {
        let (base_port, irq_number) = match n {
            0 => (0x3f8, 4),
            1 => (0x2f8, 3),
            2 => (0x3e8, 4),
            3 => (0x2e8, 3),
            _ => panic!("Invalid serial port number"),
        };
        Self {
            base_port,
            irq,
            irq_number,
            irq_state: 0,
            dll: 0,
            dlm: 0,
            iir: UART_IIR_NO_INT as u8,
            ier: 0,
            fcr: 0,
            lcr: 0,
            mcr: 0,
            lsr: UART_LSR_TEMT as u8 | UART_LSR_THRE as u8,
            msr: UART_MSR_DCD as u8 | UART_MSR_DSR as u8 | UART_MSR_CTS as u8,
            scr: UART_MCR_OUT2 as u8,
            rx_buf: VecDeque::with_capacity(FIFO_LEN),
            tx_buf: VecDeque::with_capacity(FIFO_LEN),
        }
    }

    pub fn queue_rx(&mut self, data: u8) -> Result<()> {
        if self.mcr & UART_MCR_LOOP as u8 == 0 && self.rx_buf.len() < FIFO_LEN {
            self.rx_buf.push_back(data);
            self.lsr |= UART_LSR_DR as u8;
        }
        self.update_irq()
    }

    fn flush_tx(&mut self) -> std::io::Result<()> {
        self.lsr |= UART_LSR_TEMT as u8 | UART_LSR_THRE as u8;
        let mut stdout = std::io::stdout().lock();
        let (a, b) = self.tx_buf.as_slices();
        stdout.write_all(a)?;
        stdout.write_all(b)?;
        stdout.flush()?;
        self.tx_buf.clear();
        Ok(())
    }

    fn update_irq(&mut self) -> Result<()> {
        if self.lcr & UART_FCR_CLEAR_RCVR as u8 != 0 {
            self.lcr &= !UART_FCR_CLEAR_RCVR as u8;
            self.rx_buf.clear();
            self.lsr &= !UART_LSR_DR as u8;
        }

        if self.lcr & UART_FCR_CLEAR_XMIT as u8 != 0 {
            self.lcr &= !UART_FCR_CLEAR_XMIT as u8;
            self.tx_buf.clear();
            self.lsr |= UART_LSR_TEMT as u8 | UART_LSR_THRE as u8;
        }

        let mut iir = 0;
        if self.ier & UART_IER_RDI as u8 != 0 && self.lsr & UART_LSR_DR as u8 != 0 {
            iir |= UART_IIR_RDI as u8;
        }
        if self.ier & UART_IER_THRI as u8 != 0 && self.lsr & UART_LSR_TEMT as u8 != 0 {
            iir |= UART_IIR_THRI as u8;
        }
        if iir != 0 {
            self.iir = iir;
            if self.irq_state == 0 {
                self.irq.set_level(self.irq_number, true)?;
            }
        } else {
            self.iir = UART_IIR_NO_INT as u8;
            if self.irq_state != 0 {
                self.irq.set_level(self.irq_number, false)?;
            }
        }
        self.irq_state = iir;

        if self.ier & UART_IER_THRI as u8 == 0 {
            self.flush_tx()?;
        }

        Ok(())
    }
}

impl PortIoDevice for Serial {
    fn port_range(&self) -> PortRange {
        (self.base_port..(self.base_port + 8)).into()
    }

    fn read(&mut self, port: u16, data: &mut [u8]) -> Result<()> {
        let Some(data) = data.first_mut() else {
            return Ok(());
        };
        match (port - self.base_port).into() {
            UART_RX if self.lcr & UART_LCR_DLAB as u8 != 0 => *data = self.dll,
            UART_RX if self.rx_buf.is_empty() => {}
            UART_RX if self.lsr & UART_LSR_BI as u8 != 0 => {
                self.lsr &= !UART_LSR_BI as u8;
                *data = 0;
            }
            UART_RX => {
                *data = self.rx_buf.pop_front().unwrap();
                if self.rx_buf.is_empty() {
                    self.lsr &= !UART_LSR_DR as u8;
                }
            }
            UART_IER if self.lcr & UART_LCR_DLAB as u8 != 0 => *data = self.dlm,
            UART_IER => *data = self.ier,
            UART_IIR => *data = self.iir | UART_IIR_FIFO_ENABLED_16550A as u8,
            UART_LCR => *data = self.lcr,
            UART_MCR => *data = self.mcr,
            UART_LSR => *data = self.lsr,
            UART_MSR => *data = self.msr,
            UART_SCR => *data = self.scr,
            _ => {}
        }
        self.update_irq()?;
        Ok(())
    }

    fn write(&mut self, port: u16, data: &[u8]) -> Result<()> {
        let Some(&data) = data.first() else {
            return Ok(());
        };
        match (port - self.base_port).into() {
            UART_TX if self.lcr & UART_LCR_DLAB as u8 != 0 => self.dll = data,
            UART_TX if self.mcr & UART_MCR_LOOP as u8 != 0 => {
                if self.rx_buf.len() < FIFO_LEN {
                    self.rx_buf.push_back(data);
                    self.lsr |= UART_LSR_DR as u8;
                }
            }
            UART_TX if self.tx_buf.len() < FIFO_LEN => {
                self.tx_buf.push_back(data);
                self.lsr &= !UART_LSR_TEMT as u8;
                if self.tx_buf.len() == FIFO_LEN / 2 {
                    self.lsr &= !UART_LSR_THRE as u8;
                }
                self.flush_tx()?;
            }
            UART_TX => self.lsr &= !(UART_LSR_TEMT as u8 | UART_LSR_THRE as u8),
            UART_IER if self.lcr & UART_LCR_DLAB as u8 != 0 => self.dlm = data,
            UART_IER => self.ier = data & 0xf,
            UART_FCR => self.fcr = data,
            UART_LCR => self.lcr = data,
            UART_MCR => self.mcr = data,
            UART_SCR => self.scr = data,
            _ => {}
        }
        self.update_irq()?;
        Ok(())
    }
}
