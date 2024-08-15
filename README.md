# microcosm

[![build](https://github.com/mosmeh/microcosm/workflows/build/badge.svg)](https://github.com/mosmeh/microcosm/actions)

A minimal KVM-based virtual machine monitor.

microcosm is a Firecracker-style VMM that skips BIOS, bootloader, and most of the legacy hardware emulation for simplicity. It directly boots a kernel image and provides a minimal set of devices to the guest.

## Features

- Directly boots different types of kernel images without a bootloader. Supported boot protocols:
	- [Linux](https://www.kernel.org/doc/Documentation/x86/boot.txt) (vmlinux, bzImage)
	- [PVH](https://xenbits.xen.org/docs/unstable/misc/pvh.html)
	- [Multiboot](https://www.gnu.org/software/grub/manual/multiboot/multiboot.html)
- Devices
  - Serial devices
  - RTC
  - i8042 keyboard controller (only CPU reset command)
- Multiprocessor support

## Prerequisites

x86/x86_64 Linux host with KVM support.

To set up KVM, [the instruction in the Firecracker documentation](https://github.com/firecracker-microvm/firecracker/blob/main/docs/getting-started.md) is helpful.

## Usage

```sh
# Run Linux kernel
cargo run -- \
	--kernel /path/to/linux/arch/x86_64/boot/bzImage \
	--initrd /path/to/initrd.cpio \
	--cmdline 'panic=1 console=ttyS0' \
	--cpus 2 \
	--memory 512M

# Run a kernel with Multiboot protocol
cargo run -- \
	--kernel /path/to/multiboot/kernel \
	--module /path/to/multiboot/module
```

You can use the [initrd/build.sh](initrd/build.sh) script to create a minimal initrd image for x86_64 Linux:

```sh
initrd/build.sh
# initrd/initrd.cpio is created
```
