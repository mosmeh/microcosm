#!/bin/bash

set -euo pipefail

INITRD_DIR="$(dirname "$(realpath "$0")")"

cd "$INITRD_DIR/root"

mkdir -p bin
wget https://busybox.net/downloads/binaries/1.35.0-x86_64-linux-musl/busybox -O bin/busybox
chmod +x bin/busybox

find . | cpio -o -H newc > "$INITRD_DIR/initrd.cpio"
