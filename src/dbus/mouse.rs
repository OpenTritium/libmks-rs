//! `DBus` proxy for QEMU Mouse interface.
//! <https://www.qemu.org/docs/master/interop/dbus-display.html#org.qemu.Display1.Mouse-section>
use crate::{MksResult, error::MksError, generate_watcher, impl_controller, impl_session_connect};
use derive_more::{AsRef, Deref, From};
use kanal::{AsyncReceiver, AsyncSender};
use log::{error, info, warn};
use serde_repr::Serialize_repr;
use std::hint::spin_loop;
use tokio::task::AbortHandle;
use zbus::{Result, proxy};
use zvariant::Type;

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

#[inline]
fn promote_current_thread_mouse_priority() {
    use rustix::{process::setpriority_process, thread::gettid};
    let tid = gettid();
    match setpriority_process(Some(tid), -20) {
        Ok(()) => info!("Mouse worker priority raised to highest (nice=-20)"),
        Err(e) => warn!(error:? = e; "Failed to raise mouse worker priority (needs CAP_SYS_NICE/root)"),
    }
}

async fn handle_mouse_commands(proxy: MouseProxy<'static>, cmd_rx: AsyncReceiver<Command>) -> MksResult<AbortHandle> {
    use Command::*;

    let fut = async move {
        promote_current_thread_mouse_priority();
        while let Ok(cmd) = cmd_rx.recv().await {
            match cmd {
                SetAbsPosition { x, y } => {
                    if let Err(e) = proxy.set_abs_position(x, y).await {
                        error!(error:? = e; "Mouse set_abs_position failed");
                    }
                }
                RelMotion { dx, dy } => {
                    // Virtio/modern HID path: forward the full relative delta directly.
                    if let Err(e) = proxy.rel_motion(dx, dy).await {
                        error!(error:? = e; "Mouse rel_motion failed");
                    }
                }
                Press(btn) => {
                    if let Err(e) = proxy.press(btn).await {
                        error!(error:? = e; "Mouse press failed");
                    }
                }
                Release(btn) => {
                    if let Err(e) = proxy.release(btn).await {
                        error!(error:? = e; "Mouse release failed");
                    }
                }
            }
        }
    };
    Ok(tokio::spawn(fut).abort_handle())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use zbus::interface;

    /// Mock QEMU Mouse 状态
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
    }

    /// Mock QEMU Mouse 服务
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
            self.notify.notify_waiters();
            Ok(())
        }

        async fn release(&self, button: u32) -> zbus::fdo::Result<()> {
            let mut state = self.state.lock().unwrap();
            state.last_release = Some(button);
            state.release_count += 1;
            self.notify.notify_waiters();
            Ok(())
        }

        async fn set_abs_position(&self, x: u32, y: u32) -> zbus::fdo::Result<()> {
            let mut state = self.state.lock().unwrap();
            state.last_abs_position = Some((x, y));
            state.set_abs_count += 1;
            self.notify.notify_waiters();
            Ok(())
        }

        async fn rel_motion(&self, dx: i32, dy: i32) -> zbus::fdo::Result<()> {
            let mut state = self.state.lock().unwrap();
            state.last_rel_motion = Some((dx, dy));
            state.rel_motion_count += 1;
            self.notify.notify_waiters();
            Ok(())
        }
    }

    impl MockQemuMouse {
        fn new(state: std::sync::Arc<Mutex<MockState>>, notify: std::sync::Arc<tokio::sync::Notify>) -> Self {
            Self { state, notify }
        }
    }

    /// 搭建测试环境
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

    /// 实时路径测试
    #[tokio::test]
    async fn test_mouse_realtime_path() {
        let (conn, state, notify) = setup_env().await;
        let session =
            MouseSession::connect(&conn, "/org/qemu/Display1/Mouse_0").await.expect("Failed to create session");

        let _ = session.rx.recv().await;

        session.tx.rel_motion(10, 0).unwrap();
        session.tx.rel_motion(10, 0).unwrap();
        session.tx.rel_motion(10, 0).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let notified = notify.notified();
        session.tx.press(Button::Left).unwrap();
        notified.await;

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let notified = notify.notified();
        session.tx.rel_motion(0, 50).unwrap();
        notified.await;

        let s = state.lock().unwrap();

        assert_eq!(s.press_count, 1, "Press should be executed exactly once");
        assert_eq!(s.last_press, Some(0), "Press event should not be lost (Button::Left = 0)");
        assert_eq!(s.last_rel_motion, Some((0, 50)));
        assert!(
            s.rel_motion_count >= 3,
            "Realtime path should avoid aggressive debouncing: got {}",
            s.rel_motion_count
        );
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
