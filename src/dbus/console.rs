//! `DBus` proxy for QEMU Console interface.
//! <https://www.qemu.org/docs/master/interop/dbus-display.html#org.qemu.Display1.Console-section>
use crate::{generate_handler, generate_watcher, impl_controller, impl_session_connect};
use derive_more::{AsRef, Deref, From};
use kanal::AsyncSender;
use std::{convert::TryFrom, fmt};
use zbus::{Result, proxy};
use zvariant::{OwnedFd, OwnedValue, Signature};

// 先创建命名 socket,记得避免路径重复
// 然后创建匿名 socket 用于画面更新

// path: /org/qemu/Display1/Console_$id.
#[proxy(interface = "org.qemu.Display1.Console", default_service = "org.qemu", gen_blocking = false)]
trait Console {
    /// Register a console listener, which will receive display updates, until it is disconnected.
    /// Multiple listeners may be registered simultaneously.
    /// The listener is expected to implement the org.qemu.Display1.Listener interface.
    async fn register_listener(&self, listener: &OwnedFd) -> Result<()>;

    /// Modify the dimensions and display settings.
    #[zbus(name = "SetUIInfo")]
    async fn set_ui_info(
        &self, width_mm: u16, height_mm: u16, xoff: i32, yoff: i32, width: u32, height: u32,
    ) -> Result<()>;

    /// **控制台名称（用于 UI 显示）**
    ///
    /// 这里的字符串通常是显卡设备的名称，如 "VGA", "QXL", "virtio-gpu" 等。
    /// **用途**：可以作为你 GUI 窗口的标题或 Tab 页的标签，告诉用户这是哪个屏幕。
    #[zbus(property)]
    fn label(&self) -> Result<String>;

    /// **多显示器索引 (Head Number)**
    ///
    /// 如果一个虚拟显卡有多个输出端口（多屏显示），这个数字区分它们（0, 1, 2...）。
    /// **用途**：配合 `device_address` 唯一确定"这是哪张显卡的第几个屏幕"。
    #[zbus(property)]
    fn head(&self) -> Result<u32>;

    /// **控制台内容类型**
    ///
    /// 区分这是**图形画面**还是**纯文本终端**。
    /// - `Graphic`: 传输像素数据，需要 OpenGL/纹理渲染。
    /// - `Text`: 传输字符网格（如 BIOS 界面或串口控制台），需要通过字体渲染。
    /// **用途**：决定你的 UI 应该创建一个图形渲染画布，还是一个模拟终端窗口。
    #[zbus(property)]
    fn r#type(&self) -> Result<ConsoleType>;

    /// **当前画面宽度 (Pixels)**
    ///
    /// 虚拟机当前输出图像的实际像素宽度。
    /// **用途**：收到 `PropertiesChanged` 信号更新此值时，你需要调整你的本地窗口大小或重绘纹理。
    #[zbus(property)]
    fn width(&self) -> Result<u32>;

    /// **当前画面高度 (Pixels)**
    ///
    /// 虚拟机当前输出图像的实际像素高度。
    #[zbus(property)]
    fn height(&self) -> Result<u32>;

    /// **硬件设备地址**
    ///
    /// 虚拟显卡在虚拟机总线上的物理位置，例如 "pci/0000/02.0"。
    /// **用途**：当虚拟机有多个显卡时，用来区分这个 Console 属于哪个物理设备。
    #[zbus(property)]
    fn device_address(&self) -> Result<String>;

    /// **能力发现 (Capabilities Discovery)**
    ///
    /// 返回该 Console 对象还实现了哪些**额外接口**的列表。
    /// 常见值包括：
    /// - `"org.qemu.Display1.Keyboard"`: 支持键盘输入。
    /// - `"org.qemu.Display1.Mouse"`: 支持鼠标输入。
    /// - `"org.qemu.Display1.MultiTouch"`: 支持多点触控。
    ///
    /// **用途**：**极其重要**。你不应该盲目发送鼠标事件，而应先检查这里是否包含 Mouse 接口。
    /// 这让你能根据服务端的能力，动态启用/禁用 UI 上的输入功能。
    #[zbus(property)]
    fn interfaces(&self) -> Result<Vec<String>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleType {
    Text,
    Graphic,
}

const TYPE_TEXT: &str = "Text";
const TYPE_GRAPHIC: &str = "Graphic";

impl fmt::Display for ConsoleType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use ConsoleType::*;
        match self {
            Text => write!(f, "{TYPE_TEXT}",),
            Graphic => write!(f, "{TYPE_GRAPHIC}",),
        }
    }
}

impl zvariant::Type for ConsoleType {
    const SIGNATURE: &'static Signature = <&str>::SIGNATURE;
}

impl TryFrom<OwnedValue> for ConsoleType {
    type Error = zvariant::Error;

    fn try_from(value: OwnedValue) -> std::result::Result<Self, Self::Error> {
        use ConsoleType::*;
        let s: String = value.try_into()?;
        match s.as_str() {
            TYPE_TEXT => Ok(Text),
            TYPE_GRAPHIC => Ok(Graphic),
            _ => Err(zvariant::Error::IncorrectType),
        }
    }
}

#[derive(Debug)]
pub enum Command {
    SetUiInfo { width_mm: u16, height_mm: u16, xoff: i32, yoff: i32, width: u32, height: u32 },
    RegisterListener(OwnedFd),
}

#[derive(Debug, Clone)]
pub enum Event {
    Height(u32),
    Width(u32),
    Label(String),
    Type(ConsoleType),
    DeviceAddress(String),
    Head(u32),
    Interfaces(Vec<String>),
}

#[derive(AsRef, Deref, From)]
#[derive(Clone)]
pub struct ConsoleController(AsyncSender<Command>);

impl ConsoleController {
    pub async fn set_ui_info(&self, width_mm: u16, height_mm: u16, xoff: i32, yoff: i32, width: u32, height: u32) -> crate::MksResult {
        self.0.send(Command::SetUiInfo { width_mm, height_mm, xoff, yoff, width, height }).await?;
        Ok(())
    }

    pub async fn register_listener(&self, fd: OwnedFd) -> crate::MksResult {
        self.0.send(Command::RegisterListener(fd)).await?;
        Ok(())
    }
}

impl_session_connect!(ConsoleSession, ConsoleProxy<'static>, ConsoleController, Command, Event);

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
            => p.set_ui_info(width_mm, height_mm, xoff, yoff, width, height).await,
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
        fn r#type(&self) -> String { "Graphic".to_string() }

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
            connect(&conn, "/org/qemu/Display1/Console_0".to_string()).await.expect("Failed to create session");

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
        // ===== 基本解析测试 =====

        // Test Graphic
        let val = zvariant::Value::new("Graphic");
        let owned = OwnedValue::try_from(val).unwrap();
        let ct = ConsoleType::try_from(owned).unwrap();
        assert_eq!(ct, ConsoleType::Graphic);

        // Test Text
        let val = zvariant::Value::new("Text");
        let owned = OwnedValue::try_from(val).unwrap();
        let ct = ConsoleType::try_from(owned).unwrap();
        assert_eq!(ct, ConsoleType::Text);

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

        // ===== 特征实现测试 =====

        // Test Display
        let display_str = format!("{}", ConsoleType::Graphic);
        assert_eq!(display_str, "Graphic");

        let display_str = format!("{}", ConsoleType::Text);
        assert_eq!(display_str, "Text");

        // Test zvariant::Type (检查签名是否正确)
        assert_eq!(ConsoleType::SIGNATURE, "s"); // 字符串类型

        // ===== 反向转换测试（通过 Display） =====
        // 验证 Display 输出可以被正确解析回原始类型
        let graphic_display = ConsoleType::Graphic.to_string();
        let val = zvariant::Value::new(&graphic_display);
        let owned = OwnedValue::try_from(val).unwrap();
        let parsed_back = ConsoleType::try_from(owned).unwrap();
        assert_eq!(parsed_back, ConsoleType::Graphic);

        let text_display = ConsoleType::Text.to_string();
        let val = zvariant::Value::new(&text_display);
        let owned = OwnedValue::try_from(val).unwrap();
        let parsed_back = ConsoleType::try_from(owned).unwrap();
        assert_eq!(parsed_back, ConsoleType::Text);
    }
}
