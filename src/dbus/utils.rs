use futures_util::{
    Stream, StreamExt,
    stream::{BoxStream, once},
};
use zbus::proxy::PropertyChanged;
use zvariant::OwnedValue;

#[inline]
pub fn fetch_then_update<Fut, S, T, U>(
    getter: Fut, updates: S, ctor: fn(T) -> U,
) -> BoxStream<'static, std::result::Result<U, zbus::Error>>
where
    T: TryFrom<OwnedValue> + Send + Sync + 'static,
    T::Error: Into<zbus::Error>,
    Fut: Future<Output = zbus::Result<T>> + Send + 'static,
    S: Stream<Item = PropertyChanged<'static, T>> + Send + 'static,
    U: Send + 'static,
{
    let initial_stream = once(async move { getter.await.map(ctor) });
    let update_stream = updates.then(move |msg| async move { msg.get().await.map(ctor) });
    initial_stream.chain(update_stream).boxed()
}

#[macro_use]
pub mod macros {
    #[macro_export]
    macro_rules! impl_controller {
        ($controller:ident, $cmd_type:ident, {
            $(
                $(#[$meta:meta])*
                // 重点：在这里增加 $vis:vis 匹配
                $vis:vis fn $method:ident($($arg:ident : $type:ty),* $(,)?) => $variant:ident $constructor:tt;
            )*
        }) => {
            impl $controller {
                $(
                    $(#[$meta])*
                    // 将匹配到的可见性填在这里
                    $vis async fn $method(&self, $($arg : $type),*) -> $crate::MksResult {
                        self.0.send($cmd_type::$variant $constructor).await?;
                        Ok(())
                    }
                )*
            }
        };
    }

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
                proxy: std::sync::Arc<$proxy_type>,
                event_tx: kanal::AsyncSender<$event_type>,
            ) -> $crate::MksResult<tokio::task::JoinHandle<()>> {
                $(
                    let $signal = proxy.$signal().await;
                )*
                let fut = async move {
                    let mut streams = Vec::new();
                    $(
                        let p = proxy.clone();
                        streams.push($crate::dbus::utils::fetch_then_update(
                            async move { p.$getter().await },
                            $signal,
                            $map_fn,
                        ));
                    )*
                    let mut agg_stream = futures_util::stream::select_all(streams);
                    while let Some(res) = futures_util::StreamExt::next(&mut agg_stream).await {
                        match res {
                            Ok(event) => {
                                if event_tx.send(event).await.is_err() {
                                    break;
                                }
                            }
                            Err(e) => {
                                log::warn!(error:? = e; "Error reading {} property", $log_context);
                            }
                        }
                    }
                };
                Ok(tokio::spawn(fut))
            }
        };
    }

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
                $p: std::sync::Arc<$proxy_type>,
                cmd_rx: kanal::AsyncReceiver<$cmd_type>,
            ) -> $crate::MksResult<tokio::task::JoinHandle<()>> {
                let fut = async move {
                    while let Ok(cmd) = cmd_rx.recv().await {
                        let res = match cmd {
                            $(
                                $pattern => $call,
                            )*
                        };
                        if let Err(e) = res {
                            log::error!(error:? = e; "{} failed to call method", $log_context);
                        }
                    }
                };
                Ok(tokio::spawn(fut))
            }
        };
    }

    #[macro_export]
    macro_rules! impl_session_connect {
        ($session_name:ident, $proxy_type:ty, $controller_type:ty, $command_type:ty, $event_type:ty) => {
            pub struct $session_name {
                pub tx: $controller_type,
                pub rx: kanal::AsyncReceiver<$event_type>,
                pub watch_task: tokio::task::JoinHandle<()>,
                pub cmd_handler: tokio::task::JoinHandle<()>,
            }
            pub async fn connect(conn: &zbus::Connection, path: String) -> $crate::MksResult<$session_name> {
                let proxy = std::sync::Arc::new(<$proxy_type>::new(conn, path).await?);
                let (event_tx, event_rx) = kanal::unbounded_async::<$event_type>();
                let (cmd_tx, cmd_rx) = kanal::unbounded_async::<$command_type>();
                let controller = <$controller_type>::from(cmd_tx);
                let watch_task = watch_proxy_changes(proxy.clone(), event_tx).await?;
                let cmd_handler = handle_commands(proxy.clone(), cmd_rx).await?;
                Ok($session_name { tx: controller, rx: event_rx, watch_task, cmd_handler })
            }
        };
    }
}
