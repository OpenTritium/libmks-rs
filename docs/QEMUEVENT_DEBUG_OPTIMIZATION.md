# QemuEvent Debug Optimization

## Problem

The `QemuEvent` enum had `#[derive(Debug)]` which would print the entire contents of the `data` field for events like `Scanout`, `Update`, and `CursorDefine`. For high-resolution displays, this could be **several megabytes** of pixel data.

### Risk Scenarios

1. **Logging Accidents**: `log::info!("Received event: {:?}", event)` would dump 8MB of pixel data to logs
2. **Error Messages**: `unwrap()` or `expect()` failures would print massive debug output
3. **Debugging**: Developers debugging the display pipeline would get overwhelmed

### Example Impact

```rust
// Before optimization:
let event = QemuEvent::Scanout {
    width: 1920,
    height: 1080,
    data: vec![0u8; 1920 * 1080 * 4], // 8.3MB
};
println!("{:?}", event);
// Output: Scanout { width: 1920, height: 1080, ..., data: [0, 0, 0, 0, 0, 0, ... ] }
//         ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
//         8+ MB of useless output!

// After optimization:
println!("{:?}", event);
// Output: Scanout { width: 1920, height: 1080, ..., data: <8294400 bytes> }
//         ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
//         Concise and informative!
```

## Solution

### 1. Removed `#[derive(Debug)]` and Implemented Custom `Debug`

```rust
#[derive(PartialEq)]  // Removed Debug
pub enum QemuEvent { ... }

/// Helper struct for Debug output that shows buffer size instead of contents
struct BytesDebug(usize);

impl std::fmt::Debug for BytesDebug {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<{} bytes>", self.0)
    }
}
```

### 2. Handled Large Data Variants

For variants with `Vec<u8>` data fields, we use `BytesDebug`:

```rust
Self::Scanout { width, height, stride, pixman_format, data } => f
    .debug_struct("Scanout")
    .field("width", width)
    .field("height", height)
    .field("stride", stride)
    .field("pixman_format", pixman_format)
    .field("data", &BytesDebug(data.len()))  // ← Size instead of contents
    .finish(),
```

### 3. Handled File Descriptor Variants

For `OwnedFd` fields, we show a placeholder:

```rust
Self::ScanoutDmabuf { dmabuf: _, width, height, ... } => f
    .debug_struct("ScanoutDmabuf")
    .field("dmabuf", &"<fd>")  // ← Placeholder, not raw fd
    .field("width", width)
    // ...
```

## Implementation Details

### Variants with Custom Debug Output

| Variant | Field | Before | After |
|---------|-------|--------|-------|
| `Scanout` | `data` | `[0, 255, 128, ...]` (8MB) | `<8294400 bytes>` |
| `Update` | `data` | `[0, 255, 128, ...]` (10KB) | `<10000 bytes>` |
| `CursorDefine` | `data` | `[255, 255, 255, ...]` (4KB) | `<4096 bytes>` |
| `ScanoutDmabuf` | `dmabuf` | `OwnedFd { ... }` | `<fd>` |
| `ScanoutDmabuf2` | `dmabuf` | `[OwnedFd(...), ...]` | `<N fds>` |
| `ScanoutMap` | `memfd` | `OwnedFd { ... }` | `<fd>` |

### Variants with Standard Debug

Simple variants without large data use standard debug:
- `Disable` → `Disable`
- `MouseSet` → `MouseSet { x: 100, y: 200, on: 1 }`
- `UpdateDmabuf` → `UpdateDmabuf { x: 100, y: 100, ... }`
- `UpdateMap` → `UpdateMap { x: 100, y: 100, ... }`

## Test Coverage

Added 7 new tests to verify the optimization:

1. `test_debug_scanout_doesnt_print_data` - Verifies 8.3MB data → size only
2. `test_debug_update_doesnt_print_data` - Verifies 10KB data → size only
3. `test_debug_cursor_doesnt_print_data` - Verifies 4KB cursor data → size only
4. `test_debug_scanout_dmabuf` - Verifies FD placeholder
5. `test_debug_scanout_dmabuf2` - Verifies multi-FD count display
6. `test_debug_disable` - Verifies simple variants
7. `test_debug_mouse_set` - Verifies standard debug format

All tests verify:
- ✅ Size information is present
- ✅ Actual data contents are NOT printed
- ✅ Debug string is concise (< 500 chars for all variants)

## Benefits

### 1. Log Safety
```rust
// Now safe to use in production logging:
log::info!("Received display event: {:?}", event);
// Output: "Received display event: Scanout { width: 1920, ... data: <8294400 bytes> }"
//         ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
//         Concise and informative!
```

### 2. Error Messages
```rust
// Error paths won't flood console:
let event = rx.recv().expect("Failed to receive event");
// If this fails, you get a reasonable error message, not 8MB of hex output
```

### 3. Debugging Experience
```rust
// Developers can use dbg! macro without fear:
dbg!(&event);
// Output: [src/main.rs:42] &event = Scanout { width: 1920, ... data: <8294400 bytes> }
```

### 4. Performance
- **Memory**: No need to format 8MB strings for debug output
- **CPU**: Faster debug printing (O(1) instead of O(n) for data size)
- **I/O**: Less console/log spam when debugging

## Testing

```bash
$ cargo test --lib dbus::listener::

test result: ok. 21 passed; 0 failed
# ↑ 14 original tests + 7 new debug tests
```

All existing tests continue to pass, confirming backward compatibility.

## Conclusion

This is a **zero-cost optimization** with significant practical benefits:

- ✅ **Prevents log flooding** in production
- ✅ **Improves debugging experience** for developers
- ✅ **Reduces memory pressure** when formatting debug output
- ✅ **Maintains type safety** and compile-time guarantees
- ✅ **Zero runtime overhead** (compile-time transformation)

The custom `Debug` implementation is a best practice for enums containing large data buffers, and should be applied to any similar types in the codebase.
