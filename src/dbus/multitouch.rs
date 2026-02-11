//! `DBus` proxy for QEMU MultiTouch interface.
//! <https://www.qemu.org/docs/master/interop/dbus-display.html#org.qemu.Display1.MultiTouch-section>
use crate::{MksResult, generate_handler, generate_watcher, impl_controller, impl_session_connect};
use derive_more::{AsRef, Deref, From};
use kanal::AsyncSender;
use serde_repr::Serialize_repr;
use zbus::{Result, proxy};
use zvariant::Type;

#[proxy(interface = "org.qemu.Display1.MultiTouch", default_service = "org.qemu", gen_blocking = false)]
pub trait MultiTouch {
    /// Send a touch gesture event.
    async fn send_event(&self, kind: Kind, num_slot: u64, x: f64, y: f64) -> Result<()>;

    /// The maximum number of slots.
    #[zbus(property)]
    fn max_slots(&self) -> Result<i32>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize_repr, Type)]
#[repr(u32)]
pub enum Kind {
    Begin = 0,
    Update = 1,
    End = 2,
    Cancel = 3,
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
pub struct MultiTouchController(pub AsyncSender<Command>);

impl_controller!(
    MultiTouchController,
    Command,
    {
        async fn send_event(kind: Kind, num_slot: u64, x: f64, y: f64) => SendEvent { kind, num_slot, x, y };
    }
);

impl MultiTouchController {
    /// Convenience method for touch begin
    pub async fn begin(&self, num_slot: u64, x: f64, y: f64) -> MksResult {
        self.send_event(Kind::Begin, num_slot, x, y).await
    }

    /// Convenience method for touch update
    pub async fn update(&self, num_slot: u64, x: f64, y: f64) -> MksResult {
        self.send_event(Kind::Update, num_slot, x, y).await
    }

    /// Convenience method for touch end
    pub async fn end(&self, num_slot: u64, x: f64, y: f64) -> MksResult {
        self.send_event(Kind::End, num_slot, x, y).await
    }

    /// Convenience method for touch cancel
    pub async fn cancel(&self, num_slot: u64, x: f64, y: f64) -> MksResult {
        self.send_event(Kind::Cancel, num_slot, x, y).await
    }
}

impl_session_connect!(
    MultiTouchSession,
    MultiTouchProxy<'static>,
    MultiTouchController,
    Command,
    Event,
    watch_proxy_changes,
    handle_commands,
    2048
);

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
        last_event: Option<(u32, u64, f64, f64)>,
    }

    /// Mock QEMU MultiTouch 服务
    struct MockQemuMultiTouch {
        state: std::sync::Arc<Mutex<MockState>>,
        notify: std::sync::Arc<tokio::sync::Notify>,
    }

    #[interface(name = "org.qemu.Display1.MultiTouch")]
    impl MockQemuMultiTouch {
        // --- Properties ---
        #[zbus(property)]
        fn max_slots(&self) -> i32 { self.state.lock().unwrap().max_slots }

        // --- Methods ---
        async fn send_event(&self, kind: u32, num_slot: u64, x: f64, y: f64) -> zbus::fdo::Result<()> {
            self.state.lock().unwrap().last_event = Some((kind, num_slot, x, y));
            self.notify.notify_waiters();
            Ok(())
        }
    }

    impl MockQemuMultiTouch {
        fn new(state: std::sync::Arc<Mutex<MockState>>, notify: std::sync::Arc<tokio::sync::Notify>) -> Self {
            Self { state, notify }
        }
    }

    /// 搭建测试环境
    async fn setup_env() -> (zbus::Connection, std::sync::Arc<Mutex<MockState>>, std::sync::Arc<tokio::sync::Notify>) {
        use zbus::Guid;

        let state = std::sync::Arc::new(Mutex::new(MockState { max_slots: 10, last_event: None }));

        let notify = std::sync::Arc::new(tokio::sync::Notify::new());
        let mock = MockQemuMultiTouch::new(state.clone(), notify.clone());

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

        (client_conn, state, notify)
    }

    /// 连接与初始状态同步测试
    #[tokio::test]
    async fn test_initial_state_sync() {
        let (conn, _state, _notify) = setup_env().await;
        let session = MultiTouchSession::connect(&conn, "/org/qemu/Display1/MultiTouch_0")
            .await
            .expect("Failed to create session");

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
        let (conn, state, notify) = setup_env().await;
        let session = MultiTouchSession::connect(&conn, "/org/qemu/Display1/MultiTouch_0")
            .await
            .expect("Failed to create session");

        // 消耗初始事件
        let _ = session.rx.recv().await;

        // 测试 begin
        let notified = notify.notified();
        session.tx.begin(0, 100.0, 200.0).await.expect("Failed to send begin");
        notified.await;
        {
            let s = state.lock().unwrap();
            assert_eq!(s.last_event, Some((0, 0, 100.0, 200.0)), "Kind::Begin should serialize to 0");
        }

        // 测试 update
        let notified = notify.notified();
        session.tx.update(0, 110.0, 210.0).await.expect("Failed to send update");
        notified.await;
        {
            let s = state.lock().unwrap();
            assert_eq!(s.last_event, Some((1, 0, 110.0, 210.0)), "Kind::Update should serialize to 1");
        }

        // 测试 end
        let notified = notify.notified();
        session.tx.end(0, 110.0, 210.0).await.expect("Failed to send end");
        notified.await;
        {
            let s = state.lock().unwrap();
            assert_eq!(s.last_event, Some((2, 0, 110.0, 210.0)), "Kind::End should serialize to 2");
        }
    }

    /// 触摸取消测试
    #[tokio::test]
    async fn test_touch_cancel() {
        let (conn, state, notify) = setup_env().await;
        let session = MultiTouchSession::connect(&conn, "/org/qemu/Display1/MultiTouch_0")
            .await
            .expect("Failed to create session");

        // 消耗初始事件
        let _ = session.rx.recv().await;

        // 测试 cancel
        let notified = notify.notified();
        session.tx.cancel(1, 50.0, 75.0).await.expect("Failed to send cancel");
        notified.await;
        {
            let s = state.lock().unwrap();
            assert_eq!(s.last_event, Some((3, 1, 50.0, 75.0)), "Kind::Cancel should serialize to 3");
        }
    }

    /// 多点触摸测试
    #[tokio::test]
    async fn test_multi_slot() {
        let (conn, state, notify) = setup_env().await;
        let session = MultiTouchSession::connect(&conn, "/org/qemu/Display1/MultiTouch_0")
            .await
            .expect("Failed to create session");

        // 消耗初始事件
        let _ = session.rx.recv().await;

        // 同时触摸两个点
        let notified = notify.notified();
        session.tx.begin(0, 100.0, 100.0).await.expect("Failed to send slot 0 begin");
        notified.await;

        let notified = notify.notified();
        session.tx.begin(1, 300.0, 300.0).await.expect("Failed to send slot 1 begin");
        notified.await;

        {
            let s = state.lock().unwrap();
            // 最后一个事件应该是 slot 1 的 Begin (0)
            assert_eq!(s.last_event, Some((0, 1, 300.0, 300.0)));
        }

        // 更新 slot 0
        let notified = notify.notified();
        session.tx.update(0, 110.0, 110.0).await.expect("Failed to send slot 0 update");
        notified.await;

        {
            let s = state.lock().unwrap();
            // Update = 1
            assert_eq!(s.last_event, Some((1, 0, 110.0, 110.0)));
        }
    }

    /// Kind 枚举测试
    #[test]
    fn test_kind_enum() {
        assert_eq!(Kind::Begin as u32, 0);
        assert_eq!(Kind::Update as u32, 1);
        assert_eq!(Kind::End as u32, 2);
        assert_eq!(Kind::Cancel as u32, 3);
    }
}
