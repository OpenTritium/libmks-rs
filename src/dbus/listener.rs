use crate::MksResult;
use kanal::{AsyncReceiver, AsyncSender};
use zbus::{Connection, fdo::Result, interface};
use zvariant::OwnedFd;

#[derive(Debug, PartialEq)]
pub enum QemuEvent {
    Scanout {
        width: u32,
        height: u32,
        stride: u32,
        pixman_format: u32,
        data: Vec<u8>,
    },
    Update {
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        stride: u32,
        pixman_format: u32,
        data: Vec<u8>,
    },
    ScanoutDmabuf {
        dmabuf: OwnedFd,
        width: u32,
        height: u32,
        stride: u32,
        fourcc: u32,
        modifier: u64,
        y0_top: bool,
    },
    UpdateDmabuf {
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    },
    Disable,
    MouseSet {
        x: i32,
        y: i32,
        on: i32,
    },
    CursorDefine {
        width: i32,
        height: i32,
        hot_x: i32,
        hot_y: i32,
        data: Vec<u8>,
    },
    ScanoutDmabuf2 {
        dmabuf: Vec<OwnedFd>,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        offset: Vec<u64>,
        stride: Vec<u32>,
        num_planes: u32,
        fourcc: u32,
        backing_width: u32,
        backing_height: u32,
        modifier: u64,
        y0_top: bool,
    },
}

pub struct Listener {
    tx: AsyncSender<QemuEvent>,
    ifaces: Vec<String>,
}

#[interface(name = "org.qemu.Display1.Listener")]
impl Listener {
    async fn scanout(&self, width: u32, height: u32, stride: u32, pixman_format: u32, data: Vec<u8>) -> Result<()> {
        let event = QemuEvent::Scanout { width, height, stride, pixman_format, data };
        self.tx.send(event).await.map_err(|e| zbus::Error::Failure(e.to_string()))?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn update(
        &self, x: i32, y: i32, width: i32, height: i32, stride: u32, pixman_format: u32, data: Vec<u8>,
    ) -> Result<()> {
        let event = QemuEvent::Update { x, y, width, height, stride, pixman_format, data };
        self.tx.send(event).await.map_err(|e| zbus::Error::Failure(e.to_string()))?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    #[zbus(name = "ScanoutDMABUF")]
    async fn scanout_dmabuf(
        &self, dmabuf: OwnedFd, width: u32, height: u32, stride: u32, fourcc: u32, modifier: u64, y0_top: bool,
    ) -> Result<()> {
        let event = QemuEvent::ScanoutDmabuf { dmabuf, width, height, stride, fourcc, modifier, y0_top };
        self.tx.send(event).await.map_err(|e| zbus::Error::Failure(e.to_string()))?;
        Ok(())
    }

    #[zbus(name = "UpdateDMABUF")]
    async fn update_dmabuf(&self, x: i32, y: i32, width: i32, height: i32) -> Result<()> {
        let event = QemuEvent::UpdateDmabuf { x, y, width, height };
        self.tx.send(event).await.map_err(|e| zbus::Error::Failure(e.to_string()))?;
        Ok(())
    }

    async fn disable(&self) -> Result<()> {
        let event = QemuEvent::Disable;
        self.tx.send(event).await.map_err(|e| zbus::Error::Failure(e.to_string()))?;
        Ok(())
    }

    async fn mouse_set(&self, x: i32, y: i32, on: i32) -> Result<()> {
        let event = QemuEvent::MouseSet { x, y, on };
        self.tx.send(event).await.map_err(|e| zbus::Error::Failure(e.to_string()))?;
        Ok(())
    }

    async fn cursor_define(&self, width: i32, height: i32, hot_x: i32, hot_y: i32, data: Vec<u8>) -> Result<()> {
        let event = QemuEvent::CursorDefine { width, height, hot_x, hot_y, data };
        self.tx.send(event).await.map_err(|e| zbus::Error::Failure(e.to_string()))?;
        Ok(())
    }

    #[zbus(property)]
    async fn interfaces(&self) -> Result<Vec<String>> { Ok(self.ifaces.clone()) }
}

impl Listener {
    pub fn new(tx: AsyncSender<QemuEvent>) -> Self {
        Self {
            tx,
            ifaces: vec![
                "org.qemu.Display1.Listener".to_string(),
                "org.qemu.Display1.Listener.Unix.ScanoutDMABUF2".to_string(),
            ],
        }
    }
}

#[derive(Debug, Clone)]
pub struct Dmabuf2Handler(AsyncSender<QemuEvent>);

#[interface(name = "org.qemu.Display1.Listener.Unix.ScanoutDMABUF2")]
impl Dmabuf2Handler {
    #[zbus(name = "ScanoutDMABUF2")]
    #[allow(clippy::too_many_arguments)]
    async fn scanout_dmabuf2(
        &self, dmabuf: Vec<OwnedFd>, x: u32, y: u32, width: u32, height: u32, offset: Vec<u64>, stride: Vec<u32>,
        num_planes: u32, fourcc: u32, backing_width: u32, backing_height: u32, modifier: u64, y0_top: bool,
    ) -> Result<()> {
        let event = QemuEvent::ScanoutDmabuf2 {
            dmabuf,
            x,
            y,
            width,
            height,
            offset,
            stride,
            num_planes,
            fourcc,
            backing_width,
            backing_height,
            modifier,
            y0_top,
        };
        self.0.send(event).await.map_err(|e| zbus::Error::Failure(e.to_string()))?;
        Ok(())
    }
}

impl Dmabuf2Handler {
    pub fn new(sender: AsyncSender<QemuEvent>) -> Self { Self(sender) }
}

pub async fn serve(conn: &Connection) -> MksResult<AsyncReceiver<QemuEvent>> {
    let (event_tx, event_rx) = kanal::unbounded_async::<QemuEvent>();
    let handler = Listener::new(event_tx.clone());
    let dmabuf2_handler = Dmabuf2Handler::new(event_tx);
    const LISTENER_PATH: &str = "/org/qemu/Display1/Listener";
    conn.object_server().at(LISTENER_PATH, handler).await?;
    conn.object_server().at(LISTENER_PATH, dmabuf2_handler).await?;
    Ok(event_rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    // ========== 辅助函数 ==========

    /// 创建一个用于测试的 dummy FD
    /// 使用 /dev/null 作为占位符，因为它总是存在且行为稳定
    fn create_dummy_fd() -> OwnedFd {
        let file = File::open("/dev/null").expect("Failed to open /dev/null");
        let std_fd: std::os::fd::OwnedFd = file.into();
        OwnedFd::from(std_fd)
    }

    /// 搭建测试环境 - 使用 Unix socketpair 创建 p2p 连接
    /// 使用 spawn 将服务端放在独立协程，避免 SASL 握手死锁
    async fn setup_mock_env() -> (zbus::Connection, AsyncReceiver<QemuEvent>, zbus::Connection) {
        use zbus::Guid;

        let (sock1, sock2) = std::os::unix::net::UnixStream::pair().expect("Failed to create socket pair");

        // 在单独任务中启动服务端
        let server_handle = tokio::spawn(async move {
            let server_conn = zbus::connection::Builder::unix_stream(sock1)
                .p2p()
                .server(Guid::generate())
                .expect("Failed to set server mode")
                .build()
                .await
                .expect("Failed to build server connection");

            let rx = serve(&server_conn).await.expect("Failed to serve");
            (server_conn, rx)
        });

        // 创建客户端连接
        let client_conn = zbus::connection::Builder::unix_stream(sock2)
            .p2p()
            .build()
            .await
            .expect("Failed to build client connection");

        // 等待服务端准备就绪
        let (server_conn, rx) = server_handle.await.unwrap();

        (server_conn, rx, client_conn)
    }

    // ========== 单元测试：服务注册 ==========

    /// 基础服务注册测试 - 验证对象成功挂载
    #[tokio::test]
    async fn test_serve_registers_objects() {
        let (server_conn, _rx, _client_conn) = setup_mock_env().await;

        // 检查主 Listener 接口是否注册
        let result = server_conn.object_server().interface::<_, Listener>("/org/qemu/Display1/Listener").await;
        assert!(result.is_ok(), "QemuDisplayHandler should be registered at /org/qemu/Display1/Listener");

        // 检查 DMABUF2 接口是否注册
        let result = server_conn.object_server().interface::<_, Dmabuf2Handler>("/org/qemu/Display1/Listener").await;
        assert!(result.is_ok(), "Dmabuf2Handler should be registered at /org/qemu/Display1/Listener");
    }

    // ========== 集成测试：基础 Scanout ==========

    /// 测试基础的 Scanout 消息
    #[tokio::test]
    async fn test_basic_scanout() {
        let (_server_conn, rx, client_conn) = setup_mock_env().await;

        // 发送 Scanout 消息
        let width = 100u32;
        let height = 100u32;
        let stride = 400u32;
        let pixman_format = 1u32;
        let data = vec![0u8, 255u8, 0u8, 255u8];

        client_conn
            .call_method(
                Some("org.qemu.Display1.Listener"),
                "/org/qemu/Display1/Listener",
                Some("org.qemu.Display1.Listener"),
                "Scanout",
                &(width, height, stride, pixman_format, &data),
            )
            .await
            .expect("Failed to call Scanout");

        // 接收并验证
        let event = rx.recv().await.expect("Should receive event");
        if let QemuEvent::Scanout { width: w, height: h, data: d, .. } = event {
            assert_eq!(w, 100);
            assert_eq!(h, 100);
            assert_eq!(d.len(), 4);
        } else {
            panic!("Expected Scanout event, got {:?}", event);
        }
    }

    /// 测试 Update 消息
    #[tokio::test]
    async fn test_update() {
        let (_server_conn, rx, client_conn) = setup_mock_env().await;

        let x = 10i32;
        let y = 20i32;
        let width = 50i32;
        let height = 50i32;
        let stride = 200u32;
        let pixman_format = 1u32;
        let data = vec![255u8, 0u8, 255u8, 0u8];

        client_conn
            .call_method(
                Some("org.qemu.Display1.Listener"),
                "/org/qemu/Display1/Listener",
                Some("org.qemu.Display1.Listener"),
                "Update",
                &(x, y, width, height, stride, pixman_format, &data),
            )
            .await
            .expect("Failed to call Update");

        let event = rx.recv().await.expect("Should receive event");
        if let QemuEvent::Update { x: ex, y: ey, width: ew, height: eh, .. } = event {
            assert_eq!(ex, 10);
            assert_eq!(ey, 20);
            assert_eq!(ew, 50);
            assert_eq!(eh, 50);
        } else {
            panic!("Expected Update event, got {:?}", event);
        }
    }

    /// 测试 Disable 消息
    #[tokio::test]
    async fn test_disable() {
        let (_server_conn, rx, client_conn) = setup_mock_env().await;

        client_conn
            .call_method(
                Some("org.qemu.Display1.Listener"),
                "/org/qemu/Display1/Listener",
                Some("org.qemu.Display1.Listener"),
                "Disable",
                &(),
            )
            .await
            .expect("Failed to call Disable");

        let event = rx.recv().await.expect("Should receive event");
        assert_eq!(event, QemuEvent::Disable);
    }

    // ========== 集成测试：鼠标和光标 ==========

    /// 测试 MouseSet 消息
    #[tokio::test]
    async fn test_mouse_set() {
        let (_server_conn, rx, client_conn) = setup_mock_env().await;

        client_conn
            .call_method(
                Some("org.qemu.Display1.Listener"),
                "/org/qemu/Display1/Listener",
                Some("org.qemu.Display1.Listener"),
                "MouseSet",
                &(50i32, 50i32, 1i32),
            )
            .await
            .expect("Failed to call MouseSet");

        let event = rx.recv().await.expect("Should receive event");
        if let QemuEvent::MouseSet { x, y, on } = event {
            assert_eq!(x, 50);
            assert_eq!(y, 50);
            assert_eq!(on, 1);
        } else {
            panic!("Expected MouseSet event, got {:?}", event);
        }
    }

    /// 测试 CursorDefine 消息
    #[tokio::test]
    async fn test_cursor_define() {
        let (_server_conn, rx, client_conn) = setup_mock_env().await;

        let width = 32i32;
        let height = 32i32;
        let hot_x = 0i32;
        let hot_y = 0i32;
        let data = vec![0u8; 32 * 32 * 4]; // 32x32 RGBA

        client_conn
            .call_method(
                Some("org.qemu.Display1.Listener"),
                "/org/qemu/Display1/Listener",
                Some("org.qemu.Display1.Listener"),
                "CursorDefine",
                &(width, height, hot_x, hot_y, &data),
            )
            .await
            .expect("Failed to call CursorDefine");

        let event = rx.recv().await.expect("Should receive event");
        if let QemuEvent::CursorDefine { width: w, height: h, hot_x: hx, hot_y: hy, data: d } = event {
            assert_eq!(w, 32);
            assert_eq!(h, 32);
            assert_eq!(hx, 0);
            assert_eq!(hy, 0);
            assert_eq!(d.len(), 32 * 32 * 4);
        } else {
            panic!("Expected CursorDefine event, got {:?}", event);
        }
    }

    // ========== 集成测试：DMABUF ==========

    /// 测试 ScanoutDmabuf 消息（单 FD）
    #[tokio::test]
    async fn test_scanout_dmabuf() {
        let (_server_conn, rx, client_conn) = setup_mock_env().await;

        let fd = create_dummy_fd();
        let width = 1920u32;
        let height = 1080u32;
        let stride = 7680u32;
        let fourcc = 0x34325258u32; // XR24
        let modifier = 0u64;
        let y0_top = false;

        client_conn
            .call_method(
                Some("org.qemu.Display1.Listener"),
                "/org/qemu/Display1/Listener",
                Some("org.qemu.Display1.Listener"),
                "ScanoutDMABUF",
                &(fd, width, height, stride, fourcc, modifier, y0_top),
            )
            .await
            .expect("Failed to call ScanoutDMABUF");

        let event = rx.recv().await.expect("Should receive event");
        if let QemuEvent::ScanoutDmabuf { width: w, height: h, fourcc: f, .. } = event {
            assert_eq!(w, 1920);
            assert_eq!(h, 1080);
            assert_eq!(f, 0x34325258);
        } else {
            panic!("Expected ScanoutDmabuf event, got {:?}", event);
        }
    }

    /// 测试 ScanoutDMABUF2 消息（映射到 ScanoutDmabuf）
    #[tokio::test]
    async fn test_scanout_dmabuf2() {
        let (_server_conn, rx, client_conn) = setup_mock_env().await;

        let fd = create_dummy_fd();
        let width = 1920u32;
        let height = 1080u32;
        let stride = vec![7680u32];
        let fourcc = 0x34325258u32;
        let modifier = 0u64;

        client_conn
            .call_method(
                Some("org.qemu.Display1.Listener"),
                "/org/qemu/Display1/Listener",
                Some("org.qemu.Display1.Listener.Unix.ScanoutDMABUF2"),
                "ScanoutDMABUF2",
                &(
                    vec![fd],
                    0u32,
                    0u32,
                    width,
                    height,
                    vec![0u64],
                    stride,
                    1u32,
                    fourcc,
                    width,
                    height,
                    modifier,
                    false,
                ),
            )
            .await
            .expect("Failed to call ScanoutDMABUF2");

        let event = rx.recv().await.expect("Should receive event");
        if let QemuEvent::ScanoutDmabuf2 { width: w, height: h, stride: s, fourcc: f, .. } = event {
            assert_eq!(w, 1920);
            assert_eq!(h, 1080);
            assert_eq!(s[0], 7680);
            assert_eq!(f, 0x34325258);
        } else {
            panic!("Expected ScanoutDmabuf2 event, got {:?}", event);
        }
    }

    /// 测试 UpdateDmabuf 消息
    #[tokio::test]
    async fn test_update_dmabuf() {
        let (_server_conn, rx, client_conn) = setup_mock_env().await;

        client_conn
            .call_method(
                Some("org.qemu.Display1.Listener"),
                "/org/qemu/Display1/Listener",
                Some("org.qemu.Display1.Listener"),
                "UpdateDMABUF",
                &(10i32, 20i32, 100i32, 100i32),
            )
            .await
            .expect("Failed to call UpdateDMABUF");

        let event = rx.recv().await.expect("Should receive event");
        if let QemuEvent::UpdateDmabuf { x, y, width, height } = event {
            assert_eq!(x, 10);
            assert_eq!(y, 20);
            assert_eq!(width, 100);
            assert_eq!(height, 100);
        } else {
            panic!("Expected UpdateDmabuf event, got {:?}", event);
        }
    }

    // ========== 端到端测试 ==========

    /// 完整的消息流程测试
    #[tokio::test]
    async fn test_full_message_passing_flow() {
        let (_server_conn, rx, client_conn) = setup_mock_env().await;

        // --- 测试场景 1: 基础 Scanout ---
        client_conn
            .call_method(
                Some("org.qemu.Display1.Listener"),
                "/org/qemu/Display1/Listener",
                Some("org.qemu.Display1.Listener"),
                "Scanout",
                &(100u32, 100u32, 400u32, 1u32, vec![0u8, 255u8, 0u8, 255u8]),
            )
            .await
            .expect("Failed to call Scanout");

        let event = rx.recv().await.expect("Should receive event");
        if let QemuEvent::Scanout { width, height, .. } = event {
            assert_eq!(width, 100);
            assert_eq!(height, 100);
        } else {
            panic!("Expected Scanout event, got {:?}", event);
        }

        // --- 测试场景 2: Update ---
        client_conn
            .call_method(
                Some("org.qemu.Display1.Listener"),
                "/org/qemu/Display1/Listener",
                Some("org.qemu.Display1.Listener"),
                "Update",
                &(10i32, 10i32, 50i32, 50i32, 200u32, 1u32, vec![255u8]),
            )
            .await
            .expect("Failed to call Update");

        let event = rx.recv().await.expect("Should receive event");
        if let QemuEvent::Update { x, y, width, height, .. } = event {
            assert_eq!(x, 10);
            assert_eq!(y, 10);
            assert_eq!(width, 50);
            assert_eq!(height, 50);
        } else {
            panic!("Expected Update event, got {:?}", event);
        }

        // --- 测试场景 3: DMABUF ---
        let fd = create_dummy_fd();
        client_conn
            .call_method(
                Some("org.qemu.Display1.Listener"),
                "/org/qemu/Display1/Listener",
                Some("org.qemu.Display1.Listener"),
                "ScanoutDMABUF",
                &(fd, 1920u32, 1080u32, 7680u32, 0x34325258u32, 0u64, false),
            )
            .await
            .expect("Failed to call ScanoutDMABUF");

        let event = rx.recv().await.expect("Should receive event");
        if let QemuEvent::ScanoutDmabuf { width, height, fourcc, .. } = event {
            assert_eq!(width, 1920);
            assert_eq!(height, 1080);
            assert_eq!(fourcc, 0x34325258);
        } else {
            panic!("Expected ScanoutDmabuf event, got {:?}", event);
        }

        // --- 测试场景 4: 鼠标控制 ---
        client_conn
            .call_method(
                Some("org.qemu.Display1.Listener"),
                "/org/qemu/Display1/Listener",
                Some("org.qemu.Display1.Listener"),
                "MouseSet",
                &(50i32, 50i32, 1i32),
            )
            .await
            .expect("Failed to call MouseSet");

        let event = rx.recv().await.expect("Should receive event");
        if let QemuEvent::MouseSet { x, y, on } = event {
            assert_eq!(x, 50);
            assert_eq!(y, 50);
            assert_eq!(on, 1);
        } else {
            panic!("Expected MouseSet event, got {:?}", event);
        }

        // --- 测试场景 5: 光标定义 ---
        client_conn
            .call_method(
                Some("org.qemu.Display1.Listener"),
                "/org/qemu/Display1/Listener",
                Some("org.qemu.Display1.Listener"),
                "CursorDefine",
                &(32i32, 32i32, 0i32, 0i32, vec![0u8; 32 * 32 * 4]),
            )
            .await
            .expect("Failed to call CursorDefine");

        let event = rx.recv().await.expect("Should receive event");
        if let QemuEvent::CursorDefine { width, height, data, .. } = event {
            assert_eq!(width, 32);
            assert_eq!(height, 32);
            assert_eq!(data.len(), 32 * 32 * 4);
        } else {
            panic!("Expected CursorDefine event, got {:?}", event);
        }
    }

    // ========== 压力测试 ==========

    /// 测试大量连续消息
    #[tokio::test]
    async fn test_high_throughput() {
        let (_server_conn, rx, client_conn) = setup_mock_env().await;

        let msg_count = 100u32;

        // 发送大量 Scanout 消息
        for i in 0..msg_count {
            client_conn
                .call_method(
                    Some("org.qemu.Display1.Listener"),
                    "/org/qemu/Display1/Listener",
                    Some("org.qemu.Display1.Listener"),
                    "Scanout",
                    &(i * 10, i * 10, 400u32, 1u32, vec![0u8; 400]),
                )
                .await
                .expect("Failed to call Scanout");
        }

        // 接收并验证所有消息
        for i in 0..msg_count {
            let event = rx.recv().await.expect("Should receive event");
            if let QemuEvent::Scanout { width, height, .. } = event {
                assert_eq!(width, i * 10);
                assert_eq!(height, i * 10);
            } else {
                panic!("Expected Scanout event, got {:?}", event);
            }
        }
    }
}
