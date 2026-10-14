use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "arbos-relay",
    version,
    about = "Arbos relay: registry, pairing, sync rooms, blobs, model proxy"
)]
struct Args {
    /// Listen address.
    #[arg(long, env = "ARBOS_RELAY_LISTEN", default_value = "127.0.0.1:7700")]
    listen: String,
    /// Data directory (SQLite + blobs).
    #[arg(long, env = "ARBOS_RELAY_DATA", default_value = "./.arbos-local/relay")]
    data: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = Args::parse();
    tracing::info!(listen = %args.listen, data = %args.data, "arbos-relay scaffold (server lands in the next commits)");
    Ok(())
}
