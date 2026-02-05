//! `DBus` proxy for QEMU MultiTouch interface.
//! <https://www.qemu.org/docs/master/interop/dbus-display.html#org.qemu.Display1.MultiTouch-section>
use crate::{generate_handler, generate_watcher, impl_controller, impl_session_connect};
use derive_more::{AsRef, Deref, From};
use kanal::AsyncSender;
use num_enum::TryFromPrimitive;
use serde_repr::Serialize_repr;
use zbus::{Result, proxy};
use zvariant::{Signature, Type};

#[proxy(interface = "org.qemu.Display1.MultiTouch", default_service = "org.qemu", gen_blocking = false)]
pub trait MultiTouch {
    /// Send a touch gesture event.
    async fn send_event(&self, kind: Kind, num_slot: u64, x: f64, y: f64) -> Result<()>;

    /// The maximum number of slots.
    #[zbus(property)]
    fn max_slots(&self) -> Result<i32>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize_repr, TryFromPrimitive)]
#[repr(u32)]
pub enum Kind {
    Begin = 0,
    Update = 1,
    End = 2,
    Cancel = 3,
}

impl Type for Kind {
    const SIGNATURE: &Signature = u32::SIGNATURE;
}

#[derive(Debug)]
pub enum Command {
    SendEvent { kind: Kind, num_slot: u64, x: f64, y: f64 },
}

#[derive(Debug, Clone)]
pub enum Event {
    MaxSlots(i32),
}

#[derive(Clone, AsRef, Deref, From)]
pub struct MultiTouchController(AsyncSender<Command>);

impl_controller!(
    MultiTouchController,
    Command,
    {
        fn send_event(kind: Kind, num_slot: u64, x: f64, y: f64) => SendEvent { kind, num_slot, x, y };
    }
);

impl MultiTouchController {
    /// Convenience method for touch begin
    pub async fn begin(&self, num_slot: u64, x: f64, y: f64) -> crate::MksResult {
        self.send_event(Kind::Begin, num_slot, x, y).await
    }

    /// Convenience method for touch update
    pub async fn update(&self, num_slot: u64, x: f64, y: f64) -> crate::MksResult {
        self.send_event(Kind::Update, num_slot, x, y).await
    }

    /// Convenience method for touch end
    pub async fn end(&self, num_slot: u64, x: f64, y: f64) -> crate::MksResult {
        self.send_event(Kind::End, num_slot, x, y).await
    }

    /// Convenience method for touch cancel
    pub async fn cancel(&self, num_slot: u64, x: f64, y: f64) -> crate::MksResult {
        self.send_event(Kind::Cancel, num_slot, x, y).await
    }
}

impl_session_connect!(MultiTouchSession, MultiTouchProxy<'static>, MultiTouchController, Command, Event);

generate_watcher!(
    watch_proxy_changes,
    MultiTouchProxy<'static>,
    Event,
    "multitouch",
    {
        max_slots => receive_max_slots_changed => Event::MaxSlots,
    }
);

generate_handler!(
    handle_commands,
    MultiTouchProxy<'static>,
    Command,
    "multitouch",
    |p| {
        Command::SendEvent { kind, num_slot, x, y } => p.send_event(kind, num_slot, x, y).await,
    }
);

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use zbus::interface;

    /// Mock QEMU MultiTouch 状态
    struct MockState {
        max_slots: i32,
        last_event: Option<(Kind, u64, f64, f64)>,
    }

    /// Mock QEMU MultiTouch 服务
    struct MockQemuMultiTouch {
        state: std::sync::Arc<Mutex<MockState>>,
    }

    #[interface(name = "org.qemu.Display1.MultiTouch")]
    impl MockQemuMultiTouch {
        // --- Properties ---
        #[zbus(property)]
        fn max_slots(&self) -> i32 { self.state.lock().unwrap().max_slots }

        // --- Methods ---
        async fn send_event(&self, kind: u32, num_slot: u64, x: f64, y: f64) -> zbus::fdo::Result<()> {
            let kind = Kind::try_from_primitive(kind)
                .map_err(|_| zbus::fdo::Error::InvalidArgs("Invalid touch kind".into()))?;
            self.state.lock().unwrap().last_event = Some((kind, num_slot, x, y));
            Ok(())
        }
    }

    impl MockQemuMultiTouch {
        fn new(state: std::sync::Arc<Mutex<MockState>>) -> Self { Self { state } }
    }

    /// 搭建测试环境
    async fn setup_env() -> (zbus::Connection, std::sync::Arc<Mutex<MockState>>) {
        use zbus::Guid;

        let state = std::sync::Arc::new(Mutex::new(MockState { max_slots: 10, last_event: None }));

        let mock = MockQemuMultiTouch::new(state.clone());

        let (sock1, sock2) = std::os::unix::net::UnixStream::pair().expect("Failed to create socket pair");

        tokio::spawn(async move {
            let guid = Guid::generate();
            let _server_conn = zbus::connection::Builder::unix_stream(sock1)
                .p2p()
                .server(guid)
                .expect("Failed to set server mode")
                .serve_at("/org/qemu/Display1/MultiTouch_0", mock)
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
            connect(&conn, "/org/qemu/Display1/MultiTouch_0".to_string()).await.expect("Failed to create session");

        // 验证：应当收到 max_slots 的初始事件
        let event = session.rx.recv().await.expect("Should receive initial event");
        match event {
            Event::MaxSlots(v) => {
                assert_eq!(v, 10, "Max slots should be 10");
            }
        }
    }

    /// 触摸事件测试
    #[tokio::test]
    async fn test_touch_events() {
        let (conn, state) = setup_env().await;
        let session =
            connect(&conn, "/org/qemu/Display1/MultiTouch_0".to_string()).await.expect("Failed to create session");

        // 消耗初始事件
        let _ = session.rx.recv().await;

        // 测试 begin
        session.tx.begin(0, 100.0, 200.0).await.expect("Failed to send begin");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        {
            let s = state.lock().unwrap();
            assert_eq!(s.last_event, Some((Kind::Begin, 0, 100.0, 200.0)));
        }

        // 测试 update
        session.tx.update(0, 110.0, 210.0).await.expect("Failed to send update");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        {
            let s = state.lock().unwrap();
            assert_eq!(s.last_event, Some((Kind::Update, 0, 110.0, 210.0)));
        }

        // 测试 end
        session.tx.end(0, 110.0, 210.0).await.expect("Failed to send end");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        {
            let s = state.lock().unwrap();
            assert_eq!(s.last_event, Some((Kind::End, 0, 110.0, 210.0)));
        }
    }

    /// 触摸取消测试
    #[tokio::test]
    async fn test_touch_cancel() {
        let (conn, state) = setup_env().await;
        let session =
            connect(&conn, "/org/qemu/Display1/MultiTouch_0".to_string()).await.expect("Failed to create session");

        // 消耗初始事件
        let _ = session.rx.recv().await;

        // 测试 cancel
        session.tx.cancel(1, 50.0, 75.0).await.expect("Failed to send cancel");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        {
            let s = state.lock().unwrap();
            assert_eq!(s.last_event, Some((Kind::Cancel, 1, 50.0, 75.0)));
        }
    }

    /// 多点触摸测试
    #[tokio::test]
    async fn test_multi_slot() {
        let (conn, state) = setup_env().await;
        let session =
            connect(&conn, "/org/qemu/Display1/MultiTouch_0".to_string()).await.expect("Failed to create session");

        // 消耗初始事件
        let _ = session.rx.recv().await;

        // 同时触摸两个点
        session.tx.begin(0, 100.0, 100.0).await.expect("Failed to send slot 0 begin");
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;

        session.tx.begin(1, 300.0, 300.0).await.expect("Failed to send slot 1 begin");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        {
            let s = state.lock().unwrap();
            // 最后一个事件应该是 slot 1 的
            assert_eq!(s.last_event, Some((Kind::Begin, 1, 300.0, 300.0)));
        }

        // 更新 slot 0
        session.tx.update(0, 110.0, 110.0).await.expect("Failed to send slot 0 update");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        {
            let s = state.lock().unwrap();
            assert_eq!(s.last_event, Some((Kind::Update, 0, 110.0, 110.0)));
        }
    }

    /// Kind 枚举测试
    #[test]
    fn test_kind_enum() {
        assert_eq!(Kind::Begin as u32, 0);
        assert_eq!(Kind::Update as u32, 1);
        assert_eq!(Kind::End as u32, 2);
        assert_eq!(Kind::Cancel as u32, 3);

        // Test TryFromPrimitive
        assert_eq!(Kind::try_from_primitive(0).unwrap(), Kind::Begin);
        assert_eq!(Kind::try_from_primitive(1).unwrap(), Kind::Update);
        assert_eq!(Kind::try_from_primitive(2).unwrap(), Kind::End);
        assert_eq!(Kind::try_from_primitive(3).unwrap(), Kind::Cancel);
        assert!(Kind::try_from_primitive(4).is_err());
    }
}
