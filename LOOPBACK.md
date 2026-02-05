# Mouse Cursor Loopback Implementation

## Overview

This implementation adds real-time mouse cursor following to the VM display example by creating a bidirectional event loopback mechanism.

## How It Works

```
User Action (Mouse Move)
    ↓
GTK Event Controller (VmDisplayModel)
    ↓
Coordinate Transform (Widget → VM space)
    ↓
MouseController::set_abs_position()
    ↓
mouse_cmd_rx (AsyncReceiver<mouse::Command>)
    ↓
mock_qemu_source() converts to QemuEvent::MouseSet
    ↓
VmDisplayModel receives event
    ↓
Screen updates cursor position
    ↓
Cursor renders at transformed position
```

## Key Components

### 1. Controller Creation (`create_mock_controllers`)

**Before:**
```rust
fn create_mock_controllers() -> (ConsoleController, MouseController, KeyboardController) {
    let (mouse_tx, mut mouse_rx) = kanal::unbounded_async();
    // mouse_rx consumed internally
    ...
}
```

**After:**
```rust
fn create_mock_controllers() -> (
    ConsoleController,
    MouseController,
    KeyboardController,
    kanal::AsyncReceiver<mouse::Command>,  // ← Returns mouse_rx
) {
    let (mouse_tx, mouse_rx) = kanal::unbounded_async();
    // mouse_rx returned to caller
    ...
}
```

### 2. Concurrent Event Processing (`mock_qemu_source`)

Uses `tokio::select!` to handle two streams simultaneously:

```rust
loop {
    tokio::select! {
        // Stream 1: Timer-based animation (resize, disable)
        _ = interval.tick() => {
            frame_count += 1;
            // Handle resolution changes
        }

        // Stream 2: Mouse command loopback
        Ok(cmd) = mouse_cmd_rx.recv() => {
            match cmd {
                mouse::Command::SetAbsPosition { x, y } => {
                    // Convert back to QemuEvent for cursor rendering
                    tx.send(QemuEvent::MouseSet { x: x as i32, y: y as i32, on: 1 }).await.ok();
                }
                mouse::Command::Press(btn) => info!("Press: {:?}", btn),
                mouse::Command::Release(btn) => info!("Release: {:?}", btn),
                _ => warn!("Unhandled command"),
            }
        }
    }
}
```

### 3. Coordinate Transform Accuracy

The implementation correctly handles:
- **Letterboxing**: Black bars on top/bottom when aspect ratio differs
- **Pillarboxing**: Black bars on sides when aspect ratio differs
- **Scaling**: Cursor position scales with display size

**Formula:**
```rust
// Forward transform (rendering)
vm_x_screen = offset_x + (vm_cursor_x * scale)

// Inverse transform (input)
vm_cursor_x = (screen_x - offset_x) / scale
```

## Testing the Implementation

### Run the Example

```bash
cargo run --example vm_display_with_input
```

### Test Scenarios

**Phase 1 (0-3s): 800x600 Blue**
- Move mouse over the display
- Yellow cursor should follow your mouse pointer
- Check coordinates align correctly

**Phase 2 (3-6s): 1280x720 Green**
- Resolution increases
- Cursor should still follow accurately
- Tests scaling with letterboxing

**Phase 3 (6-8s): Disable**
- Screen goes black
- Cursor disappears
- Mouse movements are ignored (logged as warnings)

**Phase 4 (8s+): 400x600 Red**
- Resolution decreases
- Cursor reappears
- Tests pillarboxing (side black bars)

### Expected Output

```
[INFO] Simulation Started: Phase 1 - 800x600 (Blue)
[INFO] Move your mouse over the VM display to see the cursor follow!
[INFO] Phase 2: Resize to 1280x720 (Green) - Check coordinate accuracy
[INFO] [Mouse] Press: Left
[INFO] [Mouse] Release: Left
[INFO] Phase 3: Disable Event - Cursor should disappear
[WARN] [Mouse] Command ignored during Disable phase
[INFO] Phase 4: Re-enable 400x600 (Red) - Check pillarboxing
```

## Performance Considerations

1. **Mouse Deduplication**: Only sends new coordinates (prevents spam)
2. **Async Processing**: All DBus calls are async (non-blocking)
3. **Fair Scheduling**: `tokio::select!` ensures both streams are processed fairly

## Extension Points

### Add Keyboard Loopback

```rust
// Return kbd_rx from create_mock_controllers
// Add to tokio::select!:
Ok(cmd) = kbd_cmd_rx.recv() => {
    // Handle keyboard commands
}
```

### Add Relative Mouse Mode

```rust
mouse::Command::RelMotion { dx, dy } => {
    // Send relative motion events instead of absolute
    tx.send(QemuEvent::MouseMove { dx, dy }).await.ok();
}
```

### Add Cursor Hotspot Support

```rust
QemuEvent::CursorDefine {
    width: 64,
    height: 64,
    hot_x: 5,  // Cursor tip offset
    hot_y: 5,
    data: cursor_data,
}
```

## Troubleshooting

### Cursor Not Following

1. Check if you're clicking the display first (to grab focus)
2. Verify you're in a non-Disable phase
3. Check logs for mouse command reception

### Incorrect Coordinates

1. Verify scale calculation in `VmDisplayModel::update()`
2. Check offset calculation for letterboxing
3. Ensure `ContentFit::Contain` is used

### Performance Issues

1. Check if mouse deduplication is working
2. Monitor task spawn rate in logs
3. Verify async/await usage (no blocking calls)

## Files Modified

- `examples/vm_display_with_input.rs`: Complete loopback implementation

## Related Files

- `src/display/vm_display.rs`: VmDisplayModel with event handlers
- `src/dbus/mouse.rs`: MouseController and Command enum
- `src/display/screen.rs`: Screen and cursor rendering logic
