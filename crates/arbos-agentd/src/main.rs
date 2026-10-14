use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "arbos-agentd", version, about = "Arbos box entrypoint")]
struct Args {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Boot the box: doc host, main agent, MCP, history store (image ENTRYPOINT).
    Boot,
    /// Run the in-box self test.
    Selftest,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = Args::parse();
    match args.cmd {
        Cmd::Boot => {
            tracing::info!("arbos-agentd boot scaffold (daemon lands in the next commits)")
        }
        Cmd::Selftest => tracing::info!("selftest scaffold"),
    }
    Ok(())
}
