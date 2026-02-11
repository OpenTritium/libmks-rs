//! `DBus` proxy for QEMU VM interface.
//! <https://www.qemu.org/docs/master/interop/dbus-display.html#org.qemu.Display1.VM-section>
use crate::generate_watcher;
use kanal::AsyncReceiver;
use tokio::task::JoinHandle;
use zbus::{Connection, Result, proxy};

#[proxy(
    interface = "org.qemu.Display1.VM",
    default_service = "org.qemu",
    default_path = "/org/qemu/Display1/VM",
    gen_blocking = false
)]
pub trait Vm {
    /// The name of the VM.
    #[zbus(property)]
    fn name(&self) -> Result<String>;

    /// The UUID of the VM.
    #[zbus(property, name = "UUID")]
    fn uuid(&self) -> Result<String>;

    /// The list of consoles available on `/org/qemu/Display1/Console_$id`.
    #[zbus(property, name = "ConsoleIDs")]
    fn console_ids(&self) -> Result<Vec<u32>>;

    /// This property lists extra interfaces provided by the /org/qemu/Display1/VM object, and can be used to detect the
    /// capabilities with which they are communicating. Unlike the standard D-Bus Introspectable interface, querying
    /// this property does not require parsing XML. (earlier version of the display interface do not provide this
    /// property)
    #[zbus(property)]
    fn interfaces(&self) -> Result<Vec<String>>;
}

#[derive(Debug, Clone)]
pub enum Event {
    Name(String),
    Uuid(String),
    ConsoleIds(Vec<u32>),
    Interfaces(Vec<String>),
}

/// VM event listener and its watch task.
pub struct VmListener {
    pub rx: AsyncReceiver<Event>,
    pub watch_task: JoinHandle<()>,
}

impl Drop for VmListener {
    fn drop(&mut self) { self.watch_task.abort(); }
}

/// Connect to the VM interface and return an event listener with its watch task.
pub async fn connect(conn: &Connection) -> crate::MksResult<VmListener> {
    let proxy = VmProxy::new(conn).await?;
    let (event_tx, event_rx) = kanal::unbounded_async::<Event>();
    let watch_task = watch_proxy_changes(proxy, event_tx).await?;
    Ok(VmListener { rx: event_rx, watch_task })
}

generate_watcher!(
    watch_proxy_changes,
    VmProxy<'static>,
    Event,
    "vm",
    {
        name         => receive_name_changed         => Event::Name,
        uuid         => receive_uuid_changed         => Event::Uuid,
        console_ids  => receive_console_ids_changed  => Event::ConsoleIds,
        interfaces   => receive_interfaces_changed   => Event::Interfaces,
    }
);

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use zbus::interface;

    /// Mock QEMU VM 状态
    struct MockState {
        name: String,
        uuid: String,
        console_ids: Vec<u32>,
        interfaces: Vec<String>,
    }

    /// Mock QEMU VM 服务
    struct MockQemuVm {
        state: std::sync::Arc<Mutex<MockState>>,
    }

    #[interface(name = "org.qemu.Display1.VM")]
    impl MockQemuVm {
        // --- Properties ---
        #[zbus(property)]
        fn name(&self) -> String { self.state.lock().unwrap().name.clone() }

        #[zbus(property, name = "UUID")]
        fn uuid(&self) -> String { self.state.lock().unwrap().uuid.clone() }

        #[zbus(property, name = "ConsoleIDs")]
        fn console_ids(&self) -> Vec<u32> { self.state.lock().unwrap().console_ids.clone() }

        #[zbus(property)]
        fn interfaces(&self) -> Vec<String> { self.state.lock().unwrap().interfaces.clone() }
    }

    impl MockQemuVm {
        fn new(state: std::sync::Arc<Mutex<MockState>>) -> Self { Self { state } }
    }

    /// 搭建测试环境
    async fn setup_env() -> (zbus::Connection, std::sync::Arc<Mutex<MockState>>) {
        use zbus::Guid;

        let state = std::sync::Arc::new(Mutex::new(MockState {
            name: "TestVM".to_string(),
            uuid: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            console_ids: vec![0, 1],
            interfaces: vec!["org.qemu.Display1.VM".to_string()],
        }));

        let mock = MockQemuVm::new(state.clone());

        let (sock1, sock2) = std::os::unix::net::UnixStream::pair().expect("Failed to create socket pair");

        tokio::spawn(async move {
            let guid = Guid::generate();
            let _server_conn = zbus::connection::Builder::unix_stream(sock1)
                .p2p()
                .server(guid)
                .expect("Failed to set server mode")
                .serve_at("/org/qemu/Display1/VM", mock)
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
        let listener = connect(&conn).await.expect("Failed to create listener");

        // 验证：应当收到所有属性的初始事件
        let mut events = Vec::new();
        for _ in 0..4 {
            if let Ok(event) = listener.rx.recv().await {
                events.push(event);
            }
        }

        // 检查是否包含预期的初始值
        let has_name = events.iter().any(|e| matches!(e, Event::Name(n) if n == "TestVM"));
        let has_uuid =
            events.iter().any(|e| matches!(e, Event::Uuid(u) if u == "550e8400-e29b-41d4-a716-446655440000"));
        let has_console_ids = events.iter().any(|e| matches!(e, Event::ConsoleIds(ids) if *ids == vec![0, 1]));
        let has_interfaces =
            events.iter().any(|e| matches!(e, Event::Interfaces(i) if i.contains(&"org.qemu.Display1.VM".to_string())));

        assert!(has_name, "Should receive initial name TestVM");
        assert!(has_uuid, "Should receive initial UUID");
        assert!(has_console_ids, "Should receive initial console IDs [0, 1]");
        assert!(has_interfaces, "Should receive initial interfaces");
    }
}
