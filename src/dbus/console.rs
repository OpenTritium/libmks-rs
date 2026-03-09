//! `DBus` proxy for QEMU Console interface.
//! <https://www.qemu.org/docs/master/interop/dbus-display.html#org.qemu.Display1.Console-section>
use crate::{generate_handler, generate_watcher, impl_controller, impl_session_connect};
use derive_more::{AsRef, Deref, From};
use kanal::AsyncSender;
use serde::Deserialize;
use std::num::{NonZeroU16, NonZeroU32};
use zbus::{Result, proxy};
use zvariant::{OwnedFd, OwnedValue, Type, Value};

// path: /org/qemu/Display1/Console_$id.
#[proxy(interface = "org.qemu.Display1.Console", default_service = "org.qemu", gen_blocking = false)]
trait Console {
    /// Registers a listener to receive display updates.
    ///
    /// The listener must implement the `org.qemu.Display1.Listener` interface.
    async fn register_listener(&self, listener: &OwnedFd) -> Result<()>;

    /// Sets the UI dimensions and display settings.
    #[zbus(name = "SetUIInfo")]
    async fn set_ui_info(
        &self, width_mm: u16, height_mm: u16, xoff: i32, yoff: i32, width: u32, height: u32,
    ) -> Result<()>;

    /// The console name (e.g., "VGA", "QXL", "virtio-gpu").
    #[zbus(property)]
    fn label(&self) -> Result<String>;

    /// The head number (0, 1, 2...), distinguishing multiple outputs on the same device.
    #[zbus(property)]
    fn head(&self) -> Result<u32>;

    /// The console type (`Graphic` or `Text`).
    #[zbus(property)]
    fn r#type(&self) -> Result<ConsoleType>;

    /// Current display width in pixels.
    #[zbus(property)]
    fn width(&self) -> Result<u32>;

    /// Current display height in pixels.
    #[zbus(property)]
    fn height(&self) -> Result<u32>;

    /// The hardware device address (e.g., "pci/0000/02.0").
    #[zbus(property)]
    fn device_address(&self) -> Result<String>;

    /// List of additional interfaces implemented by this console.
    ///
    /// Common values: `"org.qemu.Display1.Mouse"`, `"org.qemu.Display1.Keyboard"`.
    ///
    /// # Note
    /// You **must** verify the presence of the relevant interface here before sending
    /// input events (e.g., mouse clicks) to the server.
    #[zbus(property)]
    fn interfaces(&self) -> Result<Vec<String>>;
}

/// Console class reported by QEMU.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Type, Deserialize, Value, OwnedValue)]
#[zvariant(signature = "s")]
pub enum ConsoleType {
    /// Text-only terminal output.
    Text,
    /// Pixel-based graphical output.
    Graphic,
}

/// Commands consumed by the console command handler.
#[derive(Debug)]
pub enum Command {
    /// Update UI geometry and viewport metadata.
    SetUiInfo {
        width_mm: NonZeroU16,
        height_mm: NonZeroU16,
        xoff: u32,
        yoff: u32,
        width: NonZeroU32,
        height: NonZeroU32,
    },
    /// Register a listener object that implements `org.qemu.Display1.Listener`.
    RegisterListener(OwnedFd),
}

/// Property change events emitted by the console watcher.
#[derive(Debug, Clone)]
pub enum Event {
    /// New display height in pixels.
    Height(u32),
    /// New display width in pixels.
    Width(u32),
    /// New human-readable console label.
    Label(String),
    /// New console type.
    Type(ConsoleType),
    /// New hardware device address.
    DeviceAddress(String),
    /// New head index on the device.
    Head(u32),
    /// New list of optional interfaces exposed by this console.
    Interfaces(Vec<String>),
}

/// Non-blocking command sender for a console session.
#[derive(AsRef, Deref, From, Clone)]
pub struct ConsoleController(pub AsyncSender<Command>);

impl_controller!(ConsoleController, Command, {
    pub fn set_ui_info(width_mm: NonZeroU16, height_mm: NonZeroU16, xoff: u32, yoff: u32, width:NonZeroU32, height: NonZeroU32)
        => SetUiInfo { width_mm, height_mm, xoff, yoff, width, height };
    pub fn register_listener(fd: OwnedFd)
        => RegisterListener(fd);
});

impl_session_connect!(
    ConsoleSession,
    ConsoleProxy<'static>,
    ConsoleController,
    Command,
    Event,
    watch_proxy_changes,
    handle_commands,
    32
);

generate_watcher!(
    watch_proxy_changes,
    ConsoleProxy<'static>,
    Event,
    "console",
    {
        width          => receive_width_changed          => Event::Width,
        height         => receive_height_changed         => Event::Height,
        label          => receive_label_changed          => Event::Label,
        r#type         => receive_type_changed           => Event::Type,
        device_address => receive_device_address_changed => Event::DeviceAddress,
        head           => receive_head_changed           => Event::Head,
        interfaces     => receive_interfaces_changed     => Event::Interfaces,
    }
);

generate_handler!(
    handle_commands,
    ConsoleProxy<'static>,
    Command,
    "console",
    |p| {
        Command::SetUiInfo { width_mm, height_mm, xoff, yoff, width, height }
            => p.set_ui_info(
                width_mm.get(),
                height_mm.get(),
                xoff.try_into().unwrap(),
                yoff.try_into().unwrap(),
                width.get(),
                height.get(),
            ).await,
        Command::RegisterListener(fd)
            => p.register_listener(&fd).await,
    }
);

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use zbus::interface;
    use zvariant::{OwnedFd, Type};

    /// Mock QEMU Console 状态
    struct MockState {
        width: u32,
        height: u32,
        label: String,
        console_type_raw: String, // 模拟 QEMU 内部存储的原始字符串
        last_ui_info: Option<(u32, u32)>,
        listener_registered: bool,
    }

    /// Mock QEMU Console 服务
    struct MockQemuConsole {
        state: std::sync::Arc<Mutex<MockState>>,
    }

    #[interface(name = "org.qemu.Display1.Console")]
    impl MockQemuConsole {
        // --- Properties ---
        #[zbus(property)]
        fn label(&self) -> String { self.state.lock().unwrap().label.clone() }

        #[zbus(property)]
        fn head(&self) -> u32 { 0 }

        #[zbus(property)]
        fn r#type(&self) -> String { self.state.lock().unwrap().console_type_raw.clone() }

        #[zbus(property)]
        fn width(&self) -> u32 { self.state.lock().unwrap().width }

        #[zbus(property)]
        fn height(&self) -> u32 { self.state.lock().unwrap().height }

        #[zbus(property)]
        fn device_address(&self) -> String { "pci/0000/01.0".to_string() }

        #[zbus(property)]
        fn interfaces(&self) -> Vec<String> { vec!["org.qemu.Display1.Mouse".to_string()] }

        // --- Methods ---
        async fn register_listener(&self, _listener: OwnedFd) -> zbus::fdo::Result<()> {
            self.state.lock().unwrap().listener_registered = true;
            Ok(())
        }

        #[zbus(name = "SetUIInfo")]
        async fn set_ui_info(
            &self, _w_mm: u16, _h_mm: u16, _xoff: i32, _yoff: i32, width: u32, height: u32,
        ) -> zbus::fdo::Result<()> {
            self.state.lock().unwrap().last_ui_info = Some((width, height));
            Ok(())
        }
    }

    impl MockQemuConsole {
        fn new(state: std::sync::Arc<Mutex<MockState>>) -> Self { Self { state } }
    }

    /// 搭建测试环境 - 使用 Unix socketpair 创建 p2p 连接
    async fn setup_env() -> (zbus::Connection, std::sync::Arc<Mutex<MockState>>) {
        use zbus::Guid;

        let state = std::sync::Arc::new(Mutex::new(MockState {
            width: 800,
            height: 600,
            label: "VGA-1".to_string(),
            console_type_raw: "Graphic".to_string(), // 模拟 QEMU 发送的原始字符串
            last_ui_info: None,
            listener_registered: false,
        }));

        let mock = MockQemuConsole::new(state.clone());

        // 使用 Unix socketpair 创建 p2p 连接
        let (sock1, sock2) = std::os::unix::net::UnixStream::pair().expect("Failed to create socket pair");

        // 在后台任务中创建服务器端连接
        tokio::spawn(async move {
            // 生成 GUID 用于服务端标识
            let guid = Guid::generate();

            let _server_conn = zbus::connection::Builder::unix_stream(sock1)
                .p2p()
                .server(guid)
                .expect("Failed to set server mode")
                .serve_at("/org/qemu/Display1/Console_0", mock)
                .expect("Failed to serve mock")
                .build()
                .await
                .expect("Failed to build server connection");

            // 保持 server_conn 存活，让它的消息循环继续运行
            std::future::pending::<()>().await;
        });

        // 创建客户端连接
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

        // 连接到 Mock 服务
        let session =
            ConsoleSession::connect(&conn, "/org/qemu/Display1/Console_0").await.expect("Failed to create session");

        // 验证：应当收到所有属性的初始事件
        // select_all 产生的流顺序不确定，需要收集后验证
        let mut events = Vec::new();

        // 读取前 7 个事件 (width, height, label, type, head, addr, ifaces)
        for _ in 0..7 {
            if let Ok(event) = session.rx.recv().await {
                events.push(event);
            }
        }
        // 检查是否包含预期的初始值
        let has_width = events.iter().any(|e| matches!(e, Event::Width(800)));
        let has_height = events.iter().any(|e| matches!(e, Event::Height(600)));
        let has_label = events.iter().any(|e| matches!(e, Event::Label(l) if l == "VGA-1"));
        let has_type = events.iter().any(|e| matches!(e, Event::Type(ConsoleType::Graphic)));
        let has_head = events.iter().any(|e| matches!(e, Event::Head(0)));
        let has_addr = events.iter().any(|e| matches!(e, Event::DeviceAddress(a) if a == "pci/0000/01.0"));
        let has_ifaces = events
            .iter()
            .any(|e| matches!(e, Event::Interfaces(i) if i.contains(&"org.qemu.Display1.Mouse".to_string())));
        assert!(has_width, "Should receive initial width 800");
        assert!(has_height, "Should receive initial height 600");
        assert!(has_label, "Should receive initial label VGA-1");
        assert!(has_type, "Should receive initial type Graphic");
        assert!(has_head, "Should receive initial head 0");
        assert!(has_addr, "Should receive initial device_address pci/0000/01.0");
        assert!(has_ifaces, "Should receive initial interfaces containing Mouse");
    }

    /// ConsoleType 解析单元测试（无需 DBus 环境）
    #[tokio::test]
    async fn test_console_type_parsing() {
        // ===== 反序列化测试 (Deserialize): String -> Enum =====
        // 这是验证：如果我们从 QEMU 收到了 "Graphic"，我们能否正确转为 Enum

        let val = zvariant::Value::new("Graphic");
        let owned = OwnedValue::try_from(val).unwrap();
        let ct = ConsoleType::try_from(owned).unwrap();
        assert_eq!(ct, ConsoleType::Graphic);

        let val = zvariant::Value::new("Text");
        let owned = OwnedValue::try_from(val).unwrap();
        let ct = ConsoleType::try_from(owned).unwrap();
        assert_eq!(ct, ConsoleType::Text);

        // ===== 序列化测试 (Serialize): Enum -> String =====
        // 验证：如果我们发送 ConsoleType::Graphic，zbus 是否会将其转为 "Graphic" 字符串
        // 这直接测试了 serde 序列化层，确保了如果我们需要发送这个枚举，它确实会变成字符串 "Graphic"

        let v = zvariant::Value::from(ConsoleType::Graphic);
        // zvariant::Value 实现了 PartialEq，可以直接与字符串比较
        assert_eq!(v, zvariant::Value::new("Graphic"));

        let v = zvariant::Value::from(ConsoleType::Text);
        assert_eq!(v, zvariant::Value::new("Text"));

        // ===== 大小写敏感测试 =====
        // 注意：QEMU 协议中是大小写敏感的

        // 小写 "graphic" 应该失败
        let val = zvariant::Value::new("graphic");
        let owned = OwnedValue::try_from(val).unwrap();
        let result = ConsoleType::try_from(owned);
        assert!(result.is_err(), "小写 'graphic' 应该解析失败，实际得到: {:?}", result.ok());

        // 大写 "TEXT" 应该失败
        let val = zvariant::Value::new("TEXT");
        let owned = OwnedValue::try_from(val).unwrap();
        let result = ConsoleType::try_from(owned);
        assert!(result.is_err(), "大写 'TEXT' 应该解析失败，实际得到: {:?}", result.ok());

        // 混合大小写应该失败
        let val = zvariant::Value::new("GrApHiC");
        let owned = OwnedValue::try_from(val).unwrap();
        assert!(ConsoleType::try_from(owned).is_err(), "混合大小写 'GrApHiC' 应该解析失败");

        // ===== 边界值测试 =====

        // 空字符串
        let val = zvariant::Value::new("");
        let owned = OwnedValue::try_from(val).unwrap();
        assert!(ConsoleType::try_from(owned).is_err(), "空字符串应该解析失败");

        // 带有前后空格的字符串
        let val = zvariant::Value::new(" Graphic ");
        let owned = OwnedValue::try_from(val).unwrap();
        assert!(ConsoleType::try_from(owned).is_err(), "带前后空格的 ' Graphic ' 应该解析失败");

        // 带有内部空格的字符串
        let val = zvariant::Value::new("Graphic Console");
        let owned = OwnedValue::try_from(val).unwrap();
        assert!(ConsoleType::try_from(owned).is_err(), "带内部空格的 'Graphic Console' 应该解析失败");

        // ===== 各种无效输入测试 =====

        // 常见拼写错误
        let invalid_types = [
            "InvalidType",
            "Graphics", // 多了 s
            "Texts",    // 多了 s
            "Graph",    // 不完整的
            "Tex",      // 不完整的
            "VGA",      // 常见设备名但不是有效类型
            "QXL",
            "virtio-gpu",
            "123",       // 数字
            "Graphic\t", // 带 tab
            "Text\n",    // 带换行
            "Graphic\0", // 带 null 字符
        ];

        for invalid in &invalid_types {
            let val = zvariant::Value::new(*invalid);
            let owned = OwnedValue::try_from(val).unwrap();
            assert!(ConsoleType::try_from(owned).is_err(), "'{invalid}' 应该解析失败");
        }
        assert_eq!(ConsoleType::SIGNATURE, "s"); // 字符串类型
    }
}
