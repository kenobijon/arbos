use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "arbosctl", version, about = "Headless Arbos client")]
struct Args {
    /// Relay base URL.
    #[arg(long, env = "ARBOS_RELAY_URL", default_value = "http://127.0.0.1:7700")]
    relay: String,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Exchange a one-time pairing code for a device token.
    Pair { code: String },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = Args::parse();
    match args.cmd {
        Cmd::Pair { code } => {
            tracing::info!(relay = %args.relay, %code, "arbosctl scaffold (client lands in the next commits)")
        }
    }
    Ok(())
}
