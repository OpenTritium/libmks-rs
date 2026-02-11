# Smart Mouse Event Conflation

## Overview

The mouse event handler implements intelligent event conflation to optimize DBus communication with QEMU while preserving event ordering guarantees.

## Problem

In the original implementation:
- Each mouse event spawned a separate async task via `relm4::spawn()`
- Rapid mouse movements (e.g., 1000 events/sec) → 1000 DBus calls
- Concurrent tasks could complete out of order
- Click events could be delayed or lost if the queue was congested

## Solution

A dedicated consumer loop (`handle_mouse_commands_smart`) that:

1. **Waits for first event** using blocking `recv()`
2. **Drains the queue** using non-blocking `try_recv()`
3. **Conflates intelligently**:
   - **Absolute position** (`SetAbsPosition`): Keep only latest (Last Write Wins)
   - **Relative motion** (`RelMotion`): Accumulate deltas (Summation)
   - **Button events** (`Press`/`Release`): Never drop, preserve order

## Event Flow Example

### Scenario: User quickly drags mouse and clicks

```
Queue: [Move(10,10), Move(20,20), Move(30,30), Press(Left), Move(40,40)]
```

**Processing**:
1. `recv()` gets `Move(10,10)`
2. `try_recv()` gets `Move(20,20)` → replace, now `Move(20,20)`
3. `try_recv()` gets `Move(30,30)` → replace, now `Move(30,30)`
4. `try_recv()` gets `Press(Left)` → **Barrier!**
5. Send `Move(30,30)` (position before click)
6. Process `Press(Left)` (must not be dropped)
7. `recv()` gets `Move(40,40)` → next iteration

**Result to QEMU**: `Move(30,30)` → `Press(Left)` → `Move(40,40)`

### Performance Impact

- **Before**: 5 events → 5 DBus calls
- **After**: 5 events → 3 DBus calls (40% reduction)

Under heavy load (1000 mouse moves in queue):
- **Before**: 1000 DBus calls
- **After**: 1 DBus call (99.9% reduction)

## Benefits

✅ **Ordering guarantees**: Click events never reordered with moves  
✅ **Self-regulating**: Queue buildup → automatic conflation → self-clearing  
✅ **Responsive**: Mouse feels more responsive under load  
✅ **Efficient**: Drastically reduces DBus call overhead  

## Implementation Details

### Key Function: `handle_mouse_commands_smart`

Located in `src/dbus/mouse.rs`, this function:

```rust
async fn handle_mouse_commands_smart(
    proxy: Arc<MouseProxy<'static>>,
    cmd_rx: AsyncReceiver<Command>,
) -> MksResult<JoinHandle<()>>
```

**Algorithm**:
1. Blocking wait on first event
2. For `SetAbsPosition`:
   - Greedy drain all subsequent `SetAbsPosition` events
   - Keep only the latest coordinates
   - Stop if non-move event encountered (barrier)
3. For `RelMotion`:
   - Greedy drain all subsequent `RelMotion` events
   - Accumulate delta values
   - Stop if non-move event encountered (barrier)
4. For `Press`/`Release`:
   - Send immediately, no conflation

### Manual Session Implementation

Unlike other DBus interfaces, `MouseSession` is manually implemented (not via `impl_session_connect!` macro) to use the smart handler:

```rust
pub struct MouseSession {
    pub tx: MouseController,
    pub rx: kanal::AsyncReceiver<Event>,
    pub watch_task: JoinHandle<()>,
    pub cmd_handler: JoinHandle<()>,
}

pub async fn connect(conn: &zbus::Connection, path: String)
    -> MksResult<MouseSession>
```

This allows using `handle_mouse_commands_smart` instead of the generic `handle_commands` macro.

## Testing

All existing tests pass:
```bash
cargo test dbus::mouse
```

Tests verify:
- Button press/release ordering
- Absolute position handling
- Relative motion accumulation
- Session connection and property sync

## Future Considerations

### Relative Motion Support

Currently, `vm_display.rs` only uses absolute positioning (`SetAbsPosition`). The handler also supports relative motion (`RelMotion`) for future use cases (e.g., FPS games, pointer lock mode).

### Channel Bounds

The implementation uses `unbounded` channels. With smart conflation, backpressure is not needed since:
- Conflation happens in microseconds
- Queue buildup improves conflation ratio
- Only complete DBus failure would cause memory issues

## References

- Original design discussion: See code review comments
- QEMU DBus Mouse API: <https://www.qemu.org/docs/master/interop/dbus-display.html#org.qemu.Display1.Mouse-section>
