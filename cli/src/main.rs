use clap::Parser;
use microcosm::{
    device::{Rtc, Serial, I8042},
    Hypervisor,
};
use nix::sys::termios::{tcgetattr, tcsetattr, LocalFlags, SetArg, Termios};
use std::{
    ffi::{CString, NulError},
    io::Read,
    num::NonZeroUsize,
    os::fd::AsFd,
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[derive(Debug, Parser)]
struct Cli {
    /// Path to Kernel image
    #[clap(short, long)]
    kernel: PathBuf,

    /// Memory size
    #[clap(short, long, value_parser = try_parse_size, default_value = "64M")]
    memory: NonZeroUsize,

    /// Kernel command line
    #[clap(
        short,
        long,
        value_parser = try_parse_cmdline,
        default_value = "panic=1 console=ttyS0"
    )]
    cmdline: CString,

    /// Path to Linux initial ramdisk
    #[clap(long)]
    initrd: Option<PathBuf>,

    /// Paths to Multiboot modules
    #[clap(long = "module")]
    modules: Vec<PathBuf>,
}

fn try_parse_cmdline(s: &str) -> Result<CString, NulError> {
    CString::new(s)
}

fn try_parse_size(s: &str) -> Result<NonZeroUsize, String> {
    let s = s.trim();
    let mut chars = s.chars().peekable();
    match chars.peek() {
        None => return Err("Empty size".to_owned()),
        Some(c) if !c.is_ascii_digit() => return Err(format!("Unexpected character {c}")),
        _ => {}
    }

    let mut n = 0usize;
    while let Some(c) = chars.peek() {
        if !c.is_ascii_digit() {
            break;
        }
        n = n
            .checked_mul(10)
            .and_then(|n| n.checked_add(c.to_digit(10).unwrap() as usize))
            .ok_or_else(|| "Size is too large".to_owned())?;
        chars.next().unwrap();
    }

    let exp = chars.next().map_or(Ok(0), |c| match c {
        'K' | 'k' => Ok(10),
        'M' | 'm' => Ok(20),
        'G' | 'g' => Ok(30),
        _ => Err(format!("Unexpected character {c}")),
    })?;
    match chars.next() {
        Some('B' | 'b') | None => {}
        Some(c) => return Err(format!("Unexpected character {c}")),
    }

    let n = n
        .checked_shl(exp)
        .ok_or_else(|| "Size is too large".to_owned())?;
    let n = NonZeroUsize::new(n).ok_or_else(|| "Size must be positive".to_owned())?;
    Ok(n)
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let hypervisor = Hypervisor::new()?;

    let mut builder = hypervisor
        .guest(cli.kernel)
        .memory_size(cli.memory)
        .cmdline(cli.cmdline);
    if let Some(path) = cli.initrd {
        builder = builder.initrd(path);
    }
    for path in cli.modules {
        builder = builder.add_module(path);
    }

    let mut guest = builder.build()?;
    guest.add_device(Mutex::new(I8042::new()))?;
    guest.add_device(Mutex::new(Rtc::new()))?;

    let serial = Arc::new(Mutex::new(Serial::new(0, guest.irq())));
    guest.add_device(serial.clone())?;

    std::thread::spawn(move || guest.run());

    let stdin = std::io::stdin().lock();
    let mut stdin = RawModeReader::new(stdin)?;

    let mut buf = [0; 1024];
    let mut escape = false;
    'outer: loop {
        match stdin.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let mut serial = serial.lock().unwrap();
                for &b in &buf[..n] {
                    if !escape && b == 0x1 {
                        // Ctrl-A
                        escape = true;
                        continue;
                    }
                    if escape && b == b'x' {
                        break 'outer;
                    }
                    escape = false;
                    serial.queue_rx(b)?;
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::Interrupted
                        | std::io::ErrorKind::UnexpectedEof
                ) =>
            {
                break
            }
            Err(e) => return Err(e.into()),
        }
    }

    Ok(())
}

struct RawModeReader<T: AsFd> {
    inner: T,
    original_termios: Termios,
}

impl<T: AsFd> RawModeReader<T> {
    fn new(inner: T) -> nix::Result<Self> {
        let original_termios = tcgetattr(&inner)?;
        let mut raw_mode = original_termios.clone();
        raw_mode.local_flags &= !(LocalFlags::ICANON | LocalFlags::ECHO | LocalFlags::ISIG);
        tcsetattr(&inner, SetArg::TCSANOW, &raw_mode)?;
        Ok(Self {
            inner,
            original_termios,
        })
    }
}

impl<T: AsFd> Drop for RawModeReader<T> {
    fn drop(&mut self) {
        let _ = tcsetattr(&self.inner, SetArg::TCSANOW, &self.original_termios);
    }
}

impl<T: AsFd + Read> Read for RawModeReader<T> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}
