#!/bin/bash

# Start QEMU with LiveCD and test various display modes

# Configuration
ISO_PATH="livecd.iso"
RAM_SIZE="2G"
CPU_CORES="2"
SPICE_PORT="5900"

# Check if ISO file exists
if [ ! -f "$ISO_PATH" ]; then
    echo "Error: ISO file $ISO_PATH not found"
    exit 1
fi

# Parse command line arguments
while getopts ":m:g:" opt; do
    case $opt in
        m)
            DISPLAY_MODE="$OPTARG"
            ;;
        g)
            GPU_ACCEL="$OPTARG"
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

# Default display mode
if [ -z "$DISPLAY_MODE" ]; then
    DISPLAY_MODE="gtk"
fi

# Default GPU acceleration
if [ -z "$GPU_ACCEL" ]; then
    GPU_ACCEL="none"
fi

# Run QEMU with selected display mode
case $DISPLAY_MODE in
    gtk)
        echo "Running QEMU with GTK display"
        qemu-system-x86_64 \
            -m $RAM_SIZE \
            -smp $CPU_CORES \
            -cdrom $ISO_PATH \
            -boot d \
            -enable-kvm \
            -display gtk \
            -spice port=$SPICE_PORT,disable-ticketing \
            -device virtio-keyboard-pci \
            -device virtio-mouse-pci \
            -device virtio-vga,max_outputs=1,xres=1920,yres=1080 \
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
        ;;
    mks-c)
        echo "Running QEMU with MKS-C display"
        qemu-system-x86_64 \
            -m $RAM_SIZE \
            -smp $CPU_CORES \
            -cdrom $ISO_PATH \
            -boot d \
            -enable-kvm \
            -display dbus,p2p=on \
            -spice port=$SPICE_PORT,disable-ticketing \
            -device virtio-keyboard-pci \
            -device virtio-mouse-pci \
            -device virtio-vga,max_outputs=1,xres=1920,yres=1080 \
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
        ;;
    mks-rs)
        echo "Running QEMU with MKS-RS display"
        qemu-system-x86_64 \
            -m $RAM_SIZE \
            -smp $CPU_CORES \
            -cdrom $ISO_PATH \
            -boot d \
            -enable-kvm \
            -display dbus,p2p=on \
            -spice port=$SPICE_PORT,disable-ticketing \
            -device virtio-keyboard-pci \
            -device virtio-mouse-pci \
            -device virtio-vga,max_outputs=1,xres=1920,yres=1080 \
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
        ;;
    *)
        echo "Error: Unknown display mode $DISPLAY_MODE"
        echo "Supported modes: gtk, mks-c, mks-rs"
        exit 1
        ;;
esac

echo "QEMU exited"
