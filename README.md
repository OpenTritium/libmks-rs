# libmks-rs

Rust port of libmks - Mouse, Keyboard, Screen for QEMU.

## Overview

A Rust library for interacting with QEMU virtual machines via D-Bus, providing display capture and input device control (mouse, keyboard, multitouch).

## Features

- **Display Capture**: DMABUF, software rasterizer, GPU passthrough
- **Input Control**: Mouse, keyboard, and multitouch event handling
- **Wayland Support**: Wayland pointer constraints protocol
- **Async Architecture**: Built on tokio async runtime
