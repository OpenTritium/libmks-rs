# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

libmks-rs is a Rust implementation of libmks (Mouse, Keyboard, Screen) that provides D-Bus interfaces to interact with QEMU's `-display dbus` feature. The project is a Rust port of the GNOME libmks library, using GTK4/relm4 for the display widget.

## Common Commands

```bash
# Build the main library
cargo build

# Run all tests
cargo test

# Run a specific test
cargo test <test_name>

# Run clippy lints
cargo clippy --all

# Build qemu-display workspace (separate crate)
cd qemu-display && cargo build

# Run examples
cargo run --example vm_display_interactive
cargo run --example vm_display_with_input
cargo run --example scaling_mode_demo
```

### Running with QEMU

Start QEMU with D-Bus display support, then run the example:
```bash
qemu-system-x86_64 \
    -enable-kvm -cpu host \
    -device virtio-vga-gl,xres=1920,yres=1080 \
    -m 8G -smp 4 \
    -display dbus,gl=on \
    -cdrom fedora.iso -hda fedora.img -boot d
```

## Build Dependencies

On Fedora:
```bash
sudo dnf install cargo gcc usbredir-devel wayland-devel libxkbcommon-devel glib2-devel gtk4-devel gstreamer1-devel gstreamer1-plugins-base-devel
```

## Architecture

### Workspace Structure

```
libmks-rs/          # Main crate with dbus/display modules
├── src/
│   ├── dbus/       # D-Bus client (keyboard, mouse, multitouch, console)
│   └── display/    # GTK4 display widget (vm_display, coordinate, udma)
└── qemu-display/   # Separate workspace
    ├── qemu-display/   # Core D-Bus interface library
    ├── qemu-rdw/       # GTK4 widget (default member)
    ├── qemu-vnc/       # VNC server
    ├── qemu-vte/       # VTE terminal client
    ├── qemu-rdp/       # RDP server
    └── keycodemap/     # Keycode mapping
```

### Key Technologies

- **zbus**: D-Bus communication with QEMU
- **tokio**: Async runtime
- **relm4**: GTK4 declarative UI framework
- **kanal**: Sync-to-async bridge for input handling

### Input Handling Pattern

High-frequency input events (mouse/keyboard) use `kanal` channels to bridge the synchronous UI thread with async D-Bus:

```rust
// UI thread: try_send() is non-blocking
channel.try_send(InputEvent::MouseButton { ... });

// Async task: receives and forwards to D-Bus
while let Ok(event) = receiver.recv().await { ... }
```

Channel capacity (2048 for mouse, 256 for keyboard) provides backpressure. Dropped events on full channel are acceptable for high-frequency input.

### Coordinate System

Uses Cell-based caching in `coordinate.rs` for efficient screen coordinate transformations between logical and physical coordinates.
