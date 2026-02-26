//! `DBus` proxy for QEMU Keyboard interface.
//! <https://www.qemu.org/docs/master/interop/dbus-display.html#org.qemu.Display1.Keyboard>
use crate::{generate_handler, generate_watcher, impl_controller, impl_session_connect, keymaps::Qnum};
use bitflags::bitflags;
use derive_more::{AsRef, Deref, From};
use kanal::AsyncSender;
use zbus::{Result, proxy};

#[proxy(interface = "org.qemu.Display1.Keyboard", default_service = "org.qemu", gen_blocking = false)]
pub trait Keyboard {
    /// Sends a key press event.
    ///
    /// * `qnum` - The QEMU keycode (scancode).
    async fn press(&self, qnum: Qnum) -> Result<()>;

    /// Sends a key release event.
    ///
    /// * `qnum` - The QEMU keycode (scancode).
    async fn release(&self, qnum: Qnum) -> Result<()>;

    /// The current keyboard modifier state (NumLock, CapsLock, ScrollLock).
    #[zbus(property)]
    fn modifiers(&self) -> Result<LockState>;
}

#[derive(Debug)]
pub enum Command {
    Press(Qnum),
    Release(Qnum),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deref, AsRef)]
pub struct Event(pub LockState);

#[derive(AsRef, Deref, From, Clone)]
pub struct KeyboardController(pub AsyncSender<Command>);

impl_controller!(KeyboardController, Command, {
    pub fn press(keycode: Qnum) => Press(keycode);
    pub fn release(keycode: Qnum) => Release(keycode);
});

bitflags! {
    /// Represents the state of keyboard LEDs/modifiers.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
    pub struct LockState: u32 {
        const SCROLL = 1 << 0;
        const NUM    = 1 << 1;
        const CAPS   = 1 << 2;
    }
}

impl TryFrom<zvariant::OwnedValue> for LockState {
    type Error = zvariant::Error;

    #[inline]
    fn try_from(value: zvariant::OwnedValue) -> zvariant::Result<Self> {
        let val: u32 = value.try_into()?;
        Ok(LockState::from_bits_retain(val))
    }
}

impl_session_connect!(
    KeyboardSession,
    KeyboardProxy<'static>,
    KeyboardController,
    Command,
    Event,
    watch_proxy_changes,
    handle_commands,
    256
);

generate_watcher!(
    watch_proxy_changes,
    KeyboardProxy<'static>,
    Event,
    "keyboard",
    {
        modifiers => receive_modifiers_changed => Event,
    }
);

generate_handler!(
    handle_commands,
    KeyboardProxy<'static>,
    Command,
    "keyboard",
    |p| {
        Command::Press(qnum) => p.press(qnum).await,
        Command::Release(qnum) => p.release(qnum).await,
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
        notify: std::sync::Arc<tokio::sync::Notify>,
    }

    #[interface(name = "org.qemu.Display1.Keyboard")]
    impl MockQemuKeyboard {
        // --- Properties ---
        #[zbus(property)]
        fn modifiers(&self) -> u32 { self.state.lock().unwrap().modifiers.bits() }

        // --- Methods ---
        async fn press(&self, keycode: u32) -> zbus::fdo::Result<()> {
            self.state.lock().unwrap().last_press = Some(keycode);
            self.notify.notify_waiters();
            Ok(())
        }

        async fn release(&self, keycode: u32) -> zbus::fdo::Result<()> {
            self.state.lock().unwrap().last_release = Some(keycode);
            self.notify.notify_waiters();
            Ok(())
        }
    }

    impl MockQemuKeyboard {
        fn new(state: std::sync::Arc<Mutex<MockState>>, notify: std::sync::Arc<tokio::sync::Notify>) -> Self {
            Self { state, notify }
        }
    }

    /// 搭建测试环境
    async fn setup_env() -> (zbus::Connection, std::sync::Arc<Mutex<MockState>>, std::sync::Arc<tokio::sync::Notify>) {
        use zbus::Guid;

        let state = std::sync::Arc::new(Mutex::new(MockState {
            modifiers: LockState::NUM | LockState::CAPS,
            last_press: None,
            last_release: None,
        }));

        let notify = std::sync::Arc::new(tokio::sync::Notify::new());
        let mock = MockQemuKeyboard::new(state.clone(), notify.clone());

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

        (client_conn, state, notify)
    }

    /// 连接与初始状态同步测试
    #[tokio::test]
    async fn test_initial_state_sync() {
        let (conn, _state, _notify) = setup_env().await;
        let session =
            KeyboardSession::connect(&conn, "/org/qemu/Display1/Keyboard_0").await.expect("Failed to create session");

        // 验证：应当收到 modifiers 的初始事件
        let event = session.rx.recv().await.expect("Should receive initial event");
        assert!(event.contains(LockState::NUM), "Should have NUM modifier");
        assert!(event.contains(LockState::CAPS), "Should have CAPS modifier");
        assert!(!event.contains(LockState::SCROLL), "Should not have SCROLL modifier");
    }

    /// 按键测试
    #[tokio::test]
    async fn test_press_and_release() {
        let (conn, state, notify) = setup_env().await;
        let session =
            KeyboardSession::connect(&conn, "/org/qemu/Display1/Keyboard_0").await.expect("Failed to create session");

        // 消耗初始事件
        let _ = session.rx.recv().await;

        // 测试按键
        let notified = notify.notified();
        session.tx.press(Qnum::from(0x1e)).unwrap();
        notified.await;
        assert_eq!(state.lock().unwrap().last_press, Some(0x1e));

        // 测试释放
        let notified = notify.notified();
        session.tx.release(Qnum::from(0x1e)).unwrap();
        notified.await;
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
        let owned = zvariant::OwnedValue::try_from(val).unwrap();
        let modifiers = LockState::try_from(owned).unwrap();
        assert!(modifiers.contains(LockState::SCROLL));
        assert!(!modifiers.contains(LockState::NUM));
        assert!(modifiers.contains(LockState::CAPS));
    }
}
