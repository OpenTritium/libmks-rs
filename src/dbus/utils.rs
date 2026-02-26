use futures_util::{
    Future, Stream, StreamExt,
    stream::{BoxStream, once},
};
use zbus::proxy::PropertyChanged;
use zvariant::OwnedValue;

#[inline]
pub fn fetch_then_update<Fut, S, T, U, F>(getter: Fut, updates: S, ctor: F) -> BoxStream<'static, zbus::Result<U>>
where
    T: TryFrom<OwnedValue> + Send + Sync + 'static,
    T::Error: Into<zbus::Error>,
    Fut: Future<Output = zbus::Result<T>> + Send + 'static,
    S: Stream<Item = PropertyChanged<'static, T>> + Send + 'static,
    U: Send + 'static,
    F: Fn(T) -> U + Clone + Send + Sync + 'static,
{
    let ctor_init = ctor.clone();
    let initial_stream = once(async move { getter.await.map(ctor_init) });
    let update_stream = updates
        .map(move |msg| {
            let ctor = ctor.clone();
            async move { msg.get().await.map(ctor) }
        })
        .buffer_unordered(16);
    initial_stream.chain(update_stream).boxed()
}

#[macro_use]
pub mod macros {
    /// Generates sync methods for controller that send commands to channel.
    ///
    /// Generates `impl $controller { fn $method(&self, ...) }` methods
    /// that use non-blocking `try_send()` to send `$cmd_type::$variant` to the internal channel.
    ///
    /// This is designed for UI thread usage where blocking would cause freezes.
    /// Uses try_send() - if the channel is full, the event is dropped (which is
    /// acceptable for high-frequency input events like mouse moves).
    ///
    /// # Arguments
    /// * `$controller` - Controller struct name
    /// * `$cmd_type` - Command enum type
    /// * Methods: `$vis fn $method($arg: $type) => $variant($args)`
    #[macro_export]
    macro_rules! impl_controller {
        ($controller:ident, $cmd_type:ident, {
            $(
                $(#[$meta:meta])*
                $vis:vis fn $method:ident($($arg:ident : $type:ty),* $(,)?) => $variant:ident $constructor:tt;
            )*
        }) => {
            impl $controller {
                $(
                    $(#[$meta])*
                    $vis fn $method(&self, $($arg : $type),*) -> $crate::MksResult {
                        // Use try_send for non-blocking sync calls from UI thread
                        self.0.try_send($cmd_type::$variant $constructor)?;
                        Ok(())
                    }
                )*
            }
        };
    }

    /// Generates async function that watches D-Bus properties and emits events.
    ///
    /// Generates `async fn $fn_name(proxy, event_tx) -> AbortHandle`
    /// that fetches initial values and subscribes to property change signals.
    ///
    /// # Arguments
    /// * `$fn_name` - Generated function name
    /// * `$proxy_type` - zbus proxy type
    /// * `$event_type` - Event enum type
    /// * `$log_context` - Log context string
    /// * Mappings: `$getter => $signal => $map_fn`
    #[macro_export]
    macro_rules! generate_watcher {
        (
            $fn_name:ident,
            $proxy_type:ty,
            $event_type:ty,
            $log_context:literal,
            {
                $(
                    $getter:ident => $signal:ident => $map_fn:expr
                ),* $(,)?
            }
        ) => {
            async fn $fn_name(
                proxy: $proxy_type,
                event_tx: ::kanal::AsyncSender<$event_type>,
            ) -> $crate::MksResult<::tokio::task::AbortHandle> {
                $(
                    let $signal = proxy.$signal().await;
                )*

                let fut = async move {
                    let mut streams = ::std::vec::Vec::new();
                    $(
                        let p = proxy.clone();
                        streams.push($crate::fetch_then_update(
                            async move { p.$getter().await },
                            $signal,
                            $map_fn,
                        ));
                    )*

                    if streams.is_empty() {
                        ::log::warn!("No properties to watch for {}", $log_context);
                        return;
                    }

                    use ::futures_util::StreamExt;
                    let mut agg_stream = ::futures_util::stream::select_all(streams);

                    while let Some(res) = agg_stream.next().await {
                        match res {
                            Ok(event) => {
                                if event_tx.send(event).await.is_err() {
                                    break;
                                }
                            }
                            Err(e) => {
                                ::log::error!(error:? = e; "Error reading {} property", $log_context);
                            }
                        }
                    }
                };
                Ok(::tokio::spawn(fut).abort_handle())
            }
        };
    }

    /// Generates async function that handles commands by calling D-Bus methods.
    ///
    /// Generates `async fn $fn_name(proxy, cmd_rx) -> AbortHandle`
    /// that receives commands and calls corresponding D-Bus methods.
    ///
    /// # Arguments
    /// * `$fn_name` - Generated function name
    /// * `$proxy_type` - zbus proxy type
    /// * `$cmd_type` - Command enum type
    /// * `$log_context` - Log context string
    /// * Patterns: `$pattern => $dbus_call`
    #[macro_export]
    macro_rules! generate_handler {
        (
            $fn_name:ident,
            $proxy_type:ty,
            $cmd_type:ident,
            $log_context:literal,
            | $p:ident | {
                $(
                    $pattern:pat => $call:expr
                ),* $(,)?
            }
        ) => {
            async fn $fn_name(
                $p: $proxy_type,
                cmd_rx: ::kanal::AsyncReceiver<$cmd_type>,
            ) -> $crate::MksResult<::tokio::task::AbortHandle> {
                let fut = async move {
                    while let Ok(cmd) = cmd_rx.recv().await {
                        let res = match cmd {
                            $(
                                $pattern => $call,
                            )*
                        };
                        if let Err(e) = res {
                            ::log::error!(error:? = e; "{} failed to call method", $log_context);
                        }
                    }
                };
                Ok(::tokio::spawn(fut).abort_handle())
            }
        };
    }

    /// Generates session struct and `connect()` for D-Bus interface.
    ///
    /// Generates:
    /// - `pub struct $session_name { tx, rx, watch_task, cmd_handler }`
    /// - `impl $session_name { pub async fn connect(conn, path) -> Result<Self> }`
    /// - `Drop` impl that aborts tasks and closes channels
    ///
    /// Bounded channel if `$backpressure > 0`, otherwise unbounded.
    ///
    /// # Example
    /// ```ignore
    /// impl_session_connect!(
    ///     ConsoleSession, ConsoleProxy<'static>, ConsoleController,
    ///     Command, Event, watch_proxy_changes, handle_commands, 32
    /// );
    /// let session = ConsoleSession::connect(&conn, "/org/qemu/Display1/Console_0").await?;
    /// ```
    #[macro_export]
    macro_rules! impl_session_connect {
        (
            $session_name:ident,
            $proxy_type:ty,
            $controller_type:ty,
            $command_type:ty,
            $event_type:ty,
            $watcher_fn:ident,
            $handler_fn:ident
        ) => {
            $crate::impl_session_connect!(
                $session_name,
                $proxy_type,
                $controller_type,
                $command_type,
                $event_type,
                $watcher_fn,
                $handler_fn,
                0
            );
        };
        (
            $session_name:ident,
            $proxy_type:ty,
            $controller_type:ty,
            $command_type:ty,
            $event_type:ty,
            $watcher_fn:ident,
            $handler_fn:ident,
            $backpressure:expr
        ) => {
            pub struct $session_name {
                pub tx: $controller_type,
                pub rx: ::kanal::AsyncReceiver<$event_type>,
                pub watch_task: ::tokio::task::AbortHandle,
                pub cmd_handler: ::tokio::task::AbortHandle,
            }

            impl Drop for $session_name {
                fn drop(&mut self) {
                    self.watch_task.abort();
                    self.cmd_handler.abort();
                }
            }

            impl $session_name {
                pub async fn connect(conn: &zbus::Connection, path: impl Into<String>) -> $crate::MksResult<Self> {
                    let proxy = <$proxy_type>::new(conn, path.into()).await?;
                    let (event_tx, event_rx) = ::kanal::unbounded_async::<$event_type>();
                    let (cmd_tx, cmd_rx) = if $backpressure > 0 {
                        ::kanal::bounded_async::<$command_type>($backpressure)
                    } else {
                        ::kanal::unbounded_async::<$command_type>()
                    };
                    let controller = <$controller_type>::from(cmd_tx);
                    let watch_task = $watcher_fn(proxy.clone(), event_tx).await?;
                    let cmd_handler = $handler_fn(proxy, cmd_rx).await?;
                    Ok(Self { tx: controller, rx: event_rx, watch_task, cmd_handler })
                }
            }
        };
    }
}
