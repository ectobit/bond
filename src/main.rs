//!
#![warn(missing_debug_implementations, rust_2018_idioms, missing_docs)]

use bond::Client;
use tokio::signal;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    std::env::set_var("RUST_LOG", "debug,kube-runtime=warn,kube=warn");
    env_logger::init();

    log::info!("starting bond controller");
    Client::new(kube::Client::try_default().await?).run().await;

    signal::ctrl_c().await?;

    Ok(())
}
