//! The app side: poll the feed, download and verify a payload, swap it in,
//! relaunch. Every step is a plain function so the Mac app, the CLI, and the
//! Linux end-to-end test drive the same code.
//!
//! Payloads are `.tar.gz`:
//!   macOS  `Arbos.app/` at the root (the bundle to swap in)
//!   Linux  `arbos-mac` at the root (the executable to swap in)

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use anyhow::{bail, Context};
use futures::StreamExt as _;

use crate::appcast::{Appcast, Item};
use crate::sign::PublicKey;

/// Download progress: bytes so far, total when the server says.
pub type ProgressFn = Arc<dyn Fn(u64, Option<u64>) + Send + Sync>;

/// What a successful check found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Available {
    pub item: Item,
    pub url: String,
    pub length: u64,
    pub ed_signature: String,
}

/// How the running program is installed, which decides how it is replaced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Install {
    /// `…/Arbos.app` — the bundle is swapped.
    MacBundle(PathBuf),
    /// A bare executable — the file is swapped.
    Executable(PathBuf),
}

impl Install {
    /// Where this process runs from. A macOS `cargo run` binary (not inside a
    /// bundle) is an `Executable` and gets refused by [`apply`].
    pub fn detect() -> anyhow::Result<Install> {
        let exe = std::env::current_exe().context("current executable path")?;
        // Once the file under a running process is replaced, Linux reports
        // `/proc/self/exe` as "<path> (deleted)". Callers detect before they
        // swap and keep the result, but be safe here too.
        let exe = match exe.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
            Some(path) => PathBuf::from(path),
            None => exe,
        };
        let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
        if cfg!(target_os = "macos") {
            // <bundle>.app/Contents/MacOS/<exe>
            if let Some(bundle) = exe.ancestors().nth(3) {
                if bundle.extension().is_some_and(|e| e == "app") {
                    return Ok(Install::MacBundle(bundle.to_path_buf()));
                }
            }
        }
        Ok(Install::Executable(exe))
    }

    pub fn path(&self) -> &Path {
        match self {
            Install::MacBundle(p) | Install::Executable(p) => p,
        }
    }
}

fn http() -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(format!("Arbos-Updater/{}", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .context("http client")
}

/// Fetch and parse the feed.
pub async fn fetch_appcast(feed_url: &str) -> anyhow::Result<Appcast> {
    let text = http()?
        .get(feed_url)
        .send()
        .await
        .with_context(|| format!("GET {feed_url}"))?
        .error_for_status()
        .with_context(|| format!("GET {feed_url}"))?
        .text()
        .await?;
    Appcast::parse(&text)
}

/// `sw_vers -productVersion` on macOS; `None` elsewhere.
pub fn system_version() -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let out = Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .ok()?;
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!v.is_empty()).then_some(v)
}

/// Poll the feed once. `Ok(None)` means up to date.
pub async fn check(feed_url: &str, current: &str, os: &str) -> anyhow::Result<Option<Available>> {
    let appcast = fetch_appcast(feed_url).await?;
    let system = system_version();
    let Some(item) = appcast.newest_for(current, os, system.as_deref()) else {
        return Ok(None);
    };
    let enc = item
        .enclosure_for(os)
        .context("item has no enclosure for this OS")?;
    if enc.ed_signature.is_empty() {
        bail!("appcast item {} has no sparkle:edSignature", item.version);
    }
    Ok(Some(Available {
        item: item.clone(),
        url: enc.url.clone(),
        length: enc.length,
        ed_signature: enc.ed_signature.clone(),
    }))
}

/// `~/Library/Application Support/Arbos/updates` or `$XDG_DATA_HOME/arbos/updates`.
pub fn updates_dir() -> PathBuf {
    let base = dirs::data_dir().unwrap_or_else(std::env::temp_dir);
    let app = if cfg!(target_os = "macos") {
        "Arbos"
    } else {
        "arbos"
    };
    base.join(app).join("updates")
}

/// Download the payload to `dir`, check its length and Ed25519 signature,
/// and return the verified file. A payload that fails verification is
/// deleted; nothing unverified is left on disk.
pub async fn download(
    available: &Available,
    key: &PublicKey,
    dir: &Path,
    progress: Option<ProgressFn>,
) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let name = available
        .url
        .rsplit('/')
        .next()
        .filter(|n| !n.is_empty() && !n.contains(".."))
        .unwrap_or("payload.tar.gz");
    let dest = dir.join(name);
    let partial = dir.join(format!("{name}.part"));

    let response = http()?
        .get(&available.url)
        .send()
        .await
        .with_context(|| format!("GET {}", available.url))?
        .error_for_status()?;
    let total = response
        .content_length()
        .or_else(|| (available.length > 0).then_some(available.length));
    let mut file = tokio::fs::File::create(&partial)
        .await
        .with_context(|| format!("creating {}", partial.display()))?;
    let mut stream = response.bytes_stream();
    let mut got: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("reading the download")?;
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await?;
        got += chunk.len() as u64;
        if let Some(p) = &progress {
            p(got, total);
        }
    }
    tokio::io::AsyncWriteExt::flush(&mut file).await?;
    drop(file);

    let bytes = tokio::fs::read(&partial).await?;
    let verdict = verify_payload(&bytes, available, key);
    if let Err(err) = verdict {
        let _ = tokio::fs::remove_file(&partial).await;
        return Err(err);
    }
    tokio::fs::rename(&partial, &dest).await?;
    Ok(dest)
}

/// Length (when the feed states one) and signature over the bytes.
pub fn verify_payload(bytes: &[u8], available: &Available, key: &PublicKey) -> anyhow::Result<()> {
    if available.length > 0 && bytes.len() as u64 != available.length {
        bail!(
            "payload is {} bytes, the appcast says {}",
            bytes.len(),
            available.length
        );
    }
    key.verify(bytes, &available.ed_signature)
        .context("payload signature rejected; not installing")
}

/// Gatekeeper's App Translocation: an app opened straight from a download (quarantined, not
/// moved) runs from a read-only nullfs mount under `…/AppTranslocation/<uuid>/d/`. Nothing can
/// be written beside it, so an update has to go somewhere else.
pub fn is_translocated(path: &Path) -> bool {
    path.to_string_lossy().contains("/AppTranslocation/")
}

/// Can this process create files in the folder that holds `path`?
pub fn parent_writable(path: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    let probe = parent.join(format!(".arbos-update-probe-{}", std::process::id()));
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Where a relocated app goes: `/Applications` when this user can write there, else
/// `~/Applications` (created if needed).
pub fn applications_dir() -> anyhow::Result<PathBuf> {
    let system = PathBuf::from("/Applications");
    let probe = system.join(format!(".arbos-update-probe-{}", std::process::id()));
    if std::fs::write(&probe, b"").is_ok() {
        let _ = std::fs::remove_file(&probe);
        return Ok(system);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")?;
    let user = home.join("Applications");
    std::fs::create_dir_all(&user).with_context(|| format!("creating {}", user.display()))?;
    Ok(user)
}

/// The bundle a translocated or unwritable install should be replaced by: `<Applications>/<name>`.
pub fn relocation_target(install: &Install) -> anyhow::Result<PathBuf> {
    let name = install
        .path()
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "Arbos.app".into());
    Ok(applications_dir()?.join(name))
}

/// Strip the quarantine flag so Gatekeeper does not translocate the copy again.
pub fn remove_quarantine(path: &Path) {
    let _ = Command::new("/usr/bin/xattr")
        .args(["-dr", "com.apple.quarantine", &path.to_string_lossy()])
        .output();
}

/// Copy `source_bundle` (the running app, readable even when translocated) to
/// `<Applications>/<name>`, replacing what is there, and clear quarantine. Returns the new
/// install so the caller can relaunch from it. This is the "Move to Applications" action.
pub fn relocate_bundle(source_bundle: &Path) -> anyhow::Result<Install> {
    if !cfg!(target_os = "macos") {
        bail!("relocation is macOS only");
    }
    let target = relocation_target(&Install::MacBundle(source_bundle.to_path_buf()))?;
    if std::fs::canonicalize(source_bundle).ok() == std::fs::canonicalize(&target).ok() {
        return Ok(Install::MacBundle(target));
    }
    apply_into(source_bundle, &target)?;
    remove_quarantine(&target);
    Ok(Install::MacBundle(target))
}

/// Put `staged` at `target` (a bundle path that may or may not exist yet): copy beside it, then
/// swap, keeping the old copy until the new one is in place.
pub fn apply_into(staged: &Path, target: &Path) -> anyhow::Result<()> {
    let parent = target.parent().context("target has no parent")?;
    std::fs::create_dir_all(parent)?;
    let name = target
        .file_name()
        .context("target has no name")?
        .to_string_lossy();
    let pid = std::process::id();
    let fresh = parent.join(format!(".{name}.new-{pid}"));
    let old = parent.join(format!(".{name}.old-{pid}"));
    let _ = std::fs::remove_dir_all(&fresh);
    run(
        "ditto",
        &[&staged.to_string_lossy(), &fresh.to_string_lossy()],
    )
    .with_context(|| format!("copying the new app into {}", parent.display()))?;
    if target.exists() {
        if let Err(err) = std::fs::rename(target, &old) {
            let _ = std::fs::remove_dir_all(&fresh);
            return Err(err).with_context(|| {
                format!(
                    "moving {} aside (is the folder writable?)",
                    target.display()
                )
            });
        }
    }
    if let Err(err) = std::fs::rename(&fresh, target) {
        if old.exists() {
            let _ = std::fs::rename(&old, target);
        }
        let _ = std::fs::remove_dir_all(&fresh);
        return Err(err).context("installing the new app bundle");
    }
    let _ = std::fs::remove_dir_all(&old);
    Ok(())
}

/// Unpack a verified payload into `<dir>/stage-<version>` and return the
/// thing to install (`Arbos.app` or `arbos-mac`), checked for shape.
pub fn stage(
    payload: &Path,
    version: &str,
    dir: &Path,
    install: &Install,
) -> anyhow::Result<PathBuf> {
    let stage = dir.join(format!("stage-{version}"));
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage)?;
    run(
        "tar",
        &[
            "-xzf",
            &payload.to_string_lossy(),
            "-C",
            &stage.to_string_lossy(),
        ],
    )
    .context("unpacking the payload")?;
    match install {
        Install::MacBundle(current) => {
            let name = current
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "Arbos.app".into());
            let bundle = stage.join(&name);
            let exe_name = current
                .join("Contents/MacOS")
                .read_dir()
                .ok()
                .and_then(|mut d| d.next())
                .and_then(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .unwrap_or_else(|| "arbos-mac".into());
            let exe = bundle.join("Contents/MacOS").join(exe_name);
            if !exe.is_file() {
                bail!("payload has no {name}/Contents/MacOS executable");
            }
            if cfg!(target_os = "macos") {
                run(
                    "codesign",
                    &["--verify", "--deep", "--strict", &bundle.to_string_lossy()],
                )
                .context("the downloaded app failed codesign --verify")?;
            }
            Ok(bundle)
        }
        Install::Executable(current) => {
            let name = current
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "arbos-mac".into());
            let exe = stage.join(&name);
            if !exe.is_file() {
                bail!("payload has no {name} executable at its root");
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755))?;
            }
            Ok(exe)
        }
    }
}

/// Swap the staged thing into place. Two renames with the old copy kept until
/// the new one is in, so a failure leaves the installed app untouched.
pub fn apply(staged: &Path, install: &Install) -> anyhow::Result<()> {
    match install {
        Install::MacBundle(bundle) => {
            if !cfg!(target_os = "macos") {
                bail!("bundle install is macOS only");
            }
            apply_into(staged, bundle)
        }
        Install::Executable(exe) => {
            let parent = exe.parent().context("executable has no parent")?;
            let name = exe
                .file_name()
                .context("executable has no name")?
                .to_string_lossy();
            let fresh = parent.join(format!(".{name}.new-{}", std::process::id()));
            std::fs::copy(staged, &fresh)
                .with_context(|| format!("copying the new executable beside {}", exe.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&fresh, std::fs::Permissions::from_mode(0o755))?;
            }
            // rename(2) over a running executable is allowed on Linux and
            // macOS: the old inode lives on until this process exits.
            std::fs::rename(&fresh, exe).with_context(|| format!("replacing {}", exe.display()))?;
            Ok(())
        }
    }
}

/// Start a detached shell that waits for this process to exit, then launches
/// the installed app. The caller quits right after. Paths are passed as
/// positional parameters, never spliced into the script.
pub fn relaunch_after_exit(install: &Install) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        let pid = std::process::id().to_string();
        let (program, target) = match install {
            Install::MacBundle(bundle) => ("/usr/bin/open", bundle.as_path()),
            Install::Executable(exe) => ("", exe.as_path()),
        };
        let script = r#"while /bin/kill -0 "$1" 2>/dev/null; do sleep 0.2; done
if [ -n "$2" ]; then exec "$2" "$3"; else exec "$3"; fi"#;
        Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .arg("arbos-relaunch")
            .arg(&pid)
            .arg(program)
            .arg(target)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0)
            .spawn()
            .context("spawning the relauncher")?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = install;
        bail!("relaunch is not supported on this platform")
    }
}

/// Remove staged copies and payloads other than `keep`.
pub fn prune(dir: &Path, keep: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.contains(keep) {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            let _ = std::fs::remove_dir_all(&path);
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }
}

fn run(program: &str, args: &[&str]) -> anyhow::Result<()> {
    let out = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("running {program}"))?;
    if !out.status.success() {
        bail!(
            "{program} failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}
