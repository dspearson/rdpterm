/// rdpterm — secure RDP terminal server
///
/// Serves a terminal session over RDP with full Unicode, emoji, and nerd font rendering.
use anyhow::Result;
use clap::Parser;
use rdpfb::server::{RdpServer, RdpServerConfig};
use rdpfb::tls::TlsConfig;
use std::sync::Arc;
use tracing::info;

mod app;
pub mod terminal;

use app::{PasswordAuthenticator, TerminalAppFactory};

#[derive(Parser)]
#[command(name = "rdpterm", about = "Secure RDP terminal server")]
struct Cli {
    /// Listen address
    #[arg(short, long, default_value = "0.0.0.0")]
    address: String,

    /// Listen port
    #[arg(short, long, default_value_t = 3389)]
    port: u16,

    /// Default framebuffer width (client may override via negotiation)
    #[arg(long, default_value_t = 1600)]
    width: u16,

    /// Default framebuffer height (client may override via negotiation)
    #[arg(long, default_value_t = 900)]
    height: u16,

    /// Font size in points
    #[arg(short, long, default_value_t = 16.0)]
    font_size: f32,

    /// Shell to spawn (defaults to $SHELL or /bin/sh)
    #[arg(short, long)]
    shell: Option<String>,

    /// TLS certificate path
    #[arg(long, default_value = "certs/server.crt")]
    tls_cert: String,

    /// TLS private key path
    #[arg(long, default_value = "certs/server.key")]
    tls_key: String,

    /// Disable TLS (plaintext connections)
    #[arg(long)]
    no_tls: bool,

    /// Required username for authentication
    #[arg(short, long, env = "RDPTERM_USER")]
    user: Option<String>,

    /// Required password for authentication
    #[arg(long, env = "RDPTERM_PASSWORD")]
    password: Option<String>,

    /// Log level (trace, debug, info, warn, error)
    #[arg(short, long, default_value = "info")]
    log_level: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&cli.log_level)),
        )
        .init();

    let enable_tls = !cli.no_tls;
    let tls_config = if enable_tls {
        Some(TlsConfig {
            cert_path: cli.tls_cert,
            key_path: cli.tls_key,
        })
    } else {
        None
    };

    let has_auth = cli.user.is_some() || cli.password.is_some();

    info!(
        "rdpterm starting on {}:{} (TLS: {}, auth: {})",
        cli.address,
        cli.port,
        enable_tls,
        if has_auth { "enabled" } else { "disabled" }
    );
    if let Some(ref shell) = cli.shell {
        info!("Shell: {}", shell);
    }

    let config = RdpServerConfig {
        address: cli.address,
        port: cli.port,
        width: cli.width,
        height: cli.height,
        enable_tls,
        tls_config,
    };

    let app_factory = Arc::new(TerminalAppFactory {
        shell: cli.shell,
        font_size: cli.font_size,
    });

    let authenticator = if has_auth {
        Some(Arc::new(PasswordAuthenticator {
            username: cli.user,
            password: cli.password,
        }) as Arc<dyn rdpfb::application::RdpAuthenticator>)
    } else {
        None
    };

    RdpServer::new(config, app_factory, authenticator).run().await
}
