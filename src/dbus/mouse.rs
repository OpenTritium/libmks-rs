//! `DBus` proxy for QEMU Mouse interface.
//! <https://www.qemu.org/docs/master/interop/dbus-display.html#org.qemu.Display1.Mouse-section>
use crate::{generate_handler, generate_watcher, impl_session_connect};
use derive_more::{AsRef, Deref, From};
use kanal::AsyncSender;
use num_enum::TryFromPrimitive;
use serde_repr::Serialize_repr;
use zbus::{Result, proxy};
use zvariant::{Signature, Type};

#[proxy(interface = "org.qemu.Display1.Mouse", default_service = "org.qemu", gen_blocking = false)]
pub trait Mouse {
    /// **发送鼠标按键按下事件**
    ///
    /// * `button`: 目标按键（如左键、中键、右键）。
    ///
    /// 注意：鼠标点击通常需要 UI 层确保 `press` 与 `release` 成对触发，
    /// 否则虚拟机会由于按键"粘连"导致无法进行后续操作。
    async fn press(&self, button: Button) -> Result<()>;

    /// **发送鼠标按键释放事件**
    ///
    /// * `button`: 目标按键。
    async fn release(&self, button: Button) -> Result<()>;

    /// **设置绝对位置（Tablet 模式）**
    ///
    /// * `x`, `y`: 坐标值，通常映射到虚拟机显示分辨率的范围。
    ///
    /// **限制条件**：仅当属性 `is_absolute` 为 `true` 时有效。
    /// 如果在相对模式（普通鼠标模式）下调用，QEMU 服务端通常会返回错误。
    ///
    /// **用途**：适用于触屏、数位板或开启了虚拟 USB Tablet 设备的场景，
    /// 这种模式下鼠标指针在本地和虚拟机内能完美同步，不存在"漂移"问题。
    async fn set_abs_position(&self, x: u32, y: u32) -> Result<()>;

    /// **发送相对位移（Mouse 模式）**
    ///
    /// * `dx`, `dy`: 像素或步进单位的偏移量。
    ///
    /// **限制条件**：仅当属性 `is_absolute` 为 `false` 时有效。
    ///
    /// **警告**：在相对模式下，由于宿主机和虚拟机的鼠标加速度算法可能不同，
    /// 频繁调用可能导致鼠标指针"越走越偏"（即本地指针和虚拟机指针不重合）。
    async fn rel_motion(&self, dx: i32, dy: i32) -> Result<()>;

    /// **指示当前鼠标的工作模式**
    ///
    /// * `true`: **绝对定位模式**。你应该使用 `set_abs_position`，通常对应 USB Tablet。
    /// * `false`: **相对移动模式**。你应该使用 `rel_motion`，通常对应标准 PS/2 鼠标。
    ///
    /// **UI 逻辑建议**：在监听到此属性变化时，UI 层应切换底层输入捕获逻辑：
    /// 绝对模式下直接上报坐标，相对模式下则需要锁定鼠标（Pointer Lock）并计算 `delta`。
    #[zbus(property)]
    fn is_absolute(&self) -> Result<bool>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize_repr, TryFromPrimitive)]
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

impl Type for Button {
    const SIGNATURE: &Signature = u32::SIGNATURE;
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
pub struct MouseController(AsyncSender<Command>);

impl MouseController {
    pub async fn press(&self, button: Button) -> crate::MksResult {
        self.0.send(Command::Press(button)).await?;
        Ok(())
    }

    pub async fn release(&self, button: Button) -> crate::MksResult {
        self.0.send(Command::Release(button)).await?;
        Ok(())
    }

    pub async fn set_abs_position(&self, x: u32, y: u32) -> crate::MksResult {
        self.0.send(Command::SetAbsPosition { x, y }).await?;
        Ok(())
    }

    pub async fn rel_motion(&self, dx: i32, dy: i32) -> crate::MksResult {
        self.0.send(Command::RelMotion { dx, dy }).await?;
        Ok(())
    }
}

impl_session_connect!(MouseSession, MouseProxy<'static>, MouseController, Command, Event);

generate_watcher!(
    watch_proxy_changes,
    MouseProxy<'static>,
    Event,
    "mouse",
    {
        is_absolute => receive_is_absolute_changed => Event::IsAbsolute,
    }
);

generate_handler!(
    handle_commands,
    MouseProxy<'static>,
    Command,
    "mouse",
    |p| {
        Command::Press(button) => p.press(button).await,
        Command::Release(button) => p.release(button).await,
        Command::SetAbsPosition { x, y } => p.set_abs_position(x, y).await,
        Command::RelMotion { dx, dy } => p.rel_motion(dx, dy).await,
    }
);

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use zbus::interface;

    /// Mock QEMU Mouse 状态
    struct MockState {
        is_absolute: bool,
        last_press: Option<Button>,
        last_release: Option<Button>,
        last_abs_position: Option<(u32, u32)>,
        last_rel_motion: Option<(i32, i32)>,
    }

    /// Mock QEMU Mouse 服务
    struct MockQemuMouse {
        state: std::sync::Arc<Mutex<MockState>>,
    }

    #[interface(name = "org.qemu.Display1.Mouse")]
    impl MockQemuMouse {
        // --- Properties ---
        #[zbus(property)]
        fn is_absolute(&self) -> bool { self.state.lock().unwrap().is_absolute }

        // --- Methods ---
        async fn press(&self, button: u32) -> zbus::fdo::Result<()> {
            self.state.lock().unwrap().last_press = Some(Button::try_from_primitive(button).unwrap());
            Ok(())
        }

        async fn release(&self, button: u32) -> zbus::fdo::Result<()> {
            self.state.lock().unwrap().last_release = Some(Button::try_from_primitive(button).unwrap());
            Ok(())
        }

        async fn set_abs_position(&self, x: u32, y: u32) -> zbus::fdo::Result<()> {
            self.state.lock().unwrap().last_abs_position = Some((x, y));
            Ok(())
        }

        async fn rel_motion(&self, dx: i32, dy: i32) -> zbus::fdo::Result<()> {
            self.state.lock().unwrap().last_rel_motion = Some((dx, dy));
            Ok(())
        }
    }

    impl MockQemuMouse {
        fn new(state: std::sync::Arc<Mutex<MockState>>) -> Self { Self { state } }
    }

    /// 搭建测试环境
    async fn setup_env() -> (zbus::Connection, std::sync::Arc<Mutex<MockState>>) {
        use zbus::Guid;

        let state = std::sync::Arc::new(Mutex::new(MockState {
            is_absolute: true,
            last_press: None,
            last_release: None,
            last_abs_position: None,
            last_rel_motion: None,
        }));

        let mock = MockQemuMouse::new(state.clone());

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

        (client_conn, state)
    }

    /// 连接与初始状态同步测试
    #[tokio::test]
    async fn test_initial_state_sync() {
        let (conn, _state) = setup_env().await;
        let session = connect(&conn, "/org/qemu/Display1/Mouse_0".to_string()).await.expect("Failed to create session");

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
        let (conn, state) = setup_env().await;
        let session = connect(&conn, "/org/qemu/Display1/Mouse_0".to_string()).await.expect("Failed to create session");

        // 消耗初始事件
        let _ = session.rx.recv().await;

        // 测试左键按下
        session.tx.press(Button::Left).await.expect("Failed to send press");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert_eq!(state.lock().unwrap().last_press, Some(Button::Left));

        // 测试左键释放
        session.tx.release(Button::Left).await.expect("Failed to send release");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert_eq!(state.lock().unwrap().last_release, Some(Button::Left));

        // 测试右键
        session.tx.press(Button::Right).await.expect("Failed to send press");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert_eq!(state.lock().unwrap().last_press, Some(Button::Right));
    }

    /// 绝对位置测试
    #[tokio::test]
    async fn test_abs_position() {
        let (conn, state) = setup_env().await;
        let session = connect(&conn, "/org/qemu/Display1/Mouse_0".to_string()).await.expect("Failed to create session");

        // 消耗初始事件
        let _ = session.rx.recv().await;

        session.tx.set_abs_position(100, 200).await.expect("Failed to set position");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert_eq!(state.lock().unwrap().last_abs_position, Some((100, 200)));
    }

    /// 相对运动测试
    #[tokio::test]
    async fn test_rel_motion() {
        let (conn, state) = setup_env().await;
        let session = connect(&conn, "/org/qemu/Display1/Mouse_0".to_string()).await.expect("Failed to create session");

        // 消耗初始事件
        let _ = session.rx.recv().await;

        session.tx.rel_motion(10, -5).await.expect("Failed to send motion");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert_eq!(state.lock().unwrap().last_rel_motion, Some((10, -5)));
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

        // Test TryFromPrimitive
        assert_eq!(Button::try_from_primitive(0).unwrap(), Button::Left);
        assert_eq!(Button::try_from_primitive(2).unwrap(), Button::Right);
        assert!(Button::try_from_primitive(99).is_err());
    }
}
