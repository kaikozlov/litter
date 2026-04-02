//! Reconnecting IPC client wrapper with exponential backoff.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::RwLock as StdRwLock;
use std::time::Duration;

use tokio::sync::{RwLock, broadcast, watch};
use tracing::{error, info, warn};

use crate::client::handle::{IpcClient, IpcClientConfig};
use crate::error::IpcError;
use crate::handler::RequestHandler;
use crate::protocol::params::TypedBroadcast;

/// Policy for reconnection attempts.
pub struct ReconnectPolicy {
    pub initial_delay: Duration,
    pub max_delay: Duration,
    pub max_attempts: Option<u32>,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
            max_attempts: None,
        }
    }
}

/// IPC client wrapper that automatically reconnects on disconnection.
pub struct ReconnectingIpcClient {
    client: Arc<StdRwLock<Option<IpcClient>>>,
    handler: Arc<RwLock<Option<Arc<dyn RequestHandler>>>>,
    broadcast_tx: broadcast::Sender<TypedBroadcast>,
    connection_tx: watch::Sender<bool>,
    reconnect_task: tokio::task::JoinHandle<()>,
}

impl ReconnectingIpcClient {
    /// Connect to the IPC bus with automatic reconnection.
    pub async fn connect(
        config: IpcClientConfig,
        policy: ReconnectPolicy,
    ) -> Result<Self, IpcError> {
        let config = Arc::new(config);
        let connector = {
            let config = Arc::clone(&config);
            move || {
                let config = Arc::clone(&config);
                async move { IpcClient::connect_with_config(&config).await }
            }
        };
        Self::connect_with_connector(connector, policy).await
    }

    /// Connect using a custom connector that can recreate the IPC stream.
    pub async fn connect_with_connector<F, Fut>(
        connector: F,
        policy: ReconnectPolicy,
    ) -> Result<Self, IpcError>
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<IpcClient, IpcError>> + Send + 'static,
    {
        let initial_client = connector().await?;
        Ok(Self::start_with_connector(
            Some(initial_client),
            connector,
            policy,
        ))
    }

    /// Start the reconnecting wrapper with an optional initial client.
    ///
    /// When `initial_client` is `None`, the background task immediately starts
    /// trying to connect using the supplied connector.
    pub fn start_with_connector<F, Fut>(
        initial_client: Option<IpcClient>,
        connector: F,
        policy: ReconnectPolicy,
    ) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<IpcClient, IpcError>> + Send + 'static,
    {
        let connector: Arc<
            dyn Fn() -> Pin<Box<dyn Future<Output = Result<IpcClient, IpcError>> + Send>>
                + Send
                + Sync,
        > = Arc::new(move || Box::pin(connector()));
        let client = Arc::new(StdRwLock::new(initial_client));
        let handler: Arc<RwLock<Option<Arc<dyn RequestHandler>>>> = Arc::new(RwLock::new(None));
        let (broadcast_tx, _) = broadcast::channel::<TypedBroadcast>(256);
        let (connection_tx, _) = watch::channel(Self::client_lock_read(&client).as_ref().is_some());

        let reconnect_task = {
            let client = Arc::clone(&client);
            let handler = Arc::clone(&handler);
            let broadcast_tx = broadcast_tx.clone();
            let connection_tx = connection_tx.clone();
            let connector = Arc::clone(&connector);
            tokio::spawn(Self::reconnect_loop(
                policy,
                client,
                handler,
                broadcast_tx,
                connection_tx,
                connector,
            ))
        };

        Self {
            client,
            handler,
            broadcast_tx,
            connection_tx,
            reconnect_task,
        }
    }

    fn client_lock_read(
        client: &StdRwLock<Option<IpcClient>>,
    ) -> std::sync::RwLockReadGuard<'_, Option<IpcClient>> {
        match client.read() {
            Ok(guard) => guard,
            Err(error) => {
                warn!("ipc reconnect: recovering poisoned client read lock");
                error.into_inner()
            }
        }
    }

    fn client_lock_write(
        client: &StdRwLock<Option<IpcClient>>,
    ) -> std::sync::RwLockWriteGuard<'_, Option<IpcClient>> {
        match client.write() {
            Ok(guard) => guard,
            Err(error) => {
                warn!("ipc reconnect: recovering poisoned client write lock");
                error.into_inner()
            }
        }
    }

    /// Returns the current connected client, if any.
    pub fn client(&self) -> Option<IpcClient> {
        Self::client_lock_read(&self.client).clone()
    }

    /// Returns whether the wrapper currently has an active IPC client.
    pub fn is_connected(&self) -> bool {
        self.client().is_some()
    }

    /// Subscribe to typed broadcasts. Subscriptions survive reconnections.
    pub fn subscribe_broadcasts(&self) -> broadcast::Receiver<TypedBroadcast> {
        self.broadcast_tx.subscribe()
    }

    /// Subscribe to IPC connection state changes.
    pub fn subscribe_connection_state(&self) -> watch::Receiver<bool> {
        self.connection_tx.subscribe()
    }

    /// Set the request handler. It will be re-registered on reconnect.
    pub async fn set_request_handler(&self, h: Arc<dyn RequestHandler>) {
        {
            let mut guard = self.handler.write().await;
            *guard = Some(Arc::clone(&h));
        }
        if let Some(c) = self.client().as_ref() {
            c.set_request_handler(h).await;
        }
    }

    /// Stop the reconnect loop and disconnect any currently attached IPC
    /// client.
    pub async fn shutdown(&self) {
        self.reconnect_task.abort();
        let client = {
            let mut guard = Self::client_lock_write(&self.client);
            guard.take()
        };
        if let Some(client) = client {
            client.disconnect().await;
        }
        let _ = self.connection_tx.send(false);
    }

    async fn reconnect_loop(
        policy: ReconnectPolicy,
        client: Arc<StdRwLock<Option<IpcClient>>>,
        handler: Arc<RwLock<Option<Arc<dyn RequestHandler>>>>,
        broadcast_tx: broadcast::Sender<TypedBroadcast>,
        connection_tx: watch::Sender<bool>,
        connector: Arc<
            dyn Fn() -> Pin<Box<dyn Future<Output = Result<IpcClient, IpcError>> + Send>>
                + Send
                + Sync,
        >,
    ) {
        loop {
            let current_client = {
                let guard = Self::client_lock_read(&client);
                guard.clone()
            };
            if let Some(current_client) = current_client {
                let mut sub = current_client.subscribe_broadcasts();
                loop {
                    match sub.recv().await {
                        Ok(msg) => {
                            let _ = broadcast_tx.send(msg);
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    }
                }
            }

            {
                let mut guard = Self::client_lock_write(&client);
                *guard = None;
            }
            let _ = connection_tx.send(false);

            info!("ipc connection lost, starting reconnect");

            let mut delay = policy.initial_delay;
            let mut attempt = 0u32;

            let new_client = loop {
                attempt += 1;
                if let Some(max) = policy.max_attempts {
                    if attempt > max {
                        error!("ipc reconnect: max attempts ({max}) reached, giving up");
                        return;
                    }
                }

                info!("ipc reconnecting, attempt {attempt}");
                tokio::time::sleep(delay).await;

                match connector().await {
                    Ok(c) => {
                        info!("ipc reconnected");
                        break c;
                    }
                    Err(e) => {
                        warn!("ipc reconnect failed: {e}");
                        delay = (delay * 2).min(policy.max_delay);
                    }
                }
            };

            // Re-register handler if set.
            {
                let h_guard = handler.read().await;
                if let Some(h) = h_guard.as_ref() {
                    new_client.set_request_handler(Arc::clone(h)).await;
                }
            }

            // Store the new client.
            {
                let mut guard = Self::client_lock_write(&client);
                *guard = Some(new_client);
            }
            let _ = connection_tx.send(true);
        }
    }
}
