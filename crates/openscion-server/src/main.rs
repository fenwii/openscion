//! Server executable. By default it binds localhost; remote exposure requires explicit opt-in.
use anyhow::{bail,Result};
use clap::Parser;
use openscion_registry::Registry;
use openscion_server::{router,AppState};
use std::{net::SocketAddr,path::PathBuf};
use tokio::net::TcpListener;
#[derive(Parser)]
#[command(name="osc-server",version,about="OpenScion V0.6 Git-authoritative source, workspace and change integration API")]
struct Args {
  #[arg(long)] data:PathBuf,
  #[arg(long,default_value="127.0.0.1:8787")] bind:SocketAddr,
  #[arg(long,default_value_t=16)] max_git_operations:usize,
  #[arg(long,default_value_t=false)] allow_remote:bool,
}
#[tokio::main]
async fn main()->Result<()> {
  tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env()
    .unwrap_or_else(|_| "openscion_server=info".into())).init();
  let a=Args::parse();
  if !a.bind.ip().is_loopback() && !a.allow_remote {bail!("binding a non-loopback address requires --allow-remote and secure TLS termination");}
  let reg=Registry::open(&a.data)?;
  let socket=TcpListener::bind(a.bind).await?;
  tracing::info!(address=%a.bind,"OpenScion V0.6 server started");
  axum::serve(socket,router(AppState::new(reg,a.max_git_operations)))
    .with_graceful_shutdown(async {let _=tokio::signal::ctrl_c().await;}).await?;
  Ok(())
}
