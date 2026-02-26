#!/bin/bash

# Run QEMU with D-Bus display for the qemu_display example
#
# IMPORTANT: This script starts QEMU in a way that works with qemu_display example.
# - Uses -display dbus (WITHOUT p2p=on) so QEMU registers on session D-Bus as "org.qemu"
# - Does NOT use -nographic as it interferes with D-Bus display initialization
# - The qemu_display example connects via "session" D-Bus address
#
# Usage:
#   ./run_qemu_display.sh           # Default scanout mode
#   ./run_qemu_display.sh scanout   # Explicit scanout mode
#   ./run_qemu_display.sh dmabuf2   # DMABUF2 mode
#
# In another terminal, run:
#   cargo run --example qemu_display -- session [mode]

# Configuration
ISO_PATH="livecd.iso"
RAM_SIZE="8G"
CPU_CORES="8"

# Parse command line arguments
MODE="${1:-scanout}"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

echo -e "${YELLOW}=== Starting QEMU with D-Bus Display ===${NC}"
echo "Mode: $MODE"

# Check if ISO file exists
if [ ! -f "$ISO_PATH" ]; then
    echo -e "${RED}Error: ISO file $ISO_PATH not found${NC}"
    exit 1
fi

# Clean up any existing QEMU and free port 5900
echo "Cleaning up..."
pkill -9 qemu-system-x86_64 2>/dev/null || true
# Kill any process using port 5900
fuser -k 5900/tcp 2>/dev/null || true
sleep 1

# Run QEMU with D-Bus display
# Key: Without p2p=on, QEMU registers on D-Bus session bus as "org.qemu"
# This allows qemu_display example to connect via "session" address
echo -e "${GREEN}Starting QEMU...${NC}"
echo "QEMU will register on D-Bus session bus as 'org.qemu'"
echo ""
echo "In another terminal, run:"
echo "  cargo run --example qemu_display -- session $MODE"

qemu-system-x86_64 \
    -m $RAM_SIZE \
    -smp $CPU_CORES \
    -cdrom $ISO_PATH \
    -boot d \
    -enable-kvm \
    -display dbus \
    -spice port=5900,disable-ticketing=on \
    -device virtio-keyboard-pci \
    -device virtio-vga,max_outputs=1,xres=1920,yres=1080 \
    -usb \
    -device virtio-mouse-pci \
    -device usb-kbd \
    -net nic,model=virtio \
    -net user \
    -k en-us \
    -no-reboot

echo -e "${YELLOW}QEMU exited${NC}"
