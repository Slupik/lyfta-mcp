mod lyfta;
mod summary;
mod tools;

use std::{
    env,
    error::Error,
    ffi::OsString,
    io::ErrorKind,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

use rmcp::{
    ServiceExt,
    transport::{
        stdio,
        streamable_http_server::{
            StreamableHttpServerConfig, StreamableHttpService, session::never::NeverSessionManager,
        },
    },
};
use tokio_util::sync::CancellationToken;

const USAGE: &str =
    "Usage: lyfta-mcp [--env-file PATH] [--http ADDR:PORT [--allowed-host HOST]...]";

struct Args {
    env_file: PathBuf,
    http: Option<SocketAddr>,
    allowed_hosts: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args = parse_args(env::args_os().skip(1))?;
    let key = api_key(&args.env_file)?;
    let server = tools::LyftaServer::new(lyfta::LyftaClient::new(key)?)?;
    match args.http {
        None => {
            server.serve(stdio()).await?.waiting().await?;
            Ok(())
        }
        Some(addr) => serve_http(server, addr, args.allowed_hosts).await,
    }
}

/// Streamable HTTP without sessions: every request gets a fresh handler clone,
/// so the server keeps no per-client state between calls.
async fn serve_http(
    server: tools::LyftaServer,
    addr: SocketAddr,
    allowed_hosts: Vec<String>,
) -> Result<(), Box<dyn Error>> {
    let shutdown = CancellationToken::new();
    let mut hosts = vec!["localhost".to_owned(), "127.0.0.1".to_owned(), "::1".to_owned()];
    hosts.extend(allowed_hosts);
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_allowed_hosts(hosts)
        .with_cancellation_token(shutdown.child_token());
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(NeverSessionManager::default()),
        config,
    );
    let router = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!("lyfta-mcp listening on http://{addr}/mcp");
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            shutdown.cancel();
        })
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}

fn parse_args(mut args: impl Iterator<Item = OsString>) -> Result<Args, &'static str> {
    let mut parsed = Args {
        env_file: ".env".into(),
        http: None,
        allowed_hosts: Vec::new(),
    };
    while let Some(flag) = args.next() {
        let value = args.next().ok_or(USAGE)?;
        match flag.to_str() {
            Some("--env-file") => parsed.env_file = value.into(),
            Some("--http") => {
                parsed.http = Some(value.to_str().and_then(|v| v.parse().ok()).ok_or(USAGE)?)
            }
            Some("--allowed-host") => parsed
                .allowed_hosts
                .push(value.into_string().map_err(|_| USAGE)?),
            _ => return Err(USAGE),
        }
    }
    if parsed.http.is_none() && !parsed.allowed_hosts.is_empty() {
        return Err(USAGE);
    }
    Ok(parsed)
}

fn api_key(path: &Path) -> Result<Option<String>, &'static str> {
    if let Ok(key) = env::var("LYFTA_API_KEY") {
        return Ok(Some(key));
    }

    let entries = match dotenvy::from_path_iter(path) {
        Ok(entries) => entries,
        Err(dotenvy::Error::Io(err)) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Could not read the Lyfta environment file."),
    };
    for entry in entries {
        let (name, value) = entry.map_err(|_| "Invalid entry in the Lyfta environment file.")?;
        if name == "LYFTA_API_KEY" {
            return Ok(Some(value));
        }
    }
    Ok(None)
}
