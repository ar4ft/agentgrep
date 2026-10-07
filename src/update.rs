use anyhow::{Context, Result};
use clap::Subcommand;
use fs2::FileExt;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

const REPOSITORY: &str = "ar4ft/agentgrep";
const LABEL: &str = "dev.agentgrep.updater";
const BINARY_ID: &str = "dev.agentgrep.agx";
const IMAGE_ID: &str = "dev.agentgrep.agx.diskimage";
const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_EXPANDED_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Subcommand)]
pub enum Action {
    /// Check GitHub for newer releases. Does not install or alter search behavior.
    Check {
        #[arg(long)]
        prerelease: bool,
    },
    /// Install a newer signed/notarized macOS release and retain a rollback copy.
    Install {
        #[arg(long)]
        prerelease: bool,
        /// Explicit trust bootstrap for unsigned source builds. Signed releases pin their Team ID.
        #[arg(long)]
        team_id: Option<String>,
    },
    /// Restore the exact binary retained by the most recent successful update.
    Rollback,
    /// Configure opt-in automatic macOS updates with a per-user launchd job.
    Auto {
        #[command(subcommand)]
        action: AutoAction,
    },
}

#[derive(Subcommand)]
pub enum AutoAction {
    Enable {
        #[arg(long, default_value_t = 24)]
        interval_hours: u64,
        #[arg(long)]
        prerelease: bool,
        #[arg(long)]
        team_id: Option<String>,
        /// Render the LaunchAgent without installing or loading it (also works on Linux).
        #[arg(long)]
        dry_run: bool,
    },
    Disable,
    Status,
}

#[derive(Debug, Clone, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    size: u64,
    digest: Option<String>,
}
#[derive(Debug, Clone, Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}

fn target() -> String {
    let suffix = if cfg!(target_os = "macos") {
        "apple-darwin"
    } else {
        "unknown-linux-gnu"
    };
    format!("{}-{suffix}", std::env::consts::ARCH)
}

fn pinned_team(override_team: Option<&str>) -> Result<String> {
    let embedded = option_env!("AGX_APPLE_TEAM_ID").filter(|s| !s.is_empty());
    if let (Some(pin), Some(requested)) = (embedded, override_team) {
        anyhow::ensure!(
            pin == requested,
            "This build pins Apple Team ID {pin}; a different Team ID cannot be selected"
        );
    }
    let team = embedded.or(override_team).context("No Apple Team ID is pinned in this source build. Use --team-id with the publisher's independently verified Team ID, or install a signed release first")?;
    anyhow::ensure!(
        team.len() == 10
            && team
                .bytes()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()),
        "Apple Team ID must contain 10 uppercase letters/digits"
    );
    Ok(team.to_owned())
}

fn release_version(release: &Release) -> Option<Version> {
    let version = Version::parse(release.tag_name.strip_prefix('v')?).ok()?;
    if !version.build.is_empty() {
        return None;
    }
    Some(version)
}

fn select_release(releases: Vec<Release>, prerelease: bool) -> Option<(Release, Version)> {
    releases
        .into_iter()
        .filter(|r| !r.draft)
        .filter_map(|r| {
            let v = release_version(&r)?;
            if !prerelease && (r.prerelease || !v.pre.is_empty()) {
                return None;
            }
            Some((r, v))
        })
        .max_by(|a, b| a.1.cmp(&b.1))
}

fn client() -> ureq::Agent {
    ureq::Agent::config_builder()
        .https_only(true)
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .timeout_global(Some(Duration::from_secs(120)))
        .build()
        .into()
}

fn releases() -> Result<Vec<Release>> {
    let url = format!("https://api.github.com/repos/{REPOSITORY}/releases?per_page=100");
    let releases: Vec<Release> = client()
        .get(&url)
        .header("User-Agent", "agentgrep-updater")
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .call()
        .context("Could not check GitHub releases")?
        .body_mut()
        .read_json()?;
    Ok(releases)
}

fn latest(prerelease: bool) -> Result<Option<(Release, Version)>> {
    Ok(select_release(releases()?, prerelease))
}

fn select_development_release(
    releases: Vec<Release>,
    platform: &str,
) -> Option<(Release, Version)> {
    releases
        .into_iter()
        .filter_map(|r| {
            if r.draft || !r.prerelease || r.assets.iter().any(|a| a.name.ends_with(".dmg")) {
                return None;
            }
            let version = release_version(&r)?;
            let name = format!("agx-{version}-{platform}.tar.gz");
            r.assets
                .iter()
                .any(|a| a.name == name)
                .then_some((r, version))
        })
        .max_by(|a, b| a.1.cmp(&b.1))
}

fn archive_asset<'a>(release: &'a Release, version: &Version, platform: &str) -> Result<&'a Asset> {
    let name = format!("agx-{version}-{platform}.tar.gz");
    let mut assets = release.assets.iter().filter(|a| a.name == name);
    let asset = assets
        .next()
        .context("Development release has no archive for this architecture")?;
    anyhow::ensure!(
        assets.next().is_none(),
        "Duplicate development archive assets"
    );
    anyhow::ensure!(
        asset.browser_download_url
            == format!(
                "https://github.com/{REPOSITORY}/releases/download/{}/{name}",
                release.tag_name
            ),
        "Unexpected development archive URL"
    );
    anyhow::ensure!(
        asset.size > 0 && asset.size <= MAX_IMAGE_BYTES,
        "Development archive is empty or exceeds 64 MiB"
    );
    expected_digest(asset)?;
    Ok(asset)
}

fn download(asset: &Asset, destination: &Path) -> Result<()> {
    let mut output = File::create(destination)?;
    let mut response = client()
        .get(&asset.browser_download_url)
        .header("User-Agent", "agentgrep-updater")
        .call()
        .context("Release download failed")?;
    let count = std::io::copy(
        &mut response.body_mut().as_reader().take(MAX_IMAGE_BYTES + 1),
        &mut output,
    )?;
    output.sync_all()?;
    anyhow::ensure!(
        count == asset.size && count <= MAX_IMAGE_BYTES,
        "Release download size mismatch"
    );
    verify_download(asset, destination)
}

fn verify_download(asset: &Asset, destination: &Path) -> Result<()> {
    anyhow::ensure!(
        fs::metadata(destination)?.len() == asset.size && asset.size <= MAX_IMAGE_BYTES,
        "Release download size mismatch"
    );
    anyhow::ensure!(
        sha256(destination)? == expected_digest(asset)?,
        "Release download checksum mismatch; executable unchanged"
    );
    Ok(())
}

fn extract_development(
    archive: &Path,
    candidate: &Path,
    version: &Version,
    platform: &str,
) -> Result<()> {
    let root = format!("agx-{version}-{platform}");
    let binary_path = format!("{root}/agx");
    let decoder = flate2::read::GzDecoder::new(File::open(archive)?).take(MAX_EXPANDED_BYTES + 1);
    let mut tar = tar::Archive::new(decoder);
    let mut found = false;
    let mut expanded = 0u64;
    for (count, entry) in tar.entries()?.enumerate() {
        anyhow::ensure!(count < 2048, "Development archive exceeds 2048 entries");
        let mut entry = entry?;
        let raw = entry.path_bytes();
        anyhow::ensure!(
            raw.len() <= 4096,
            "Development archive path exceeds 4096 bytes"
        );
        let path = std::str::from_utf8(&raw).context("Archive path must be UTF-8")?;
        let trimmed = path.strip_suffix('/').unwrap_or(path);
        let mut parts = trimmed.split('/');
        anyhow::ensure!(
            parts.next() == Some(root.as_str())
                && parts.all(|p| !p.is_empty() && p != "." && p != ".."),
            "Unsafe development archive path"
        );
        let kind = entry.header().entry_type();
        anyhow::ensure!(
            kind.is_file() || kind.is_dir(),
            "Development archive contains links or special files"
        );
        expanded = expanded
            .checked_add(entry.size())
            .context("Archive size overflow")?;
        anyhow::ensure!(
            expanded <= MAX_EXPANDED_BYTES && entry.size() <= MAX_IMAGE_BYTES,
            "Development archive exceeds expanded size limits"
        );
        if path == binary_path {
            anyhow::ensure!(
                kind.is_file() && !found,
                "Archive must contain exactly one regular agx executable"
            );
            found = true;
            let mut output = File::create(candidate)?;
            std::io::copy(&mut entry, &mut output)?;
            output.sync_all()?;
        }
    }
    anyhow::ensure!(found, "Development archive has no agx executable");
    Ok(())
}

fn install_development_archive(
    destination: &Path,
    archive: &Path,
    state_dir: &Path,
    current: Version,
    version: Version,
    platform: &str,
) -> Result<Installed> {
    anyhow::ensure!(
        version > current,
        "Development updates cannot reinstall or downgrade"
    );
    let work = tempfile::tempdir()?;
    let candidate = work.path().join("agx");
    extract_development(archive, &candidate, &version, platform)?;
    replace_binary(
        destination,
        &candidate,
        state_dir,
        current,
        version.clone(),
        |staged| {
            anyhow::ensure!(
                executable_version(staged)? == version,
                "Development executable version does not match release tag"
            );
            Ok(())
        },
    )
}

pub fn development(check_only: bool) -> Result<Value> {
    let platform = target();
    anyhow::ensure!(
        matches!(
            platform.as_str(),
            "aarch64-apple-darwin" | "x86_64-apple-darwin" | "x86_64-unknown-linux-gnu"
        ),
        "Development updates support Apple Silicon/Intel Mac and Linux x86_64 only"
    );
    let destination = std::env::current_exe()?.canonicalize()?;
    let current = executable_version(&destination)?;
    let warning = "Explicit development update: GitHub HTTPS/SHA-256 only; no Apple signature or notarization verification";
    let Some((release, version)) = select_development_release(releases()?, &platform) else {
        return Ok(
            json!({"channel":"unsigned-development","installed":false,"update_available":false,"status":"no_release_in_selected_channel","current_version":current,"warning":warning}),
        );
    };
    let asset = archive_asset(&release, &version, &platform)?;
    let mut result = json!({"channel":"unsigned-development","current_version":current,"latest_version":version,"target":platform,"update_available":version>current,"installed":false,"verification":"github-sha256","warning":warning,"release":format!("https://github.com/{REPOSITORY}/releases/tag/{}",release.tag_name)});
    if check_only {
        return Ok(result);
    }
    let state_dir = data_dir()?;
    let _lock = lock(&destination, &state_dir)?;
    // Re-read under the updater lock so another update cannot make us downgrade.
    let current = executable_version(&destination)?;
    result["current_version"] = json!(current);
    result["update_available"] = json!(version > current);
    if version <= current {
        result["status"] = json!("up_to_date");
        return Ok(result);
    }
    eprintln!("warning: {warning}");
    let work = tempfile::tempdir()?;
    let archive = work.path().join("update.tar.gz");
    download(asset, &archive)?;
    let state = install_development_archive(
        &destination,
        &archive,
        &state_dir,
        current,
        version,
        &platform,
    )?;
    result["installed"] = json!(true);
    result["previous_version"] = json!(state.previous_version);
    result["executable"] = json!(destination);
    result["rollback_available"] = json!(true);
    Ok(result)
}

fn image_asset<'a>(release: &'a Release, version: &Version, platform: &str) -> Result<&'a Asset> {
    let name = format!("agx-{version}-{platform}.dmg");
    let mut candidates = release.assets.iter().filter(|a| a.name == name);
    let asset = candidates.next().context("Release has no signed/notarized DMG for this architecture; unsigned tar archives are not accepted by the updater")?;
    anyhow::ensure!(
        candidates.next().is_none(),
        "Release has duplicate DMG assets"
    );
    let expected = format!(
        "https://github.com/{REPOSITORY}/releases/download/{}/{name}",
        release.tag_name
    );
    anyhow::ensure!(
        asset.browser_download_url == expected,
        "Unexpected release download URL"
    );
    anyhow::ensure!(
        asset.size > 0 && asset.size <= MAX_IMAGE_BYTES,
        "Release DMG exceeds the 64 MiB update limit or is empty"
    );
    expected_digest(asset)?;
    Ok(asset)
}

fn expected_digest(asset: &Asset) -> Result<String> {
    let digest = asset
        .digest
        .as_deref()
        .and_then(|s| s.strip_prefix("sha256:"))
        .context("GitHub asset metadata has no SHA-256 digest; update refused")?;
    anyhow::ensure!(
        digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid GitHub SHA-256 digest"
    );
    Ok(digest.to_ascii_lowercase())
}

fn sha256(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn check(prerelease: bool) -> Result<Value> {
    let current = Version::parse(env!("CARGO_PKG_VERSION"))?;
    let platform = target();
    let Some((release, version)) = latest(prerelease)? else {
        return Ok(
            json!({"current_version":current,"update_available":false,"status":"no_release_in_selected_channel","channel":if prerelease {"including-prereleases"} else {"stable"}}),
        );
    };
    let asset = image_asset(&release, &version, &platform);
    Ok(
        json!({"current_version":current,"latest_version":version,"update_available":version>current,"target":platform,"signed_dmg_available":asset.is_ok(),"apple_team_id":option_env!("AGX_APPLE_TEAM_ID").filter(|s| !s.is_empty()),"install_requirement":asset.err().map(|e|e.to_string()),"release":format!("https://github.com/{REPOSITORY}/releases/tag/{}",release.tag_name)}),
    )
}

fn command(arguments: &[&str], operation: &str) -> Result<std::process::Output> {
    let (program, args) = arguments.split_first().context("Missing program")?;
    let result = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("{operation}: required macOS tool unavailable"))?;
    anyhow::ensure!(
        result.status.success(),
        "{operation} failed: {}",
        String::from_utf8_lossy(&result.stderr)
            .chars()
            .take(1000)
            .collect::<String>()
    );
    Ok(result)
}

fn apple_requirement(team: &str, identifier: &str) -> String {
    format!(
        "identifier \"{identifier}\" and anchor apple generic and certificate leaf[subject.OU] = \"{team}\" and certificate leaf[field.1.2.840.113635.100.6.1.13] exists"
    )
}

fn verify_signature(path: &Path, team: &str, identifier: &str) -> Result<()> {
    let requirement = apple_requirement(team, identifier);
    command(
        &[
            "/usr/bin/codesign",
            "--verify",
            "--strict",
            "-R",
            &requirement,
            path.to_str().context("Non-UTF8 update path")?,
        ],
        "Verify Developer ID signature and pinned Team ID",
    )?;
    Ok(())
}

fn executable_version(path: &Path) -> Result<Version> {
    let result = Command::new(path).arg("--version").output()?;
    anyhow::ensure!(result.status.success(), "Executable version check failed");
    let text = String::from_utf8(result.stdout)?;
    Version::parse(
        text.trim()
            .strip_prefix("agx ")
            .context("Downloaded executable is not agx")?,
    )
    .context("Invalid executable version")
}

fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn data_dir() -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("dev", "agentgrep", "agentgrep")
        .context("Cannot locate updater state directory")?;
    Ok(dirs.data_local_dir().join("updates"))
}

fn paths(destination: &Path, state_dir: &Path) -> (PathBuf, PathBuf) {
    let key = blake3::hash(destination.to_string_lossy().as_bytes())
        .to_hex()
        .to_string();
    (
        state_dir.join(format!("{key}.json")),
        state_dir.join(format!("{key}.lock")),
    )
}

fn lock(destination: &Path, state_dir: &Path) -> Result<File> {
    private_dir(state_dir)?;
    let (_, path) = paths(destination, state_dir);
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    file.try_lock_exclusive()
        .context("Another update or rollback is running for this executable")?;
    Ok(file)
}

#[derive(Debug, Serialize, Deserialize)]
struct Installed {
    destination: PathBuf,
    backup: PathBuf,
    backup_sha256: String,
    installed_sha256: String,
    previous_version: Version,
    installed_version: Version,
}

fn staged_copy(source: &Path, destination: &Path) -> Result<tempfile::NamedTempFile> {
    let parent = destination
        .parent()
        .context("Executable has no parent directory")?;
    let mut staged = tempfile::Builder::new()
        .prefix(".agx-update-")
        .tempfile_in(parent)
        .context("Cannot write executable directory; updates never invoke sudo")?;
    let mut original = File::open(source)?;
    std::io::copy(&mut original, &mut staged)?;
    staged.as_file().sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        staged
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o755))?;
    }
    Ok(staged)
}

fn replace_binary(
    destination: &Path,
    candidate: &Path,
    state_dir: &Path,
    previous_version: Version,
    installed_version: Version,
    verify: impl Fn(&Path) -> Result<()>,
) -> Result<Installed> {
    anyhow::ensure!(
        fs::symlink_metadata(destination)?.file_type().is_file(),
        "Executable target must be a regular file"
    );
    // Close the writable descriptor before executing the staged version check
    // (Linux refuses to execute a file that is still open for writing).
    let staged = staged_copy(candidate, destination)?.into_temp_path();
    verify(&staged)?; // Verify the exact copy to be installed, before changing anything.
    let backup_temp = staged_copy(destination, destination)?;
    let backup_sha256 = sha256(backup_temp.path())?;
    let installed_sha256 = sha256(&staged)?;
    let (old_file, backup) = backup_temp.keep().map_err(|e| e.error)?;
    drop(old_file);
    let state = Installed {
        destination: destination.into(),
        backup,
        backup_sha256,
        installed_sha256,
        previous_version,
        installed_version,
    };
    let (state_path, _) = paths(destination, state_dir);
    let mut state_temp = tempfile::NamedTempFile::new_in(state_dir)?;
    serde_json::to_writer(&mut state_temp, &state)?;
    state_temp.flush()?;
    state_temp.as_file().sync_all()?;
    let previous_state: Option<Installed> = fs::read(&state_path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    if let Err(error) = staged.persist(destination) {
        let _ = fs::remove_file(&state.backup);
        return Err(error.error.into());
    }
    if let Err(error) = state_temp.persist(&state_path) {
        staged_copy(&state.backup,destination)?.persist(destination).map_err(|e|e.error).context("Updater state failed to save and automatic restoration failed; retained backup is available")?;
        return Err(error.error.into());
    }
    if let Some(old) = previous_state
        && old.backup != state.backup
        && managed_backup(&old, destination)
    {
        let _ = fs::remove_file(old.backup);
    }
    Ok(state)
}

fn managed_backup(state: &Installed, destination: &Path) -> bool {
    state.destination == destination
        && state.backup.parent() == destination.parent()
        && state
            .backup
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with(".agx-update-"))
}

fn rollback_at(destination: &Path, state_dir: &Path) -> Result<Value> {
    let _lock = lock(destination, state_dir)?;
    let (state_path, _) = paths(destination, state_dir);
    let state: Installed = serde_json::from_slice(
        &fs::read(&state_path).context("No previous update is available for rollback")?,
    )?;
    anyhow::ensure!(
        managed_backup(&state, destination),
        "Rollback record does not belong to this executable"
    );
    anyhow::ensure!(
        sha256(destination)? == state.installed_sha256,
        "Executable changed since the last update; rollback refused"
    );
    anyhow::ensure!(
        fs::symlink_metadata(&state.backup)?.file_type().is_file(),
        "Rollback backup must be a regular file"
    );
    let staged = staged_copy(&state.backup, destination)?;
    anyhow::ensure!(
        sha256(staged.path())? == state.backup_sha256,
        "Rollback backup checksum changed; rollback refused"
    );
    staged.persist(destination).map_err(|e| e.error)?;
    fs::remove_file(state_path)?;
    fs::remove_file(&state.backup)?;
    Ok(json!({"rolled_back":true,"version":state.previous_version,"executable":destination}))
}

struct Mounted(PathBuf);
impl Drop for Mounted {
    fn drop(&mut self) {
        let _ = Command::new("/usr/bin/hdiutil")
            .arg("detach")
            .arg(&self.0)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn install(prerelease: bool, override_team: Option<&str>) -> Result<Value> {
    anyhow::ensure!(
        cfg!(target_os = "macos"),
        "Verified automatic installation currently supports macOS only; use source/package updates on Linux"
    );
    let team = pinned_team(override_team)?;
    let destination = std::env::current_exe()?.canonicalize()?;
    let state_dir = data_dir()?;
    let _lock = lock(&destination, &state_dir)?;
    let current = executable_version(&destination)?;
    let Some((release, version)) = latest(prerelease)? else {
        return Ok(json!({"installed":false,"status":"no_release_in_selected_channel"}));
    };
    if version <= current {
        return Ok(json!({"installed":false,"status":"up_to_date","current_version":current}));
    }
    let asset = image_asset(&release, &version, &target())?;
    let work = tempfile::tempdir()?;
    let image = work.path().join("update.dmg");
    let mut output = File::create(&image)?;
    let mut response = client()
        .get(&asset.browser_download_url)
        .header("User-Agent", "agentgrep-updater")
        .call()
        .context("Release download failed")?;
    let copied = std::io::copy(
        &mut response.body_mut().as_reader().take(MAX_IMAGE_BYTES + 1),
        &mut output,
    )?;
    output.sync_all()?;
    anyhow::ensure!(
        copied == asset.size && copied <= MAX_IMAGE_BYTES,
        "Release download size mismatch"
    );
    anyhow::ensure!(
        sha256(&image)? == expected_digest(asset)?,
        "Release download checksum mismatch; executable unchanged"
    );
    verify_signature(&image, &team, IMAGE_ID)?;
    command(
        &[
            "/usr/bin/xcrun",
            "stapler",
            "validate",
            image.to_str().unwrap(),
        ],
        "Validate stapled Apple notarization ticket",
    )?;
    command(
        &[
            "/usr/sbin/spctl",
            "--assess",
            "--type",
            "open",
            "--context",
            "context:primary-signature",
            image.to_str().unwrap(),
        ],
        "Gatekeeper assessment",
    )?;
    let mount = work.path().join("mount");
    fs::create_dir(&mount)?;
    command(
        &[
            "/usr/bin/hdiutil",
            "attach",
            "-readonly",
            "-nobrowse",
            "-noautoopen",
            "-mountpoint",
            mount.to_str().unwrap(),
            image.to_str().unwrap(),
        ],
        "Mount verified release DMG",
    )?;
    let _mounted = Mounted(mount.clone());
    let candidate = mount.join("agx");
    anyhow::ensure!(
        fs::symlink_metadata(&candidate)?.file_type().is_file(),
        "DMG must contain a regular agx executable at its root"
    );
    let state = replace_binary(
        &destination,
        &candidate,
        &state_dir,
        current,
        version.clone(),
        |staged| {
            verify_signature(staged, &team, BINARY_ID)?;
            anyhow::ensure!(
                executable_version(staged)? == version,
                "Signed executable version does not match release tag"
            );
            Ok(())
        },
    )?;
    Ok(
        json!({"installed":true,"version":state.installed_version,"previous_version":state.previous_version,"executable":destination,"rollback_available":true,"apple_team_id":team}),
    )
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn launch_plist(
    executable: &Path,
    log: &Path,
    team: &str,
    hours: u64,
    prerelease: bool,
) -> Result<String> {
    anyhow::ensure!(
        (1..=168).contains(&hours),
        "interval_hours must be between 1 and 168"
    );
    let mut arguments = vec![
        executable.to_string_lossy().to_string(),
        "update".into(),
        "install".into(),
        "--team-id".into(),
        team.into(),
    ];
    if prerelease {
        arguments.push("--prerelease".into());
    }
    let arguments = arguments
        .iter()
        .map(|s| format!("<string>{}</string>", xml(s)))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n<key>Label</key><string>{LABEL}</string>\n<key>ProgramArguments</key><array>{arguments}</array>\n<key>StartInterval</key><integer>{}</integer>\n<key>RunAtLoad</key><false/>\n<key>ProcessType</key><string>Background</string>\n<key>StandardOutPath</key><string>{}</string>\n<key>StandardErrorPath</key><string>{}</string>\n</dict></plist>\n",
        hours * 3600,
        xml(&log.to_string_lossy()),
        xml(&log.with_extension("errors.log").to_string_lossy())
    ))
}

fn launch_path() -> Result<PathBuf> {
    Ok(directories::BaseDirs::new()
        .context("Cannot locate home")?
        .home_dir()
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

fn launch_domain() -> Result<String> {
    let result = command(&["/usr/bin/id", "-u"], "Read launchd user ID")?;
    Ok(format!("gui/{}", String::from_utf8(result.stdout)?.trim()))
}

fn auto(action: AutoAction) -> Result<Value> {
    let path = launch_path()?;
    match action {
        AutoAction::Status => Ok(
            json!({"supported":cfg!(target_os="macos"),"enabled":path.is_file(),"launch_agent":path,"log":data_dir()?.join("automatic.log")}),
        ),
        AutoAction::Disable => {
            anyhow::ensure!(
                cfg!(target_os = "macos"),
                "launchd automatic updates are supported on macOS only"
            );
            let domain = launch_domain()?;
            let _ = Command::new("/bin/launchctl")
                .args(["bootout", &format!("{domain}/{LABEL}")])
                .output();
            if path.exists() {
                fs::remove_file(&path)?;
            }
            Ok(json!({"enabled":false,"launch_agent":path}))
        }
        AutoAction::Enable {
            interval_hours,
            prerelease,
            team_id,
            dry_run,
        } => {
            let team = pinned_team(team_id.as_deref())?;
            let executable = std::env::current_exe()?.canonicalize()?;
            let state_dir = data_dir()?;
            let plist = launch_plist(
                &executable,
                &state_dir.join("automatic.log"),
                &team,
                interval_hours,
                prerelease,
            )?;
            if dry_run {
                return Ok(
                    json!({"enabled":false,"dry_run":true,"launch_agent":path,"plist":plist,"apple_team_id":team}),
                );
            }
            anyhow::ensure!(
                cfg!(target_os = "macos"),
                "launchd automatic updates are supported on macOS only"
            );
            private_dir(&state_dir)?;
            fs::create_dir_all(path.parent().unwrap())?;
            let previous = fs::read(&path).ok();
            let domain = launch_domain()?;
            let mut temporary = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
            temporary.write_all(plist.as_bytes())?;
            temporary.flush()?;
            let _ = Command::new("/bin/launchctl")
                .args(["bootout", &format!("{domain}/{LABEL}")])
                .output();
            temporary.persist(&path).map_err(|e| e.error)?;
            if let Err(error) = command(
                &[
                    "/bin/launchctl",
                    "bootstrap",
                    &domain,
                    path.to_str().unwrap(),
                ],
                "Load automatic updater LaunchAgent",
            ) {
                if let Some(bytes) = previous {
                    fs::write(&path, bytes)?;
                    let _ = Command::new("/bin/launchctl")
                        .args(["bootstrap", &domain, path.to_str().unwrap()])
                        .output();
                } else {
                    let _ = fs::remove_file(&path);
                }
                return Err(error);
            }
            Ok(
                json!({"enabled":true,"interval_hours":interval_hours,"channel":if prerelease {"including-prereleases"} else {"stable"},"apple_team_id":team,"launch_agent":path,"executable":executable,"first_check":"after interval; runs only in the user GUI login session"}),
            )
        }
    }
}

pub fn run(action: Action) -> Result<Value> {
    match action {
        Action::Check { prerelease } => check(prerelease),
        Action::Install {
            prerelease,
            team_id,
        } => install(prerelease, team_id.as_deref()),
        Action::Rollback => rollback_at(&std::env::current_exe()?.canonicalize()?, &data_dir()?),
        Action::Auto { action } => auto(action),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn release(tag: &str, prerelease: bool, draft: bool) -> Release {
        Release {
            tag_name: tag.into(),
            prerelease,
            draft,
            assets: vec![],
        }
    }
    #[test]
    fn release_channels_use_semver_and_reject_drafts_or_mislabeled_prereleases() {
        let releases = vec![
            release("v0.9.0", false, false),
            release("v0.10.0", false, false),
            release("v1.0.0-rc.1", false, false),
            release("v2.0.0", false, true),
            release("invalid", false, false),
            release("v0.11.0", true, false),
        ];
        assert_eq!(
            select_release(releases.clone(), false).unwrap().1,
            Version::parse("0.10.0").unwrap()
        );
        assert_eq!(
            select_release(releases, true).unwrap().1,
            Version::parse("1.0.0-rc.1").unwrap()
        );
    }
    #[test]
    fn update_assets_reject_external_urls_bad_digests_and_oversized_downloads() {
        let version = Version::parse("1.0.0").unwrap();
        let name = "agx-1.0.0-aarch64-apple-darwin.dmg";
        let mut release = release("v1.0.0", false, false);
        release.assets.push(Asset {
            name: name.into(),
            browser_download_url: format!(
                "https://github.com/{REPOSITORY}/releases/download/v1.0.0/{name}"
            ),
            size: 100,
            digest: Some(format!("sha256:{}", "a".repeat(64))),
        });
        assert!(image_asset(&release, &version, "aarch64-apple-darwin").is_ok());
        release.assets[0].browser_download_url = "https://github.com.evil.test/update.dmg".into();
        assert!(image_asset(&release, &version, "aarch64-apple-darwin").is_err());
        release.assets[0].browser_download_url =
            format!("https://github.com/{REPOSITORY}/releases/download/v1.0.0/{name}");
        release.assets[0].digest = None;
        assert!(image_asset(&release, &version, "aarch64-apple-darwin").is_err());
        release.assets[0].digest = Some(format!("sha256:{}", "a".repeat(64)));
        release.assets[0].size = MAX_IMAGE_BYTES + 1;
        assert!(image_asset(&release, &version, "aarch64-apple-darwin").is_err());
    }
    #[test]
    fn launch_agent_escapes_paths_pins_team_and_has_no_search_hook() {
        let plist = launch_plist(
            Path::new("/Users/A & B/<work>/agx"),
            Path::new("/logs/a&b"),
            "ABCDE12345",
            24,
            false,
        )
        .unwrap();
        assert!(plist.contains("A &amp; B/&lt;work&gt;"));
        assert!(plist.contains("<integer>86400</integer>"));
        assert!(plist.contains("<string>install</string>"));
        assert!(plist.contains("<string>ABCDE12345</string>"));
        assert!(!plist.contains("--prerelease"));
        assert!(
            launch_plist(Path::new("/agx"), Path::new("/log"), "ABCDE12345", 0, false).is_err()
        );
    }
    #[test]
    fn failed_candidate_verification_preserves_binary_and_creates_no_rollback_record() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("agx");
        let candidate = directory.path().join("new");
        let state = directory.path().join("state");
        fs::write(&destination, b"old").unwrap();
        fs::write(&candidate, b"new").unwrap();
        private_dir(&state).unwrap();
        let result = replace_binary(
            &destination,
            &candidate,
            &state,
            Version::new(1, 0, 0),
            Version::new(2, 0, 0),
            |_| anyhow::bail!("wrong Developer ID"),
        );
        assert!(result.is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"old");
        assert_eq!(fs::read_dir(state).unwrap().count(), 0);
    }
    #[test]
    fn atomic_update_and_exact_rollback_retain_original_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("agx");
        let candidate = directory.path().join("new");
        let state_dir = directory.path().join("state");
        fs::write(&destination, b"old").unwrap();
        fs::write(&candidate, b"new").unwrap();
        private_dir(&state_dir).unwrap();
        replace_binary(
            &destination,
            &candidate,
            &state_dir,
            Version::new(1, 0, 0),
            Version::new(2, 0, 0),
            |path| {
                anyhow::ensure!(fs::read(path)? == b"new", "wrong staged bytes");
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"new");
        rollback_at(&destination, &state_dir).unwrap();
        assert_eq!(fs::read(destination).unwrap(), b"old");
    }
    #[test]
    fn rollback_refuses_modified_executable_and_corrupted_backup() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("agx");
        let candidate = directory.path().join("new");
        let state_dir = directory.path().join("state");
        fs::write(&destination, b"old").unwrap();
        fs::write(&candidate, b"new").unwrap();
        private_dir(&state_dir).unwrap();
        let state = replace_binary(
            &destination,
            &candidate,
            &state_dir,
            Version::new(1, 0, 0),
            Version::new(2, 0, 0),
            |_| Ok(()),
        )
        .unwrap();
        fs::write(&destination, b"custom").unwrap();
        assert!(rollback_at(&destination, &state_dir).is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"custom");
        fs::write(&destination, b"new").unwrap();
        fs::write(&state.backup, b"corrupt").unwrap();
        assert!(rollback_at(&destination, &state_dir).is_err());
        assert_eq!(fs::read(destination).unwrap(), b"new");
    }
    #[test]
    fn updater_lock_excludes_concurrent_install_or_rollback() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("agx");
        let state = directory.path().join("state");
        let first = lock(&destination, &state).unwrap();
        assert!(lock(&destination, &state).is_err());
        drop(first);
        assert!(lock(&destination, &state).is_ok());
    }

    fn archive_release(tag: &str, pre: bool, draft: bool, platform: &str) -> Release {
        let mut result = release(tag, pre, draft);
        let version = tag.trim_start_matches('v');
        let name = format!("agx-{version}-{platform}.tar.gz");
        result.assets.push(Asset {
            browser_download_url: format!(
                "https://github.com/{REPOSITORY}/releases/download/{tag}/{name}"
            ),
            name,
            size: 100,
            digest: Some(format!("sha256:{}", "a".repeat(64))),
        });
        result
    }

    #[test]
    fn development_selection_uses_prerelease_flag_architecture_and_semver_not_signed_images() {
        let platform = "aarch64-apple-darwin";
        let mut signed = archive_release("v20.0.0-beta.1", true, false, platform);
        signed.assets.push(Asset {
            name: "agx.dmg".into(),
            browser_download_url: "unused".into(),
            size: 1,
            digest: None,
        });
        let candidates = vec![
            archive_release("v0.9.0", true, false, platform),
            archive_release("v0.10.0", true, false, platform),
            archive_release("v50.0.0", false, false, platform),
            archive_release("v60.0.0", true, true, platform),
            archive_release("v70.0.0", true, false, "x86_64-apple-darwin"),
            release("v80.0.0", true, false),
            signed,
            archive_release("invalid", true, false, platform),
        ];
        assert_eq!(
            select_development_release(candidates, platform).unwrap().1,
            Version::new(0, 10, 0)
        );
        assert!(
            select_development_release(vec![release("v9.0.0", false, false)], platform).is_none()
        );
        let unsigned = archive_release("v0.10.0", true, false, platform);
        assert!(image_asset(&unsigned, &Version::new(0, 10, 0), platform).is_err());
    }

    #[test]
    fn development_assets_and_downloads_require_exact_urls_size_and_digest() {
        let platform = "aarch64-apple-darwin";
        let version = Version::new(9, 0, 0);
        let mut r = archive_release("v9.0.0", true, false, platform);
        assert!(archive_asset(&r, &version, platform).is_ok());
        let good = r.assets[0].clone();
        r.assets[0].browser_download_url = "https://evil.test/archive".into();
        assert!(archive_asset(&r, &version, platform).is_err());
        r.assets[0] = good.clone();
        r.assets[0].size = MAX_IMAGE_BYTES + 1;
        assert!(archive_asset(&r, &version, platform).is_err());
        r.assets[0] = good.clone();
        r.assets[0].digest = None;
        assert!(archive_asset(&r, &version, platform).is_err());
        r.assets = vec![good.clone(), good.clone()];
        assert!(archive_asset(&r, &version, platform).is_err());
        let dir = tempfile::tempdir().unwrap();
        let downloaded = dir.path().join("archive");
        fs::write(&downloaded, b"fixture").unwrap();
        let mut asset = good;
        asset.size = 7;
        asset.digest = Some(format!("sha256:{}", sha256(&downloaded).unwrap()));
        verify_download(&asset, &downloaded).unwrap();
        fs::write(&downloaded, b"changed").unwrap();
        assert!(verify_download(&asset, &downloaded).is_err());
        fs::write(&downloaded, b"short").unwrap();
        assert!(verify_download(&asset, &downloaded).is_err());
    }

    fn fixture_archive(path: &Path, entries: &[(&str, tar::EntryType, &[u8])]) {
        let gzip =
            flate2::write::GzEncoder::new(File::create(path).unwrap(), flate2::Compression::fast());
        let mut builder = tar::Builder::new(gzip);
        for (name, kind, bytes) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o755);
            header.set_size(bytes.len() as u64);
            header.set_entry_type(*kind);
            // Raw headers allow constructing hostile paths rejected by Builder::append_data.
            header.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
            header.set_cksum();
            builder.append(&header, *bytes).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();
    }

    #[test]
    fn development_archive_rejects_traversal_links_duplicates_and_missing_binary() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("archive");
        let candidate = dir.path().join("candidate");
        let platform = "x86_64-apple-darwin";
        let version = Version::new(9, 0, 0);
        let normal = "agx-9.0.0-x86_64-apple-darwin/agx";
        for bad in [
            "../agx",
            "/agx",
            "agx-9.0.0-x86_64-apple-darwin/../agx",
            "wrong/agx",
        ] {
            fixture_archive(&archive, &[(bad, tar::EntryType::Regular, b"bad")]);
            assert!(extract_development(&archive, &candidate, &version, platform).is_err());
        }
        for kind in [
            tar::EntryType::Symlink,
            tar::EntryType::Link,
            tar::EntryType::Fifo,
        ] {
            fixture_archive(&archive, &[(normal, kind, b"")]);
            assert!(extract_development(&archive, &candidate, &version, platform).is_err());
        }
        fixture_archive(
            &archive,
            &[
                (normal, tar::EntryType::Regular, b"one"),
                (normal, tar::EntryType::Regular, b"two"),
            ],
        );
        assert!(extract_development(&archive, &candidate, &version, platform).is_err());
        fixture_archive(
            &archive,
            &[(
                "agx-9.0.0-x86_64-apple-darwin/README",
                tar::EntryType::Regular,
                b"doc",
            )],
        );
        assert!(extract_development(&archive, &candidate, &version, platform).is_err());
        let entries = vec![
            (
                "agx-9.0.0-x86_64-apple-darwin/doc",
                tar::EntryType::Regular,
                &b""[..]
            );
            2049
        ];
        fixture_archive(&archive, &entries);
        assert!(extract_development(&archive, &candidate, &version, platform).is_err());
        let mut gzip = flate2::write::GzEncoder::new(
            File::create(&archive).unwrap(),
            flate2::Compression::fast(),
        );
        let mut header = tar::Header::new_gnu();
        header.set_path(normal).unwrap();
        header.set_mode(0o755);
        header.set_size(MAX_IMAGE_BYTES + 1);
        header.set_cksum();
        gzip.write_all(header.as_bytes()).unwrap();
        gzip.finish().unwrap();
        assert!(
            extract_development(&archive, &candidate, &version, platform)
                .unwrap_err()
                .to_string()
                .contains("size limits")
        );
    }

    #[test]
    fn development_archive_accepts_pax_metadata_used_by_native_release_packages() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("archive");
        let candidate = dir.path().join("candidate");
        let platform = target();
        let gzip = flate2::write::GzEncoder::new(
            File::create(&archive).unwrap(),
            flate2::Compression::fast(),
        );
        let mut builder = tar::Builder::new(gzip);
        builder
            .append_pax_extensions([("mtime", &b"1.5"[..])])
            .unwrap();
        let mut header = tar::Header::new_gnu();
        header.set_size(6);
        header.set_mode(0o755);
        builder
            .append_data(
                &mut header,
                format!("agx-9.0.0-{platform}/agx"),
                &b"source"[..],
            )
            .unwrap();
        builder.into_inner().unwrap().finish().unwrap();
        extract_development(&archive, &candidate, &Version::new(9, 0, 0), &platform).unwrap();
        assert_eq!(fs::read(candidate).unwrap(), b"source");
    }

    #[cfg(unix)]
    #[test]
    fn development_install_replaces_exact_executable_and_rolls_back_without_apple_tools() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("agx");
        fs::write(&destination, b"original bytes").unwrap();
        let archive = dir.path().join("archive");
        let state_dir = dir.path().join("state");
        private_dir(&state_dir).unwrap();
        let platform = target();
        let name = format!("agx-9.0.0-{platform}/agx");
        fixture_archive(
            &archive,
            &[(
                &name,
                tar::EntryType::Regular,
                b"#!/bin/sh\nprintf 'agx 8.0.0\\n'\n",
            )],
        );
        assert!(
            install_development_archive(
                &destination,
                &archive,
                &state_dir,
                Version::new(1, 0, 0),
                Version::new(9, 0, 0),
                &platform
            )
            .is_err()
        );
        assert_eq!(fs::read(&destination).unwrap(), b"original bytes");
        assert_eq!(fs::read_dir(&state_dir).unwrap().count(), 0);
        let binary = b"#!/bin/sh\nprintf 'agx 9.0.0\\n'\n";
        fixture_archive(&archive, &[(&name, tar::EntryType::Regular, binary)]);
        for current in [Version::new(9, 0, 0), Version::new(10, 0, 0)] {
            assert!(
                install_development_archive(
                    &destination,
                    &archive,
                    &state_dir,
                    current,
                    Version::new(9, 0, 0),
                    &platform
                )
                .is_err()
            );
            assert_eq!(fs::read(&destination).unwrap(), b"original bytes");
        }
        install_development_archive(
            &destination,
            &archive,
            &state_dir,
            Version::new(1, 0, 0),
            Version::new(9, 0, 0),
            &platform,
        )
        .unwrap();
        assert_eq!(fs::read(&destination).unwrap(), binary);
        assert_eq!(
            executable_version(&destination).unwrap(),
            Version::new(9, 0, 0)
        );
        rollback_at(&destination, &state_dir).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"original bytes");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn real_codesign_rejects_an_unsigned_test_executable() {
        assert!(
            verify_signature(&std::env::current_exe().unwrap(), "ABCDE12345", BINARY_ID).is_err()
        );
    }
}
