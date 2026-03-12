#!/bin/bash

# Configuration
ISO_PATH="livecd.ubuntu.iso"
RAM_SIZE="8G"
CPU_CORES="8"

# Default settings
GPU_MODE="virtgpu"
MOUSE_MODE="relative"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

# Print Help
print_help() {
    echo -e "${CYAN}Usage: $0 [OPTIONS]${NC}"
    echo ""
    echo "Options:"
    echo "  -g, --gpu <mode>     Set GPU mode: ${YELLOW}vga, virtgpu, virtgpu-vulkan${NC} (default: $GPU_MODE)"
    echo "  -m, --mouse <mode>   Set Mouse mode: ${YELLOW}relative, absolute${NC} (default: $MOUSE_MODE)"
    echo "  -h, --help           Show this help message"
    echo ""
    echo "Mouse Drivers (Using Latest VirtIO):"
    echo "  relative -> virtio-mouse-pci   (Best for gaming/FPS, sends raw dx/dy)"
    echo "  absolute -> virtio-tablet-pci  (Best for UI/Desktop, perfectly syncs cursor)"
    echo ""
    echo "Examples:"
    echo "  $0 -g virtgpu -m relative"
    echo "  $0 --gpu virtgpu-vulkan --mouse absolute"
    exit 0
}

# Parse command line arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        -g|--gpu)
            if [[ -z "$2" || "$2" == -* ]]; then
                echo -e "${RED}Error: Missing value for $1${NC}"
                print_help
            fi
            GPU_MODE="$2"
            shift 2
            ;;
        -m|--mouse)
            if [[ -z "$2" || "$2" == -* ]]; then
                echo -e "${RED}Error: Missing value for $1${NC}"
                print_help
            fi
            MOUSE_MODE="$2"
            shift 2
            ;;
        -h|--help)
            print_help
            ;;
        *)
            echo -e "${RED}Unknown option: $1${NC}"
            print_help
            ;;
    esac
done

echo -e "${YELLOW}=== Starting QEMU with D-Bus Display ===${NC}"
echo "GPU Mode:   $GPU_MODE"
echo "Mouse Mode: $MOUSE_MODE"

# -----------------------------------------------------------------------------
# 1. Configure Mouse Arguments
# -----------------------------------------------------------------------------
if [[ "$MOUSE_MODE" == "absolute" ]]; then
    # Modern virtio-tablet instead of legacy usb-tablet
    MOUSE_ARGS=("-device" "virtio-tablet-pci")
elif [[ "$MOUSE_MODE" == "relative" ]]; then
    MOUSE_ARGS=("-device" "virtio-mouse-pci")
else
    echo -e "${RED}Error: Invalid mouse mode '$MOUSE_MODE'. Use 'relative' or 'absolute'.${NC}"
    exit 1
fi

# -----------------------------------------------------------------------------
# 2. Configure GPU, Display, and Machine/Memory Arguments
# -----------------------------------------------------------------------------
# Base machine args (will be appended with shared memory if needed)
MACHINE_ARGS=("-machine" "q35")
MEMORY_ARGS=("-m" "$RAM_SIZE")
DISPLAY_ARGS=()
VGA_ARGS=()

case "$GPU_MODE" in
    vga)
        # VirtIO 2D mode: modern driver path without OpenGL/shared-memory requirements
        MACHINE_ARGS=("-machine" "q35")
        MEMORY_ARGS=("-m" "$RAM_SIZE")
        DISPLAY_ARGS=("-display" "dbus")
        VGA_ARGS=("-device" "virtio-vga,max_outputs=1,xres=1920,yres=1080")
        ;;
    virtgpu)
        # VirtIO GPU with VirGL 3D (requires OpenGL on DBus and Shared Memory for DMABUF)
        MACHINE_ARGS=("-machine" "q35,memory-backend=mem")
        MEMORY_ARGS=("-object" "memory-backend-memfd,id=mem,size=$RAM_SIZE,share=on" "-m" "$RAM_SIZE")
        DISPLAY_ARGS=("-display" "dbus,gl=on")
        VGA_ARGS=("-device" "virtio-vga-gl,max_outputs=1,xres=1920,yres=1080")
        ;;
    virtgpu-vulkan)
        # VirtIO GPU with Venus Vulkan support (Requires hostmem, blob, and Venus enabled)
        MACHINE_ARGS=("-machine" "q35,memory-backend=mem")
        MEMORY_ARGS=("-object" "memory-backend-memfd,id=mem,size=$RAM_SIZE,share=on" "-m" "$RAM_SIZE")
        DISPLAY_ARGS=("-display" "dbus,gl=on")
        # hostmem requires a size; blob=true and venus=on are required for Vulkan support.
        VGA_ARGS=("-device" "virtio-vga-gl,max_outputs=1,xres=1920,yres=1080,blob=true,hostmem=4G,venus=on")
        ;;
    *)
        echo -e "${RED}Error: Invalid GPU mode '$GPU_MODE'. Use 'vga', 'virtgpu', or 'virtgpu-vulkan'.${NC}"
        exit 1
        ;;
esac

# -----------------------------------------------------------------------------
# 3. Pre-flight Checks & Cleanup
# -----------------------------------------------------------------------------
if [ ! -f "$ISO_PATH" ]; then
    echo -e "${RED}Error: ISO file $ISO_PATH not found${NC}"
    exit 1
fi

# Clean up any existing QEMU process
echo "Cleaning up existing processes..."
pkill -9 qemu-system-x86_64 2>/dev/null || true
sleep 1

# -----------------------------------------------------------------------------
# 4. Build and Execute QEMU Command
# -----------------------------------------------------------------------------
echo -e "${GREEN}Starting QEMU...${NC}"
echo "QEMU will register on D-Bus session bus as 'org.qemu'"
echo ""
echo "In another terminal, run your rust client based on the mode you chose."

QEMU_CMD=(
    qemu-system-x86_64
    "${MACHINE_ARGS[@]}"
    "${MEMORY_ARGS[@]}"
    -smp "$CPU_CORES"
    -cdrom "$ISO_PATH"
    -boot d
    -enable-kvm
    "${DISPLAY_ARGS[@]}"
    -device virtio-keyboard-pci
    "${VGA_ARGS[@]}"
    -usb
    "${MOUSE_ARGS[@]}"
    -device usb-kbd
    -net nic,model=virtio
    -net user
    -k en-us
    -no-reboot
)

echo -e "${CYAN}Executing: ${QEMU_CMD[*]}${NC}"
echo "------------------------------------------------------------------"

"${QEMU_CMD[@]}"

echo -e "${YELLOW}QEMU exited${NC}"
