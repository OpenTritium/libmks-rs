# GTK4-Wayland Pointer Lock Implementation Status

## ✅ Completed (Phase 1-2)

### 1. Dependencies Added ✓
- `wayland-client = "0.31"` 
- `wayland-protocols = "0.32"`
- `gdk4-wayland = { version = "0.11.0-alpha.3", features = ["wayland_crate"] }`

### 2. Core Infrastructure ✓
- Created `src/display/wayland_lock.rs` module (200+ lines)
- Added Wayland-specific imports and types
- Implemented `WaylandLock`, `WaylandState`, `LockedPointerSession` structs
- Added Dispatch trait implementations for Wayland protocols

### 3. Integration with VM Display ✓
- Added `wayland_lock: Option<WaylandLock>` field to `VmDisplayModel`
- Added `WaylandRelativeMotion { dx, dy }` message variant
- Created `init_wayland_lock()` helper function (checks WAYLAND_DISPLAY env var)
- Integrated `dispatch_pending()` calls in `update()` loop
- Updated `MouseMove` handler to ignore absolute motion when Wayland lock active
- Updated `ToggleCapture` to use Wayland lock

### 4. Safe API Design ✓
- Using `gdk4-wayland`'s `wayland_crate` feature (when fully implemented)
- Avoids unsafe pointer conversions
- Uses environment variable check for Wayland detection

## ⚠️ Placeholder / TODO Items

### Critical TODOs
1. **Protocol Import Issues**
   - `wayland_protocols::unstable` path not working
   - Need to fix import paths for:
     - `zwp_pointer_constraints_v1`
     - `zwp_relative_pointer_manager_v1`
     - `zwp_locked_pointer_v1`
     - `zwp_relative_pointer_v1`

2. **Registry Global Binding**
   - Need to enumerate globals in `Dispatch<wl_registry>` implementation
   - Bind pointer constraints and relative pointer manager globals
   - Handle version checking

3. **Pointer Lock Implementation**
   - `lock_pointer()` is a stub - needs actual protocol calls
   - Need to get `wl_surface` from GTK4 native surface
   - Need to create locked pointer and relative pointer objects
   - Need to set up relative motion event handlers

4. **GLib Main Loop Integration**
   - Event pump not integrated into GLib main loop yet
   - Currently using manual `dispatch_pending()` calls
   - Need to implement FD-based event source for lowest latency

5. **Surface Binding**
   - Cannot get `wl_surface` from GTK4 widgets yet
   - Need to extract from `widgets.input_plane.native().surface()`
   - Requires proper type casting with gdk4-wayland

## 🏗️ Architecture Notes

### Current Approach
- **Separate Wayland Connection**: Creates independent connection (not sharing with GDK)
- **Placeholder Types**: Using u32 placeholders for missing protocol types
- **Environment Detection**: Checks `WAYLAND_DISPLAY` env var for Wayland detection
- **Safe at Rest**: No unsafe code in current implementation

### Intended Design (from plan)
- Use `gdk4-wayland`'s `WaylandSurfaceExtManual::wl_surface()` for safe surface access
- Integrate event queue into GLib main loop via FD source
- Handle relative motion events and send to `mouse_ctrl.rel_motion()`
- Auto-unlock on drop via `LockedPointerSession`

## 📊 Build Status
- **Compilation**: ✅ Success (with 4 warnings)
- **Warnings**: Unused imports and dead code (expected for placeholder code)
- **Platform**: Wayland-only (panics if not available, per user requirement)

## 🚀 Next Steps for Full Implementation

1. Fix wayland-protocols imports (research correct feature flags)
2. Implement registry global enumeration and binding
3. Get wl_surface from GTK4 native surface
4. Implement actual pointer lock protocol calls
5. Set up relative motion event handling
6. Add GLib FD source integration
7. Test on actual Wayland compositor
8. Handle compositor protocol unavailability gracefully

