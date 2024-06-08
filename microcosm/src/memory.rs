use crate::{Error, Result};
use nix::sys::mman::{mmap, mmap_anonymous, munmap, MapFlags, ProtFlags};
use std::{
    mem::{align_of, size_of},
    num::NonZeroUsize,
    os::fd::AsFd,
    ptr::NonNull,
};
use zerocopy::AsBytes;

pub struct Mmapped<T> {
    ptr: NonNull<T>,
    size: NonZeroUsize,
}

impl<T: Copy> Mmapped<T> {
    pub fn new_anonymous(size: NonZeroUsize) -> nix::Result<Self> {
        assert!(size.get() >= size_of::<T>());
        let ptr = unsafe {
            mmap_anonymous(
                None,
                size,
                ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
                MapFlags::MAP_PRIVATE,
            )?
        };
        Ok(Self {
            ptr: ptr.cast(),
            size,
        })
    }

    pub fn new_file(fd: impl AsFd, size: NonZeroUsize) -> nix::Result<Self> {
        assert!(size.get() >= size_of::<T>());
        let ptr = unsafe {
            mmap(
                None,
                size,
                ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
                MapFlags::MAP_SHARED,
                &fd,
                0,
            )?
        };
        Ok(Self {
            ptr: ptr.cast(),
            size,
        })
    }

    pub fn as_ptr(&self) -> *mut T {
        self.ptr.as_ptr()
    }

    pub fn as_ref(&self) -> &T {
        unsafe { self.ptr.as_ref() }
    }

    pub fn as_mut_slice(&mut self) -> &mut [T] {
        let len = self.size.get() / size_of::<T>();
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), len) }
    }
}

unsafe impl<T: Send> Send for Mmapped<T> {}

impl<T> Drop for Mmapped<T> {
    fn drop(&mut self) {
        let _ = unsafe { munmap(self.ptr.cast(), self.size.get()) };
    }
}

pub struct RangeAllocator {
    addr: u64,
}

impl RangeAllocator {
    pub fn new(start: u64) -> Self {
        Self { addr: start }
    }

    pub fn raw_alloc(&mut self, size: usize, align: usize) -> u64 {
        let addr = self.addr.next_multiple_of(align as u64);
        self.addr = addr + size as u64;
        addr
    }

    pub fn alloc<T>(&mut self) -> u64 {
        self.raw_alloc(size_of::<T>(), align_of::<T>())
    }

    pub fn alloc_array<T>(&mut self, count: usize) -> u64 {
        self.raw_alloc(size_of::<T>() * count, align_of::<T>())
    }
}

pub trait CopyToGuest {
    fn copy_to_guest(&self, memory: &mut [u8], addr: impl Into<u64>) -> Result<()>;
}

impl<T: AsBytes + ?Sized> CopyToGuest for T {
    fn copy_to_guest(&self, memory: &mut [u8], addr: impl Into<u64>) -> Result<()> {
        let addr: u64 = addr.into();
        let addr: usize = addr.try_into().map_err(|_| {
            // `addr` is larger than the maximum size of `memory`
            Error::OutOfGuestMemory
        })?;
        let memory = memory.get_mut(addr..).ok_or(Error::OutOfGuestMemory)?;
        self.write_to_prefix(memory).ok_or(Error::OutOfGuestMemory)
    }
}
