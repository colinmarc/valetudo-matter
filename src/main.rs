use std::str::FromStr;

use anyhow::{Context, bail};
use log::info;
use smol::LocalExecutor;

use crate::{device::Device, http::ValetudoClient};

mod device;
mod general_diagnostics;
mod generated;
mod handlers;
mod http;
mod identity;
mod matter_audit;
mod net;
mod node;
mod node_lifetime;
mod persistence_path;
mod power_source;
mod wifi_diagnostics;

const DEFAULT_VALETUDO_URI: &str = "http://localhost:80";

fn main() -> Result<(), anyhow::Error> {
    env_logger::init_from_env(env_logger::Env::default().default_filter_or("valetudo_matter=info"));

    let ex: &'static LocalExecutor<'static> = Box::leak(Box::new(LocalExecutor::new()));

    let fut = run(ex);
    smol::block_on(ex.run(fut))
}

async fn run(ex: &'static LocalExecutor<'static>) -> anyhow::Result<()> {
    let valetudo_uri =
        std::env::var("VALETUDO_MATTER_BASE_URI").unwrap_or(DEFAULT_VALETUDO_URI.to_owned());
    let valetudo_uri =
        hyper::Uri::from_str(&valetudo_uri).context("Failed to parse VALETUDO_URI")?;
    if valetudo_uri.authority().is_none() {
        bail!("Invalid VALETUDO_MATTER_BASE_URI: {valetudo_uri}");
    }

    // Read-only support report: no Matter node, persistence or commissioning.
    if std::env::args().nth(1).as_deref() == Some("--diagnostics") {
        let client = ValetudoClient::new(ex, valetudo_uri);
        let native = std::env::var("VALETUDO_MATTER_NATIVE_DIAGNOSTICS").as_deref() == Ok("1");
        let identity = identity::Identity::load(&client, native).await?;
        println!("{}", serde_json::to_string_pretty(identity.diagnostics())?);
        return Ok(());
    }

    let persistence_dir = persistence_path::resolve()?;
    info!("Matter state directory: {}", persistence_dir.display());

    if let Some(parent) = persistence_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }

    info!("Connecting to {valetudo_uri}");
    let client = ValetudoClient::new(ex, valetudo_uri);
    let robot = Device::init(client.clone()).await?;

    let robot: &'static Device = Box::leak(Box::new(robot));
    node::run(robot, &persistence_dir).await?;
    Ok(())
}
