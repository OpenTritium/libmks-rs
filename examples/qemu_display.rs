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
//! 3. For each console: a. Create Listener server (org.qemu.Display1.Listener) b. Register listener with console (sends
//!    file descriptor) c. Set UI info (display dimensions)
//! 4. Forward keyboard/mouse events from InputHandler to QEMU

use libmks_rs::{
    dbus::{
        console::{ConsoleController, ConsoleSession},
        listener::{self, Event as QemuEvent},
        vm,
    },
    display::{
        input_daemon::{InputBusSetup, InputDaemon, InputStateEvent},
        input_handler::InputHandler,
        vm_display::{GrabShortcut, InputMode, Message as VmDisplayMsg, ScalingMode, VmDisplayInit, VmDisplayModel},
    },
};
use log::{error, info, warn};
use relm4::{Controller, gtk::prelude::*, prelude::*};
use std::{
    num::{NonZeroU16, NonZeroU32},
    path::PathBuf,
};

/// Must hold these connections for the lifetime of the application
struct AppResources {
    /// D-Bus connection to QEMU (kept alive to maintain the connection)
    _conn: zbus::Connection,
    /// Listener D-Bus connection (kept alive to receive display events)
    _listener_conn: zbus::Connection,
    /// Console session (kept alive to keep background tasks running)
    _console_session: ConsoleSession,
    /// Input daemon (kept alive to keep input worker and watchers running)
    _input_daemon: InputDaemon,
}

struct AppModel {
    display: Option<Controller<VmDisplayModel>>,
    resources: Option<AppResources>,
    main_container: gtk::Overlay,
    scaling_mode: ScalingMode,
    input_mode: InputMode,
    guest_mouse_is_absolute: Option<bool>,
}

enum AppMsg {
    Ignore,
    SetScalingMode(ScalingMode),
    SetInputMode(InputMode),
    GuestMouseModeChanged {
        is_absolute: bool,
    },
    Connected {
        resources: AppResources,
        console_ctrl: ConsoleController,
        input_handler: InputHandler,
        input_state_rx: kanal::AsyncReceiver<InputStateEvent>,
        event_rx: kanal::AsyncReceiver<QemuEvent>,
    },
    ConnectFailed(String),
}

impl std::fmt::Debug for AppMsg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ignore => write!(f, "Ignore"),
            Self::SetScalingMode(mode) => f.debug_tuple("SetScalingMode").field(mode).finish(),
            Self::SetInputMode(mode) => f.debug_tuple("SetInputMode").field(mode).finish(),
            Self::GuestMouseModeChanged { is_absolute } => {
                f.debug_struct("GuestMouseModeChanged").field("is_absolute", is_absolute).finish()
            }
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

            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,

                gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_spacing: 10,
                    set_margin_all: 10,

                    gtk::Label {
                        set_label: "Scaling:",
                    },
                    #[name = "scale_dropdown"]
                    gtk::DropDown {
                        set_model: Some(&gtk::StringList::new(&[
                            "Resize Guest (Auto)",
                            "Fixed Guest (Scaled)",
                        ])),
                        #[watch]
                        set_selected: match model.scaling_mode {
                            ScalingMode::ResizeGuest => 0,
                            ScalingMode::FixedGuest => 1,
                        },
                    },

                    gtk::Separator {
                        set_orientation: gtk::Orientation::Vertical,
                    },

                    gtk::Label {
                        set_label: "Input:",
                    },
                    #[name = "input_dropdown"]
                    gtk::DropDown {
                        set_model: Some(&gtk::StringList::new(&[
                            "Seamless (Office/Tablet)",
                            "Locked (Gaming/FPS)",
                        ])),
                        #[watch]
                        set_selected: match model.input_mode {
                            InputMode::Seamless => 0,
                            InputMode::Confined => 1,
                        },

                        #[watch]
                        set_sensitive: model.guest_mouse_is_absolute != Some(false),
                    },

                },

                #[local_ref]
                main_container -> gtk::Overlay {
                    set_hexpand: true,
                    set_vexpand: true,
                }
            }
        }
    }

    fn init(
        (socket_path, mode): (PathBuf, ListenerMode), root: Self::Root, sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        // Create loading spinner widget
        let spinner = gtk::Spinner::builder()
            .spinning(true)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .width_request(64)
            .height_request(64)
            .build();

        let label = gtk::Label::builder().label("Connecting to QEMU...").halign(gtk::Align::Center).build();

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

        // Spawn background connection task using the global Tokio runtime
        // This works because #[tokio::main] creates a persistent multi-threaded runtime
        let sender_clone = sender.clone();
        relm4::spawn_local(async move {
            info!("[BACKGROUND] Starting connection task...");
            match connect_to_qemu(socket_path, mode).await {
                Ok((resources, console_ctrl, input_handler, input_state_rx, event_rx)) => {
                    info!("[BACKGROUND] Connection successful, sending message to UI...");
                    // Note: resources now contains console_session to keep background tasks alive
                    sender_clone.input(AppMsg::Connected {
                        resources,
                        console_ctrl,
                        input_handler,
                        input_state_rx,
                        event_rx,
                    });
                }
                Err(e) => {
                    error!("[BACKGROUND] Connection failed: {}", e);
                    sender_clone.input(AppMsg::ConnectFailed(e.to_string()));
                }
            }
        });

        // Initial model with loading state
        let model = AppModel {
            display: None,
            resources: None,
            main_container: main_container.clone(),
            scaling_mode: ScalingMode::ResizeGuest,
            input_mode: InputMode::Seamless,
            guest_mouse_is_absolute: None,
        };
        let widgets = view_output!();

        let sender_clone = sender.clone();
        widgets.scale_dropdown.connect_selected_item_notify(move |dropdown| {
            let mode = match dropdown.selected() {
                0 => ScalingMode::ResizeGuest,
                1 => ScalingMode::FixedGuest,
                _ => return,
            };
            sender_clone.input(AppMsg::SetScalingMode(mode));
        });

        let sender_clone = sender.clone();
        widgets.input_dropdown.connect_selected_item_notify(move |dropdown| {
            let mode = match dropdown.selected() {
                0 => InputMode::Seamless,
                1 => InputMode::Confined,
                _ => return,
            };
            sender_clone.input(AppMsg::SetInputMode(mode));
        });

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::Ignore => {}
            AppMsg::SetScalingMode(mode) => {
                if self.scaling_mode == mode {
                    return;
                }
                self.scaling_mode = mode;
                if let Some(display) = &self.display {
                    display.emit(VmDisplayMsg::SetScalingMode(mode));
                }
            }
            AppMsg::SetInputMode(mode) => {
                let effective_mode = if self.guest_mouse_is_absolute == Some(false) && mode == InputMode::Seamless {
                    warn!("Guest mouse is relative; forcing InputMode::Confined");
                    InputMode::Confined
                } else {
                    mode
                };
                if self.input_mode == effective_mode {
                    return;
                }
                self.input_mode = effective_mode;
                if let Some(display) = &self.display {
                    display.emit(VmDisplayMsg::SetInputCaptureMode(effective_mode));
                }
            }
            AppMsg::GuestMouseModeChanged { is_absolute } => {
                self.guest_mouse_is_absolute = Some(is_absolute);
                if !is_absolute && self.input_mode != InputMode::Confined {
                    self.input_mode = InputMode::Confined;
                    if let Some(display) = &self.display {
                        display.emit(VmDisplayMsg::SetInputCaptureMode(InputMode::Confined));
                    }
                }
                if let Some(display) = &self.display {
                    display.emit(VmDisplayMsg::MouseModeChanged { is_absolute });
                }
            }
            AppMsg::Connected { resources, console_ctrl, input_handler, input_state_rx, event_rx } => {
                info!("[UPDATE] Connected message received, setting up display...");

                // 1. Store resources to keep D-Bus connections alive
                self.resources = Some(resources);

                // 2. Launch VmDisplayModel
                let display_controller = VmDisplayModel::builder()
                    .launch(VmDisplayInit {
                        rx: event_rx,
                        console_ctrl,
                        input_handler,
                        grab_shortcut: GrabShortcut::default(),
                    })
                    .forward(sender.input_sender(), |_| AppMsg::Ignore);

                // 3. Listen to input state events and forward to UI
                {
                    let app_sender = sender.input_sender().clone();
                    relm4::spawn_local(async move {
                        while let Ok(event) = input_state_rx.recv().await {
                            if let InputStateEvent::MouseIsAbsolute(is_abs) = event {
                                app_sender.emit(AppMsg::GuestMouseModeChanged { is_absolute: is_abs });
                            }
                        }
                    });
                }

                // 4. Get the widget and replace loading with VM display
                let display_widget = display_controller.widget();
                self.main_container.set_child(Some(display_widget));

                // 5. Store controller to prevent it from being dropped
                self.display = Some(display_controller);
                if let Some(display) = &self.display {
                    display.emit(VmDisplayMsg::SetScalingMode(self.scaling_mode));
                    display.emit(VmDisplayMsg::SetInputCaptureMode(self.input_mode));
                    if let Some(is_absolute) = self.guest_mouse_is_absolute {
                        display.emit(VmDisplayMsg::MouseModeChanged { is_absolute });
                    }
                }
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
/// Four levels of D-Bus Display capabilities:
/// - Scanout: Core interface only, QEMU sends framebuffer via D-Bus
/// - ScanoutDMABUF: Core + single-plane DMA-BUF (always enabled in core)
/// - ScanoutDMABUF2: Core + single-plane + multi-plane DMA-BUF
/// - ScanoutMap: Core + Unix.Map shared-memory scanout
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ListenerMode {
    /// Basic scanout mode - QEMU sends framebuffer via D-Bus (may still use single-plane DMABUF)
    #[default]
    Scanout,
    /// Same as Scanout (single-plane DMABUF is always in core interface)
    ScanoutDMABUF,
    /// Full DMABUF support - multi-plane DMA-BUF enabled
    ScanoutDMABUF2,
    /// Shared-memory map support via org.qemu.Display1.Listener.Unix.Map
    ScanoutMap,
}

impl ListenerMode {
    const CLI_MODES: &'static str = "scanout | scanoutdmabuf | scanoutdmabuf2 | scanoutmap";

    fn parse_cli(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "scanout" => Some(Self::Scanout),
            "scanoutdmabuf" | "dmabuf" => Some(Self::ScanoutDMABUF),
            "scanoutdmabuf2" | "dmabuf2" | "gl" => Some(Self::ScanoutDMABUF2),
            "scanoutmap" | "scnoutmap" | "map" => Some(Self::ScanoutMap),
            _ => None,
        }
    }
}

fn print_usage(bin: &str) {
    eprintln!("Usage: {} <qemu-dbus-socket-path> [mode]", bin);
    eprintln!();
    eprintln!("Arguments:");
    eprintln!("  qemu-dbus-socket-path  Path to QEMU D-Bus socket");
    eprintln!("  mode                   Listener mode: {} (default: scanout)", ListenerMode::CLI_MODES);
    eprintln!();
    eprintln!("Examples:");
    eprintln!("  {} /run/user/1000/qemu-dbus-p2p.0", bin);
    eprintln!("  {} /run/user/1000/qemu-dbus-p2p.0 scanoutdmabuf", bin);
    eprintln!("  {} /run/user/1000/qemu-dbus-p2p.0 scanoutdmabuf2", bin);
    eprintln!("  {} /run/user/1000/qemu-dbus-p2p.0 scanoutmap", bin);
}

/// Connect to QEMU and return the event receiver for display updates.
///
/// Connect to QEMU and send events via the provided kanal async channel.
///
/// Returns:
/// - resources: D-Bus connections that must be kept alive
/// - console_ctrl: Console controller for sending commands
/// - input_handler: UI-thread input handler that sends commands to the input worker
/// - input_state_rx: Input state events (mouse mode/modifiers/touch slots)
/// - event_rx: Receiver for display events (to pass to VmDisplayModel)
async fn connect_to_qemu(
    socket_path: std::path::PathBuf, mode: ListenerMode,
) -> anyhow::Result<(
    AppResources,
    ConsoleController,
    InputHandler,
    kanal::AsyncReceiver<InputStateEvent>,
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
            zbus::connection::Builder::unix_stream(socket).p2p().build().await?
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
        if let vm::Event::ConsoleIds(ids) = event {
            info!("VM has {} consoles: {:?}", ids.len(), ids);
            console_ids = Some(ids);
            break;
        }
    }

    let console_id = *console_ids
        .ok_or_else(|| anyhow::anyhow!("No consoles available from QEMU"))?
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
    let mut has_multitouch = false;

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
                                has_multitouch = ifaces.contains(&"org.qemu.Display1.MultiTouch".to_string());
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

    // ================================================================
    // FIX: Use correct timing - send FD to QEMU first, then establish D-Bus connection
    // Key: .serve_at() must be called before .build()!
    // Reference: https://gitlab.com/marcandre.lureau/qemu-display
    // ================================================================

    info!("Creating socketpair for listener...");
    let (socket_server, socket_client) = std::os::unix::net::UnixStream::pair()?;
    info!("Socketpair created");

    // Send client_fd to QEMU first!
    // This is key: QEMU needs to receive the fd before it can respond to D-Bus handshake
    use std::os::fd::OwnedFd;
    let std_fd: OwnedFd = socket_client.into();
    let fd: zvariant::OwnedFd = std_fd.into();
    console_ctrl.register_listener(fd)?;
    info!("Listener registered with console (fd sent to QEMU)");

    // Now establish D-Bus connection
    // Key: .serve_at() must be called before .build() so the object is registered when connection is built
    info!("Creating listener D-Bus connection...");

    // Create listener handler
    let enable_dmabuf2 = mode == ListenerMode::ScanoutDMABUF2;
    let enable_map = mode == ListenerMode::ScanoutMap;
    let listener_opts = listener::Options::builder().with_dmabuf2(enable_dmabuf2).with_map(enable_map).build();
    info!("Listener mode: {:?}, DMABUF2: {}, MAP: {}", mode, enable_dmabuf2, enable_map);

    // Register all interfaces on the builder before .build()
    // so QEMU sees the full interface set on first introspection.
    let mut builder = zbus::connection::Builder::unix_stream(socket_server).p2p();
    builder = builder
        .serve_at("/org/qemu/Display1/Listener", listener::Listener::from_opts(listener_opts, event_tx.clone()))?;
    if enable_dmabuf2 {
        let dmabuf2_handler = listener::Dmabuf2Handler(event_tx.clone());
        builder = builder.serve_at("/org/qemu/Display1/Listener", dmabuf2_handler)?;
    }
    if enable_map {
        let map_handler = listener::MapHandler(event_tx.clone());
        builder = builder.serve_at("/org/qemu/Display1/Listener", map_handler)?;
    }

    let listener_conn = builder.build().await?;
    info!("Listener D-Bus connection established with all interfaces");

    info!("Listener server registered at /org/qemu/Display1/Listener");

    // Set UI info
    let width_mm = NonZeroU16::new(1).unwrap();
    let height_mm = NonZeroU16::new(1).unwrap();
    let console_width = NonZeroU32::new(console_width.max(1)).unwrap();
    let console_height = NonZeroU32::new(console_height.max(1)).unwrap();
    console_ctrl.set_ui_info(width_mm, height_mm, 0, 0, console_width, console_height)?;
    info!("Set UI info: {}x{}", console_width, console_height);

    // Build unified input bus from discovered console interfaces.
    info!("Setting up input bus: keyboard={}, mouse={}, multitouch={}", has_keyboard, has_mouse, has_multitouch);
    let input_setup = InputBusSetup::builder()
        .conn(conn.clone())
        .console_path(console_path.clone())
        .with_keyboard(has_keyboard)
        .with_mouse(has_mouse)
        .with_multitouch(has_multitouch)
        .build();
    let (input_handler, input_state_rx, input_daemon) = input_setup.dispatch().await?;

    // Create resources package to keep connections alive
    // Note: console_session must be kept alive to maintain background tasks (watch_task, cmd_handler)
    let resources = AppResources {
        _conn: conn,
        _listener_conn: listener_conn,
        _console_session: console_session,
        _input_daemon: input_daemon,
    };

    Ok((resources, console_ctrl, input_handler, input_state_rx, event_rx))
}

#[tokio::main]
async fn main() {
    // Parse command line arguments
    let args: Vec<String> = std::env::args().collect();
    let bin = args.first().map_or("qemu_display", String::as_str);

    let (socket_path, mode) = match args.len() {
        2 => (PathBuf::from(&args[1]), ListenerMode::Scanout),
        3 => {
            let path = PathBuf::from(&args[1]);
            let mode = match ListenerMode::parse_cli(args[2].as_str()) {
                Some(mode) => mode,
                None => {
                    eprintln!("Error: invalid mode '{}'", args[2]);
                    print_usage(bin);
                    std::process::exit(2);
                }
            };
            (path, mode)
        }
        _ => {
            print_usage(bin);
            std::process::exit(1);
        }
    };

    // Initialize logging
    env_logger::Builder::from_default_env().filter_level(log::LevelFilter::Info).init();

    info!("Starting QEMU D-Bus Display example, mode: {:?}", mode);

    // Run the GTK application
    // Important: Use with_args to hide socket path from GTK
    // GTK will try to "open files" if it sees unknown arguments
    let app = RelmApp::new("com.falcon.display.qemu");
    app.with_args(vec![args[0].clone()]).run::<AppModel>((socket_path, mode));
}
