#!/bin/bash

# Run QEMU with D-Bus display and test the qemu_display example

# Configuration
ISO_PATH="livecd.iso"
RAM_SIZE="2G"
CPU_CORES="2"
DISPLAY_MODE="dbus"
SPICE_PORT="5900"

# Check if ISO file exists
if [ ! -f "$ISO_PATH" ]; then
    echo "Error: ISO file $ISO_PATH not found"
    exit 1
fi

# Parse command line arguments
while getopts ":m:" opt; do
    case $opt in
        m)
            DISPLAY_MODE="$OPTARG"
            ;;
        \?)
            echo "Invalid option: -$OPTARG" >&2
            exit 1
            ;;
        :)
            echo "Option -$OPTARG requires an argument" >&2
            exit 1
            ;;
    esac
done

# Run QEMU
qemu-system-x86_64 \
    -m $RAM_SIZE \
    -smp $CPU_CORES \
    -cdrom $ISO_PATH \
    -boot d \
    -display $DISPLAY_MODE \
    -spice port=$SPICE_PORT,disable-ticketing \
    -device virtio-keyboard-pci \
    -device virtio-mouse-pci \
    -device virtio-vga \
    -chardev stdio,id=char0,mux=on \
    -mon chardev=char0,mode=readline \
    -serial chardev:char0 \
    -parallel none \
    -usb \
    -device usb-tablet \
    -device usb-kbd \
    -net nic,model=virtio \
    -net user \
    -k en-us \
    -no-reboot \
    -no-shutdown

echo "QEMU exited"
