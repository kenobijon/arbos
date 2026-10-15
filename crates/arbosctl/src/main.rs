//! Headless Arbos client (design §9 M0): pair, create project, open chat, send,
//! stream the reply, resume by cursor after a kill, stop/start the box.

mod client;
mod room;

use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use serde_json::{json, Value};

use crate::client::{Config, Relay};

#[derive(Parser, Debug)]
#[command(name = "arbosctl", version, about = "Headless Arbos client")]
struct Args {
    /// Relay base URL (overrides the saved one).
    #[arg(long, env = "ARBOS_RELAY_URL")]
    relay: Option<String>,
    /// Config file (device token, cursors).
    #[arg(long, env = "ARBOSCTL_CONFIG")]
    config: Option<std::path::PathBuf>,
    /// Print JSON instead of text where it applies.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Exchange a one-time pairing code for a device token (D35).
    Pair {
        code: String,
        #[arg(long, default_value = "arbosctl")]
        name: String,
    },
    /// Projects.
    Project {
        #[command(subcommand)]
        cmd: ProjectCmd,
    },
    /// Chats.
    Chat {
        #[command(subcommand)]
        cmd: ChatCmd,
    },
    /// Box lifecycle.
    Box {
        #[command(subcommand)]
        cmd: BoxCmd,
    },
    /// Today's and all-time metered usage.
    Usage { project: String },
    /// Recorded timings (boot, wake, TTFT, cold start).
    Timings { project: String },
    /// Devices.
    Devices {
        #[command(subcommand)]
        cmd: DeviceCmd,
    },
}

#[derive(Subcommand, Debug)]
enum ProjectCmd {
    Create {
        #[arg(long)]
        name: String,
        #[arg(long)]
        instructions: Option<String>,
        /// Box provider: daytona (default when the relay has a key), process, docker.
        #[arg(long)]
        provider: Option<String>,
        /// Shorthand for `--provider process` (runs agentd on the relay host).
        #[arg(long)]
        local_box: bool,
    },
    List,
    Status {
        project: String,
    },
}

#[derive(Subcommand, Debug)]
enum ChatCmd {
    /// Create a chat; prints its id.
    Open {
        project: String,
        #[arg(long)]
        title: Option<String>,
    },
    List {
        project: String,
    },
    /// Send a message; with --stream, print the reply as it arrives.
    Send {
        project: String,
        chat: String,
        text: String,
        #[arg(long)]
        stream: bool,
        /// Give up waiting for the reply after N seconds.
        #[arg(long, default_value_t = 600)]
        timeout: u64,
    },
    /// Join the room at the saved cursor and print rows as they arrive (resume after a kill).
    Stream {
        project: String,
        chat: String,
        /// Exit once the newest assistant message is done.
        #[arg(long)]
        until_done: bool,
        #[arg(long, default_value_t = 600)]
        timeout: u64,
    },
    /// Print the local replica of the chat (messages as JSON) after catching up.
    Dump {
        project: String,
        chat: String,
    },
    /// Stop the running turn in a chat now (aborts the main agent's pi turn / act loop).
    Stop {
        project: String,
        chat: String,
    },
    /// Forget the local replica and cursor for a chat.
    Reset {
        project: String,
        chat: String,
    },
}

#[derive(Subcommand, Debug)]
enum BoxCmd {
    Stop {
        project: String,
        #[arg(long)]
        force: bool,
    },
    Start {
        project: String,
    },
    Wake {
        project: String,
    },
    Status {
        project: String,
    },
}

#[derive(Subcommand, Debug)]
enum DeviceCmd {
    List,
    Revoke { id: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,loro_internal=error".into()),
        )
        .init();
    let args = Args::parse();
    let mut cfg = Config::load(args.config.clone())?;
    if let Some(r) = &args.relay {
        cfg.relay_url = r.trim_end_matches('/').to_owned();
    }
    let relay = Relay::new(&cfg);
    match args.cmd {
        Cmd::Pair { code, name } => {
            let v = relay
                .post_unauth("/pair", &json!({ "code": code, "name": name }))
                .await?;
            cfg.device_token = v
                .get("device_token")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            cfg.device_id = v
                .get("device_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            cfg.save()?;
            println!(
                "paired as device {} (token saved to {})",
                cfg.device_id,
                cfg.path.display()
            );
        }
        Cmd::Project { cmd } => match cmd {
            ProjectCmd::Create {
                name,
                instructions,
                provider,
                local_box,
            } => {
                let provider = provider.or(if local_box {
                    Some("process".into())
                } else {
                    None
                });
                let t0 = std::time::Instant::now();
                let v = relay.post("/projects", &json!({ "name": name, "instructions": instructions, "provider": provider })).await?;
                if args.json {
                    println!("{}", serde_json::to_string_pretty(&v)?);
                } else {
                    println!(
                        "project {} ({}) created in {:.1}s",
                        v.pointer("/project/id")
                            .and_then(Value::as_str)
                            .unwrap_or("?"),
                        name,
                        t0.elapsed().as_secs_f64()
                    );
                    println!(
                        "pinned chat {}",
                        v.pointer("/pinned_chat/id")
                            .and_then(Value::as_str)
                            .unwrap_or("?")
                    );
                    println!(
                        "box: {}",
                        v.get("sandbox").map(|s| s.to_string()).unwrap_or_default()
                    );
                }
            }
            ProjectCmd::List => {
                let v = relay.get("/projects").await?;
                if args.json {
                    println!("{}", serde_json::to_string_pretty(&v)?);
                } else if let Some(a) = v.as_array() {
                    for p in a {
                        println!(
                            "{}  {:<20} {:<16} box={}",
                            p["id"].as_str().unwrap_or(""),
                            p["name"].as_str().unwrap_or(""),
                            p["state"].as_str().unwrap_or(""),
                            p["box"].as_str().unwrap_or("")
                        );
                    }
                }
            }
            ProjectCmd::Status { project } => {
                let v = relay.get(&format!("/projects/{project}/status")).await?;
                println!("{}", serde_json::to_string_pretty(&v)?);
            }
        },
        Cmd::Chat { cmd } => match cmd {
            ChatCmd::Open { project, title } => {
                let v = relay
                    .post(
                        &format!("/projects/{project}/chats"),
                        &json!({ "title": title }),
                    )
                    .await?;
                let id = v.pointer("/chat/id").and_then(Value::as_str).unwrap_or("?");
                if args.json {
                    println!("{}", serde_json::to_string_pretty(&v)?);
                } else {
                    println!("{id}");
                }
            }
            ChatCmd::List { project } => {
                let v = relay.get(&format!("/projects/{project}/chats")).await?;
                println!("{}", serde_json::to_string_pretty(&v)?);
            }
            ChatCmd::Send {
                project,
                chat,
                text,
                stream,
                timeout,
            } => {
                let project = resolve_project(&relay, &project).await?;
                room::send(&mut cfg, &project, &chat, &text, stream, timeout).await?;
            }
            ChatCmd::Stream {
                project,
                chat,
                until_done,
                timeout,
            } => {
                let project = resolve_project(&relay, &project).await?;
                room::stream(&mut cfg, &project, &chat, until_done, timeout).await?;
            }
            ChatCmd::Dump { project, chat } => {
                let project = resolve_project(&relay, &project).await?;
                room::dump(&mut cfg, &project, &chat).await?;
            }
            ChatCmd::Stop { project, chat } => {
                let project = resolve_project(&relay, &project).await?;
                room::stop(&mut cfg, &project, &chat).await?;
            }
            ChatCmd::Reset { project, chat } => {
                let project = resolve_project(&relay, &project).await?;
                room::reset(&cfg, &project, &chat)?;
                println!("reset local replica for {project}/{chat}");
            }
        },
        Cmd::Box { cmd } => match cmd {
            BoxCmd::Stop { project, force } => {
                let t0 = std::time::Instant::now();
                let v = relay
                    .post(
                        &format!("/projects/{project}/stop?force={force}"),
                        &json!({}),
                    )
                    .await?;
                println!("stopped in {:.1}s: {v}", t0.elapsed().as_secs_f64());
            }
            BoxCmd::Start { project } => {
                let t0 = std::time::Instant::now();
                let v = relay
                    .post(&format!("/projects/{project}/start"), &json!({}))
                    .await?;
                println!("start accepted in {:.1}s: {v}", t0.elapsed().as_secs_f64());
            }
            BoxCmd::Wake { project } => {
                let v = relay
                    .post(&format!("/projects/{project}/wake"), &json!({}))
                    .await?;
                println!("{v}");
            }
            BoxCmd::Status { project } => {
                let v = relay.get(&format!("/projects/{project}/status")).await?;
                println!("{}", serde_json::to_string_pretty(&v)?);
            }
        },
        Cmd::Usage { project } => {
            let v = relay.get(&format!("/projects/{project}/usage")).await?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Cmd::Timings { project } => {
            let v = relay.get(&format!("/projects/{project}/status")).await?;
            let t = v.get("timings").cloned().unwrap_or(json!([]));
            if args.json {
                println!("{}", serde_json::to_string_pretty(&t)?);
            } else if let Some(a) = t.as_array() {
                for x in a {
                    println!(
                        "{:>8} ms  {:<34} {}",
                        x["ms"].as_i64().unwrap_or(0),
                        x["kind"].as_str().unwrap_or(""),
                        x["detail"].as_str().unwrap_or("")
                    );
                }
            }
        }
        Cmd::Devices { cmd } => match cmd {
            DeviceCmd::List => println!(
                "{}",
                serde_json::to_string_pretty(&relay.get("/devices").await?)?
            ),
            DeviceCmd::Revoke { id } => {
                relay.delete(&format!("/devices/{id}")).await?;
                println!("revoked {id}");
            }
        },
    }
    Ok(())
}

async fn resolve_project(relay: &Relay, id_or_name: &str) -> Result<String> {
    let v = relay.get(&format!("/projects/{id_or_name}")).await?;
    v.pointer("/project/id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("no such project {id_or_name}"))
}
