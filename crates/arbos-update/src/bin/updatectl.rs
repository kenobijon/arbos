//! `arbos-updatectl`: the release side of the updater, and a headless client
//! for testing it.
//!
//!   keygen  --public-out crates/arbos-update/update-key.pub   > private.key
//!   sign    --key-file private.key  FILE…                       (signatures)
//!   verify  --signature SIG FILE                                 (compiled-in key)
//!   appcast --version 0.1.0.212 --build 212 --enclosure URL=FILE[=OS] … --out appcast.xml
//!   check   --feed URL --current 0.1.0.211                       (what would install)
//!   fetch   --feed URL --current 0.1.0.211 --dir DIR             (download + verify + unpack)
//!
//! The private key comes from `--key-file` or `$ARBOS_SPARKLE_PRIVATE_KEY`;
//! it is never printed or written by anything but `keygen`.

use std::path::PathBuf;

use anyhow::{bail, Context};
use clap::{Parser, Subcommand};

use arbos_update::appcast::{self, Appcast, Enclosure, Item};
use arbos_update::client::{self, Install};
use arbos_update::sign::{sha256_hex, PrivateKey, PublicKey};

#[derive(Parser)]
#[command(
    name = "arbos-updatectl",
    about = "Arbos update feed: keys, signatures, appcast"
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Generate an Ed25519 key pair. Private line to stdout, public line to a file.
    Keygen {
        /// Where to write `ed25519 <public>` (the committed, compiled-in half).
        #[arg(long)]
        public_out: Option<PathBuf>,
    },
    /// Sign files; prints `<file>\t<length>\t<sha256>\t<edSignature>` per file.
    Sign {
        #[arg(long, env = "ARBOS_SPARKLE_PRIVATE_KEY_FILE")]
        key_file: Option<PathBuf>,
        files: Vec<PathBuf>,
    },
    /// Verify a signature against the compiled-in public key (or `--public-file`).
    Verify {
        #[arg(long)]
        signature: String,
        #[arg(long)]
        public_file: Option<PathBuf>,
        file: PathBuf,
    },
    /// Write an appcast item for one build, merged into `--previous` if given.
    Appcast {
        /// Full build version the app orders on, e.g. 0.1.0.212.
        #[arg(long)]
        version: String,
        /// `sparkle:version` (build number), e.g. 212. Defaults to the last component.
        #[arg(long)]
        build: Option<String>,
        /// `URL=FILE` or `URL=FILE=OS` (OS defaults to macos). Repeatable.
        #[arg(long = "enclosure", required = true)]
        enclosures: Vec<String>,
        #[arg(long)]
        link: Option<String>,
        #[arg(long)]
        notes: Option<String>,
        #[arg(long)]
        notes_file: Option<PathBuf>,
        #[arg(long, default_value = "14.0")]
        minimum_system_version: String,
        #[arg(long, default_value = "Arbos")]
        title: String,
        /// An existing appcast.xml to merge into (older items are kept).
        #[arg(long)]
        previous: Option<PathBuf>,
        #[arg(long, default_value_t = 10)]
        keep: usize,
        #[arg(long, env = "ARBOS_SPARKLE_PRIVATE_KEY_FILE")]
        key_file: Option<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Verify every enclosure of one item in an appcast file (the newest, or
    /// `--version`) against the payload files in `--dir`, with the compiled-in
    /// public key. The release pipeline runs this so a key mismatch never
    /// leaves the runner.
    VerifyAppcast {
        #[arg(long)]
        appcast: PathBuf,
        #[arg(long)]
        dir: PathBuf,
        #[arg(long)]
        public_file: Option<PathBuf>,
        /// Check this item instead of the newest one (a stable build merged
        /// into an edge feed that already carries newer edge builds).
        #[arg(long)]
        version: Option<String>,
    },
    /// Fetch a feed and print the item that would install, as JSON.
    Check {
        #[arg(long, default_value = arbos_update::DEFAULT_FEED_URL)]
        feed: String,
        #[arg(long)]
        current: String,
        #[arg(long, default_value = arbos_update::current_os())]
        os: String,
    },
    /// Check, download, verify, and unpack into `--dir`; does not install.
    Fetch {
        #[arg(long, default_value = arbos_update::DEFAULT_FEED_URL)]
        feed: String,
        #[arg(long)]
        current: String,
        #[arg(long, default_value = arbos_update::current_os())]
        os: String,
        #[arg(long)]
        dir: PathBuf,
        #[arg(long)]
        public_file: Option<PathBuf>,
        /// Stage as if this were the installed app (`…/Arbos.app` or an
        /// executable path); defaults to this binary.
        #[arg(long)]
        install_path: Option<PathBuf>,
        /// Also swap the staged build into `--install-path` (no relaunch).
        #[arg(long)]
        apply: bool,
    },
}

fn load_private(key_file: Option<PathBuf>) -> anyhow::Result<PrivateKey> {
    if let Some(path) = key_file {
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        return PrivateKey::parse(&text);
    }
    if let Ok(text) = std::env::var("ARBOS_SPARKLE_PRIVATE_KEY") {
        return PrivateKey::parse(&text);
    }
    bail!("no private key: pass --key-file or set ARBOS_SPARKLE_PRIVATE_KEY")
}

fn load_public(public_file: Option<PathBuf>) -> anyhow::Result<PublicKey> {
    match public_file {
        Some(path) => {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            PublicKey::parse_line(&text)
        }
        None => arbos_update::public_key()
            .context("no public key compiled in (crates/arbos-update/update-key.pub is empty)"),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Cmd::Keygen { public_out } => {
            let key = PrivateKey::generate();
            let public = key.public().to_line();
            match public_out {
                Some(path) => {
                    let header = "# The public half of the Arbos update signing key (Ed25519, Sparkle\n# `SUPublicEDKey` encoding). Compiled into the app: a payload installs only\n# when its sparkle:edSignature verifies against this line. The private half\n# is in the 1Password vault \"Arbos\" and in CI; it is never committed.\n";
                    std::fs::write(&path, format!("{header}{public}\n"))
                        .with_context(|| format!("writing {}", path.display()))?;
                    eprintln!("public key written to {}", path.display());
                }
                None => eprintln!("{public}"),
            }
            println!("{}", key.to_line());
        }
        Cmd::Sign { key_file, files } => {
            let key = load_private(key_file)?;
            for file in files {
                let bytes =
                    std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
                println!(
                    "{}\t{}\t{}\t{}",
                    file.display(),
                    bytes.len(),
                    sha256_hex(&bytes),
                    key.sign(&bytes)
                );
            }
        }
        Cmd::Verify {
            signature,
            public_file,
            file,
        } => {
            let key = load_public(public_file)?;
            let bytes =
                std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
            key.verify(&bytes, &signature)?;
            println!("ok: {} ({} bytes) verifies", file.display(), bytes.len());
        }
        Cmd::Appcast {
            version,
            build,
            enclosures,
            link,
            notes,
            notes_file,
            minimum_system_version,
            title,
            previous,
            keep,
            key_file,
            out,
        } => {
            let key = load_private(key_file)?;
            let notes = match (notes, notes_file) {
                (Some(n), _) => Some(n),
                (None, Some(path)) => Some(std::fs::read_to_string(path)?),
                (None, None) => None,
            };
            let mut encs = Vec::new();
            for spec in enclosures {
                let mut parts = spec.splitn(3, '=');
                let url = parts.next().unwrap_or_default().to_string();
                let file = parts
                    .next()
                    .with_context(|| format!("--enclosure {spec}: want URL=FILE[=OS]"))?;
                let os = parts.next().unwrap_or("macos").to_string();
                let bytes = std::fs::read(file).with_context(|| format!("reading {file}"))?;
                eprintln!(
                    "{file}: {} bytes, sha256 {}, os {os}",
                    bytes.len(),
                    sha256_hex(&bytes)
                );
                encs.push(Enclosure {
                    url,
                    length: bytes.len() as u64,
                    os,
                    ed_signature: key.sign(&bytes),
                });
            }
            let build =
                build.unwrap_or_else(|| version.rsplit('.').next().unwrap_or(&version).to_string());
            let item = Item {
                title: version.clone(),
                version: version.clone(),
                build,
                minimum_system_version: Some(minimum_system_version),
                pub_date: Some(appcast::pub_date_now()),
                link,
                notes,
                enclosures: encs,
            };
            let mut feed = match previous {
                Some(path) if path.exists() => {
                    let text = std::fs::read_to_string(&path)?;
                    Appcast::parse(&text).unwrap_or_else(|err| {
                        eprintln!("previous appcast ignored: {err:#}");
                        Appcast {
                            title: title.clone(),
                            items: Vec::new(),
                        }
                    })
                }
                _ => Appcast {
                    title: title.clone(),
                    items: Vec::new(),
                },
            };
            feed.title = title;
            feed.push_newest(item, keep);
            std::fs::write(&out, feed.to_xml())
                .with_context(|| format!("writing {}", out.display()))?;
            eprintln!("{} items -> {}", feed.items.len(), out.display());
        }
        Cmd::VerifyAppcast {
            appcast,
            dir,
            public_file,
            version,
        } => {
            let key = load_public(public_file)?;
            let text = std::fs::read_to_string(&appcast)
                .with_context(|| format!("reading {}", appcast.display()))?;
            let feed = Appcast::parse(&text)?;
            let newest = match &version {
                Some(v) => feed
                    .items
                    .iter()
                    .find(|i| &i.version == v)
                    .with_context(|| format!("appcast has no item {v}"))?,
                None => feed
                    .items
                    .iter()
                    .max_by_key(|i| i.parsed_version())
                    .context("appcast has no items")?,
            };
            let mut checked = 0;
            for enc in &newest.enclosures {
                let name = enc.url.rsplit('/').next().unwrap_or_default();
                let path = dir.join(name);
                if !path.is_file() {
                    bail!(
                        "{}: enclosure {name} is not in {}",
                        newest.version,
                        dir.display()
                    );
                }
                let bytes = std::fs::read(&path)?;
                if bytes.len() as u64 != enc.length {
                    bail!(
                        "{name}: {} bytes on disk, appcast says {}",
                        bytes.len(),
                        enc.length
                    );
                }
                key.verify(&bytes, &enc.ed_signature).with_context(|| {
                    format!("{name}: signature does not verify against the compiled-in key")
                })?;
                println!("ok: {name} ({} bytes, {})", bytes.len(), enc.os);
                checked += 1;
            }
            if checked == 0 {
                bail!("{}: no enclosures to verify", newest.version);
            }
        }
        Cmd::Check { feed, current, os } => match client::check(&feed, &current, &os).await? {
            Some(available) => {
                println!("{}", serde_json::to_string_pretty(&available.item)?);
            }
            None => {
                println!("up to date ({current})");
            }
        },
        Cmd::Fetch {
            feed,
            current,
            os,
            dir,
            public_file,
            install_path,
            apply,
        } => {
            let key = load_public(public_file)?;
            let Some(available) = client::check(&feed, &current, &os).await? else {
                println!("up to date ({current})");
                return Ok(());
            };
            eprintln!("downloading {} ({} bytes)", available.url, available.length);
            let payload = client::download(&available, &key, &dir, None).await?;
            eprintln!("verified {}", payload.display());
            let install = match install_path {
                Some(path) if path.extension().is_some_and(|e| e == "app") => {
                    Install::MacBundle(path)
                }
                Some(path) => Install::Executable(path),
                None => Install::detect()?,
            };
            let staged = client::stage(&payload, &available.item.version, &dir, &install)?;
            println!("{}", staged.display());
            if apply {
                client::apply(&staged, &install)?;
                println!(
                    "installed {} into {}",
                    available.item.version,
                    install.path().display()
                );
            }
        }
    }
    Ok(())
}
