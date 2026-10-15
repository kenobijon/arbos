//! In-app updates (D42). A tokio worker polls the appcast, downloads and
//! verifies on request, swaps the bundle, and asks the UI to relaunch. The
//! UI reads [`UpdateState`] off `AppState` and shows one button in the rail.
//!
//! Environment (all optional):
//!   ARBOS_UPDATE_FEED            feed URL (default: the GitHub release appcast)
//!   ARBOS_UPDATE_CHANNEL         `edge` (default: every green push to main) or `stable` (tags)
//!   ARBOS_UPDATE_INTERVAL_SECS   poll interval (default 3600; first poll after 5 s)
//!   ARBOS_UPDATE_DISABLE=1       never poll (dev runs)
//!
//! Every check/install step is also appended to `~/Library/Logs/Arbos/update.log` (or
//! `$XDG_STATE_HOME/arbos/update.log`), because a bundled app has no stderr anyone reads.

use std::sync::Arc;
use std::time::Duration;

use arbos_update::client::{self, Available, Install};
use tokio::sync::mpsc;

/// `0.1.0` from Cargo.toml, with the CI build number behind it (`0.1.0.212`)
/// when the bundle script exported `ARBOS_BUILD` at compile time.
pub fn current_version() -> String {
    // A stable (tag) build is exactly the Cargo version — the same string as the tag, the
    // appcast item, and the DMG name; edge builds append the commit count.
    if option_env!("ARBOS_DEFAULT_UPDATE_CHANNEL") == Some("stable") {
        return env!("CARGO_PKG_VERSION").to_string();
    }
    match option_env!("ARBOS_BUILD") {
        Some(build) if !build.is_empty() => format!("{}.{}", env!("CARGO_PKG_VERSION"), build),
        _ => env!("CARGO_PKG_VERSION").to_string(),
    }
}

pub const EDGE_FEED_URL: &str =
    "https://github.com/unarbos/arbos/releases/download/edge/appcast.xml";
pub const STABLE_FEED_URL: &str = arbos_update::DEFAULT_FEED_URL;

pub fn feed_url() -> String {
    if let Ok(url) = std::env::var("ARBOS_UPDATE_FEED") {
        if !url.is_empty() {
            return url;
        }
    }
    // The user's choice first; else the channel this build was made for (release.yml sets
    // ARBOS_DEFAULT_UPDATE_CHANNEL=stable for `v*` tag builds, edge for pushes to main), else edge.
    let built_for = option_env!("ARBOS_DEFAULT_UPDATE_CHANNEL").unwrap_or("edge");
    match std::env::var("ARBOS_UPDATE_CHANNEL")
        .ok()
        .filter(|c| !c.is_empty())
        .as_deref()
        .unwrap_or(built_for)
    {
        "stable" => STABLE_FEED_URL.to_string(),
        _ => EDGE_FEED_URL.to_string(),
    }
}

fn poll_interval() -> Duration {
    std::env::var("ARBOS_UPDATE_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(3600))
}

#[derive(Clone, Debug, PartialEq)]
pub enum UpdatePhase {
    /// No check has finished yet.
    Idle,
    Checking,
    UpToDate,
    /// A newer build is in the feed; the rail button shows.
    Available(Available),
    Downloading {
        available: Available,
        got: u64,
        total: Option<u64>,
    },
    Installing(Available),
    /// Installed; the app relaunches as soon as the UI calls `relaunch`.
    Restarting(Available),
    Failed {
        message: String,
        available: Option<Available>,
    },
}

#[derive(Clone, Debug)]
pub struct UpdateState {
    pub current_version: String,
    pub feed_url: String,
    pub phase: UpdatePhase,
    pub last_checked_at: Option<u64>,
    /// The running copy is translocated or in an unwritable folder: offer "Move to Applications".
    pub needs_relocation: bool,
}

impl UpdateState {
    pub fn new() -> Self {
        Self {
            current_version: current_version(),
            feed_url: feed_url(),
            phase: UpdatePhase::Idle,
            last_checked_at: None,
            needs_relocation: needs_relocation(),
        }
    }

    /// The build the rail button offers, in any phase that has one.
    pub fn available(&self) -> Option<&Available> {
        match &self.phase {
            UpdatePhase::Available(a)
            | UpdatePhase::Downloading { available: a, .. }
            | UpdatePhase::Installing(a)
            | UpdatePhase::Restarting(a) => Some(a),
            UpdatePhase::Failed { available, .. } => available.as_ref(),
            UpdatePhase::Idle | UpdatePhase::Checking | UpdatePhase::UpToDate => None,
        }
    }

    /// The rail button is visible whenever a newer build is known, through
    /// download, install, and a failure that can be retried.
    pub fn button_visible(&self) -> bool {
        self.available().is_some() || self.needs_relocation
    }

    /// True when the button's click should move the app rather than install an update.
    pub fn button_moves(&self) -> bool {
        self.needs_relocation && self.available().is_none()
    }

    pub fn busy(&self) -> bool {
        matches!(
            self.phase,
            UpdatePhase::Downloading { .. }
                | UpdatePhase::Installing(_)
                | UpdatePhase::Restarting(_)
        )
    }

    /// Short label for the rail button.
    pub fn button_label(&self) -> String {
        if self.button_moves() {
            return match &self.phase {
                UpdatePhase::Installing(_) | UpdatePhase::Restarting(_) => "Moving".into(),
                UpdatePhase::Failed { .. } => "Retry".into(),
                _ => "Move to Applications".into(),
            };
        }
        match &self.phase {
            UpdatePhase::Available(_) => "Update".into(),
            UpdatePhase::Downloading { got, total, .. } => match total {
                Some(t) if *t > 0 => format!("{}%", (got * 100 / t).min(100)),
                _ => "Downloading".into(),
            },
            UpdatePhase::Installing(_) => "Installing".into(),
            UpdatePhase::Restarting(_) => "Restarting".into(),
            UpdatePhase::Failed { .. } => "Retry".into(),
            UpdatePhase::Idle | UpdatePhase::Checking | UpdatePhase::UpToDate => String::new(),
        }
    }

    /// One line for the settings popover.
    pub fn status_line(&self) -> String {
        if self.button_moves()
            && matches!(
                self.phase,
                UpdatePhase::Idle | UpdatePhase::UpToDate | UpdatePhase::Checking
            )
        {
            return format!(
                "Arbos {} · running from a read-only copy; move it to Applications to enable updates",
                self.current_version
            );
        }
        match &self.phase {
            UpdatePhase::Idle => format!("Arbos {}", self.current_version),
            UpdatePhase::Checking => format!("Arbos {} · checking…", self.current_version),
            UpdatePhase::UpToDate => format!("Arbos {} · up to date", self.current_version),
            UpdatePhase::Available(a) => format!(
                "Arbos {} · {} is available",
                self.current_version, a.item.version
            ),
            UpdatePhase::Downloading { available, .. } => {
                format!("Downloading {}…", available.item.version)
            }
            UpdatePhase::Installing(a) => format!("Installing {}…", a.item.version),
            UpdatePhase::Restarting(a) => format!("Restarting into {}…", a.item.version),
            UpdatePhase::Failed { message, .. } => {
                // The full text is in ~/Library/Logs/Arbos/update.log; the
                // popover row only has room for the first sentence.
                let brief = message.lines().next().unwrap_or_default();
                let brief: String = if brief.chars().count() > 120 {
                    format!("{}…", brief.chars().take(119).collect::<String>())
                } else {
                    brief.to_string()
                };
                format!("Update failed: {brief}")
            }
        }
    }
}

impl Default for UpdateState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub enum UpdateCommand {
    Check,
    Install,
    /// Copy the running app into Applications and relaunch from there (no new version needed).
    MoveToApplications,
}

#[derive(Debug)]
pub enum UpdateEvent {
    Checking,
    UpToDate,
    Available(Box<Available>),
    Progress {
        got: u64,
        total: Option<u64>,
    },
    Installing,
    /// Swapped in; carries where the app lives so the relaunch does not
    /// re-detect it from a replaced executable.
    Installed(Install),
    Failed(String),
}

/// The worker: a poll loop plus on-demand check/install. Runs on tokio.
pub async fn run(
    state: UpdateState,
    mut commands: mpsc::UnboundedReceiver<UpdateCommand>,
    events: mpsc::UnboundedSender<UpdateEvent>,
) {
    if std::env::var("ARBOS_UPDATE_DISABLE").is_ok_and(|v| v == "1") {
        return;
    }
    let os = arbos_update::current_os();
    let current = state.current_version.clone();
    let feed = state.feed_url.clone();
    let mut known: Option<Available> = None;
    let mut next_poll = tokio::time::Instant::now() + Duration::from_secs(5);

    loop {
        let command = tokio::select! {
            _ = tokio::time::sleep_until(next_poll) => UpdateCommand::Check,
            cmd = commands.recv() => match cmd {
                Some(cmd) => cmd,
                None => return,
            },
        };
        match command {
            UpdateCommand::Check => {
                next_poll = tokio::time::Instant::now() + poll_interval();
                let _ = events.send(UpdateEvent::Checking);
                match client::check(&feed, &current, os).await {
                    Ok(Some(available)) => {
                        if known.as_ref().map(|k| &k.item.version) != Some(&available.item.version)
                        {
                            log(format!(
                                "check {feed}: {} available (running {current})",
                                available.item.version
                            ));
                        }
                        known = Some(available.clone());
                        let _ = events.send(UpdateEvent::Available(Box::new(available)));
                    }
                    Ok(None) => {
                        known = None;
                        let _ = events.send(UpdateEvent::UpToDate);
                    }
                    Err(err) => {
                        log(format!("check {feed} failed: {err:#}"));
                        // A failed poll is not news; keep whatever we knew.
                        if known.is_none() {
                            let _ = events.send(UpdateEvent::UpToDate);
                        }
                    }
                }
            }
            UpdateCommand::MoveToApplications => {
                let _ = events.send(UpdateEvent::Installing);
                match move_to_applications() {
                    Ok(moved) => {
                        let _ = events.send(UpdateEvent::Installed(moved));
                    }
                    Err(err) => {
                        log(format!("move to Applications failed: {err:#}"));
                        let _ = events.send(UpdateEvent::Failed(format!("{err:#}")));
                    }
                }
            }
            UpdateCommand::Install => {
                let Some(available) = known.clone() else {
                    let _ = events.send(UpdateEvent::Failed("no update to install".into()));
                    continue;
                };
                let outcome = install(&available, &events).await;
                match outcome {
                    Ok(installed) => {
                        let _ = events.send(UpdateEvent::Installed(installed));
                    }
                    Err(err) => {
                        log(format!("install failed: {err:#}"));
                        let _ = events.send(UpdateEvent::Failed(format!("{err:#}")));
                    }
                }
            }
        }
    }
}

/// Where a person can always get the newest build by hand (the fallback the failure notice offers).
pub const DOWNLOAD_URL: &str = "https://arbos.life/download/mac";

fn log_path() -> std::path::PathBuf {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    if cfg!(target_os = "macos") {
        home.join("Library/Logs/Arbos/update.log")
    } else {
        std::env::var_os("XDG_STATE_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| home.join(".local/state"))
            .join("arbos/update.log")
    }
}

/// One line to the update log (best effort).
pub fn log(line: impl AsRef<str>) {
    let path = log_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write;
        let ts = chrono_like_now();
        let _ = writeln!(f, "{ts} {}", line.as_ref());
    }
    eprintln!("[update] {}", line.as_ref());
}

fn chrono_like_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}")
}

/// Where the update goes. A copy that cannot be swapped in place — App Translocation (opened
/// from Downloads or the DMG) or an unwritable folder — is installed into Applications instead,
/// and the app relaunches from there; the stale copy in Downloads can be thrown away.
fn install_target(install: &Install) -> anyhow::Result<(Install, bool)> {
    if cfg!(target_os = "macos") && matches!(install, Install::Executable(_)) {
        anyhow::bail!("not running from Arbos.app; install the app to update it");
    }
    let path = install.path();
    if client::is_translocated(path) || !client::parent_writable(path) {
        let target = client::relocation_target(install)?;
        return Ok((Install::MacBundle(target), true));
    }
    Ok((install.clone(), false))
}

/// Is the running copy somewhere it cannot update itself (so the rail offers "Move to
/// Applications" even when no newer build exists)?
pub fn needs_relocation() -> bool {
    if !cfg!(target_os = "macos") {
        return false;
    }
    match Install::detect() {
        Ok(Install::MacBundle(p)) => client::is_translocated(&p) || !client::parent_writable(&p),
        _ => false,
    }
}

/// "Move to Applications": copy the running bundle into Applications, clear quarantine, and
/// hand back the new location for the relaunch.
pub fn move_to_applications() -> anyhow::Result<Install> {
    let install = Install::detect()?;
    let Install::MacBundle(bundle) = &install else {
        anyhow::bail!("not running from Arbos.app");
    };
    log(format!("move {} to Applications", bundle.display()));
    let moved = client::relocate_bundle(bundle)?;
    log(format!("moved to {}", moved.path().display()));
    Ok(moved)
}

async fn install(
    available: &Available,
    events: &mpsc::UnboundedSender<UpdateEvent>,
) -> anyhow::Result<Install> {
    let key = arbos_update::public_key()
        .ok_or_else(|| anyhow::anyhow!("this build has no update public key"))?;
    let running = Install::detect()?;
    let (install, relocated) = install_target(&running)?;
    log(format!(
        "install {} from {} into {}{}",
        available.item.version,
        available.url,
        install.path().display(),
        if relocated {
            format!(
                " (running copy at {} cannot be updated in place)",
                running.path().display()
            )
        } else {
            String::new()
        }
    ));
    let dir = client::updates_dir();
    client::prune(&dir, &available.item.version);
    let progress_tx = events.clone();
    let progress: client::ProgressFn = Arc::new(move |got, total| {
        let _ = progress_tx.send(UpdateEvent::Progress { got, total });
    });
    let payload = client::download(available, &key, &dir, Some(progress)).await?;
    log(format!("downloaded and verified {}", payload.display()));
    let _ = events.send(UpdateEvent::Installing);
    // Staging reads the running bundle's layout (executable name); apply targets `install`.
    let staged = client::stage(&payload, &available.item.version, &dir, &running)?;
    log(format!("staged {}", staged.display()));
    client::apply(&staged, &install)?;
    if relocated {
        client::remove_quarantine(install.path());
    }
    log(format!(
        "installed {} at {}",
        available.item.version,
        install.path().display()
    ));
    let _ = std::fs::remove_file(&payload);
    Ok(install)
}

/// Open the download page in the browser (the fallback offered when self-update cannot work).
pub fn open_download_page() {
    if cfg!(target_os = "macos") {
        let _ = std::process::Command::new("/usr/bin/open")
            .arg(DOWNLOAD_URL)
            .spawn();
    } else {
        let _ = std::process::Command::new("xdg-open")
            .arg(DOWNLOAD_URL)
            .spawn();
    }
}

/// Called by the UI once `Installed` arrives: schedule the relaunch; the
/// caller quits the app.
pub fn relaunch(install: &Install) -> anyhow::Result<()> {
    client::relaunch_after_exit(install)
}
