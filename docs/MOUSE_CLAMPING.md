# Mouse Coordinate Clamping Optimization

## Problem

The original implementation discarded mouse events when the calculated VM coordinates were slightly outside the valid range `[0, vm_resolution)`:

```rust
// Old code - dropped edge events
if vm_input_x >= 0.0 && vm_input_x < vm_w as f64 && ... {
    send_to_vm(target_x, target_y);
}
```

**Impact:** When users quickly moved the mouse toward the screen edge, the cursor would stop 1-2 pixels before reaching the actual edge, making it difficult to click UI elements at screen corners (close buttons, taskbar, Start menu, etc.).

## Solution

Implemented coordinate **clamping** to ensure all mouse events are mapped to valid coordinates:

```rust
// New code - clamps to valid range
let raw_x = (x / canvas_w) * vm_w as f64;
let max_x = (vm_w.saturating_sub(1)) as f64;
let clamped_x = raw_x.clamp(0.0, max_x);
```

## Benefits

✅ **Edge Accessibility**: VM cursor now reaches absolute screen edges (0 and max-1)
✅ **Better UX**: Smooth, native-like mouse feel when flicking to edges
✅ **Robustness**: Uses `saturating_sub` to prevent underflow on zero-sized resolutions
✅ **Simpler Logic**: Removed redundant range checks since `clamp` guarantees validity

## Technical Details

- **Clamping Range**: `[0.0, vm_w - 1]` and `[0.0, vm_h - 1]`
- **Safety**: `saturating_sub(1)` handles edge case where `vm_w == 0`
- **Performance**: No measurable impact (single `f64::clamp` per coordinate)

## Example

Before clamping:
- User moves mouse to `x = -5` (slightly outside canvas left edge)
- Event discarded → VM cursor stops at x=2

After clamping:
- User moves mouse to `x = -5`
- Event clamped to `x = 0` → VM cursor reaches actual left edge
