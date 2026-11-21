//! Updates: finding a newer release of Scrollback on GitHub, downloading it,
//! checking it, and putting it in place of the running copy.
//!
//! Each release carries a `SHA256SUMS` file listing its packages; a download
//! is only installed if it matches. Where Scrollback cannot replace itself
//! (a .deb install, or a copy somewhere it cannot write), it says a release
//! is out and links to it instead.
//!
//! Checking asks GitHub's API for the latest published release and sends
//! nothing about the person or their sessions.

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Where releases are published.
pub const REPOSITORY: &str = "frankievalentine/Scrollback";
/// The version running now.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// A release newer than this one.
#[derive(Debug, Clone)]
pub struct Release {
    /// Its version, such as `0.1.3`.
    pub version: String,
    /// Its page on GitHub, with its notes.
    pub page: String,
    /// The package for this system, when there is one.
    pub package: Option<Package>,
    /// Where its checksums are, when it has them.
    pub sums: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Package {
    pub name: String,
    pub url: String,
    pub size: u64,
}

impl Release {
    /// Whether this copy can download and install it by itself. Not if it
    /// was installed before and this copy is still the old version, which
    /// would otherwise install it over and over.
    pub fn installable(&self) -> bool {
        self.package.is_some()
            && self.sums.is_some()
            && !self.tried_before()
            && matches!(method(), Method::Replace(_) | Method::Installer | Method::Prefix(_))
    }

    fn tried_before(&self) -> bool {
        std::fs::read_to_string(tried_path()).is_ok_and(|tried| tried.trim() == self.version)
    }
}

/// Notes the version last installed, kept beside the index.
fn tried_path() -> PathBuf {
    crate::data_dir().join("update-installed")
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

/// Where to ask. `SCROLLBACK_UPDATE_URL` points at a local test server
/// during development; nothing but this machine is accepted there.
fn api_url() -> String {
    if let Ok(url) = std::env::var("SCROLLBACK_UPDATE_URL") {
        let local = ["http://127.0.0.1:", "http://localhost:"].iter().any(|start| url.starts_with(start));
        if local {
            return url;
        }
    }
    format!("https://api.github.com/repos/{REPOSITORY}/releases/latest")
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .user_agent(format!("Scrollback/{CURRENT}"))
        .build()
        .into()
}

/// Versions as numbers, so 0.10.0 comes after 0.9.0. Anything after a `-`
/// (a pre-release) is ignored.
fn parts(version: &str) -> Vec<u64> {
    version
        .trim_start_matches('v')
        .split('-')
        .next()
        .unwrap_or("")
        .split('.')
        .map(|part| part.parse().unwrap_or(0))
        .collect()
}

/// Whether `candidate` is a later version than `current`.
pub fn newer(candidate: &str, current: &str) -> bool {
    parts(candidate) > parts(current)
}

/// The package a release offers for this system.
fn package_name(version: &str) -> Option<String> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", _) => Some(format!("scrollback-{version}-macos.zip")),
        ("windows", "x86_64") => Some(format!("scrollback-{version}-windows-x64-setup.exe")),
        ("linux", "x86_64") => Some(format!("scrollback-{version}-linux-x86_64.tar.gz")),
        _ => None,
    }
}

/// The latest published release, if it is newer than this one.
pub fn check() -> Result<Option<Release>> {
    let mut response = agent(Duration::from_secs(15))
        .get(&api_url())
        .header("Accept", "application/vnd.github+json")
        .call()
        .context("reaching GitHub")?;
    let status = response.status().as_u16();
    if status == 404 {
        return Ok(None);
    }
    if status != 200 {
        bail!("GitHub answered {status}");
    }
    let release: ApiRelease = response.body_mut().read_json().context("reading GitHub's answer")?;
    let version = release.tag_name.trim_start_matches('v').to_string();
    if release.draft || release.prerelease || !newer(&version, CURRENT) {
        return Ok(None);
    }
    let wanted = package_name(&version);
    let find = |name: &str| release.assets.iter().find(|asset| asset.name == name);
    Ok(Some(Release {
        package: wanted.as_deref().and_then(find).map(|asset| Package {
            name: asset.name.clone(),
            url: asset.browser_download_url.clone(),
            size: asset.size,
        }),
        sums: find("SHA256SUMS").map(|asset| asset.browser_download_url.clone()),
        page: release.html_url,
        version,
    }))
}

/// Where downloads are kept until they are installed.
fn downloads() -> PathBuf {
    crate::data_dir().join("updates")
}

/// Download a release's package and check it against the release's
/// checksums. Returns the file.
pub fn download(release: &Release) -> Result<PathBuf> {
    let package = release.package.as_ref().ok_or_else(|| anyhow!("no package for this system"))?;
    let sums_url = release.sums.as_ref().ok_or_else(|| anyhow!("the release lists no checksums"))?;
    let agent = agent(Duration::from_secs(600));

    let mut response = agent.get(sums_url).call().context("downloading the checksums")?;
    if response.status().as_u16() != 200 {
        bail!("could not download the checksums ({})", response.status());
    }
    let sums = response.body_mut().read_to_string()?;
    let expected = sums
        .lines()
        .filter_map(|line| line.split_once(char::is_whitespace))
        .find(|(_, name)| name.trim().trim_start_matches('*') == package.name)
        .map(|(hash, _)| hash.trim().to_lowercase())
        .ok_or_else(|| anyhow!("{} is not in the checksums", package.name))?;

    let folder = downloads();
    // Earlier downloads are of no more use.
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder)?;
    let path = folder.join(&package.name);
    let mut response = agent.get(&package.url).call().context("downloading the update")?;
    if response.status().as_u16() != 200 {
        bail!("could not download the update ({})", response.status());
    }
    let mut body = response.body_mut().with_config().limit(1024 * 1024 * 1024).reader();
    let mut file = std::fs::File::create(&path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let read = body.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        std::io::Write::write_all(&mut file, &buffer[..read])?;
        total += read as u64;
    }
    drop(file);
    let got: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if got != expected || (package.size > 0 && total != package.size) {
        let _ = std::fs::remove_file(&path);
        bail!("the download does not match the release's checksum, so it was not kept");
    }
    Ok(path)
}

/// How this copy of Scrollback can be updated.
#[derive(Debug, Clone, PartialEq)]
pub enum Method {
    /// macOS: replace this app bundle.
    Replace(PathBuf),
    /// Windows: run the release's installer.
    Installer,
    /// Linux: unpack the tarball over this prefix, such as `~/.local`.
    Prefix(PathBuf),
    /// By hand, for this reason.
    Manual(String),
}

/// How the running copy can be updated, from where it is.
pub fn method() -> Method {
    let Ok(exe) = std::env::current_exe().and_then(|exe| exe.canonicalize()) else {
        return Method::Manual("Scrollback cannot tell where it is running from".into());
    };
    if cfg!(target_os = "macos") {
        // …/Scrollback.app/Contents/MacOS/scrollback
        let bundle = exe.ancestors().nth(3).map(Path::to_path_buf);
        return match bundle {
            Some(bundle) if bundle.extension().is_some_and(|e| e == "app") => {
                if writable(bundle.parent().unwrap_or(&bundle)) {
                    Method::Replace(bundle)
                } else {
                    Method::Manual(format!("Scrollback cannot write to {}", bundle.parent().unwrap_or(&bundle).display()))
                }
            }
            _ => Method::Manual("Scrollback is not running from an app".into()),
        };
    }
    if cfg!(windows) {
        // The installer puts it in Programs and registers an uninstaller
        // beside it; a copy from the zip has neither.
        let installed = exe.parent().is_some_and(|dir| dir.join("unins000.exe").is_file());
        return if installed {
            Method::Installer
        } else {
            Method::Manual("this copy was not installed with the installer".into())
        };
    }
    // Linux: …/bin/scrollback-app, put there by install.sh.
    let prefix = exe.parent().filter(|bin| bin.ends_with("bin")).and_then(Path::parent);
    match prefix {
        Some(prefix) if prefix.starts_with("/usr") && !prefix.starts_with("/usr/local") => {
            Method::Manual("installed with the system's package manager".into())
        }
        Some(prefix) if writable(&prefix.join("bin")) => Method::Prefix(prefix.to_path_buf()),
        Some(prefix) => Method::Manual(format!("Scrollback cannot write to {}", prefix.display())),
        None => Method::Manual("Scrollback is not installed with install.sh".into()),
    }
}

fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".scrollback-write-test-{}", std::process::id()));
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// Put a downloaded release in place, and arrange for Scrollback to start
/// again once this copy has quit. The caller quits straight after.
pub fn install(release: &Release, package: &Path) -> Result<()> {
    let _ = std::fs::write(tried_path(), &release.version);
    match method() {
        Method::Replace(bundle) => install_mac(release, package, &bundle),
        Method::Installer => {
            // The installer closes this copy, replaces it, and opens the
            // new one when run silently.
            Command::new(package)
                .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/CLOSEAPPLICATIONS"])
                .spawn()
                .context("starting the installer")?;
            Ok(())
        }
        Method::Prefix(prefix) => install_linux(package, &prefix),
        Method::Manual(why) => bail!("{why}"),
    }
}

fn install_mac(release: &Release, package: &Path, bundle: &Path) -> Result<()> {
    let unpacked = downloads().join("unpacked");
    let _ = std::fs::remove_dir_all(&unpacked);
    std::fs::create_dir_all(&unpacked)?;
    run(Command::new("/usr/bin/ditto").arg("-x").arg("-k").arg(package).arg(&unpacked), "unpacking the update")?;
    let fresh = unpacked.join("Scrollback.app");
    if !fresh.is_dir() {
        bail!("the update holds no Scrollback.app");
    }
    // It must be the version it says it is, and whole.
    let output = Command::new("/usr/libexec/PlistBuddy")
        .args(["-c", "Print :CFBundleShortVersionString"])
        .arg(fresh.join("Contents/Info.plist"))
        .output()?;
    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if version != release.version {
        bail!("the update says it is version {version:?}, not {}", release.version);
    }
    run(
        Command::new("/usr/bin/codesign").args(["--verify", "--deep", "--strict"]).arg(&fresh),
        "checking the update's signature",
    )?;

    // Swap the bundles: the old one aside, the new one in, the old one gone.
    let parent = bundle.parent().ok_or_else(|| anyhow!("the app has no folder"))?;
    let aside = parent.join(format!(".Scrollback-{}.app", std::process::id()));
    std::fs::rename(bundle, &aside).context("moving the old version aside")?;
    let placed = std::fs::rename(&fresh, bundle).or_else(|_| {
        // A different disk: copy it instead.
        run(Command::new("/usr/bin/ditto").arg(&fresh).arg(bundle), "copying the update into place")
    });
    if let Err(err) = placed {
        let _ = std::fs::rename(&aside, bundle);
        return Err(err.context("putting the update in place"));
    }
    let _ = std::fs::remove_dir_all(&aside);
    let _ = std::fs::remove_dir_all(&unpacked);
    let _ = std::fs::remove_file(package);
    // Development aid: against a test server, start the new copy directly,
    // so it keeps the test's environment instead of the person's own.
    if std::env::var_os("SCROLLBACK_UPDATE_URL").is_some() {
        return reopen_after_exit(&mut Command::new(bundle.join("Contents/MacOS/scrollback")));
    }
    reopen_after_exit(Command::new("/usr/bin/open").arg(bundle))
}

fn install_linux(package: &Path, prefix: &Path) -> Result<()> {
    let unpacked = downloads().join("unpacked");
    let _ = std::fs::remove_dir_all(&unpacked);
    std::fs::create_dir_all(&unpacked)?;
    run(Command::new("tar").arg("-xzf").arg(package).arg("-C").arg(&unpacked), "unpacking the update")?;
    let script = std::fs::read_dir(&unpacked)?
        .flatten()
        .map(|entry| entry.path().join("install.sh"))
        .chain([unpacked.join("install.sh")])
        .find(|path| path.is_file())
        .ok_or_else(|| anyhow!("the update holds no install.sh"))?;
    run(Command::new("sh").arg(&script).env("PREFIX", prefix), "installing the update")?;
    let _ = std::fs::remove_dir_all(&unpacked);
    let _ = std::fs::remove_file(package);
    reopen_after_exit(&mut Command::new(prefix.join("bin/scrollback-app")))
}

fn run(command: &mut Command, doing: &str) -> Result<()> {
    let output = command.output().with_context(|| doing.to_string())?;
    if !output.status.success() {
        let why = String::from_utf8_lossy(&output.stderr);
        bail!("{doing} failed: {}", why.trim());
    }
    Ok(())
}

/// Start `command` once this process has exited, from a small shell that
/// waits for it.
fn reopen_after_exit(command: &mut Command) -> Result<()> {
    let program = command.get_program().to_string_lossy().into_owned();
    let args: Vec<String> = command.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect();
    let script = r#"pid="$1"; shift; while kill -0 "$pid" 2>/dev/null; do sleep 0.2; done; exec "$@""#;
    Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .arg("sh")
        .arg(std::process::id().to_string())
        .arg(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("arranging to start again")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_as_numbers() {
        assert!(newer("0.1.3", "0.1.2"));
        assert!(newer("v0.10.0", "0.9.9"));
        assert!(newer("1.0.0", "0.99.99"));
        assert!(!newer("0.1.2", "0.1.2"));
        assert!(!newer("0.1.1", "0.1.2"));
        assert!(!newer("0.1.2-beta", "0.1.2"));
    }
}
