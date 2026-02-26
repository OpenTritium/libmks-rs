//! Example: Connect to real QEMU instance via D-Bus Display protocol.
//!
//! This example demonstrates connecting to a real QEMU VM using the MKS
//! (QEMU Machine Protocol via D-Bus Display) protocol.
//!
//! ## Usage
//!
//! First, start QEMU with D-Bus display:
//! ```bash
//! qemu-system-x86_64 \
//!   -display dbus \
//!   -spice port=5900,disable-ticketing \
//!   -device virtio-keyboard-pci \
//!   -device virtio-mouse-pci \
//!   ...
//! ```
//!
//! QEMU will output the D-Bus socket path to use. Then run this example:
//! ```bash
//! cargo run --example qemu_display -- /path/to/qemu/dbus/socket
//! ```
//!
//! ## Protocol Handshake (based on C implementation in libmks/)
//!
//! 1. Create P2P D-Bus connection to QEMU via Unix socket
//! 2. Query VM interface at /org/qemu/Display1/VM for console IDs
//! 3. For each console:
//!    a. Create Listener server (org.qemu.Display1.Listener)
//!    b. Register listener with console (sends file descriptor)
//!    c. Set UI info (display dimensions)
//! 4. Forward keyboard/mouse events from InputHandler to QEMU

use libmks_rs::{
    dbus::{
        console::{ConsoleController, ConsoleSession},
        keyboard::KeyboardController,
        listener::{self, Event as QemuEvent},
        mouse::MouseController,
        vm,
    },
    display::{
        input_handler::InputHandler,
        vm_display::{GrabShortcut, VmDisplayInit, VmDisplayModel},
    },
};
use log::{error, info};
use relm4::{Controller, gtk::prelude::*, prelude::*};
use std::path::PathBuf;

/// Must hold these connections for the lifetime of the application
struct AppResources {
    conn: zbus::Connection,
    listener_conn: zbus::Connection,
}

struct AppModel {
    display: Option<Controller<VmDisplayModel>>,
    resources: Option<AppResources>,
    main_container: gtk::Overlay,
}

enum AppMsg {
    Ignore,
    Connected {
        resources: AppResources,
        console_ctrl: ConsoleController,
        mouse_ctrl: MouseController,
        kbd_ctrl: KeyboardController,
        event_rx: kanal::AsyncReceiver<QemuEvent>,
    },
    ConnectFailed(String),
}

impl std::fmt::Debug for AppMsg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ignore => write!(f, "Ignore"),
            Self::Connected { .. } => write!(f, "Connected {{ ... }}"),
            Self::ConnectFailed(arg0) => f.debug_tuple("ConnectFailed").field(arg0).finish(),
        }
    }
}

#[relm4::component]
impl SimpleComponent for AppModel {
    type Init = (PathBuf, ListenerMode);
    type Input = AppMsg;
    type Output = ();

    view! {
        gtk::Window {
            set_title: Some("VM Display: QEMU D-Bus Connection"),
            set_default_width: 1024,
            set_default_height: 768,

            #[local_ref]
            main_container -> gtk::Overlay {}
        }
    }

    fn init((socket_path, mode): (PathBuf, ListenerMode), root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        // Create a multi-threaded runtime
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("Failed to create tokio runtime");

        // Create loading spinner widget
        let spinner = gtk::Spinner::builder()
            .spinning(true)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .width_request(64)
            .height_request(64)
            .build();

        let label = gtk::Label::builder()
            .label("Connecting to QEMU...")
            .halign(gtk::Align::Center)
            .build();

        let loading_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(16)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        loading_box.append(&spinner);
        loading_box.append(&label);

        // Create main container (Overlay) and set loading as initial child
        let main_container = gtk::Overlay::new();
        main_container.set_child(Some(&loading_box));

        // Spawn background connection task in a separate thread to keep runtime alive
        let sender_clone = sender.clone();
        std::thread::spawn(move || {
            rt.block_on(async move {
                info!("[BACKGROUND] Starting connection task...");
                match connect_to_qemu(socket_path, mode).await {
                    Ok((resources, console_ctrl, mouse_ctrl, kbd_ctrl, event_rx)) => {
                        info!("[BACKGROUND] Connection successful, sending message to UI...");
                        sender_clone.input(AppMsg::Connected {
                            resources,
                            console_ctrl,
                            mouse_ctrl,
                            kbd_ctrl,
                            event_rx,
                        });
                        // Keep the async runtime alive forever
                        futures_util::future::pending::<()>().await;
                    }
                    Err(e) => {
                        error!("[BACKGROUND] Connection failed: {}", e);
                        sender_clone.input(AppMsg::ConnectFailed(e.to_string()));
                    }
                }
            });
        });

        // Initial model with loading state
        let model = AppModel {
            display: None,
            resources: None,
            main_container: main_container.clone(),
        };
        let widgets = view_output!();

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::Ignore => {}
            AppMsg::Connected { resources, console_ctrl, mouse_ctrl, kbd_ctrl, event_rx } => {
                info!("[UPDATE] Connected message received, setting up display...");

                // 1. Store resources to keep D-Bus connections alive
                self.resources = Some(resources);

                // 2. Create input handler
                let input_handler = InputHandler::builder()
                    .mouse(mouse_ctrl)
                    .keyboard(kbd_ctrl)
                    .build();

                // 3. Launch VmDisplayModel
                let display_controller = VmDisplayModel::builder()
                    .launch(VmDisplayInit {
                        rx: event_rx,
                        console_ctrl,
                        input_handler,
                        grab_shortcut: GrabShortcut::default(),
                    })
                    .forward(sender.input_sender(), |_| AppMsg::Ignore);

                // 4. Get the widget and replace loading with VM display
                let display_widget = display_controller.widget();
                self.main_container.set_child(Some(display_widget));

                // 5. Store controller to prevent it from being dropped
                self.display = Some(display_controller);
            }
            AppMsg::ConnectFailed(error_msg) => {
                error!("[UPDATE] Connection failed: {}", error_msg);
                let label = gtk::Label::new(Some(&format!("Error: {}", error_msg)));
                self.main_container.set_child(Some(&label));
            }
        }
    }
}

/// Listener mode - determines what capabilities we advertise to QEMU
///
/// Three levels of D-Bus Display capabilities:
/// - Scanout: Core interface only, QEMU sends framebuffer via D-Bus
/// - ScanoutDMABUF: Core + single-plane DMA-BUF (always enabled in core)
/// - ScanoutDMABUF2: Core + single-plane + multi-plane DMA-BUF
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListenerMode {
    /// Basic scanout mode - QEMU sends framebuffer via D-Bus (may still use single-plane DMABUF)
    Scanout,
    /// Same as Scanout (single-plane DMABUF is always in core interface)
    ScanoutDMABUF,
    /// Full DMABUF support - multi-plane DMA-BUF enabled
    ScanoutDMABUF2,
}

impl Default for ListenerMode {
    fn default() -> Self { Self::Scanout }
}

impl From<&str> for ListenerMode {
    fn from(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "scanout" => Self::Scanout,
            "scanoutdmabuf" | "dmabuf" => Self::ScanoutDMABUF,
            "scanoutdmabuf2" | "dmabuf2" | "gl" => Self::ScanoutDMABUF2,
            _ => Self::Scanout,
        }
    }
}

/// Connect to QEMU and return the event receiver for display updates.
///
/// Connect to QEMU and send events via the provided kanal async channel.
///
/// Returns:
/// - console_ctrl: Console controller for sending commands
/// - resources: D-Bus connections that must be kept alive
/// - console_ctrl: Console controller for sending commands
/// - mouse_ctrl: Mouse controller
/// - keyboard_ctrl: Keyboard controller
/// - event_rx: Receiver for display events (to pass to VmDisplayModel)
async fn connect_to_qemu(
    socket_path: std::path::PathBuf,
    _mode: ListenerMode,
) -> anyhow::Result<(
    AppResources,
    ConsoleController,
    MouseController,
    KeyboardController,
    kanal::AsyncReceiver<QemuEvent>,
)> {
    info!("Connecting to QEMU at {:?}", socket_path);

    // Create channel for display events - this will be used by the listener
    let (event_tx, event_rx) = kanal::bounded_async::<QemuEvent>(8192);

    // Create D-Bus connection to QEMU
    // Support both:
    // 1. Unix socket path (original P2P mode): /path/to/socket
    // 2. D-Bus address (session bus mode): unix:path=/run/user/1000/bus or just "session"
    let conn = if socket_path.as_os_str() == "session" {
        // Connect to session D-Bus
        zbus::connection::Builder::session()
            .map_err(|e| anyhow::anyhow!("Failed to create session D-Bus connection: {}", e))?
            .build()
            .await?
    } else {
        // Try as D-Bus address first (e.g., "unix:path=/path/to/socket")
        let path_str = socket_path.to_string_lossy();
        if path_str.starts_with("unix:") || path_str.contains(':') {
            // Treat as D-Bus address
            let address_str: &str = &path_str;
            zbus::connection::Builder::address(address_str)
                .map_err(|e| anyhow::anyhow!("Failed to create D-Bus connection: {}", e))?
                .build()
                .await?
        } else {
            // Original behavior: treat as Unix socket path
            let socket = std::os::unix::net::UnixStream::connect(&socket_path)?;
            zbus::connection::Builder::unix_stream(socket)
                .p2p()
                .build()
                .await?
        }
    };

    info!("Connected to QEMU via D-Bus");

    // Query VM interface for console IDs
    // Reference: mks-session.c uses GDBusObjectManager to discover objects
    let vm_listener = vm::connect(&conn).await?;
    info!("Connected to VM interface");

    // Get console IDs from VM
    let mut console_ids = None;
    while let Ok(event) = vm_listener.rx.recv().await {
        match event {
            vm::Event::ConsoleIds(ids) => {
                info!("VM has {} consoles: {:?}", ids.len(), ids);
                console_ids = Some(ids);
                break;
            }
            _ => {}
        }
    }

    let console_id = *console_ids.ok_or_else(|| anyhow::anyhow!("No consoles available from QEMU"))?
        .first()
        .ok_or_else(|| anyhow::anyhow!("Empty console list from QEMU"))?;
    info!("Using console {}", console_id);

    // Connect to console session
    // Reference: mks-device.c creates console session for each console
    let console_path = format!("/org/qemu/Display1/Console_{}", console_id);
    let console_session = ConsoleSession::connect(&conn, &console_path).await?;
    let console_ctrl = console_session.tx.clone();
    info!("Connected to console at {}", console_path);

    // Wait for initial console properties
    let mut console_width = 800u32;
    let mut console_height = 600u32;
    let mut has_keyboard = false;
    let mut has_mouse = false;

    // Use timeout to avoid waiting forever
    let mut events_received = 0;
    let timeout = tokio::time::Duration::from_secs(2);
    loop {
        tokio::select! {
            result = console_session.rx.recv() => {
                match result {
                    Ok(event) => {
                        events_received += 1;
                        info!("Console event {}: {:?}", events_received, event);
                        match event {
                            libmks_rs::dbus::console::Event::Width(w) => {
                                console_width = w;
                                info!("Console width: {}", w);
                            }
                            libmks_rs::dbus::console::Event::Height(h) => {
                                console_height = h;
                                info!("Console height: {}", h);
                            }
                            libmks_rs::dbus::console::Event::Interfaces(ifaces) => {
                                has_keyboard = ifaces.contains(&"org.qemu.Display1.Keyboard".to_string());
                                has_mouse = ifaces.contains(&"org.qemu.Display1.Mouse".to_string());
                                info!("Console interfaces: {:?}", ifaces);
                            }
                            _ => {}
                        }
                    }
                    Err(_) => {
                        info!("Console event stream ended, received {} events", events_received);
                        break;
                    }
                }
            }
            _ = tokio::time::sleep(timeout) => {
                info!("Timeout waiting for console events, received {} events", events_received);
                break;
            }
        }
    }

    // Create keyboard and mouse controllers
    // These need proper sessions to be created for the console's keyboard/mouse interfaces
    let kbd_path = format!("/org/qemu/Display1/Console_{}", console_id);
    let mouse_path = format!("/org/qemu/Display1/Console_{}", console_id);

    // Create keyboard session if available
    info!("Creating keyboard session, has_keyboard: {}", has_keyboard);
    let kbd_ctrl = if has_keyboard {
        let kbd_session = libmks_rs::dbus::keyboard::KeyboardSession::connect(&conn, &kbd_path).await?;
        kbd_session.tx.clone()
    } else {
        let (tx, _rx) = kanal::unbounded_async();
        KeyboardController::from(tx)
    };
    info!("Keyboard session created");

    // Create mouse session if available
    info!("Creating mouse session, has_mouse: {}", has_mouse);
    let mouse_ctrl = if has_mouse {
        let mouse_session = libmks_rs::dbus::mouse::MouseSession::connect(&conn, &mouse_path).await?;
        mouse_session.tx.clone()
    } else {
        let (tx, _rx) = kanal::unbounded_async();
        MouseController::from(tx)
    };
    info!("Mouse session created");

    // ================================================================
    // FIX: 使用正确的时序 - 先传 FD 给 QEMU，再建立 D-Bus 连接
    // 关键：.serve_at() 必须在 .build() 之前调用！
    // 参考: https://gitlab.com/marcandre.lureau/qemu-display
    // ================================================================

    info!("Creating socketpair for listener...");
    let (socket_server, socket_client) = std::os::unix::net::UnixStream::pair()?;
    info!("Socketpair created");

    // 先把 client_fd 传给 QEMU！
    // 这是关键：QEMU 需要先收到 fd 才能响应 D-Bus 握手
    use std::os::fd::OwnedFd;
    let std_fd: OwnedFd = socket_client.into();
    let fd: zvariant::OwnedFd = std_fd.into();
    console_ctrl.register_listener(fd).await?;
    info!("Listener registered with console (fd sent to QEMU)");

    // 现在建立 D-Bus 连接
    // 关键：.serve_at() 必须在 .build() 之前调用，这样对象在连接建立时就已经注册了
    info!("Creating listener D-Bus connection...");

    // 创建 listener handler
    let enable_dmabuf2 = _mode == ListenerMode::ScanoutDMABUF2;
    let listener_opts = listener::Options::builder()
        .with_dmabuf2(enable_dmabuf2)
        .with_map(false)
        .build();
    info!("Listener mode: {:?}, DMABUF2: {}", _mode, enable_dmabuf2);

    // Use kanal AsyncSender directly
    // 使用 .serve_at() 在 .build() 之前注册对象
    let builder = zbus::connection::Builder::unix_stream(socket_server)
        .p2p()
        .serve_at("/org/qemu/Display1/Listener", listener::Listener::from_opts(listener_opts.clone(), event_tx.clone()))?;

    // 如果启用了 DMABUF2，也需要注册
    if enable_dmabuf2 {
        // 需要单独处理...
    }

    // 最后才 build
    let listener_conn = builder.build().await?;
    info!("Listener D-Bus connection established");

    // 注册额外的接口
    if listener_opts.with_dmabuf2 {
        let dmabuf2_handler = listener::Dmabuf2Handler(event_tx.clone());
        listener_conn.object_server().at("/org/qemu/Display1/Listener", dmabuf2_handler).await?;
    }
    if listener_opts.with_map {
        let map_handler = listener::MapHandler(event_tx);
        listener_conn.object_server().at("/org/qemu/Display1/Listener", map_handler).await?;
    }

    info!("Listener server registered at /org/qemu/Display1/Listener");

    // Set UI info
    console_ctrl.set_ui_info(
        0, 0, 0, 0,  // width_mm, height_mm, xoff, yoff (use defaults)
        console_width,
        console_height,
    ).await?;
    info!("Set UI info: {}x{}", console_width, console_height);

    // Create resources package to keep connections alive
    let resources = AppResources {
        conn,
        listener_conn,
    };

    Ok((resources, console_ctrl, mouse_ctrl, kbd_ctrl, event_rx))
}

fn main() {
    // Parse command line arguments
    let args: Vec<String> = std::env::args().collect();

    let (socket_path, mode) = match args.len() {
        2 => (PathBuf::from(&args[1]), ListenerMode::Scanout),
        3 => {
            let path = PathBuf::from(&args[1]);
            let mode = ListenerMode::from(args[2].as_str());
            (path, mode)
        }
        _ => {
            eprintln!("用法: {} <qemu-dbus-socket-path> [mode]", args[0]);
            eprintln!("");
            eprintln!("参数:");
            eprintln!("  qemu-dbus-socket-path  QEMU D-Bus socket 路径");
            eprintln!("  mode                  监听模式: scanout | scanoutdmabuf | scanoutdmabuf2 (默认: scanout)");
            eprintln!("");
            eprintln!("示例:");
            eprintln!("  {} /run/user/1000/qemu-dbus-p2p.0", args[0]);
            eprintln!("  {} /run/user/1000/qemu-dbus-p2p.0 scanoutdmabuf2", args[0]);
            std::process::exit(1);
        }
    };

    // Initialize logging
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();

    info!("Starting QEMU D-Bus Display example, mode: {:?}", mode);

    // Run the GTK application
    // Important: Use with_args to hide socket path from GTK
    // GTK will try to "open files" if it sees unknown arguments
    let app = RelmApp::new("com.falcon.display.qemu");
    app.with_args(vec![args[0].clone()]).run::<AppModel>((socket_path, mode));
}
