# Resize Event Debouncing

## Overview

The resize event handler implements **debouncing** to optimize DBus communication with QEMU during window resize operations. This prevents flooding QEMU with hundreds of resize requests during window dragging.

## Problem

**Original implementation**:
```rust
DisplayMsg::CanvasResize(w, h) => {
    // ... update canvas_size ...
    if scaling_mode == ResizeGuest {
        relm4::spawn(async move {  // ❌ One task per resize event
            console.set_ui_info(...).await;
        });
    }
}
```

**Issues**:
- Window drag (e.g., 1920×1080 → 800×600) → **hundreds of async tasks** spawned
- Each resize event → separate DBus call
- Excessive CPU and DBus overhead
- No coordination between concurrent resize operations

## Solution: Single Worker with Debounce Pattern

### Architecture

A **single worker task** implements the debounce pattern using `tokio::select!`:

1. **Idle State**: Blocks waiting for first resize event
2. **Debouncing State**: 
   - Waits 200ms for user to stop dragging
   - If new resize arrives → resets timer (updates stored size)
   - On timeout → sends final size to QEMU
   - Returns to idle state

### Data Structure

```rust
#[derive(Debug, Clone, Copy)]
pub struct ResizeCommand {
    pub w: u32,
    pub h: u32,
    pub mm_per_pixel: f64,  // DPI info passed with command
}
```

**Design**:
- `Copy` trait: Small (24 bytes), efficient to pass
- Self-contained: All necessary data included
- No shared state: No `Arc<Mutex<...>>` needed

### Worker Implementation

```rust
fn spawn_resize_debouncer(
    rx: AsyncReceiver<ResizeCommand>,
    console: ConsoleController,
) {
    relm4::spawn(async move {
        // Outer loop: Idle state
        while let Ok(mut cmd) = rx.recv().await {
            // Inner loop: Debouncing state
            loop {
                tokio::select! {
                    // Branch A: New resize before timeout
                    Ok(new_cmd) = rx.recv() => {
                        cmd = new_cmd;  // Update, reset timer
                    }
                    // Branch B: Timeout (user stopped dragging)
                    _ = tokio::time::sleep(Duration::from_millis(200)) => {
                        console.set_ui_info(...).await;
                        break;  // Return to idle
                    }
                }
            }
        }
    });
}
```

**Key points**:
- ✅ Single task for entire app lifetime
- ✅ Timer resets automatically (new `sleep` each loop)
- ✅ Non-blocking UI: `try_send()` never blocks
- ✅ Bounded channel: Prevents unbounded memory growth

## Integration

### Model Changes

```rust
pub struct VmDisplayModel {
    // ... existing fields ...
    resize_tx: AsyncSender<ResizeCommand>,
}
```

### Init Function

```rust
fn init(...) -> ComponentParts<Self> {
    // Bounded channel with small buffer
    let (resize_tx, resize_rx) = kanal::bounded_async(32);
    
    // Start single worker
    spawn_resize_debouncer(resize_rx, init.console_ctrl.clone());
    
    let model = VmDisplayModel {
        // ... existing fields ...
        resize_tx,
        // ...
    };
    // ...
}
```

### Update Handlers

**CanvasResize handler**:
```rust
DisplayMsg::CanvasResize(w, h) => {
    if w > 0 && h > 0 {
        self.canvas_size = (w as f64, h as f64);
        self.changes.cursor = true;

        if self.scaling_mode == ScalingMode::ResizeGuest {
            let _ = self.resize_tx.try_send(ResizeCommand {
                w: w as u32,
                h: h as u32,
                mm_per_pixel: self.mm_per_pixel,
            });
        }
    }
}
```

**SetScalingMode handler**:
```rust
DisplayMsg::SetScalingMode(mode) => {
    self.scaling_mode = mode;
    if mode == ScalingMode::ResizeGuest {
        let (w, h) = self.canvas_size;
        if w > 0.0 && h > 0.0 {
            let _ = self.resize_tx.try_send(ResizeCommand {
                w: w as u32,
                h: h as u32,
                mm_per_pixel: self.mm_per_pixel,
            });
        }
    }
}
```

## Behavior Comparison

### Before Debouncing

| Event | Tasks Spawned | DBus Calls | Latency |
|-------|---------------|------------|---------|
| Drag window (100 resizes) | 100 | 100 | Immediate (but wasteful) |
| Mode switch | 1 | 1 | Immediate |

### After Debouncing

| Event | Tasks Spawned | DBus Calls | Latency |
|-------|---------------|------------|---------|
| Drag window (100 resizes) | 1 | 1 | 200ms after drag ends |
| Mode switch | 1 | 1 | 200ms (consistent) |

**Example scenario**:
```
User drags window: 1920×1080 → 1500×800 → 1200×600 → 1000×500
Timeline:
0ms:    Resize 1920×1080 arrives → start 200ms timer
50ms:   Resize 1500×800 arrives → reset timer (update cmd)
100ms:  Resize 1200×600 arrives → reset timer (update cmd)
150ms:  Resize 1000×500 arrives → reset timer (update cmd)
350ms:  Timer expires → send 1000×500 to QEMU
```

**Result**: 4 resize events → **1 DBus call** (75% reduction)

## Performance Impact

### Task Reduction
- **Before**: 100 resizes → 100 spawned tasks
- **After**: 100 resizes → **1 worker task** (99% reduction)

### DBus Call Reduction  
- **Before**: Each resize → 1 DBus call
- **After**: Entire drag sequence → **1 DBus call** (99% reduction)

### Memory Impact
- Channel: 32 commands × 24 bytes = **768 bytes** (negligible)
- Worker task: 1 stack = **~8 KB** (constant)

## Configuration

### Debounce Timeout: 200ms

**Rationale**: Standard UI interaction threshold for "stopped operation"  
**Trade-off**: 
- Too short (< 100ms): May send intermediate sizes during smooth drags
- Too long (> 500ms): Feels laggy to user

### Channel Capacity: 32

**Rationale**: 
- 32 commands ≈ 0.5 seconds at 60Hz resize rate
- Sufficient to buffer during DBus call
- Small enough to prevent memory issues

**Alternative**:
- `bounded(2)`: Latest-only aggressive dropping
- `unbounded`: No backpressure (not recommended)

## Testing

### Manual Testing
1. Rapidly drag window corner
2. Verify only final size sent to QEMU
3. Check logs for "Resize debounced" messages

### Expected Log Output
```
INFO Scaling mode set to: ResizeGuest
INFO Resize debounced: 1920x1080 (508mm x 285mm)
INFO Resize debounced: 800x600 (211mm x 158mm)
```

## Related Features

This debouncing system complements:
- **Mouse Event Conflation** (`src/dbus/mouse.rs`): Merges rapid mouse moves
- **Coordinate Clamping** (`src/display/vm_display.rs`): Ensures edges are reachable
- **Dynamic DPI Detection** (`src/display/vm_display.rs`): Accurate physical dimensions

Together, they provide a complete high-performance input handling system.

## References

- Original design discussion: Code review feedback
- Tokio select pattern: [Tokio Documentation](https://tokio.rs/tokio/topics/select)
- Debouncing concept: [UI Patterns](https://dreampuf.github.io/Notebook/observer-pattern/debounce.html)
