#![allow(clippy::module_inception)]
use anyhow::Result;
use axum::{extract::DefaultBodyLimit, http, routing::{delete, get, post, put}, Router};
use dashmap::DashMap;
use tracing_panic::panic_hook;
use tracing_subscriber::{fmt::{self, time::ChronoLocal}, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};
use std::{path::PathBuf, sync::Arc, env::var};
use axum::http::header::HOST;
use axum::http::Request;
use axum::middleware::{from_fn, Next};
use axum::response::{IntoResponse, Response};
use tokio::{fs, sync::RwLock, time::Instant};
use tower_http::trace::TraceLayer;
use lazy_static::lazy_static;

// Consts
mod consts;
pub use consts::*;

// Errors
pub use api::errors::{ApiResult, ApiError};

// API
mod api;
use api::{
    figura::{ws, info as api_info, profile as api_profile, auth as api_auth, assets as api_assets},
    lambda::{internal as lambda_internal, },
    // v1::{},
};

// Auth
mod auth;
use auth::{UManager, check_auth};

// Config
mod state;
use state::{Config, AppState};

// Utils
mod utils;
use utils::*;

lazy_static! {
    pub static ref LOGGER_VAR: String = {
        var(LOGGER_ENV).unwrap_or(String::from("info"))
    };
    pub static ref CONFIG_VAR: String = {
        var(CONFIG_ENV).unwrap_or(String::from("Config.toml"))
    };
    pub static ref LOGS_VAR: String = {
        var(LOGS_ENV).unwrap_or(String::from("logs"))
    };
    pub static ref ASSETS_VAR: String = {
        var(ASSETS_ENV).unwrap_or(String::from("data/assets"))
    };
    pub static ref AVATARS_VAR: String = {
        var(AVATARS_ENV).unwrap_or(String::from("data/avatars"))
    };
}

#[tokio::main]
async fn main() -> Result<()> {
    // 1. Set up env
    let _ = dotenvy::dotenv();

    // 2. Set up logging
    let file_appender = tracing_appender::rolling::never(&*LOGS_VAR, get_log_file(&LOGS_VAR));
    let timer = ChronoLocal::new(String::from("%Y-%m-%dT%H:%M:%S%.3f%:z"));

    let file_layer = fmt::layer()
        .with_ansi(false) // Disable ANSI colors for file logs
        .with_timer(timer.clone())
        .pretty()
        .with_writer(file_appender);

    // Create a layer for the terminal
    let terminal_layer = fmt::layer()
        .with_ansi(true)
        .with_timer(timer)
        .pretty()
        .with_writer(std::io::stdout);

    // Combine the layers and set the global subscriber
    tracing_subscriber::registry()
        .with(EnvFilter::from(&*LOGGER_VAR))
        .with(file_layer)
        .with(terminal_layer)
        .init();

    // std::panic::set_hook(Box::new(panic_hook));
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        panic_hook(panic_info);
        prev_hook(panic_info);
    }));

    // 3. Display info about current instance and check updates
    tracing::info!("The Sculptor v{SCULPTOR_VERSION} ({REPOSITORY})");

    match get_latest_version(REPOSITORY).await {
        Ok(latest_version) => {
            if latest_version > semver::Version::parse(SCULPTOR_VERSION).expect("SCULPTOR_VERSION does not match SemVer!") {
                tracing::info!("Available new v{latest_version}! Check https://github.com/{REPOSITORY}/releases");
            } else {
                tracing::info!("Sculptor are up to date!");
            }
        },
        Err(e) => {
            tracing::error!("Can't fetch Sculptor updates due: {e:?}");
        },
    }

    // 4. Starting an app() that starts to serve. If app() returns true, the sculptor will be restarted. for future
    loop {
        if !app().await? {
            break;
        }
    }

    Ok(())
}

async fn app() -> Result<bool> {
    // Preparing for launch
    {
        let path = PathBuf::from(&*AVATARS_VAR);
        if !path.exists() {
            fs::create_dir_all(path).await.expect("Can't create avatars folder!");
            tracing::info!("Created avatars directory");
        }
    }

    // Config
    let config = Arc::new(RwLock::new(Config::parse(CONFIG_VAR.clone().into())));
    let listen = config.read().await.listen.clone();
    let limit = get_limit_as_bytes(config.read().await.limitations.max_avatar_size as usize);

    if config.read().await.assets_updater_enabled {
        // Force update assets if folder or hash file doesn't exists.
        if !(PathBuf::from(&*ASSETS_VAR).is_dir() && get_path_to_assets_hash().is_file()) {
            tracing::debug!("Removing broken assets...");
            remove_assets().await
        }
        match get_commit_sha(FIGURA_ASSETS_COMMIT_URL).await {
            Ok(sha) => {
                if is_assets_outdated(&sha).await.unwrap_or_else(|e| {tracing::error!("Can't check assets state due: {:?}", e); false}) {
                    remove_assets().await;
                    match tokio::task::spawn_blocking(|| { download_assets() }).await.unwrap() {
                        Err(e) => tracing::error!("Assets outdated! Can't download new version due: {:?}", e),
                        Ok(_) => {
                            match write_sha_to_file(&sha).await {
                                Ok(_) => tracing::info!("Assets successfully updated!"),
                                Err(e) => tracing::error!("Assets successfully updated! Can't create assets hash file due: {:?}", e),
                            }
                        }
                    };
                } else { tracing::info!("Assets are up to date!") }
            },
            Err(e) => tracing::error!("Can't get assets last commit! Assets update check aborted due {:?}", e)
        }
    }

    // State
    let state = AppState {
        uptime: Instant::now(),
        user_manager: Arc::new(UManager::new()),
        session: Arc::new(DashMap::new()),
        subscribes: Arc::new(DashMap::new()),
        figura_versions: Arc::new(RwLock::new(None)),
        config,
    };

    // Automatic update of configuration/ban list while the server is running
    tokio::spawn(update_advanced_users(
        CONFIG_VAR.clone().into(),
        Arc::clone(&state.user_manager),
        Arc::clone(&state.session),
        Arc::clone(&state.config)
    ));
    if state.config.read().await.mc_folder.exists() {
        tokio::spawn(update_bans_from_minecraft(
            state.config.read().await.mc_folder.clone(),
            Arc::clone(&state.user_manager),
            Arc::clone(&state.session)
        ));
    }

    let api = Router::new()
        .nest("//auth", api_auth::router()) // => /api//auth ¯\_(ツ)_/¯
        .nest("//assets", api_assets::router())
        .nest("/v1", api::v1::router(limit))
        .route("/limits", get(api_info::limits))
        .route("/version", get(api_info::version))
        .route("/motd", get(api_info::motd))
        .route("/equip", post(api_profile::equip_avatar))
        .route("/:uuid", get(api_profile::user_info))
        .route("/:uuid/avatar", get(api_profile::download_avatar))
        .route("/avatar", put(api_profile::upload_avatar).layer(DefaultBodyLimit::max(limit)))
        .route("/avatar", delete(api_profile::delete_avatar));

    let internal = Router::new()
        .route("/:uuid/temp", put(lambda_internal::temp_avatar))
        .route("/:uuid/avatar", put(lambda_internal::upload_avatar))
        .route("/:uuid/avatar", delete(lambda_internal::delete_avatar))
        .route("/:uuid/event", get(lambda_internal::user_event))
        .route("/health", get(|| async { "ok internal" }))
        .layer(from_fn(internal_applicator));

    let app = Router::new()
        .nest("/api", api)
        .route("/api/", get(check_auth))
        .route("/ws", get(ws))
        .nest("/internal", internal)
        .with_state(state)
        .layer(TraceLayer::new_for_http().on_request(()))
        .route("/health", get(|| async { "ok" }));

    let listener = tokio::net::TcpListener::bind(listen).await?;
    tracing::info!("Listening on {}", listener.local_addr()?);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    tracing::info!("Serve stopped.");
    Ok(false)
}

async fn internal_applicator(request: Request<axum::body::Body>, next: Next) -> Response {
    let host_header = request
        .headers()
        .get(&HOST)
        .map(|value| value.as_ref().to_owned())
        ;
    let response = next.run(request).await;
    let allow = String::from("lambda");
    let host = host_header.as_deref();
    if host.is_none() || !allow.as_bytes().eq(host.unwrap()) {
        let mut resp = "".into_response();
        *resp.status_mut() = http::status::StatusCode::FORBIDDEN;
        return resp;
    }
    response
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {
            tracing::info!("Ctrl+C signal received");
        },
        () = terminate => {
            tracing::info!("Terminate signal received");
        },
    }
}
