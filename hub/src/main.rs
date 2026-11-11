//! `pack-sync-hub`: serves the pack, and manages players' codes from the command line.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use clap::{Parser, Subcommand};
use pack_sync_hub::{FileStore, PackApi, PackConfig, router};
use tracing_subscriber::EnvFilter;

const DEFAULT_BIND: &str = "127.0.0.1:8140";
const DEFAULT_DATA: &str = "/var/lib/pack-sync-hub";

#[derive(Parser)]
#[command(about = "Serves a Horizon modpack's signed builds to pack-sync")]
struct Cli {
    /// The folder with codes.json and state.json
    #[arg(long, global = true, default_value = DEFAULT_DATA)]
    data: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the API; needs PACK_GITHUB_REPO, PACK_GITHUB_TOKEN and PACK_SERVER_KEY
    Serve {
        /// Keep it on loopback and put a reverse proxy with HTTPS in front
        #[arg(long, default_value = DEFAULT_BIND)]
        bind: SocketAddr,
    },
    /// Players' download codes
    Code {
        #[command(subcommand)]
        action: CodeAction,
    },
}

#[derive(Subcommand)]
enum CodeAction {
    /// Issue a code (it replaces the player's earlier one) and print it once
    Add { name: String },
    /// Cancel a player's code
    Revoke { name: String },
    /// List who has a code
    List,
}

async fn serve(store: FileStore, bind: SocketAddr) -> Result<(), String> {
    let config = PackConfig::from_env()
        .map_err(|error| error.to_string())?
        .ok_or("set PACK_GITHUB_REPO, PACK_GITHUB_TOKEN and PACK_SERVER_KEY")?;
    let api = PackApi::from_config(&config, Arc::new(store)).map_err(|error| error.to_string())?;
    let app: Router = router(api).route("/healthz", get(|| async { "ok" }));
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|error| format!("cannot listen on {bind}: {error}"))?;
    tracing::info!(%bind, repo = %config.github_repo, "serving the pack");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown())
    .await
    .map_err(|error| error.to_string())
}

async fn shutdown() {
    let interrupt = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = interrupt => {},
        () = terminate => {},
    }
}

async fn manage(store: &FileStore, action: CodeAction) -> Result<(), String> {
    match action {
        CodeAction::Add { name } => {
            let code = store.add_player(&name).await.map_err(|e| e.to_string())?;
            println!("{code}");
            eprintln!("Give this code to {name}; it is shown only now.");
        }
        CodeAction::Revoke { name } => {
            if !store
                .revoke_player(&name)
                .await
                .map_err(|e| e.to_string())?
            {
                return Err(format!("{name} has no code"));
            }
            println!("Revoked the code of {name}.");
        }
        CodeAction::List => {
            for player in store.players().await.map_err(|e| e.to_string())? {
                println!("{}\tsince {}", player.name, player.created);
            }
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let cli = Cli::parse();
    let store = FileStore::new(&cli.data);
    let result = match cli.command {
        Command::Serve { bind } => serve(store, bind).await,
        Command::Code { action } => manage(&store, action).await,
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("pack-sync-hub: {error}");
            ExitCode::FAILURE
        }
    }
}
