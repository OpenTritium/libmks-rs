//! `DBus` proxy for QEMU Mouse interface.
//! <https://www.qemu.org/docs/master/interop/dbus-display.html#org.qemu.Display1.Mouse-section>
use crate::{MksResult, error::MksError, generate_watcher, impl_controller, impl_session_connect, mks_error};
use derive_more::{AsRef, Deref, From};
use kanal::{AsyncReceiver, AsyncSender};
use serde_repr::Serialize_repr;
use std::hint::spin_loop;
use tokio::task::AbortHandle;
use zbus::{Result, proxy};
use zvariant::Type;

const LOG_TARGET: &str = "mks.dbus.mouse";

#[proxy(interface = "org.qemu.Display1.Mouse", default_service = "org.qemu", gen_blocking = false)]
pub trait Mouse {
    /// Sends a button press event.
    ///
    /// The button must be released later to avoid a "stuck" state.
    async fn press(&self, button: Button) -> Result<()>;

    /// Sends a button release event.
    async fn release(&self, button: Button) -> Result<()>;

    /// Sets the absolute mouse pointer position.
    ///
    /// **Protocol Constraint**: This method **fails** if `IsAbsolute` is `false`.
    /// Used when the guest device is configured as an absolute pointing device (e.g., USB Tablet).
    async fn set_abs_position(&self, x: u32, y: u32) -> Result<()>;

    /// Sends a relative mouse motion event.
    ///
    /// **Protocol Constraint**: This method **fails** if `IsAbsolute` is `true`.
    /// Used when the guest device is configured as a standard relative mouse (e.g., PS/2 Mouse).
    async fn rel_motion(&self, dx: i32, dy: i32) -> Result<()>;

    /// Indicates whether the guest input device expects absolute coordinates.
    ///
    /// * `true`: Client **MUST** use `set_abs_position`.
    /// * `false`: Client **MUST** use `rel_motion`.
    #[zbus(property)]
    fn is_absolute(&self) -> Result<bool>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize_repr, Type)]
#[repr(u32)]
pub enum Button {
    Left = 0,
    Middle = 1,
    Right = 2,
    WheelUp = 3,
    WheelDown = 4,
    Side = 5,
    Extra = 6,
}

impl Button {
    pub fn from_xorg(button: u32) -> Option<Self> {
        use Button::*;
        match button {
            1 => Some(Left),
            2 => Some(Middle),
            3 => Some(Right),
            4 => Some(WheelUp),
            5 => Some(WheelDown),
            8 => Some(Side),
            9 => Some(Extra),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub enum Command {
    Press(Button),
    Release(Button),
    SetAbsPosition { x: u32, y: u32 },
    RelMotion { dx: i32, dy: i32 },
}

#[derive(Debug, Clone)]
pub enum Event {
    IsAbsolute(bool),
}

#[derive(Clone, AsRef, Deref, From)]
pub struct MouseController(pub AsyncSender<Command>);

impl_controller!(MouseController, Command, {
    pub fn press(button: Button) => Press(button);
    pub fn release(button: Button) => Release(button);
});

impl MouseController {
    /// Spin tries for lock-free realtime send before falling back to regular try_send path.
    const REALTIME_SEND_SPIN: usize = 8;

    #[inline]
    fn try_send_spin(&self, cmd: Command) -> MksResult<()> {
        let mut pending = Some(cmd);
        for _ in 0..Self::REALTIME_SEND_SPIN {
            if self.0.try_send_option_realtime(&mut pending)? {
                return Ok(());
            }
            spin_loop();
        }
        // Fallback to normal try_send path without blocking UI thread.
        if self.0.try_send_option(&mut pending)? {
            Ok(())
        } else {
            Err(MksError::MouseError("Mouse command queue contention: dropped realtime event".into()))
        }
    }

    #[inline]
    pub fn try_set_abs_position(&self, x: u32, y: u32) -> MksResult<()> {
        self.try_send_spin(Command::SetAbsPosition { x, y })
    }

    #[inline]
    pub fn rel_motion(&self, dx: i32, dy: i32) -> MksResult<()> { self.try_send_spin(Command::RelMotion { dx, dy }) }
}

impl_session_connect!(
    MouseSession,
    MouseProxy<'static>,
    MouseController,
    Command,
    Event,
    watch_proxy_changes,
    handle_mouse_commands,
    8192
);

generate_watcher!(
    watch_proxy_changes,
    MouseProxy<'static>,
    Event,
    "mouse",
    {
        is_absolute => receive_is_absolute_changed => Event::IsAbsolute,
    }
);

#[derive(Debug, Default)]
enum PendingMove {
    #[default]
    None,
    Abs {
        x: u32,
        y: u32,
    },
    Rel {
        dx: i32,
        dy: i32,
    },
}

async fn flush_pending_move(proxy: &MouseProxy<'static>, pending_move: &mut PendingMove) {
    match std::mem::take(pending_move) {
        PendingMove::None => {}
        PendingMove::Abs { x, y } => {
            if let Err(e) = proxy.set_abs_position(x, y).await {
                mks_error!(error:? = e; "Mouse set_abs_position failed");
            }
        }
        PendingMove::Rel { dx, dy } => {
            if let Err(e) = proxy.rel_motion(dx, dy).await {
                mks_error!(error:? = e; "Mouse rel_motion failed");
            }
        }
    }
}

async fn handle_mouse_commands(proxy: MouseProxy<'static>, cmd_rx: AsyncReceiver<Command>) -> MksResult<AbortHandle> {
    use Command::*;
    let fut = async move {
        while let Ok(mut cmd) = cmd_rx.recv().await {
            let mut pending_move = PendingMove::None;
            let mut rx_open = true;
            loop {
                match cmd {
                    SetAbsPosition { x, y } => {
                        if matches!(pending_move, PendingMove::Rel { .. }) {
                            flush_pending_move(&proxy, &mut pending_move).await;
                        }
                        match &mut pending_move {
                            PendingMove::None => pending_move = PendingMove::Abs { x, y },
                            PendingMove::Abs { x: pending_x, y: pending_y } => {
                                *pending_x = x;
                                *pending_y = y;
                            }
                            PendingMove::Rel { .. } => unreachable!("Relative move must be flushed before abs"),
                        }
                    }
                    RelMotion { dx, dy } => {
                        if matches!(pending_move, PendingMove::Abs { .. }) {
                            flush_pending_move(&proxy, &mut pending_move).await;
                        }
                        match &mut pending_move {
                            PendingMove::None => pending_move = PendingMove::Rel { dx, dy },
                            PendingMove::Rel { dx: pending_dx, dy: pending_dy } => {
                                // Saturate at i32 bounds to avoid overflow under extreme backlog.
                                *pending_dx = pending_dx.saturating_add(dx);
                                *pending_dy = pending_dy.saturating_add(dy);
                            }
                            PendingMove::Abs { .. } => unreachable!("Absolute move must be flushed before relative"),
                        }
                    }
                    Press(btn) => {
                        flush_pending_move(&proxy, &mut pending_move).await;
                        if let Err(e) = proxy.press(btn).await {
                            mks_error!(error:? = e; "Mouse press failed");
                        }
                    }
                    Release(btn) => {
                        flush_pending_move(&proxy, &mut pending_move).await;
                        if let Err(e) = proxy.release(btn).await {
                            mks_error!(error:? = e; "Mouse release failed");
                        }
                    }
                }
                match cmd_rx.try_recv_realtime() {
                    Ok(Some(next_cmd)) => cmd = next_cmd,
                    Ok(None) => break,
                    Err(_) => {
                        rx_open = false;
                        break;
                    }
                }
            }
            flush_pending_move(&proxy, &mut pending_move).await;
            if !rx_open {
                break;
            }
        }
    };
    Ok(tokio::spawn(fut).abort_handle())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::Mutex, time::Duration};
    use zbus::interface;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum RecordedEvent {
        Press(u32),
        Release(u32),
        Abs(u32, u32),
        Rel(i32, i32),
    }

    /// Mock QEMU Mouse state
    struct MockState {
        is_absolute: bool,
        last_press: Option<u32>,
        last_release: Option<u32>,
        last_abs_position: Option<(u32, u32)>,
        last_rel_motion: Option<(i32, i32)>,
        press_count: u32,
        release_count: u32,
        set_abs_count: u32,
        rel_motion_count: u32,
        events: Vec<RecordedEvent>,
    }

    /// Mock QEMU Mouse service
    struct MockQemuMouse {
        state: std::sync::Arc<Mutex<MockState>>,
        notify: std::sync::Arc<tokio::sync::Notify>,
    }

    #[interface(name = "org.qemu.Display1.Mouse")]
    impl MockQemuMouse {
        // --- Properties ---
        #[zbus(property)]
        fn is_absolute(&self) -> bool { self.state.lock().unwrap().is_absolute }

        // --- Methods ---
        async fn press(&self, button: u32) -> zbus::fdo::Result<()> {
            let mut state = self.state.lock().unwrap();
            state.last_press = Some(button);
            state.press_count += 1;
            state.events.push(RecordedEvent::Press(button));
            self.notify.notify_waiters();
            Ok(())
        }

        async fn release(&self, button: u32) -> zbus::fdo::Result<()> {
            let mut state = self.state.lock().unwrap();
            state.last_release = Some(button);
            state.release_count += 1;
            state.events.push(RecordedEvent::Release(button));
            self.notify.notify_waiters();
            Ok(())
        }

        async fn set_abs_position(&self, x: u32, y: u32) -> zbus::fdo::Result<()> {
            let mut state = self.state.lock().unwrap();
            state.last_abs_position = Some((x, y));
            state.set_abs_count += 1;
            state.events.push(RecordedEvent::Abs(x, y));
            self.notify.notify_waiters();
            Ok(())
        }

        async fn rel_motion(&self, dx: i32, dy: i32) -> zbus::fdo::Result<()> {
            let mut state = self.state.lock().unwrap();
            state.last_rel_motion = Some((dx, dy));
            state.rel_motion_count += 1;
            state.events.push(RecordedEvent::Rel(dx, dy));
            self.notify.notify_waiters();
            Ok(())
        }
    }

    impl MockQemuMouse {
        fn new(state: std::sync::Arc<Mutex<MockState>>, notify: std::sync::Arc<tokio::sync::Notify>) -> Self {
            Self { state, notify }
        }
    }

    /// Set up test environment.
    async fn setup_env() -> (zbus::Connection, std::sync::Arc<Mutex<MockState>>, std::sync::Arc<tokio::sync::Notify>) {
        use zbus::Guid;

        let state = std::sync::Arc::new(Mutex::new(MockState {
            is_absolute: true,
            last_press: None,
            last_release: None,
            last_abs_position: None,
            last_rel_motion: None,
            press_count: 0,
            release_count: 0,
            set_abs_count: 0,
            rel_motion_count: 0,
            events: vec![],
        }));

        let notify = std::sync::Arc::new(tokio::sync::Notify::new());
        let mock = MockQemuMouse::new(state.clone(), notify.clone());

        let (sock1, sock2) = std::os::unix::net::UnixStream::pair().expect("Failed to create socket pair");

        tokio::spawn(async move {
            let guid = Guid::generate();
            let _server_conn = zbus::connection::Builder::unix_stream(sock1)
                .p2p()
                .server(guid)
                .expect("Failed to set server mode")
                .serve_at("/org/qemu/Display1/Mouse_0", mock)
                .expect("Failed to serve mock")
                .build()
                .await
                .expect("Failed to build server connection");
            std::future::pending::<()>().await;
        });

        let client_conn = zbus::connection::Builder::unix_stream(sock2)
            .p2p()
            .build()
            .await
            .expect("Failed to build client connection");

        (client_conn, state, notify)
    }

    async fn wait_for_event_count(state: &std::sync::Arc<Mutex<MockState>>, expected: usize) {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if state.lock().unwrap().events.len() >= expected {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("Timed out waiting for mouse events");
    }

    /// 连接与初始状态同步测试
    #[tokio::test]
    async fn test_initial_state_sync() {
        let (conn, _state, _notify) = setup_env().await;
        let session =
            MouseSession::connect(&conn, "/org/qemu/Display1/Mouse_0").await.expect("Failed to create session");

        // 验证：应当收到 is_absolute 的初始事件
        let event = session.rx.recv().await.expect("Should receive initial event");
        match event {
            Event::IsAbsolute(v) => {
                assert!(v, "Mouse should be absolute");
            }
        }
    }

    /// 鼠标按钮测试
    #[tokio::test]
    async fn test_button_press_and_release() {
        let (conn, state, notify) = setup_env().await;
        let session =
            MouseSession::connect(&conn, "/org/qemu/Display1/Mouse_0").await.expect("Failed to create session");

        // 消耗初始事件
        let _ = session.rx.recv().await;

        // 测试左键按下
        let notified = notify.notified();
        session.tx.press(Button::Left).expect("Failed to send press");
        notified.await;
        assert_eq!(state.lock().unwrap().last_press, Some(0), "Button::Left should serialize to 0");

        // 测试左键释放
        let notified = notify.notified();
        session.tx.release(Button::Left).expect("Failed to send release");
        notified.await;
        assert_eq!(state.lock().unwrap().last_release, Some(0));

        // 测试右键
        let notified = notify.notified();
        session.tx.press(Button::Right).expect("Failed to send press");
        notified.await;
        assert_eq!(state.lock().unwrap().last_press, Some(2), "Button::Right should serialize to 2");
    }

    /// 绝对位置测试
    #[tokio::test]
    async fn test_abs_position() {
        let (conn, state, notify) = setup_env().await;
        let session =
            MouseSession::connect(&conn, "/org/qemu/Display1/Mouse_0").await.expect("Failed to create session");

        // 消耗初始事件
        let _ = session.rx.recv().await;

        let notified = notify.notified();
        session.tx.try_set_abs_position(100, 200).expect("Failed to set position");
        notified.await;
        assert_eq!(state.lock().unwrap().last_abs_position, Some((100, 200)));
    }

    /// 相对运动测试
    #[tokio::test]
    async fn test_rel_motion() {
        let (conn, state, notify) = setup_env().await;
        let session =
            MouseSession::connect(&conn, "/org/qemu/Display1/Mouse_0").await.expect("Failed to create session");

        // 消耗初始事件
        let _ = session.rx.recv().await;

        let notified = notify.notified();
        session.tx.rel_motion(10, -5).expect("Failed to send motion");
        notified.await;
        assert_eq!(state.lock().unwrap().last_rel_motion, Some((10, -5)));
    }

    /// Realtime command path test.
    #[tokio::test]
    async fn test_mouse_realtime_path() {
        let (conn, state, _notify) = setup_env().await;
        let session =
            MouseSession::connect(&conn, "/org/qemu/Display1/Mouse_0").await.expect("Failed to create session");

        let _ = session.rx.recv().await;

        session.tx.rel_motion(10, 0).unwrap();
        session.tx.rel_motion(10, 0).unwrap();
        session.tx.rel_motion(10, 0).unwrap();
        session.tx.press(Button::Left).unwrap();
        session.tx.rel_motion(0, 50).unwrap();

        wait_for_event_count(&state, 3).await;

        let s = state.lock().unwrap();

        assert_eq!(s.press_count, 1, "Press should be executed exactly once");
        assert_eq!(s.last_press, Some(0), "Press event should not be lost (Button::Left = 0)");
        assert_eq!(s.last_rel_motion, Some((0, 50)));
        let press_idx =
            s.events.iter().position(|event| *event == RecordedEvent::Press(0)).expect("Press event should exist");
        assert!(
            s.events[..press_idx].iter().any(|event| matches!(event, RecordedEvent::Rel(_, _))),
            "Relative motion should be flushed before press barrier"
        );
        assert!(
            s.events[press_idx + 1..].contains(&RecordedEvent::Rel(0, 50)),
            "Relative motion after press should be delivered"
        );
    }

    #[tokio::test]
    async fn test_abs_backlog_coalesces_to_last() {
        let (conn, state, _notify) = setup_env().await;
        let (cmd_tx, cmd_rx) = kanal::bounded_async::<Command>(8192);

        assert!(cmd_tx.try_send(Command::SetAbsPosition { x: 10, y: 20 }).unwrap());
        assert!(cmd_tx.try_send(Command::SetAbsPosition { x: 30, y: 40 }).unwrap());
        assert!(cmd_tx.try_send(Command::SetAbsPosition { x: 50, y: 60 }).unwrap());
        drop(cmd_tx);

        let proxy = MouseProxy::new(&conn, "/org/qemu/Display1/Mouse_0").await.expect("Failed to build mouse proxy");
        let handler = handle_mouse_commands(proxy, cmd_rx).await.expect("Failed to spawn mouse command handler");

        wait_for_event_count(&state, 1).await;
        handler.abort();

        let s = state.lock().unwrap();
        assert_eq!(s.set_abs_count, 1, "Absolute backlog should coalesce to one update");
        assert_eq!(s.last_abs_position, Some((50, 60)));
        assert_eq!(s.events, vec![RecordedEvent::Abs(50, 60)]);
    }

    #[tokio::test]
    async fn test_rel_backlog_coalesces_by_sum() {
        let (conn, state, _notify) = setup_env().await;
        let (cmd_tx, cmd_rx) = kanal::bounded_async::<Command>(8192);

        assert!(cmd_tx.try_send(Command::RelMotion { dx: 10, dy: 5 }).unwrap());
        assert!(cmd_tx.try_send(Command::RelMotion { dx: -3, dy: 7 }).unwrap());
        assert!(cmd_tx.try_send(Command::RelMotion { dx: 8, dy: -9 }).unwrap());
        drop(cmd_tx);

        let proxy = MouseProxy::new(&conn, "/org/qemu/Display1/Mouse_0").await.expect("Failed to build mouse proxy");
        let handler = handle_mouse_commands(proxy, cmd_rx).await.expect("Failed to spawn mouse command handler");

        wait_for_event_count(&state, 1).await;
        handler.abort();

        let s = state.lock().unwrap();
        assert_eq!(s.rel_motion_count, 1, "Relative backlog should coalesce to one update");
        assert_eq!(s.last_rel_motion, Some((15, 3)));
        assert_eq!(s.events, vec![RecordedEvent::Rel(15, 3)]);
    }

    #[tokio::test]
    async fn test_button_is_barrier_for_move_flush() {
        let (conn, state, _notify) = setup_env().await;
        let (cmd_tx, cmd_rx) = kanal::bounded_async::<Command>(8192);

        assert!(cmd_tx.try_send(Command::RelMotion { dx: 10, dy: 0 }).unwrap());
        assert!(cmd_tx.try_send(Command::RelMotion { dx: 5, dy: 0 }).unwrap());
        assert!(cmd_tx.try_send(Command::Press(Button::Left)).unwrap());
        assert!(cmd_tx.try_send(Command::RelMotion { dx: 2, dy: 3 }).unwrap());
        assert!(cmd_tx.try_send(Command::Release(Button::Left)).unwrap());
        drop(cmd_tx);

        let proxy = MouseProxy::new(&conn, "/org/qemu/Display1/Mouse_0").await.expect("Failed to build mouse proxy");
        let handler = handle_mouse_commands(proxy, cmd_rx).await.expect("Failed to spawn mouse command handler");

        wait_for_event_count(&state, 4).await;
        handler.abort();

        let s = state.lock().unwrap();
        assert_eq!(
            s.events,
            vec![
                RecordedEvent::Rel(15, 0),
                RecordedEvent::Press(0),
                RecordedEvent::Rel(2, 3),
                RecordedEvent::Release(0)
            ]
        );
    }

    #[tokio::test]
    async fn test_mixed_abs_rel_switch_flushes() {
        let (conn, state, _notify) = setup_env().await;
        let (cmd_tx, cmd_rx) = kanal::bounded_async::<Command>(8192);

        assert!(cmd_tx.try_send(Command::SetAbsPosition { x: 1, y: 1 }).unwrap());
        assert!(cmd_tx.try_send(Command::SetAbsPosition { x: 2, y: 2 }).unwrap());
        assert!(cmd_tx.try_send(Command::RelMotion { dx: 3, dy: 4 }).unwrap());
        assert!(cmd_tx.try_send(Command::RelMotion { dx: 1, dy: -2 }).unwrap());
        assert!(cmd_tx.try_send(Command::SetAbsPosition { x: 50, y: 60 }).unwrap());
        drop(cmd_tx);

        let proxy = MouseProxy::new(&conn, "/org/qemu/Display1/Mouse_0").await.expect("Failed to build mouse proxy");
        let handler = handle_mouse_commands(proxy, cmd_rx).await.expect("Failed to spawn mouse command handler");

        wait_for_event_count(&state, 3).await;
        handler.abort();

        let s = state.lock().unwrap();
        assert_eq!(s.events, vec![RecordedEvent::Abs(2, 2), RecordedEvent::Rel(4, 2), RecordedEvent::Abs(50, 60)]);
    }

    /// Button 枚举测试
    #[test]
    fn test_button_enum() {
        assert_eq!(Button::Left as u32, 0);
        assert_eq!(Button::Middle as u32, 1);
        assert_eq!(Button::Right as u32, 2);
        assert_eq!(Button::WheelUp as u32, 3);
        assert_eq!(Button::WheelDown as u32, 4);
        assert_eq!(Button::Side as u32, 5);
        assert_eq!(Button::Extra as u32, 6);
    }
}
