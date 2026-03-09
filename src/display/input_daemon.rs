use super::input_handler::{Capability, InputHandler};
use crate::{
    MksResult,
    dbus::{
        keyboard::{KeyboardProxy, KeyboardProxyBlocking},
        mouse::{Button, MouseProxy, MouseProxyBlocking},
        multitouch::{Kind as TouchKind, MultiTouchProxy, MultiTouchProxyBlocking},
        utils::fetch_then_update,
    },
    keymaps::Qnum,
    mks_debug, mks_error,
};
use InputStateEvent::*;
use futures_util::{StreamExt, stream::BoxStream};
use kanal::{AsyncReceiver, AsyncSender, Receiver};
use std::mem;
use tokio::task::{AbortHandle, spawn_blocking};
use typed_builder::TypedBuilder;
use zbus::Connection;

const LOG_TARGET: &str = "mks.display.input_bus";

#[derive(Debug, Clone, Copy)]
/// Commands sent to the input daemon thread.
pub enum InputCommand {
    KbdPress(Qnum),
    KbdRelease(Qnum),
    MousePress(Button),
    MouseRelease(Button),
    MouseSetAbs(u32, u32),
    MouseRel(i32, i32),
    Touch { kind: TouchKind, num_slot: u64, x: f64, y: f64 },
    Shutdown,
}

#[derive(Debug, Clone, Copy)]
pub enum WatchCommand {
    Update(Capability),
    Shutdown,
}

#[derive(Debug, Clone, Copy)]
/// State updates emitted by input property watchers.
pub enum InputStateEvent {
    ModifiersChanged(u32),
    MouseIsAbsolute(bool),
    TouchMaxSlots(i32),
}

/// Background input worker and watcher task handles.
pub struct InputDaemon {
    abort: AbortHandle,
    shutdown: Option<Box<dyn FnOnce()>>, // Triggers the blocking I/O thread shutdown.
    shutdown_prop: Option<Box<dyn FnOnce()>>, // Gracefully stops property watchers.
}

impl Drop for InputDaemon {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            shutdown();
        }
        if let Some(shutdown) = self.shutdown_prop.take() {
            shutdown();
        }
        self.abort.abort();
    }
}

#[derive(Debug, Default)]
enum PendingMove {
    #[default]
    None,
    Abs {
        x: u32,
        y: u32,
    },
    Rel {
        dx: i32,
        dy: i32,
    },
}

#[derive(Debug, Default)]
struct PropWatchers {
    keyboard: Option<AbortHandle>,
    mouse: Option<AbortHandle>,
    touch: Option<AbortHandle>,
}

#[derive(Clone, Default)]
struct AsyncInputProxies {
    keyboard: Option<KeyboardProxy<'static>>,
    mouse: Option<MouseProxy<'static>>,
    touch: Option<MultiTouchProxy<'static>>,
}

#[derive(Default)]
struct BlockingInputProxies {
    keyboard: Option<KeyboardProxyBlocking<'static>>,
    mouse: Option<MouseProxyBlocking<'static>>,
    touch: Option<MultiTouchProxyBlocking<'static>>,
}

impl AsyncInputProxies {
    async fn new(conn: &Connection, path: String) -> Self {
        let keyboard = match KeyboardProxy::new(conn, path.clone()).await {
            Ok(proxy) => Some(proxy),
            Err(e) => {
                mks_error!(error:? = e; "Failed to create async keyboard proxy");
                None
            }
        };
        let mouse = match MouseProxy::new(conn, path.clone()).await {
            Ok(proxy) => Some(proxy),
            Err(e) => {
                mks_error!(error:? = e; "Failed to create async mouse proxy");
                None
            }
        };
        let touch = match MultiTouchProxy::new(conn, path).await {
            Ok(proxy) => Some(proxy),
            Err(e) => {
                mks_error!(error:? = e; "Failed to create async multitouch proxy");
                None
            }
        };
        Self { keyboard, mouse, touch }
    }

    fn into_blocking(self) -> BlockingInputProxies {
        BlockingInputProxies {
            keyboard: self.keyboard.map(|proxy| KeyboardProxyBlocking::from(proxy.into_inner())),
            mouse: self.mouse.map(|proxy| MouseProxyBlocking::from(proxy.into_inner())),
            touch: self.touch.map(|proxy| MultiTouchProxyBlocking::from(proxy.into_inner())),
        }
    }
}

#[derive(TypedBuilder)]
/// Builder-style setup for input watchers, command channel, and I/O thread.
pub struct InputBusSetup {
    conn: Connection,
    #[builder(setter(into))]
    console_path: String,
    #[builder(default = true)]
    with_keyboard: bool,
    #[builder(default = true)]
    with_mouse: bool,
    #[builder(default = true)]
    with_multitouch: bool,
}

impl InputBusSetup {
    /// Finalizes setup.
    ///
    /// Returns:
    /// `InputHandler`: command entry for input events and watcher updates.
    /// `AsyncReceiver<InputStateEvent>`: watcher-produced input state events.
    /// `InputDaemon`: guard that owns shutdown hooks and the worker thread.
    pub async fn dispatch(self) -> MksResult<(InputHandler, AsyncReceiver<InputStateEvent>, InputDaemon)> {
        let Self { conn, console_path, with_keyboard, with_mouse, with_multitouch } = self;
        // Initial capabilities are derived from setup flags.
        let init_cap = Capability { keyboard: with_keyboard, mouse: with_mouse, multitouch: with_multitouch };
        let async_proxies = AsyncInputProxies::new(&conn, console_path).await;
        let blocking_proxies = async_proxies.clone().into_blocking();
        // Small buffer: only capability updates and manager shutdown.
        let (watch_cmd_tx, watch_cmd_rx) = kanal::bounded_async::<WatchCommand>(2);
        // Forwarded property-state events.
        let (state_tx, state_rx) = kanal::bounded_async::<InputStateEvent>(8);
        // Bridge from async context into the blocking I/O thread.
        let (input_cmd_tx, input_cmd_rx) = kanal::bounded::<InputCommand>(8192);
        // Start dynamic property watchers.
        spawn_prop_watch_manager(async_proxies, state_tx, watch_cmd_rx, init_cap);
        let handler = InputHandler::builder()
            // Keep a sync sender because relm `update` runs synchronously.
            .input_cmd_tx(input_cmd_tx.clone())
            .watch_cmd_tx(watch_cmd_tx.clone())
            .capability(init_cap)
            .build();
        let shutdown = Box::new(move || {
            if let Err(e) = input_cmd_tx.send(InputCommand::Shutdown) {
                mks_error!(error:? =e; "Failed to send shutdown command to input daemon");
            }
        });
        let shutdown_prop = Box::new(move || {
            if let Err(e) = watch_cmd_tx.as_sync().send(WatchCommand::Shutdown) {
                mks_error!(error:? =e; "Failed to send shutdown command to watcher manager");
            }
        });
        let abort = spawn_blocking_input_thread(blocking_proxies, input_cmd_rx);
        let daemon = InputDaemon { abort, shutdown: Some(shutdown), shutdown_prop: Some(shutdown_prop) };
        Ok((handler, state_rx, daemon))
    }
}

fn spawn_blocking_input_thread(proxies: BlockingInputProxies, cmd_rx: Receiver<InputCommand>) -> AbortHandle {
    let f = move || {
        // Process commands from the synchronous blocking channel.
        while let Ok(mut cmd) = cmd_rx.recv() {
            let mut pending_move = PendingMove::None;
            loop {
                if handle_cmd(cmd, &proxies, &mut pending_move) {
                    mks_debug!("Blocking input thread exited after shutdown command");
                    return;
                }
                match cmd_rx.try_recv() {
                    Ok(Some(next_cmd)) => cmd = next_cmd,
                    Ok(None) => break,
                    Err(_) => return,
                }
            }
            if let Some(proxy) = proxies.mouse.as_ref() {
                flush_mouse_move(&mut pending_move, proxy);
            }
        }
    };
    spawn_blocking(f).abort_handle()
}

/// Return `true` if need shutdown
fn handle_cmd(cmd: InputCommand, proxies: &BlockingInputProxies, pending_move: &mut PendingMove) -> bool {
    use InputCommand::*;
    use PendingMove::*;
    match cmd {
        KbdPress(q) => {
            let Some(proxy) = proxies.keyboard.as_ref() else {
                mks_error!("Keyboard proxy unavailable; dropping key-press event");
                return false;
            };
            if let Err(e) = proxy.press(q) {
                mks_error!(error:? = e; "Failed to send keyboard press command");
            }
        }
        KbdRelease(q) => {
            let Some(proxy) = proxies.keyboard.as_ref() else {
                mks_error!("Keyboard proxy unavailable; dropping key-release event");
                return false;
            };
            if let Err(e) = proxy.release(q) {
                mks_error!(error:? = e; "Failed to send keyboard release command");
            }
        }
        MouseSetAbs(x, y) => {
            let Some(proxy) = proxies.mouse.as_ref() else {
                *pending_move = PendingMove::None;
                return false;
            };
            if matches!(pending_move, Rel { .. }) {
                flush_mouse_move(pending_move, proxy);
            }
            match pending_move {
                None => *pending_move = Abs { x, y },
                Abs { x: px, y: py } => {
                    *px = x;
                    *py = y;
                }
                Rel { .. } => unreachable!("relative move must be flushed before abs"),
            }
        }
        MouseRel(dx, dy) => {
            let Some(proxy) = proxies.mouse.as_ref() else {
                *pending_move = PendingMove::None;
                return false;
            };
            // Flush pending movement when switching motion mode.
            if matches!(pending_move, Abs { .. }) {
                flush_mouse_move(pending_move, proxy);
            }
            // Start a new relative move or coalesce with the pending one.
            match pending_move {
                None => *pending_move = Rel { dx, dy },
                Rel { dx: pdx, dy: pdy } => {
                    *pdx = pdx.saturating_add(dx);
                    *pdy = pdy.saturating_add(dy);
                }
                Abs { .. } => unreachable!("absolute move must be flushed before relative"),
            }
        }
        MousePress(btn) => {
            let Some(proxy) = proxies.mouse.as_ref() else {
                *pending_move = PendingMove::None;
                mks_error!("Mouse proxy unavailable; dropping mouse-press event");
                return false;
            };
            flush_mouse_move(pending_move, proxy);
            if let Err(e) = proxy.press(btn) {
                mks_error!(error:? = e; "Failed to send mouse press command");
            }
        }
        MouseRelease(btn) => {
            let Some(proxy) = proxies.mouse.as_ref() else {
                *pending_move = PendingMove::None;
                mks_error!("Mouse proxy unavailable; dropping mouse-release event");
                return false;
            };
            flush_mouse_move(pending_move, proxy);
            if let Err(e) = proxy.release(btn) {
                mks_error!(error:? = e; "Failed to send mouse release command");
            }
        }
        Touch { kind, num_slot, x, y } => {
            let Some(proxy) = proxies.touch.as_ref() else {
                mks_error!("Multitouch proxy unavailable; dropping touch event");
                return false;
            };
            if let Err(e) = proxy.send_event(kind, num_slot, x, y) {
                mks_error!(error:? = e; "Failed to send multitouch event command");
            }
        }
        Shutdown => {
            if let Some(proxy) = proxies.mouse.as_ref() {
                flush_mouse_move(pending_move, proxy);
            } else {
                *pending_move = PendingMove::None;
            }
            return true;
        }
    }
    false
}

/// Flushes the coalesced pending mouse movement event.
#[inline]
fn flush_mouse_move(pending_move: &mut PendingMove, proxy: &MouseProxyBlocking<'static>) {
    use PendingMove::*;
    match mem::take(pending_move) {
        None => {}
        Abs { x, y } => {
            if let Err(e) = proxy.set_abs_position(x, y) {
                mks_error!(error:? = e; "Failed to send absolute mouse position");
            }
        }
        Rel { dx, dy } => {
            if let Err(e) = proxy.rel_motion(dx, dy) {
                mks_error!(error:? = e; "Failed to send relative mouse motion command");
            }
        }
    }
}

/// Spawns the property watcher manager and reacts to watcher commands.
/// Forwards property events via `state_tx`.
#[inline]
fn spawn_prop_watch_manager(
    proxies: AsyncInputProxies, state_tx: AsyncSender<InputStateEvent>, watch_cmd_rx: AsyncReceiver<WatchCommand>,
    init: Capability,
) {
    let fut = async move {
        let mut watchers = PropWatchers::default();
        sync_prop_watchers(&proxies, &state_tx, &mut watchers, init).await;
        while let Ok(cmd) = watch_cmd_rx.recv().await {
            match cmd {
                WatchCommand::Update(update) => {
                    sync_prop_watchers(&proxies, &state_tx, &mut watchers, update).await;
                }
                WatchCommand::Shutdown => {
                    if let Some(task) = watchers.keyboard.take() {
                        task.abort();
                    }
                    if let Some(task) = watchers.mouse.take() {
                        task.abort();
                    }
                    if let Some(task) = watchers.touch.take() {
                        task.abort();
                    }
                    break;
                }
            }
        }
    };
    tokio::spawn(fut);
}

/// Reconciles running watcher tasks with the desired capabilities.
async fn sync_prop_watchers(
    proxies: &AsyncInputProxies, state_tx: &AsyncSender<InputStateEvent>, watchers: &mut PropWatchers,
    desired: Capability,
) {
    if desired.keyboard {
        if watchers.keyboard.is_none() {
            if let Some(proxy) = proxies.keyboard.clone() {
                watchers.keyboard = spawn_keyboard_watcher(proxy, state_tx.clone()).await;
            } else {
                mks_error!("Keyboard watcher requested but async proxy is unavailable; watcher not started");
            }
        }
    } else {
        if let Some(task) = watchers.keyboard.take() {
            task.abort();
        }
    }
    if desired.mouse {
        if watchers.mouse.is_none() {
            if let Some(proxy) = proxies.mouse.clone() {
                watchers.mouse = spawn_mouse_watcher(proxy, state_tx.clone()).await;
            } else {
                mks_error!("Mouse watcher requested but async proxy is unavailable; watcher not started");
            }
        }
    } else {
        if let Some(task) = watchers.mouse.take() {
            task.abort();
        }
    }
    if desired.multitouch {
        if watchers.touch.is_none() {
            if let Some(proxy) = proxies.touch.clone() {
                watchers.touch = spawn_touch_watcher(proxy, state_tx.clone()).await;
            } else {
                mks_error!("Multitouch watcher requested but async proxy is unavailable; watcher not started");
            }
        }
    } else {
        if let Some(task) = watchers.touch.take() {
            task.abort();
        }
    }
}

async fn spawn_keyboard_watcher(
    proxy: KeyboardProxy<'static>, tx: AsyncSender<InputStateEvent>,
) -> Option<AbortHandle> {
    let updates = proxy.receive_modifiers_changed().await;
    let p = proxy.clone();
    let stream = fetch_then_update(async move { p.modifiers().await }, updates, |state| ModifiersChanged(state.bits()));
    Some(spawn_state_event_forwarder(
        stream,
        tx,
        "Failed to decode keyboard modifiers update",
        "Failed to forward keyboard modifiers update",
    ))
}

async fn spawn_mouse_watcher(proxy: MouseProxy<'static>, tx: AsyncSender<InputStateEvent>) -> Option<AbortHandle> {
    let updates = proxy.receive_is_absolute_changed().await;
    let p = proxy.clone();
    let stream = fetch_then_update(async move { p.is_absolute().await }, updates, MouseIsAbsolute);
    Some(spawn_state_event_forwarder(
        stream,
        tx,
        "Failed to decode mouse absolute-mode update",
        "Failed to forward mouse absolute-mode update",
    ))
}

async fn spawn_touch_watcher(proxy: MultiTouchProxy<'static>, tx: AsyncSender<InputStateEvent>) -> Option<AbortHandle> {
    let updates = proxy.receive_max_slots_changed().await;
    let p = proxy.clone();
    let stream = fetch_then_update(async move { p.max_slots().await }, updates, TouchMaxSlots);
    Some(spawn_state_event_forwarder(
        stream,
        tx,
        "Failed to decode multitouch max-slots update",
        "Failed to forward multitouch max-slots update",
    ))
}

#[inline]
fn spawn_state_event_forwarder(
    mut stream: BoxStream<'static, zbus::Result<InputStateEvent>>, tx: AsyncSender<InputStateEvent>,
    decode_failed_log: &'static str, send_failed_log: &'static str,
) -> AbortHandle {
    tokio::spawn(async move {
        while let Some(changed_state) = stream.next().await {
            let state = match changed_state {
                Ok(state) => state,
                Err(e) => {
                    mks_error!(error:? = e; "{decode_failed_log}");
                    continue;
                }
            };
            if let Err(e) = tx.send(state).await {
                mks_error!(error:? = e; "{send_failed_log}");
            }
        }
    })
    .abort_handle()
}
