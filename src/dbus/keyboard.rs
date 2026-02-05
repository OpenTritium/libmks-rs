//! `DBus` proxy for QEMU Keyboard interface.
//! <https://www.qemu.org/docs/master/interop/dbus-display.html#org.qemu.Display1.Keyboard-section>
use crate::{generate_handler, generate_watcher, impl_controller, impl_session_connect};
use bitflags::bitflags;
use derive_more::{AsMut, AsRef, Deref, DerefMut, From};
use kanal::AsyncSender;
use zbus::{Result, proxy};
use zvariant::OwnedValue;

#[proxy(interface = "org.qemu.Display1.Keyboard", default_service = "org.qemu", gen_blocking = false)]
pub trait Keyboard {
    /// **发送按键按下事件 (Press)**
    ///
    /// * `keycode`: 扫描码 (Scan Code)。 这是 QEMU 规定的按键数字编号（通常基于 QKeyCode 或 Linux Input Event codes）。
    ///   比如：Q_KEY_CODE_A, Q_KEY_CODE_CTRL 等。
    ///
    /// 当你在 UI 上按下一个键时，调用此方法告诉虚拟机：“嘿，用户按下了 A 键”。
    async fn press(&self, keycode: u32) -> Result<()>;

    /// **发送按键释放事件 (Release)**
    ///
    /// * `keycode`: 同上。
    ///
    /// 当你在 UI 上松开一个键时，调用此方法。
    /// 注意：必须成对调用 press 和 release，否则虚拟机里那个键会一直卡住（连击）。
    async fn release(&self, keycode: u32) -> Result<()>;

    /// **获取当前的修饰键状态 (Modifiers)**
    ///
    /// * 返回值: `Result<Modifiers>` (通常是 u32)。
    ///
    /// 这个属性告诉你：**虚拟机当前认为哪些修饰键（Ctrl/Alt/Shift）是处于“按下”状态的。**
    ///
    /// # 它是干啥的？
    /// 1. **状态同步**：当 UI 刚连接上来时，需要知道虚拟机里是不是已经按着 Ctrl 了。
    /// 2. **组合键逻辑**：虽然通常由虚拟机处理组合键，但在某些高级场景下， UI 可能需要知道当前状态来改变鼠标行为（比如
    ///    Ctrl+Click 多选）。
    #[zbus(property)]
    fn modifiers(&self) -> Result<LockState>;
}

bitflags! {
    /// 没错这玩意儿只用来表示 LED 状态
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
    pub struct LockState: u32 {
        const SCROLL = 1 << 0;
        const NUM    = 1 << 1;
        const CAPS   = 1 << 2;
    }
}

impl TryFrom<OwnedValue> for LockState {
    type Error = zvariant::Error;

    fn try_from(value: OwnedValue) -> std::result::Result<Self, Self::Error> {
        let val: u32 = value.try_into()?;
        Ok(LockState::from_bits_retain(val))
    }
}

#[derive(Debug)]
pub enum Command {
    Press(u32),
    Release(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, AsRef, AsMut, DerefMut, Deref, From)]
pub struct Event(pub LockState);

#[derive(AsRef, Deref, From)]
#[derive(Clone)]
pub struct KeyboardController(AsyncSender<Command>);

impl KeyboardController {
    pub async fn press(&self, keycode: u32) -> crate::MksResult {
        self.0.send(Command::Press(keycode)).await?;
        Ok(())
    }

    pub async fn release(&self, keycode: u32) -> crate::MksResult {
        self.0.send(Command::Release(keycode)).await?;
        Ok(())
    }
}

impl_session_connect!(KeyboardSession, KeyboardProxy<'static>, KeyboardController, Command, Event);

generate_watcher!(
    watch_proxy_changes,
    KeyboardProxy<'static>,
    Event,
    "keyboard",
    {
        modifiers => receive_modifiers_changed => Event
    }
);

generate_handler!(
    handle_commands,
    KeyboardProxy<'static>,
    Command,
    "keyboard",
    |p| {
        Command::Press(keycode) => p.press(keycode).await,
        Command::Release(keycode) => p.release(keycode).await,
    }
);

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use zbus::interface;

    /// Mock QEMU Keyboard 状态
    struct MockState {
        modifiers: LockState,
        last_press: Option<u32>,
        last_release: Option<u32>,
    }

    /// Mock QEMU Keyboard 服务
    struct MockQemuKeyboard {
        state: std::sync::Arc<Mutex<MockState>>,
    }

    #[interface(name = "org.qemu.Display1.Keyboard")]
    impl MockQemuKeyboard {
        // --- Properties ---
        #[zbus(property)]
        fn modifiers(&self) -> u32 { self.state.lock().unwrap().modifiers.bits() }

        // --- Methods ---
        async fn press(&self, keycode: u32) -> zbus::fdo::Result<()> {
            self.state.lock().unwrap().last_press = Some(keycode);
            Ok(())
        }

        async fn release(&self, keycode: u32) -> zbus::fdo::Result<()> {
            self.state.lock().unwrap().last_release = Some(keycode);
            Ok(())
        }
    }

    impl MockQemuKeyboard {
        fn new(state: std::sync::Arc<Mutex<MockState>>) -> Self { Self { state } }
    }

    /// 搭建测试环境
    async fn setup_env() -> (zbus::Connection, std::sync::Arc<Mutex<MockState>>) {
        use zbus::Guid;

        let state = std::sync::Arc::new(Mutex::new(MockState {
            modifiers: LockState::NUM | LockState::CAPS,
            last_press: None,
            last_release: None,
        }));

        let mock = MockQemuKeyboard::new(state.clone());

        let (sock1, sock2) = std::os::unix::net::UnixStream::pair().expect("Failed to create socket pair");

        tokio::spawn(async move {
            let guid = Guid::generate();
            let _server_conn = zbus::connection::Builder::unix_stream(sock1)
                .p2p()
                .server(guid)
                .expect("Failed to set server mode")
                .serve_at("/org/qemu/Display1/Keyboard_0", mock)
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
        let session =
            connect(&conn, "/org/qemu/Display1/Keyboard_0".to_string()).await.expect("Failed to create session");

        // 验证：应当收到 modifiers 的初始事件
        let event = session.rx.recv().await.expect("Should receive initial event");
        assert!(event.contains(LockState::NUM), "Should have NUM modifier");
        assert!(event.contains(LockState::CAPS), "Should have CAPS modifier");
        assert!(!event.contains(LockState::SCROLL), "Should not have SCROLL modifier");
    }

    /// 按键测试
    #[tokio::test]
    async fn test_press_and_release() {
        let (conn, state) = setup_env().await;
        let session =
            connect(&conn, "/org/qemu/Display1/Keyboard_0".to_string()).await.expect("Failed to create session");

        // 消耗初始事件
        let _ = session.rx.recv().await;

        // 测试按键
        session.tx.press(0x1e).await.expect("Failed to send press");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert_eq!(state.lock().unwrap().last_press, Some(0x1e));

        // 测试释放
        session.tx.release(0x1e).await.expect("Failed to send release");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert_eq!(state.lock().unwrap().last_release, Some(0x1e));
    }

    /// Modifiers 解析单元测试
    #[test]
    fn test_modifiers_parsing() {
        // Test individual flags
        let scroll_only = LockState::from_bits_retain(0b001);
        assert!(scroll_only.contains(LockState::SCROLL));
        assert!(!scroll_only.contains(LockState::NUM));
        assert!(!scroll_only.contains(LockState::CAPS));

        let num_only = LockState::from_bits_retain(0b010);
        assert!(!num_only.contains(LockState::SCROLL));
        assert!(num_only.contains(LockState::NUM));
        assert!(!num_only.contains(LockState::CAPS));

        let caps_only = LockState::from_bits_retain(0b100);
        assert!(!caps_only.contains(LockState::SCROLL));
        assert!(!caps_only.contains(LockState::NUM));
        assert!(caps_only.contains(LockState::CAPS));

        // Test combined flags
        let all = LockState::SCROLL | LockState::NUM | LockState::CAPS;
        assert_eq!(all.bits(), 0b111);
        assert!(all.contains(LockState::SCROLL));
        assert!(all.contains(LockState::NUM));
        assert!(all.contains(LockState::CAPS));

        // Test empty
        let empty = LockState::empty();
        assert!(!empty.contains(LockState::SCROLL));
        assert!(!empty.contains(LockState::NUM));
        assert!(!empty.contains(LockState::CAPS));
    }

    /// 从 OwnedValue 转换测试
    #[test]
    fn test_modifiers_from_owned_value() {
        let val = zvariant::Value::new(0b101u32); // SCROLL | CAPS
        let owned = OwnedValue::try_from(val).unwrap();
        let modifiers = LockState::try_from(owned).unwrap();
        assert!(modifiers.contains(LockState::SCROLL));
        assert!(!modifiers.contains(LockState::NUM));
        assert!(modifiers.contains(LockState::CAPS));
    }
}
