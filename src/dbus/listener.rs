//! # QEMU Display Listener
//!
//! D-Bus server for `org.qemu.Display1.Listener` with zero-loss event forwarding.
//!
//! Three interfaces on one object path fan into a single event channel:
//!
//! * `org.qemu.Display1.Listener` — Core framebuffer/cursor events
//! * `org.qemu.Display1.Listener.Unix.ScanoutDMABUF2` — Multi-plane GPU offload
//! * `org.qemu.Display1.Listener.Unix.Map` — Shared memory (memfd)
//!
//! Reference: <https://www.qemu.org/docs/master/interop/dbus-display.html#org.qemu.Display1.Listener-section>
use crate::MksResult;
use Event::*;
use derive_more::{AsRef, Deref, From, Into};
use kanal::{AsyncReceiver, AsyncSender};
use std::{borrow::Borrow, fmt};
use typed_builder::TypedBuilder;
use zbus::{Connection, DBusError, interface};
use zvariant::{OwnedFd, Type};

/// Errors returned by listener methods.
#[derive(Debug, DBusError)]
#[zbus(prefix = "org.qemu.Display1.Listener")]
pub enum EmitError {
    /// Event channel closed before the event could be forwarded.
    ChannelClosed(String),
}

/// Byte payload wrapper used in listener events.
#[derive(Clone, PartialEq, Eq, AsRef, Deref, From, Into, Type)]
pub struct Blob(pub Vec<u8>);

impl fmt::Debug for Blob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "Blob(len={})", self.0.len()) }
}

impl Borrow<[u8]> for Blob {
    #[inline]
    fn borrow(&self) -> &[u8] { &self.0 }
}

/// Unified event stream for all listener interfaces.
#[derive(Debug, PartialEq)]
pub enum Event {
    /// Full framebuffer image.
    Scanout { width: u32, height: u32, stride: u32, pixman_format: u32, data: Blob },
    /// Partial framebuffer rectangle update.
    Update { x: i32, y: i32, width: i32, height: i32, stride: u32, pixman_format: u32, data: Blob },
    /// Framebuffer export through a single DMABUF fd.
    ScanoutDmabuf { dmabuf: OwnedFd, width: u32, height: u32, stride: u32, fourcc: u32, modifier: u64, y0_top: bool },
    /// Partial update for the current DMABUF scanout.
    UpdateDmabuf { x: i32, y: i32, width: i32, height: i32 },
    /// Disable display output.
    Disable,
    /// Update host cursor position/visibility.
    MouseSet { x: i32, y: i32, on: i32 },
    /// Define a cursor image and hotspot.
    CursorDefine { width: i32, height: i32, hot_x: i32, hot_y: i32, data: Blob },
    /// Multi-plane DMABUF scanout payload.
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
    /// Framebuffer export through shared memory mapping.
    ScanoutMap { memfd: OwnedFd, offset: u32, width: u32, height: u32, stride: u32, pixman_format: u32 },
    /// Partial update for the current mapped scanout.
    UpdateMap { x: i32, y: i32, width: i32, height: i32 },
}

trait EventEmitter {
    fn sender(&self) -> &AsyncSender<Event>;

    async fn emit(&self, event: Event) -> Result<(), EmitError> {
        self.sender().send(event).await.map_err(|e| EmitError::ChannelClosed(e.to_string()))
    }
}

/// Main implementation of `org.qemu.Display1.Listener`.
pub struct Listener {
    /// Event sink shared by all interface handlers.
    pub tx: AsyncSender<Event>,
    /// Interface names exposed by the `Interfaces` property.
    pub ifaces: Box<[&'static str]>,
}

impl EventEmitter for Listener {
    #[inline]
    fn sender(&self) -> &AsyncSender<Event> { &self.tx }
}

#[interface(name = "org.qemu.Display1.Listener", spawn = false, introspection_docs = false)]
impl Listener {
    async fn scanout(
        &self, width: u32, height: u32, stride: u32, pixman_format: u32, data: Vec<u8>,
    ) -> Result<(), EmitError> {
        self.emit(Scanout { width, height, stride, pixman_format, data: data.into() }).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn update(
        &self, x: i32, y: i32, width: i32, height: i32, stride: u32, pixman_format: u32, data: Vec<u8>,
    ) -> Result<(), EmitError> {
        self.emit(Update { x, y, width, height, stride, pixman_format, data: data.into() }).await
    }

    #[allow(clippy::too_many_arguments)]
    #[zbus(name = "ScanoutDMABUF")]
    async fn scanout_dmabuf(
        &self, dmabuf: OwnedFd, width: u32, height: u32, stride: u32, fourcc: u32, modifier: u64, y0_top: bool,
    ) -> Result<(), EmitError> {
        self.emit(ScanoutDmabuf { dmabuf, width, height, stride, fourcc, modifier, y0_top }).await
    }

    #[zbus(name = "UpdateDMABUF")]
    async fn update_dmabuf(&self, x: i32, y: i32, width: i32, height: i32) -> Result<(), EmitError> {
        self.emit(UpdateDmabuf { x, y, width, height }).await
    }

    async fn disable(&self) -> Result<(), EmitError> { self.emit(Event::Disable).await }

    async fn mouse_set(&self, x: i32, y: i32, on: i32) -> Result<(), EmitError> {
        self.emit(MouseSet { x, y, on }).await
    }

    async fn cursor_define(
        &self, width: i32, height: i32, hot_x: i32, hot_y: i32, data: Vec<u8>,
    ) -> Result<(), EmitError> {
        self.emit(CursorDefine { width, height, hot_x, hot_y, data: data.into() }).await
    }

    #[zbus(property(emits_changed_signal = "const"))]
    async fn interfaces(&self) -> zbus::fdo::Result<Vec<String>> {
        Ok(self.ifaces.iter().map(|s| s.to_string()).collect())
    }
}

/// Core listener interface name.
pub const IFACE_DISPLAY_LISTENER: &str = "org.qemu.Display1.Listener";
/// Optional shared-memory scanout interface name.
pub const IFACE_SCANOUT_MAP: &str = "org.qemu.Display1.Listener.Unix.Map";
/// Optional multi-plane DMABUF scanout interface name.
pub const IFACE_SCANOUT_DMABUF2: &str = "org.qemu.Display1.Listener.Unix.ScanoutDMABUF2";

/// Feature flags controlling which listener interfaces are exported.
#[derive(TypedBuilder, Clone, Debug)]
pub struct Options {
    #[builder(default = true)]
    /// Export `org.qemu.Display1.Listener.Unix.ScanoutDMABUF2`.
    pub with_dmabuf2: bool,
    #[builder(default = false)]
    /// Export `org.qemu.Display1.Listener.Unix.Map`.
    pub with_map: bool,
}

impl Listener {
    /// Builds a listener and computes the `Interfaces` property from [`Options`].
    #[inline]
    pub fn from_opts(opts: Options, tx: AsyncSender<Event>) -> Self {
        let mut ifaces = Vec::with_capacity(3);
        ifaces.push(IFACE_DISPLAY_LISTENER);
        if opts.with_dmabuf2 {
            ifaces.push(IFACE_SCANOUT_DMABUF2);
        }
        if opts.with_map {
            ifaces.push(IFACE_SCANOUT_MAP);
        }
        Self { tx, ifaces: ifaces.into_boxed_slice() }
    }
}

/// Handler for `org.qemu.Display1.Listener.Unix.ScanoutDMABUF2`.
#[derive(Debug, Clone, AsRef, Deref)]
pub struct Dmabuf2Handler(pub AsyncSender<Event>);

impl EventEmitter for Dmabuf2Handler {
    #[inline]
    fn sender(&self) -> &AsyncSender<Event> { &self.0 }
}

#[interface(name = "org.qemu.Display1.Listener.Unix.ScanoutDMABUF2", spawn = false, introspection_docs = false)]
impl Dmabuf2Handler {
    #[zbus(name = "ScanoutDMABUF2")]
    #[allow(clippy::too_many_arguments)]
    async fn scanout_dmabuf2(
        &self, dmabuf: Vec<OwnedFd>, x: u32, y: u32, width: u32, height: u32, offset: Vec<u64>, stride: Vec<u32>,
        num_planes: u32, fourcc: u32, backing_width: u32, backing_height: u32, modifier: u64, y0_top: bool,
    ) -> Result<(), EmitError> {
        self.emit(ScanoutDmabuf2 {
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
        })
        .await
    }
}

/// Handler for `org.qemu.Display1.Listener.Unix.Map`.
#[derive(Debug, Clone, AsRef, Deref)]
pub struct MapHandler(pub AsyncSender<Event>);

impl EventEmitter for MapHandler {
    #[inline]
    fn sender(&self) -> &AsyncSender<Event> { &self.0 }
}

#[interface(name = "org.qemu.Display1.Listener.Unix.Map", spawn = false, introspection_docs = false)]
impl MapHandler {
    #[allow(clippy::too_many_arguments)]
    async fn scanout_map(
        &self, memfd: OwnedFd, offset: u32, width: u32, height: u32, stride: u32, pixman_format: u32,
    ) -> Result<(), EmitError> {
        self.emit(ScanoutMap { memfd, offset, width, height, stride, pixman_format }).await
    }

    async fn update_map(&self, x: i32, y: i32, width: i32, height: i32) -> Result<(), EmitError> {
        self.emit(UpdateMap { x, y, width, height }).await
    }
}

/// Registers listener objects on `/org/qemu/Display1/Listener` and returns the event receiver.
pub async fn serve(conn: &Connection, opts: Options) -> MksResult<AsyncReceiver<Event>> {
    let (event_tx, event_rx) = kanal::bounded_async::<Event>(8192);
    const LISTENER_PATH: &str = "/org/qemu/Display1/Listener";
    let handler = Listener::from_opts(opts.clone(), event_tx.clone());
    conn.object_server().at(LISTENER_PATH, handler).await?;
    if opts.with_dmabuf2 {
        let dmabuf2_handler = Dmabuf2Handler(event_tx.clone());
        conn.object_server().at(LISTENER_PATH, dmabuf2_handler).await?;
    }
    if opts.with_map {
        let map_handler = MapHandler(event_tx);
        conn.object_server().at(LISTENER_PATH, map_handler).await?;
    }
    Ok(event_rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    fn create_dummy_fd() -> OwnedFd {
        let file = File::open("/dev/null").expect("Failed to open /dev/null");
        let std_fd: std::os::fd::OwnedFd = file.into();
        OwnedFd::from(std_fd)
    }

    async fn setup_mock_env() -> (zbus::Connection, AsyncReceiver<Event>, zbus::Connection) {
        use zbus::Guid;

        let (sock1, sock2) = std::os::unix::net::UnixStream::pair().expect("Failed to create socket pair");

        let server_future = async move {
            let conn = zbus::connection::Builder::unix_stream(sock1)
                .p2p()
                .server(Guid::generate())
                .expect("Failed to set server mode")
                .build()
                .await
                .expect("Failed to build server connection");

            let rx = serve(&conn, Options::builder().build()).await.expect("Failed to serve");
            (conn, rx)
        };

        let client_future = async move {
            zbus::connection::Builder::unix_stream(sock2)
                .p2p()
                .build()
                .await
                .expect("Failed to build client connection")
        };

        let ((server_conn, rx), client_conn) = tokio::join!(server_future, client_future);

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        (server_conn, rx, client_conn)
    }

    #[tokio::test]
    async fn test_serve_registers_objects() {
        let (server_conn, _rx, _client_conn) = setup_mock_env().await;

        let result = server_conn.object_server().interface::<_, Listener>("/org/qemu/Display1/Listener").await;
        assert!(result.is_ok(), "Listener should be registered");

        let result = server_conn.object_server().interface::<_, Dmabuf2Handler>("/org/qemu/Display1/Listener").await;
        assert!(result.is_ok(), "Dmabuf2Handler should be registered");
    }

    #[tokio::test]
    async fn test_map_handler_conditional_registration() {
        use zbus::Guid;

        let (sock1, sock2) = std::os::unix::net::UnixStream::pair().expect("Failed to create socket pair");

        let server_handle = tokio::spawn(async move {
            let server_conn = zbus::connection::Builder::unix_stream(sock1)
                .p2p()
                .server(Guid::generate())
                .expect("Failed to set server mode")
                .build()
                .await
                .expect("Failed to build server connection");

            let _rx = serve(&server_conn, Options::builder().with_map(false).build()).await.expect("Failed to serve");
            server_conn
        });

        let _client_conn = zbus::connection::Builder::unix_stream(sock2)
            .p2p()
            .build()
            .await
            .expect("Failed to build client connection");

        let server_conn = server_handle.await.unwrap();

        let result = server_conn.object_server().interface::<_, MapHandler>("/org/qemu/Display1/Listener").await;
        assert!(result.is_err(), "MapHandler should not be registered when with_map=false");

        let (sock1, sock2) = std::os::unix::net::UnixStream::pair().expect("Failed to create socket pair");

        let server_handle = tokio::spawn(async move {
            let server_conn = zbus::connection::Builder::unix_stream(sock1)
                .p2p()
                .server(Guid::generate())
                .expect("Failed to set server mode")
                .build()
                .await
                .expect("Failed to build server connection");

            let _rx = serve(&server_conn, Options::builder().with_map(true).build()).await.expect("Failed to serve");
            server_conn
        });

        let _client_conn = zbus::connection::Builder::unix_stream(sock2)
            .p2p()
            .build()
            .await
            .expect("Failed to build client connection");

        let server_conn = server_handle.await.unwrap();

        let result = server_conn.object_server().interface::<_, MapHandler>("/org/qemu/Display1/Listener").await;
        assert!(result.is_ok(), "MapHandler should be registered when with_map=true");
    }

    #[tokio::test]
    async fn test_basic_scanout() {
        let (_server_conn, rx, client_conn) = setup_mock_env().await;

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

        let event = rx.recv().await.expect("Should receive event");
        if let Event::Scanout { width: w, height: h, data: d, .. } = event {
            assert_eq!(w, 100);
            assert_eq!(h, 100);
            assert_eq!(d.len(), 4);
        } else {
            panic!("Expected Scanout event, got {:?}", event);
        }
    }

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
        if let Event::Scanout { width, height, .. } = event {
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
        if let Event::Update { x, y, width, height, .. } = event {
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
        if let Event::ScanoutDmabuf { width, height, fourcc, .. } = event {
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
        if let Event::MouseSet { x, y, on } = event {
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
        if let Event::CursorDefine { width, height, data, .. } = event {
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
            if let Event::Scanout { width, height, .. } = event {
                assert_eq!(width, i * 10);
                assert_eq!(height, i * 10);
            } else {
                panic!("Expected Scanout event, got {:?}", event);
            }
        }
    }

    // ========== D-Bus 错误名称测试 ==========

    /// 测试自定义错误类型的 Rust 侧转换
    #[tokio::test]
    async fn test_custom_error_names() {
        // 当通道关闭时，应该得到 EmitError::ChannelClosed
        // 它会被序列化为 "org.qemu.Display1.Listener.ChannelClosed"
        let (tx, _rx) = kanal::bounded_async::<Event>(1);
        drop(_rx); // 立即关闭接收端

        let listener = Listener::from_opts(Options::builder().with_dmabuf2(false).with_map(false).build(), tx);

        let result = listener.emit(Event::Disable).await;

        // 验证错误类型
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, EmitError::ChannelClosed(_)));

        // 验证 DBusError trait 生成的错误名称
        assert_eq!(err.name().as_str(), "org.qemu.Display1.Listener.ChannelClosed");
    }

    /// 测试完整的 D-Bus 线路上的错误传播
    ///
    /// 这个测试验证客户端在 D-Bus 协议层面收到的确实是自定义错误名称，
    /// 而不是通用的 "org.freedesktop.DBus.Error.Failed"。
    #[tokio::test]
    async fn test_dbus_error_propagation() {
        let (_server_conn, rx, client_conn) = setup_mock_env().await;

        // 关闭服务端的接收通道，触发 ChannelClosed 错误
        drop(rx);

        // 客户端调用 Disable 方法
        let result = client_conn
            .call_method(
                Some("org.qemu.Display1.Listener"),
                "/org/qemu/Display1/Listener",
                Some("org.qemu.Display1.Listener"),
                "Disable",
                &(),
            )
            .await;

        // 验证客户端收到的是特定的 D-Bus 错误，而不是通用的 Failed
        assert!(result.is_err());
        let err = result.unwrap_err();

        match err {
            zbus::Error::MethodError(name, message, _) => {
                // 验证错误名称是自定义的，而不是通用的 Failed
                assert_eq!(
                    name.as_str(),
                    "org.qemu.Display1.Listener.ChannelClosed",
                    "Expected custom error name, got: {}",
                    name.as_str()
                );
                // 验证错误消息包含了有用的信息（如果有消息的话）
                if let Some(msg) = message {
                    assert!(
                        msg.contains("closed") || msg.contains("channel"),
                        "Error message should mention channel closure: {}",
                        msg
                    );
                }
            }
            other => panic!("Expected MethodError, got {:?}", other),
        }
    }
}
