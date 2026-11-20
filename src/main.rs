use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use std::{io::Write, path::PathBuf, sync::Arc};
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[command(name = "aurabot", version, about = "Standalone Rust QQ AI companion")]
struct Cli {
    #[arg(short, long, default_value = "config.toml")]
    config: PathBuf,
    #[command(subcommand)]
    command: Option<Command>,
}
#[derive(Subcommand)]
enum Command {
    Init,
    Check,
    Serve,
}
#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "aurabot=info,tower_http=warn".into()),
        )
        .init();
    let cli = Cli::parse();
    let command = cli.command.unwrap_or(Command::Serve);
    match command {
        Command::Init => init(&cli.config),
        Command::Check => {
            let c = aurabot::config::Config::load(&cli.config)?;
            println!(
                "Configuration valid; OneBot={}, QQ official={}, voice={}",
                c.onebot.enabled, c.official.enabled, c.voice.enabled
            );
            Ok(())
        }
        Command::Serve => serve(cli.config).await,
    }
}
fn init(path: &std::path::Path) -> Result<()> {
    if path.exists() {
        bail!("{} already exists", path.display())
    }
    if let Some(p) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(p)?
    }
    let text = toml::to_string_pretty(&aurabot::config::Config::default())?;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    f.write_all(text.as_bytes())?;
    println!(
        "Created {}. Set AURABOT_MODEL_KEY, then configure the channels.",
        path.display()
    );
    Ok(())
}
async fn serve(path: PathBuf) -> Result<()> {
    let cfg = aurabot::config::Config::load(&path)?;
    let admin = admin_token(&cfg.server.data_dir, &cfg.server.admin_token_env)?;
    cfg.validate()?;
    let cfg = Arc::new(cfg);
    let db = PathBuf::from(&cfg.server.data_dir).join("aurabot.sqlite");
    let store = aurabot::store::Store::open(&db)?;
    let engine = aurabot::engine::Engine::new(cfg.clone(), store.clone())?;
    engine.start_workers().await;
    let stop = CancellationToken::new();
    let sched = tokio::spawn(aurabot::engine::run_scheduler(engine.clone(), stop.clone()));
    if cfg.onebot.enabled {
        tokio::spawn(aurabot::adapters::onebot::run(cfg.clone(), engine.clone()));
    }
    if cfg.official.enabled && cfg.official.transport == "websocket" {
        tokio::spawn(aurabot::adapters::official::gateway(
            cfg.clone(),
            engine.clone(),
        ));
    }
    let router = aurabot::http::router(engine, cfg.clone(), &admin);
    let addr: std::net::SocketAddr = cfg.server.listen.parse()?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .context("bind server listener")?;
    println!("Aurabot management panel: http://{addr}/");
    println!("Database: {}", db.display());
    if cfg.server.admin_token_env.is_empty() || std::env::var(&cfg.server.admin_token_env).is_err()
    {
        println!(
            "Management token: stored in {}/admin.token",
            cfg.server.data_dir
        );
    }
    let shutdown = async move {
        let _ = tokio::signal::ctrl_c().await;
        stop.cancel();
    };
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown)
        .await
        .context("HTTP server failed")?;
    sched.abort();
    Ok(())
}
fn admin_token(data_dir: &str, env: &str) -> Result<String> {
    if let Ok(token) = std::env::var(env) {
        if token.len() < 24 {
            bail!("{env} must be at least 24 characters")
        };
        return Ok(token);
    }
    let path = PathBuf::from(data_dir).join("admin.token");
    std::fs::create_dir_all(data_dir)?;
    if path.exists() {
        let t = std::fs::read_to_string(&path)?.trim().to_owned();
        if t.len() < 24 {
            bail!("admin token file is invalid")
        };
        return Ok(t);
    }
    let token = uuid::Uuid::new_v4().to_string() + &uuid::Uuid::new_v4().to_string();
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    f.write_all(token.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(token)
}
