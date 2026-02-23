//! Unified HTTP server for all webhook routes.
//!
//! Composes route fragments from HttpChannel, WASM channel router, etc.
//! into a single axum server. Channels define routes but never spawn servers.

use std::net::SocketAddr;
use std::path::PathBuf;

use axum::Router;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::error::ChannelError;

/// TLS certificate and key paths for the webhook server.
pub struct TlsConfig {
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
}

/// Configuration for the unified webhook server.
pub struct WebhookServerConfig {
    /// Address to bind the server to.
    pub addr: SocketAddr,
    /// Optional TLS configuration. When present, the server uses HTTPS.
    pub tls: Option<TlsConfig>,
}

/// A single HTTP server that hosts all webhook routes.
///
/// Channels contribute route fragments via `add_routes()`, then a single
/// `start()` call binds the listener and spawns the server task.
pub struct WebhookServer {
    config: WebhookServerConfig,
    routes: Vec<Router>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    handle: Option<JoinHandle<()>>,
}

impl WebhookServer {
    /// Create a new webhook server with the given bind address.
    pub fn new(config: WebhookServerConfig) -> Self {
        Self {
            config,
            routes: Vec::new(),
            shutdown_tx: None,
            handle: None,
        }
    }

    /// Accumulate a route fragment. Each fragment should already have its
    /// state applied via `.with_state()`.
    pub fn add_routes(&mut self, router: Router) {
        self.routes.push(router);
    }

    /// Bind the listener, merge all route fragments, and spawn the server.
    pub async fn start(&mut self) -> Result<(), ChannelError> {
        let mut app = Router::new();
        for fragment in self.routes.drain(..) {
            app = app.merge(fragment);
        }

        let addr = self.config.addr;

        if let Some(ref tls) = self.config.tls {
            let rustls_config =
                axum_server::tls_rustls::RustlsConfig::from_pem_file(&tls.cert_path, &tls.key_path)
                    .await
                    .map_err(|e| ChannelError::StartupFailed {
                        name: "webhook_server".to_string(),
                        reason: format!("Failed to load TLS certificate/key: {}", e),
                    })?;

            tracing::info!("Webhook server (HTTPS) listening on {}", addr);

            let axum_handle = axum_server::Handle::new();
            let shutdown_handle = axum_handle.clone();

            let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
            self.shutdown_tx = Some(shutdown_tx);

            let handle = tokio::spawn(async move {
                // Drive the shutdown signal into the axum-server handle.
                tokio::spawn(async move {
                    let _ = shutdown_rx.await;
                    tracing::info!("Webhook server shutting down");
                    shutdown_handle.graceful_shutdown(None);
                });

                if let Err(e) = axum_server::bind_rustls(addr, rustls_config)
                    .handle(axum_handle)
                    .serve(app.into_make_service())
                    .await
                {
                    tracing::error!("Webhook server (HTTPS) error: {}", e);
                }
            });

            self.handle = Some(handle);
        } else {
            let listener = tokio::net::TcpListener::bind(addr)
                .await
                .map_err(|e| ChannelError::StartupFailed {
                    name: "webhook_server".to_string(),
                    reason: format!("Failed to bind to {}: {}", addr, e),
                })?;

            tracing::info!("Webhook server listening on {}", addr);

            let (shutdown_tx, shutdown_rx) = oneshot::channel();
            self.shutdown_tx = Some(shutdown_tx);

            let handle = tokio::spawn(async move {
                if let Err(e) = axum::serve(listener, app)
                    .with_graceful_shutdown(async {
                        let _ = shutdown_rx.await;
                        tracing::info!("Webhook server shutting down");
                    })
                    .await
                {
                    tracing::error!("Webhook server error: {}", e);
                }
            });

            self.handle = Some(handle);
        }

        Ok(())
    }

    /// Signal graceful shutdown and wait for the server task to finish.
    pub async fn shutdown(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.await;
        }
    }
}
