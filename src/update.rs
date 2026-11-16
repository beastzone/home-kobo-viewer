//! Updates over Wi-Fi: the setup tool publishes a bundle (layout, program,
//! launcher scripts) into a folder Home Assistant serves under `/local/`, and
//! the Kobo fetches it from there.
//!
//! `/local/` needs no login, so two things keep this safe. The published
//! layout carries no secrets: the Home Assistant token and the update key are
//! blanked, and the Kobo keeps its own. And the manifest listing every file's
//! SHA-256 is signed with that update key, which only the setup tool and the
//! Kobo hold (it reaches the Kobo over USB, once). A Kobo applies nothing
//! whose manifest signature or file hash is wrong, and nothing older than what
//! it already has.

use crate::config::AppConfig;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

pub const CONFIG: &str = "config.yaml";
pub const PROGRAM: &str = "kobo-ha-dashboard";
/// The only names a bundle may hold; a manifest naming anything else is refused.
pub const FILES: [&str; 4] = [CONFIG, PROGRAM, "launch.sh", "wifi.sh"];
/// Folder under Home Assistant's `www` (served as `/local/kobo-ha/`)
pub const FOLDER: &str = "kobo-ha";
/// The dashboard exits with this to ask launch.sh to start it again.
pub const EXIT_RESTART: i32 = 75;
const STATE_FILE: &str = "update-state.json";
/// launch.sh leaves this when a new program died at once and was put back.
const ROLLED_BACK: &str = "update-rolled-back";
const MAX_PROGRAM_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileEntry {
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    /// Milliseconds since the Unix epoch at publish time; only ever goes up
    pub serial: u64,
    pub app_version: String,
    pub files: BTreeMap<String, FileEntry>,
}

/// What this Kobo has already taken, kept next to the program.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct State {
    #[serde(default)]
    pub serial: u64,
    /// Hash of the program waiting in `kobo-ha-dashboard.new`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staged_program: Option<String>,
    /// Hash of a program that was tried and rolled back: never fetched again
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejected_program: Option<String>,
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// HMAC-SHA256 (RFC 2104), written out because it is fifteen lines.
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(block.map(|b| b ^ 0x36));
    inner.update(message);
    let mut outer = Sha256::new();
    outer.update(block.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

pub fn sign(key: &str, manifest: &[u8]) -> String {
    hex(&hmac_sha256(key.as_bytes(), manifest))
}

/// Compare without stopping at the first difference.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A fresh update key: 32 random bytes as hex.
pub fn new_key() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|e| e.to_string())?;
    Ok(hex(&bytes))
}

/// The layout as it is published: no token, no update key, no local paths.
pub fn sanitized(config: &AppConfig) -> AppConfig {
    let mut c = config.clone();
    c.home_assistant.token = String::new();
    c.updates.key = String::new();
    c.updates.publish_dir = String::new();
    c
}

/// A published layout, with the secrets this Kobo already holds put back.
pub fn merge_secrets(mut published: AppConfig, local: &AppConfig) -> AppConfig {
    published.home_assistant.token = local.home_assistant.token.clone();
    published.updates.key = local.updates.key.clone();
    // Whatever was published, a Kobo that updates over Wi-Fi keeps doing so.
    published.updates.enabled = local.updates.enabled;
    published
}

/// Check a manifest against its signature and the serial already applied.
/// `Ok(None)` means it is genuine but nothing new.
pub fn verify_manifest(key: &str, manifest: &[u8], signature: &str, applied_serial: u64) -> Result<Option<Manifest>, String> {
    if key.is_empty() {
        return Err("no update key on this device".to_string());
    }
    if !same(&sign(key, manifest), signature.trim()) {
        return Err("manifest signature does not match".to_string());
    }
    let m: Manifest = serde_json::from_slice(manifest).map_err(|e| format!("manifest: {}", e))?;
    if let Some(odd) = m.files.keys().find(|name| !FILES.contains(&name.as_str())) {
        return Err(format!("manifest names an unexpected file: {}", odd));
    }
    Ok((m.serial > applied_serial).then_some(m))
}

// ---------------------------------------------------------------------------
// Setup tool side
// ---------------------------------------------------------------------------

/// Write a signed bundle into `dir`. The signature goes last, so a Kobo that
/// looks halfway through sees a mismatch and simply tries again later.
pub fn publish(dir: &Path, config: &AppConfig, program: &Path, launch_sh: &str, wifi_sh: &str, serial: u64) -> Result<Manifest, String> {
    if config.updates.key.is_empty() {
        return Err("no update key".to_string());
    }
    let io = |what: &str, e: std::io::Error| format!("{}: {}", what, e);
    std::fs::create_dir_all(dir).map_err(|e| io(&dir.display().to_string(), e))?;
    let yaml = serde_yaml::to_string(&sanitized(config)).map_err(|e| e.to_string())?;
    let program_bytes = std::fs::read(program).map_err(|e| io(&program.display().to_string(), e))?;
    let contents: [(&str, &[u8]); 4] =
        [(CONFIG, yaml.as_bytes()), (PROGRAM, &program_bytes), ("launch.sh", launch_sh.as_bytes()), ("wifi.sh", wifi_sh.as_bytes())];

    let mut files = BTreeMap::new();
    for (name, bytes) in contents {
        std::fs::write(dir.join(name), bytes).map_err(|e| io(name, e))?;
        files.insert(name.to_string(), FileEntry { sha256: sha256_hex(bytes), size: bytes.len() as u64 });
    }
    let manifest = Manifest { serial, app_version: env!("CARGO_PKG_VERSION").to_string(), files };
    let json = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("manifest.json"), &json).map_err(|e| io("manifest.json", e))?;
    std::fs::write(dir.join("manifest.sig"), sign(&config.updates.key, &json)).map_err(|e| io("manifest.sig", e))?;
    Ok(manifest)
}

// ---------------------------------------------------------------------------
// Kobo side
// ---------------------------------------------------------------------------

pub fn load_state(app_dir: &Path) -> State {
    std::fs::read(app_dir.join(STATE_FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_state(app_dir: &Path, state: &State) -> Result<(), String> {
    let json = serde_json::to_vec_pretty(state).map_err(|e| e.to_string())?;
    replace_file(&app_dir.join(STATE_FILE), &json)
}

/// Write beside the target, then rename over it: never a half-written file.
fn replace_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, path)).map_err(|e| format!("{}: {}", path.display(), e))
}

/// At start-up: if launch.sh had to put the old program back, remember which
/// one failed so it is not downloaded and tried again on every check.
pub fn note_rollback(app_dir: &Path) {
    let marker = app_dir.join(ROLLED_BACK);
    if !marker.exists() {
        return;
    }
    let mut state = load_state(app_dir);
    if let Some(bad) = state.staged_program.take() {
        tracing::warn!("The last program update did not start and was rolled back; it will not be fetched again");
        state.rejected_program = Some(bad);
    }
    let _ = save_state(app_dir, &state);
    let _ = std::fs::remove_file(marker);
}

/// Which files of a new manifest have to be fetched, given what is on disk.
pub fn files_to_fetch(manifest: &Manifest, app_dir: &Path, state: &State) -> Vec<String> {
    manifest
        .files
        .iter()
        .filter(|(name, entry)| {
            if name.as_str() == PROGRAM && state.rejected_program.as_deref() == Some(entry.sha256.as_str()) {
                return false;
            }
            // The layout on disk holds this Kobo's secrets, so it never
            // matches the published one byte for byte: always take it.
            name.as_str() == CONFIG || std::fs::read(app_dir.join(name.as_str())).map(|b| sha256_hex(&b)) .ok().as_deref() != Some(entry.sha256.as_str())
        })
        .map(|(name, _)| name.clone())
        .collect()
}

/// Put verified files in place. The program is only staged as
/// `kobo-ha-dashboard.new`: launch.sh swaps it in, and back out if it fails.
/// Returns whether anything changed.
pub fn apply(app_dir: &Path, local: &AppConfig, manifest: &Manifest, fetched: &BTreeMap<String, Vec<u8>>) -> Result<bool, String> {
    // Everything is checked before anything is written.
    let mut merged_yaml = None;
    for (name, bytes) in fetched {
        let entry = manifest.files.get(name).ok_or_else(|| format!("{} is not in the manifest", name))?;
        if bytes.len() as u64 != entry.size || !same(&sha256_hex(bytes), &entry.sha256) {
            return Err(format!("{} does not match its hash in the manifest", name));
        }
        if name == CONFIG {
            let published: AppConfig = serde_yaml::from_slice(bytes).map_err(|e| format!("published layout: {}", e))?;
            merged_yaml = Some(serde_yaml::to_string(&merge_secrets(published, local)).map_err(|e| e.to_string())?);
        }
    }

    let mut state = load_state(app_dir);
    let mut changed = false;
    for (name, bytes) in fetched {
        match name.as_str() {
            CONFIG => {
                let yaml = merged_yaml.as_deref().unwrap_or_default();
                let path = app_dir.join(CONFIG);
                if std::fs::read_to_string(&path).ok().as_deref() != Some(yaml) {
                    let _ = std::fs::copy(&path, app_dir.join("config.yaml.bak"));
                    replace_file(&path, yaml.as_bytes())?;
                    changed = true;
                }
            }
            PROGRAM => {
                replace_file(&app_dir.join(format!("{}.new", PROGRAM)), bytes)?;
                state.staged_program = Some(manifest.files[name].sha256.clone());
                changed = true;
            }
            // Scripts take effect the next time the dashboard is opened.
            _ => replace_file(&app_dir.join(name), bytes)?,
        }
    }
    state.serial = manifest.serial;
    save_state(app_dir, &state)?;
    Ok(changed)
}

/// Where this Kobo looks for its bundle: the setup tool's update server if
/// one is named, otherwise the folder Home Assistant serves.
pub fn base_url(config: &AppConfig) -> String {
    let direct = config.updates.url.trim().trim_end_matches('/');
    if direct.is_empty() {
        format!("{}/local/{}", config.home_assistant.url.trim_end_matches('/'), FOLDER)
    } else {
        direct.to_string()
    }
}

pub fn active(config: &AppConfig) -> bool {
    let somewhere = !config.updates.url.trim().is_empty() || !config.home_assistant.url.is_empty();
    config.updates.enabled && !config.updates.key.is_empty() && somewhere
}

/// Whether `name` is something an update server may hand out.
pub fn servable(name: &str) -> bool {
    FILES.contains(&name) || name == "manifest.json" || name == "manifest.sig"
}

async fn fetch(client: &reqwest::Client, url: &str, limit: u64) -> Result<Vec<u8>, String> {
    let resp = client.get(url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{}: HTTP {}", url, resp.status()));
    }
    if resp.content_length().is_some_and(|n| n > limit) {
        return Err(format!("{} is larger than expected", url));
    }
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err(format!("{} is larger than expected", url));
    }
    Ok(bytes.to_vec())
}

/// Look for a newer bundle and apply it. `Ok(true)`: restart to take it up.
pub async fn check(config: &AppConfig, app_dir: &Path) -> Result<bool, String> {
    if !active(config) {
        return Ok(false);
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .connect_timeout(std::time::Duration::from_secs(8))
        .build()
        .map_err(|e| e.to_string())?;
    let base = base_url(config);
    // The query only defeats caches along the way.
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let manifest_bytes = fetch(&client, &format!("{}/manifest.json?t={}", base, stamp), 64 * 1024).await?;
    let signature = fetch(&client, &format!("{}/manifest.sig?t={}", base, stamp), 1024).await?;
    let state = load_state(app_dir);
    let Some(manifest) = verify_manifest(&config.updates.key, &manifest_bytes, &String::from_utf8_lossy(&signature), state.serial)? else {
        return Ok(false);
    };
    tracing::info!("Update {} (version {}) found", manifest.serial, manifest.app_version);

    let mut fetched = BTreeMap::new();
    for name in files_to_fetch(&manifest, app_dir, &state) {
        let entry = &manifest.files[&name];
        if entry.size > MAX_PROGRAM_BYTES {
            return Err(format!("{} is larger than expected", name));
        }
        let bytes = fetch(&client, &format!("{}/{}?t={}", base, name, manifest.serial), entry.size).await?;
        fetched.insert(name, bytes);
    }
    let changed = apply(app_dir, config, &manifest, &fetched)?;
    tracing::info!("Update {} applied ({} files{})", manifest.serial, fetched.len(), if changed { ", restarting" } else { "" });
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("kobo-ha-update-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn config_with_key() -> AppConfig {
        let mut c = AppConfig::default();
        c.home_assistant.url = "http://192.168.1.50:8123".to_string();
        c.home_assistant.token = "SECRET-TOKEN".to_string();
        c.updates.enabled = true;
        c.updates.key = "k".repeat(64);
        c.updates.publish_dir = "\\\\ha\\config\\www\\kobo-ha".to_string();
        c
    }

    /// Publish from `pc`, then hand the Kobo every file it asks for.
    fn deliver(bundle: &Path, kobo: &Path, local: &AppConfig) -> Result<bool, String> {
        let manifest_bytes = std::fs::read(bundle.join("manifest.json")).unwrap();
        let sig = std::fs::read_to_string(bundle.join("manifest.sig")).unwrap();
        let state = load_state(kobo);
        let Some(m) = verify_manifest(&local.updates.key, &manifest_bytes, &sig, state.serial)? else { return Ok(false) };
        let fetched = files_to_fetch(&m, kobo, &state).into_iter().map(|n| (n.clone(), std::fs::read(bundle.join(&n)).unwrap())).collect();
        apply(kobo, local, &m, &fetched)
    }

    /// The real thing end to end: a bundle published into a folder, served
    /// the way Home Assistant serves `/local/`, fetched by `check`.
    #[tokio::test]
    async fn check_fetches_a_published_bundle_over_http() {
        use axum::{extract::Path as UrlPath, http::StatusCode, routing::get, Router};
        let (bundle, kobo) = (temp("pub-http"), temp("kobo-http"));
        let program = bundle.join("src-program");
        std::fs::write(&program, b"PROGRAM v2").unwrap();

        let served = bundle.clone();
        let app = Router::new().route(
            "/local/kobo-ha/:name",
            get(move |UrlPath(name): UrlPath<String>| {
                let dir = served.clone();
                async move { std::fs::read(dir.join(name)).map_err(|_| StatusCode::NOT_FOUND) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let mut pc = config_with_key();
        pc.home_assistant.url = format!("http://127.0.0.1:{}/", port);
        let mut local = pc.clone();
        local.pages.clear();
        std::fs::write(kobo.join(CONFIG), serde_yaml::to_string(&local).unwrap()).unwrap();
        std::fs::write(kobo.join(PROGRAM), b"PROGRAM v1").unwrap();

        // Nothing published yet: an error to log, nothing changed
        assert!(check(&local, &kobo).await.is_err());
        publish(&bundle, &pc, &program, "#!/bin/sh\n# new\n", "w", 500).unwrap();
        assert_eq!(check(&local, &kobo).await, Ok(true));
        assert_eq!(std::fs::read(kobo.join("kobo-ha-dashboard.new")).unwrap(), b"PROGRAM v2");
        assert_eq!(std::fs::read_to_string(kobo.join("launch.sh")).unwrap(), "#!/bin/sh\n# new\n");
        let now: AppConfig = serde_yaml::from_str(&std::fs::read_to_string(kobo.join(CONFIG)).unwrap()).unwrap();
        assert_eq!((now.pages.len(), now.home_assistant.token.as_str()), (pc.pages.len(), "SECRET-TOKEN"));
        assert_eq!(check(&now, &kobo).await, Ok(false), "already applied");

        // The setup tool's own update server instead of Home Assistant's folder
        let mut direct = now.clone();
        direct.updates.url = format!("http://127.0.0.1:{}/local/kobo-ha/", port);
        direct.home_assistant.url = "http://192.0.2.1:8123".to_string();
        assert_eq!(base_url(&direct), format!("http://127.0.0.1:{}/local/kobo-ha", port));
        publish(&bundle, &pc, &program, "#!/bin/sh\n# newer\n", "w", 600).unwrap();
        assert_eq!(check(&direct, &kobo).await, Ok(true));
        assert_eq!(load_state(&kobo).serial, 600);
        assert!(servable("manifest.sig") && servable(PROGRAM) && !servable("../config.yaml") && !servable("backups"));

        // Updates switched off, or no key: it does not even look
        let mut off = now.clone();
        off.updates.enabled = false;
        assert!(!active(&off));
        let _ = (std::fs::remove_dir_all(bundle), std::fs::remove_dir_all(kobo));
    }

    /// launch.sh with stand-in programs: a good update is swapped in, a
    /// program that dies at once or cannot run is swapped back out.
    #[cfg(unix)]
    #[test]
    fn launch_sh_swaps_a_new_program_in_and_a_broken_one_back_out() {
        use std::os::unix::fs::PermissionsExt;
        let launch = include_str!("../deploy/launch.sh");
        // Stand-in for the dashboard: counts its runs; on the first, stages
        // whatever is in `next` as the update and asks to be restarted.
        let v1 = "#!/bin/sh\n[ \"$1\" = --version ] && { echo v1; exit 0; }\ncd \"$(dirname \"$0\")\"\necho run >>v1-runs\nif [ -f next ]; then mv next kobo-ha-dashboard.new; exit 75; fi\nexit 0\n";
        let run = |name: &str, next: Option<&str>| {
            let dir = temp(name);
            let exe = |file: &str, body: &str| {
                std::fs::write(dir.join(file), body).unwrap();
                std::fs::set_permissions(dir.join(file), std::fs::Permissions::from_mode(0o755)).unwrap();
            };
            exe(PROGRAM, v1);
            if let Some(body) = next {
                exe("next", body);
            }
            let script = launch
                .replace("/mnt/onboard/.kobo-ha", dir.to_str().unwrap())
                .replace("/tmp/kobo-ha-dashboard.lock", dir.join("lock").to_str().unwrap())
                .replace("sleep 1\n", "")
                .replace("sleep 2\n", "");
            std::fs::write(dir.join("launch-test.sh"), script).unwrap();
            let status = std::process::Command::new("sh").arg(dir.join("launch-test.sh")).status().unwrap();
            assert!(status.success());
            dir
        };
        let runs = |dir: &Path, file: &str| std::fs::read_to_string(dir.join(file)).map(|s| s.lines().count()).unwrap_or(0);
        let version = |dir: &Path| String::from_utf8(std::process::Command::new(dir.join(PROGRAM)).arg("--version").output().unwrap().stdout).unwrap();

        // A good update: v2 takes over and runs; v1 is kept as .old
        let good = run("launch-good", Some("#!/bin/sh\n[ \"$1\" = --version ] && { echo v2; exit 0; }\necho run >>\"$(dirname \"$0\")/v2-runs\"\nexit 0\n"));
        assert_eq!(version(&good).trim(), "v2");
        assert_eq!((runs(&good, "v1-runs"), runs(&good, "v2-runs")), (1, 1));
        assert!(good.join("kobo-ha-dashboard.old").exists() && !good.join(ROLLED_BACK).exists());

        // An update that starts and dies at once: v1 comes back and runs again
        let dies = run("launch-dies", Some("#!/bin/sh\n[ \"$1\" = --version ] && { echo v2; exit 0; }\necho run >>\"$(dirname \"$0\")/v2-runs\"\nexit 1\n"));
        assert_eq!(version(&dies).trim(), "v1");
        assert_eq!((runs(&dies, "v1-runs"), runs(&dies, "v2-runs")), (2, 1));
        assert!(dies.join(ROLLED_BACK).exists());

        // An update that cannot run at all is never swapped in
        let broken = run("launch-broken", Some("#!/bin/sh\nexit 1\n"));
        assert_eq!(version(&broken).trim(), "v1");
        assert_eq!(runs(&broken, "v1-runs"), 2);
        assert!(broken.join(ROLLED_BACK).exists() && !broken.join("kobo-ha-dashboard.new").exists());

        // No update at all: one run, as before
        let plain = run("launch-plain", None);
        assert_eq!(runs(&plain, "v1-runs"), 1);
        for d in [good, dies, broken, plain] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn hmac_matches_rfc_4231() {
        // Test case 2 of RFC 4231
        assert_eq!(hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
        // A key longer than the block is hashed first (test case 6)
        let key = [0xaau8; 131];
        assert_eq!(
            hex(&hmac_sha256(&key, b"Test Using Larger Than Block-Size Key - Hash Key First")),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn nothing_secret_is_published_and_the_kobo_keeps_its_own() {
        let (bundle, kobo) = (temp("pub-secret"), temp("kobo-secret"));
        let program = bundle.join("src-program");
        std::fs::write(&program, b"PROGRAM v2").unwrap();
        let pc = config_with_key();
        publish(&bundle, &pc, &program, "#!/bin/sh\n", "#!/bin/sh\n", 100).unwrap();
        for name in ["config.yaml", "manifest.json", "manifest.sig"] {
            let text = std::fs::read_to_string(bundle.join(name)).unwrap();
            assert!(!text.contains("SECRET-TOKEN") && !text.contains(&pc.updates.key) && !text.contains("www"), "{} leaks", name);
        }

        // The Kobo has an older layout with its own token and the same key
        let mut local = config_with_key();
        local.pages.clear();
        std::fs::write(kobo.join(CONFIG), serde_yaml::to_string(&local).unwrap()).unwrap();
        std::fs::write(kobo.join(PROGRAM), b"PROGRAM v1").unwrap();
        assert_eq!(deliver(&bundle, &kobo, &local), Ok(true));

        let now: AppConfig = serde_yaml::from_str(&std::fs::read_to_string(kobo.join(CONFIG)).unwrap()).unwrap();
        assert_eq!(now.pages.len(), pc.pages.len(), "the published pages arrived");
        assert_eq!(now.home_assistant.token, "SECRET-TOKEN");
        assert_eq!(now.updates.key, pc.updates.key);
        // The running program is untouched; the new one waits for launch.sh
        assert_eq!(std::fs::read(kobo.join(PROGRAM)).unwrap(), b"PROGRAM v1");
        assert_eq!(std::fs::read(kobo.join("kobo-ha-dashboard.new")).unwrap(), b"PROGRAM v2");
        // The same bundle again is nothing new
        assert_eq!(deliver(&bundle, &kobo, &local), Ok(false));
        let _ = (std::fs::remove_dir_all(bundle), std::fs::remove_dir_all(kobo));
    }

    #[test]
    fn a_bundle_signed_with_another_key_or_altered_is_refused() {
        let (bundle, kobo) = (temp("pub-forged"), temp("kobo-forged"));
        let program = bundle.join("src-program");
        std::fs::write(&program, b"PROGRAM").unwrap();
        let pc = config_with_key();
        publish(&bundle, &pc, &program, "a", "b", 100).unwrap();
        let before = serde_yaml::to_string(&pc).unwrap();
        std::fs::write(kobo.join(CONFIG), &before).unwrap();

        let mut other = pc.clone();
        other.updates.key = "x".repeat(64);
        assert!(deliver(&bundle, &kobo, &other).unwrap_err().contains("signature"));
        let mut none = pc.clone();
        none.updates.key.clear();
        assert!(deliver(&bundle, &kobo, &none).is_err());

        // A file swapped after signing: caught by its hash, nothing written
        std::fs::write(bundle.join(PROGRAM), b"MALWARE").unwrap();
        assert!(deliver(&bundle, &kobo, &pc).unwrap_err().contains("hash"));
        assert!(!kobo.join("kobo-ha-dashboard.new").exists());
        assert_eq!(std::fs::read_to_string(kobo.join(CONFIG)).unwrap(), before);
        assert_eq!(load_state(&kobo).serial, 0);

        // A manifest reaching outside the four known files
        let evil = br#"{"serial":999,"app_version":"x","files":{"../launch.sh":{"sha256":"00","size":1}}}"#;
        assert!(verify_manifest(&pc.updates.key, evil, &sign(&pc.updates.key, evil), 0).unwrap_err().contains("unexpected file"));
        let _ = (std::fs::remove_dir_all(bundle), std::fs::remove_dir_all(kobo));
    }

    #[test]
    fn an_older_bundle_is_not_taken_and_a_rolled_back_program_is_not_fetched_again() {
        let (bundle, kobo) = (temp("pub-old"), temp("kobo-old"));
        let program = bundle.join("src-program");
        std::fs::write(&program, b"BROKEN PROGRAM").unwrap();
        let pc = config_with_key();
        std::fs::write(kobo.join(CONFIG), serde_yaml::to_string(&pc).unwrap()).unwrap();
        std::fs::write(kobo.join(PROGRAM), b"GOOD PROGRAM").unwrap();
        publish(&bundle, &pc, &program, "a", "b", 200).unwrap();
        assert_eq!(deliver(&bundle, &kobo, &pc), Ok(true));

        // launch.sh tried it, it died, the old one is back
        std::fs::remove_file(kobo.join("kobo-ha-dashboard.new")).unwrap();
        std::fs::write(kobo.join(ROLLED_BACK), b"").unwrap();
        note_rollback(&kobo);
        assert!(!kobo.join(ROLLED_BACK).exists());
        assert_eq!(load_state(&kobo).rejected_program, Some(sha256_hex(b"BROKEN PROGRAM")));

        // Published again (a layout change): the layout comes, that program does not
        publish(&bundle, &pc, &program, "a", "b", 300).unwrap();
        deliver(&bundle, &kobo, &pc).unwrap();
        assert!(!kobo.join("kobo-ha-dashboard.new").exists());
        assert_eq!(load_state(&kobo).serial, 300);

        // Replaying the earlier, genuinely signed bundle does nothing
        publish(&bundle, &pc, &program, "a", "b", 250).unwrap();
        assert_eq!(deliver(&bundle, &kobo, &pc), Ok(false));
        let _ = (std::fs::remove_dir_all(bundle), std::fs::remove_dir_all(kobo));
    }
}
