//! Self-update from GitHub releases.
//!
//! At most once a day an interactive run asks GitHub for the latest release
//! tag — one HEAD request, capped at a few seconds, answered by the redirect
//! `/releases/latest` already does, so no API rate limit applies. When the tag
//! is newer, the release tarball is fetched, checked against its `.sha256`,
//! smoke-tested, swapped over the running binary and re-executed with the
//! same arguments. Any failure along the way is quiet: an update must never
//! stand between the user and the harness they asked for.

use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use crate::{home, paint, VERSION};

const REPO: &str = "Lulzx/lulz-router";
const CHECK_EVERY: Duration = Duration::from_secs(24 * 3600);

fn stamp_path() -> PathBuf {
    home().join(".cache/lulz/update-check")
}

/// The once-a-day gate. Touched before the check rather than after it, so an
/// offline machine pays the timeout once a day, not on every launch.
fn due() -> bool {
    let p = stamp_path();
    let fresh = fs::metadata(&p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age < CHECK_EVERY);
    if fresh {
        return false;
    }
    if fs::create_dir_all(p.parent().unwrap()).is_ok() {
        let _ = fs::write(&p, "");
    }
    true
}

/// Run before any command. Re-executes into the new binary on success and
/// otherwise returns, having changed nothing.
pub fn auto(args: &[String]) {
    use std::io::IsTerminal;
    // Scripts and pipes get a predictable binary; so does anyone who asks.
    if env::var_os("LULZ_NO_UPDATE").is_some() || !std::io::stderr().is_terminal() {
        return;
    }
    let Some(exe) = current_exe() else { return };
    if is_dev_build(&exe) || !due() {
        return;
    }
    let Some(latest) = latest_tag() else { return };
    if !newer(&latest, VERSION) {
        return;
    }
    match install(&latest, &exe) {
        Ok(()) => {
            eprintln!(
                "  {} lulz {VERSION} → {}",
                paint("updated", "35;1"),
                latest.trim_start_matches('v')
            );
            let err = Command::new(&exe).args(args).exec();
            // exec only returns on failure; the new binary is in place, so
            // carry on with this one rather than leaving the user stranded.
            eprintln!("  {} could not restart lulz: {err}", paint("warn", "33;1"));
        }
        Err(e) => {
            eprintln!(
                "  {} lulz {} is out ({e}); reinstall with\n  curl -fsSL https://raw.githubusercontent.com/{REPO}/main/install.sh | sh",
                paint("update", "33;1"),
                latest.trim_start_matches('v')
            );
        }
    }
}

/// `lulz update`: the same path, on demand and out loud.
pub fn cmd_update() -> Result<(), String> {
    let exe = current_exe().ok_or("could not locate the running lulz binary")?;
    if is_dev_build(&exe) {
        return Err(format!(
            "{} is a cargo build; update it with git pull && cargo build",
            exe.display()
        ));
    }
    let latest = latest_tag().ok_or("could not reach GitHub for the latest release")?;
    // Counts as today's check, so the next launch doesn't ask again.
    let _ = fs::create_dir_all(stamp_path().parent().unwrap());
    let _ = fs::write(stamp_path(), "");
    if !newer(&latest, VERSION) {
        println!("lulz {VERSION} is the latest release");
        return Ok(());
    }
    install(&latest, &exe)?;
    println!("lulz {VERSION} → {}", latest.trim_start_matches('v'));
    Ok(())
}

fn current_exe() -> Option<PathBuf> {
    env::current_exe().ok()?.canonicalize().ok()
}

/// A binary inside a cargo target dir is someone's working copy; replacing
/// it with a release would silently throw their changes away.
fn is_dev_build(exe: &Path) -> bool {
    let s = exe.to_string_lossy();
    s.contains("/target/debug/") || s.contains("/target/release/")
}

/// `v0.4.1` from the redirect GitHub answers `/releases/latest` with.
fn latest_tag() -> Option<String> {
    let out = Command::new("curl")
        .args([
            "-sI",
            "--max-time",
            "3",
            "-o",
            "/dev/null",
            "-w",
            "%{redirect_url}",
            &format!("https://github.com/{REPO}/releases/latest"),
        ])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    tag_from_redirect(&String::from_utf8_lossy(&out.stdout))
}

fn tag_from_redirect(url: &str) -> Option<String> {
    let tag = url.trim().rsplit_once("/tag/")?.1;
    parse_version(tag)?;
    Some(tag.to_string())
}

fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let mut it = v.trim().trim_start_matches('v').splitn(3, '.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next()?.parse().ok()?;
    // Drop any pre-release or build suffix on the patch number.
    let patch = it.next()?;
    let digits: String = patch.chars().take_while(char::is_ascii_digit).collect();
    Some((major, minor, digits.parse().ok()?))
}

fn newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// The release asset name install.sh uses for this build.
fn target() -> Option<&'static str> {
    match (env::consts::OS, env::consts::ARCH) {
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-gnu"),
        _ => None,
    }
}

fn install(tag: &str, exe: &Path) -> Result<(), String> {
    let target = target().ok_or("no prebuilt binary for this platform")?;
    let dir = exe.parent().ok_or("odd install path")?;
    let tmp = env::temp_dir().join(format!("lulz-update-{}", std::process::id()));
    fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let r = fetch_and_swap(tag, target, &tmp, exe, dir);
    let _ = fs::remove_dir_all(&tmp);
    r
}

fn fetch_and_swap(tag: &str, target: &str, tmp: &Path, exe: &Path, dir: &Path) -> Result<(), String> {
    let asset = format!("lulz-{tag}-{target}.tar.gz");
    let url = format!("https://github.com/{REPO}/releases/download/{tag}/{asset}");
    let tarball = tmp.join(&asset);
    let sums = tmp.join(format!("{asset}.sha256"));
    download(&url, &tarball)?;
    download(&format!("{url}.sha256"), &sums)?;

    let want = fs::read_to_string(&sums).map_err(|e| e.to_string())?;
    let want = want.split_whitespace().next().unwrap_or_default();
    if want.is_empty() || sha256(&tarball)? != want {
        return Err("checksum mismatch".into());
    }

    let st = Command::new("tar")
        .arg("-xzf")
        .arg(&tarball)
        .arg("-C")
        .arg(tmp)
        .status()
        .map_err(|e| format!("tar: {e}"))?;
    let fresh = tmp.join("lulz");
    if !st.success() || !fresh.is_file() {
        return Err("could not unpack the release".into());
    }

    // Never swap in a binary that can't even name itself.
    let out = Command::new(&fresh)
        .arg("--version")
        .env("LULZ_NO_UPDATE", "1")
        .output()
        .map_err(|e| format!("new binary would not run: {e}"))?;
    let expect = format!("lulz {}", tag.trim_start_matches('v'));
    if String::from_utf8_lossy(&out.stdout).trim() != expect {
        return Err("new binary reported the wrong version".into());
    }

    // Copy next to the target, then rename over it: a new inode rather than
    // an in-place rewrite, which macOS kills on launch for a signed binary.
    let staged = dir.join(format!(".lulz-update-{}", std::process::id()));
    fs::copy(&fresh, &staged).map_err(|e| format!("{}: {e}", dir.display()))?;
    let _ = fs::set_permissions(&staged, fs::Permissions::from_mode(0o755));
    fs::rename(&staged, exe).map_err(|e| {
        let _ = fs::remove_file(&staged);
        format!("{}: {e}", exe.display())
    })
}

fn download(url: &str, to: &Path) -> Result<(), String> {
    let st = Command::new("curl")
        .args(["-fsSL", "--connect-timeout", "5", "--max-time", "60", "-o"])
        .arg(to)
        .arg(url)
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("curl: {e}"))?;
    if st.success() {
        Ok(())
    } else {
        Err("download failed".into())
    }
}

fn sha256(file: &Path) -> Result<String, String> {
    for (bin, args) in [("shasum", &["-a", "256"][..]), ("sha256sum", &[][..])] {
        if let Ok(out) = Command::new(bin).args(args).arg(file).output() {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout);
                return Ok(s.split_whitespace().next().unwrap_or_default().to_string());
            }
        }
    }
    Err("no shasum or sha256sum to verify the download".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_tag_from_the_latest_redirect() {
        assert_eq!(
            tag_from_redirect("https://github.com/Lulzx/lulz-router/releases/tag/v0.4.1\n").as_deref(),
            Some("v0.4.1")
        );
        // No release yet: GitHub bounces to the releases page instead.
        assert_eq!(tag_from_redirect("https://github.com/Lulzx/lulz-router/releases"), None);
        assert_eq!(tag_from_redirect(""), None);
    }

    #[test]
    fn only_a_strictly_newer_release_counts() {
        assert!(newer("v0.4.1", "0.4.0"));
        assert!(newer("v0.10.0", "0.9.9"));
        assert!(newer("v1.0.0", "0.99.99"));
        assert!(!newer("v0.4.1", "0.4.1"));
        assert!(!newer("v0.4.0", "0.4.1"));
        assert!(!newer("garbage", "0.4.1"));
        assert_eq!(parse_version("v1.2.3-rc.1"), Some((1, 2, 3)));
    }

    #[test]
    fn cargo_builds_are_left_alone() {
        assert!(is_dev_build(Path::new("/src/lulz-router/target/release/lulz")));
        assert!(is_dev_build(Path::new("/src/lulz-router/target/debug/lulz")));
        assert!(!is_dev_build(Path::new("/Users/me/.local/bin/lulz")));
    }
}
