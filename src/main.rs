use std::str::FromStr;

use anyhow::{Context, bail};
use log::info;
use smol::LocalExecutor;

use crate::{device::Device, http::ValetudoClient};

mod device;
mod generated;
mod handlers;
mod http;
mod net;
mod node;

const DEFAULT_VALETUDO_URI: &str = "http://localhost:80";

fn main() -> Result<(), anyhow::Error> {
    env_logger::init_from_env(env_logger::Env::default());

    let ex: &'static LocalExecutor<'static> = Box::leak(Box::new(LocalExecutor::new()));

    let fut = run(ex);
    smol::block_on(ex.run(fut))
}

async fn run(ex: &'static LocalExecutor<'static>) -> anyhow::Result<()> {
    let valetudo_uri = std::env::var("VALETUDO_URI").unwrap_or(DEFAULT_VALETUDO_URI.to_owned());
    let valetudo_uri =
        hyper::Uri::from_str(&valetudo_uri).context("Failed to parse VALETUDO_URI")?;
    if valetudo_uri.authority().is_none() {
        bail!("Invalid VALETUDO_URI: {valetudo_uri}");
    }

    info!("Connecting to {valetudo_uri}");
    let client = ValetudoClient::new(ex, valetudo_uri);
    let robot = Device::init(client.clone()).await?;

    let robot: &'static Device = Box::leak(Box::new(robot));
    ex.spawn(robot.monitor_status(client.clone())).detach();

    node::run(robot).await?;
    Ok(())
}
