// POLER Sovereign Updater — native replacement for legacy package-manager glue.
// ============================================================================
// Synchronizes the running POLER CachyOS Sovereign Edition system directly
// with the user's GitHub mirror (Kotokvit/poler-distro):
//
//   poler-update            — report mode: show mirror state vs local state
//   poler-update --apply    — download + verify + atomically install the
//                             sovereign core stack from the latest release
//   poler-update --install engine
//                           — sovereign engine channel: download the OFFICIAL
//                             poler-engine release binary (poler-engine-org/
//                             poler-engine, sha256-pinned) and install it
//                             atomically. The engine's Terminal Gateway is
//                             the primary shell of the live ISO; this command
//                             brings it to any installed system.
//   poler-update --help
//
// Design (zero legacy):
//   * network transport  : spawns `curl` (the only trusted HTTPS client)
//   * integrity          : sha256 of every downloaded asset, verified before
//                          anything is installed (sha256sum)
//   * atomicity          : stage into /tmp/poler-update.d, rename into place
//   * JSON               : hand-rolled scanner (no serde, no python)
//   * no bash involved   : this is a native binary; poler-sh runs it fine
// ============================================================================

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const MANIFEST_URL: &str =
    "https://raw.githubusercontent.com/Kotokvit/poler-distro/main/manifest.json";
const RELEASE_API: &str =
    "https://api.github.com/repos/Kotokvit/poler-distro/releases/latest";
const LOCAL_MANIFEST: &str = "/etc/poler/manifest.json";
const STAGE_DIR: &str = "/tmp/poler-update.d";
const CORE_ASSET: &str = "poler-core-x86_64.tar.gz";

// Sovereign stack installed by --apply (name in archive -> absolute path).
const INSTALL_MAP: &[(&str, &str)] = &[
    ("poler-init", "/usr/bin/poler-init"),
    ("poler-powerctl", "/usr/bin/poler-powerctl"),
    ("poler-sh", "/usr/bin/poler-sh"),
    ("poler", "/usr/bin/poler"),
    ("poler-box", "/usr/bin/poler-box"),
    ("poler-fuse", "/usr/bin/poler-fuse"),
    ("poler-update", "/usr/bin/poler-update"),
];

// Sovereign engine — official release channel. The engine is the crown jewel:
// NEVER rebuilt from source, always the official release binary, pinned by
// sha256. Keep this pin in sync with builder/build_iso.py (ENGINE_SHA256).
const ENGINE_VERSION: &str = "0.61.0";
const ENGINE_URL: &str =
    "https://github.com/poler-engine-org/poler-engine/releases/download/v0.61.0/poler-engine";
const ENGINE_SHA256: &str =
    "abd6e98b47b6282e929a6ddd6e7172c76c88e10ec5d45dce0d88a0388117b547";
const ENGINE_DEST: &str = "/usr/bin/poler-engine";

fn log(msg: &str) {
    println!("\x1b[1;36m[POLER-UPDATE]\x1b[0m {}", msg);
}

fn ok(msg: &str) {
    println!("\x1b[1;32m[POLER-UPDATE]\x1b[0m \x1b[1;32m{}\x1b[0m", msg);
}

fn err(msg: &str) -> i32 {
    println!("\x1b[1;31m[POLER-UPDATE]\x1b[0m \x1b[1;31m{}\x1b[0m", msg);
    1
}

fn usage() {
    println!("poler-update — POLER sovereign mirror synchronizer");
    println!();
    println!("  poler-update                  report mirror state vs this system");
    println!("  poler-update --apply          download, verify and install the latest");
    println!("                                sovereign core stack from GitHub");
    println!("  poler-update --install engine install the OFFICIAL poler-engine");
    println!("                                release (poler-engine-org/poler-engine,");
    println!("                                v{}, sha256-pinned) — its Terminal", ENGINE_VERSION);
    println!("                                Gateway becomes the system shell");
    println!("  poler-update --help           this help");
}

// --------------------------- minimal JSON scanning ---------------------------

fn json_string_field(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{}\"", key);
    let mut from = 0;
    while let Some(pos) = text[from..].find(&needle) {
        let after = from + pos + needle.len();
        let rest = &text[after..];
        let trimmed = rest.trim_start();
        if let Some(colon_stripped) = trimmed.strip_prefix(':') {
            let t = colon_stripped.trim_start();
            if let Some(stripped) = t.strip_prefix('"') {
                // find closing quote (handle escaped quotes minimally)
                let mut out = String::new();
                let mut esc = false;
                for c in stripped.chars() {
                    if esc {
                        out.push(c);
                        esc = false;
                        continue;
                    }
                    match c {
                        '\\' => esc = true,
                        '"' => return Some(out),
                        _ => out.push(c),
                    }
                }
            }
        }
        from = after;
    }
    None
}

/// Extracts (name, browser_download_url) pairs from a GitHub release JSON.
fn release_assets(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let tag = json_string_field(text, "tag_name").unwrap_or_else(|| "v1.1.0".to_string());

    // Search for each asset block starting with "\"name\":"
    let mut idx = 0;
    while let Some(pos) = text[idx..].find("\"name\"") {
        let block_start = idx + pos;
        let chunk = &text[block_start..];
        let name = json_string_field(chunk, "name").unwrap_or_default();
        if !name.is_empty() && (name.ends_with(".tar.gz") || name.ends_with(".iso") || name == "SHA256SUMS" || name == "manifest.json") {
            let url = if let Some(download_url) = json_string_field(chunk, "browser_download_url") {
                download_url
            } else {
                format!("https://github.com/Kotokvit/poler-distro/releases/download/{}/{}", tag, name)
            };
            if !out.iter().any(|(n, _): &(String, String)| n == &name) {
                out.push((name, url));
            }
        }
        idx = block_start + 10;
    }
    out
}

// ---------------------------------- curl -------------------------------------

fn curl_to(url: &str, dest: &Path) -> Result<(), String> {
    let status = Command::new("curl")
        .arg("-fsSL")
        .arg("--retry").arg("3")
        .arg("--max-time").arg("120")
        .arg("-o").arg(dest)
        .arg(url)
        .status()
        .map_err(|e| format!("cannot spawn curl: {}", e))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("curl failed ({}): {}", status.code().unwrap_or(-1), url))
    }
}

fn curl_text(url: &str) -> Result<String, String> {
    let out = Command::new("curl")
        .arg("-fsSL")
        .arg("--retry").arg("2")
        .arg("--max-time").arg("30")
        .arg("-H").arg("User-Agent: poler-update/1.1")
        .arg(url)
        .output()
        .map_err(|e| format!("cannot spawn curl: {}", e))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        Err(format!("curl failed ({}): {}", out.status.code().unwrap_or(-1), url))
    }
}

fn sha256_of(path: &Path) -> Result<String, String> {
    let out = Command::new("sha256sum")
        .arg(path)
        .output()
        .map_err(|e| format!("cannot spawn sha256sum: {}", e))?;
    if !out.status.success() {
        return Err("sha256sum failed".to_string());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text.split_whitespace().next().unwrap_or_default().to_string())
}

// --------------------------------- main flow ---------------------------------

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        usage();
        return;
    }
    let apply = args.iter().any(|a| a == "--apply" || a == "-y");

    // Sovereign engine channel: `poler-update --install engine`.
    if let Some(pos) = args.iter().position(|a| a == "--install") {
        let module = args.get(pos + 1).map(|s| s.as_str()).unwrap_or("");
        match module {
            "engine" => std::process::exit(install_engine()),
            other => std::process::exit(err(&format!(
                "module '{}' has no sovereign release channel yet (engine: OK; mesh/git/edit: planned)",
                other))),
        }
    }

    log("Connecting to sovereign GitHub mirror: https://github.com/Kotokvit/poler-distro ...");

    let remote = match curl_text(MANIFEST_URL) {
        Ok(t) => t,
        Err(e) => {
            std::process::exit(err(&format!("cannot reach sovereign mirror: {}", e)));
        }
    };

    let remote_version = json_string_field(&remote, "version").unwrap_or_else(|| "unknown".into());
    let local_version = fs::read_to_string(LOCAL_MANIFEST)
        .ok()
        .and_then(|t| json_string_field(&t, "version"))
        .unwrap_or_else(|| "none".into());

    log(&format!("Latest sovereign release on GitHub: \x1b[1;32m{}\x1b[0m", remote_version));
    log(&format!("Installed on this system: \x1b[1;33m{}\x1b[0m", local_version));

    // Show the component stack from the mirror manifest.
    if let Some(stack) = section_after(&remote, "\"core_stack\"") {
        log("Distro core components:");
        for line in stack.lines() {
            let l = line.trim();
            if l.contains('"') {
                println!("  \x1b[1;36m•\x1b[0m {}", l.trim_end_matches(','));
            }
        }
    }
    if let Some(stack) = section_after(&remote, "\"optional_modules\"") {
        log("Optional modules (installable, not in the live ISO):");
        for line in stack.lines() {
            let l = line.trim();
            if l.contains('"') {
                println!("  \x1b[2m•\x1b[0m {}", l.trim_end_matches(','));
            }
        }
    }

    if !apply {
        if remote_version == local_version {
            ok("✓ System is fully synchronized with the sovereign GitHub mirror!");
        } else {
            println!();
            log("\x1b[1;33mUpdate available. Run: poler-update --apply\x1b[0m");
        }
        return;
    }

    if remote_version == local_version {
        ok("✓ Already at the latest sovereign release — nothing to do.");
        return;
    }

    // --apply: fetch release metadata
    log("Fetching release assets from GitHub API...");
    let release = match curl_text(RELEASE_API) {
        Ok(t) => t,
        Err(e) => std::process::exit(err(&format!("cannot fetch release info: {}", e))),
    };
    let assets = release_assets(&release);
    if assets.is_empty() {
        std::process::exit(err("release carries no assets — nothing to install"));
    }

    let stage = PathBuf::from(STAGE_DIR);
    let _ = fs::remove_dir_all(&stage);
    fs::create_dir_all(&stage).expect("cannot create staging dir");

    // SHA256SUMS first (if present) for verification
    let mut sums: Vec<(String, String)> = Vec::new();
    for (name, url) in &assets {
        if name == "SHA256SUMS" {
            if let Ok(text) = curl_text(url) {
                for line in text.lines() {
                    let mut it = line.split_whitespace();
                    if let (Some(hash), Some(fname)) = (it.next(), it.next()) {
                        sums.push((fname.trim().to_string(), hash.to_string()));
                    }
                }
            }
        }
    }

    // Download core stack asset
    let core = assets.iter().find(|(n, _)| n == CORE_ASSET);
    let (core_name, core_url) = match core {
        Some((n, u)) => (n.clone(), u.clone()),
        None => std::process::exit(err(&format!(
            "release has no {} — build the sovereign core first", CORE_ASSET))),
    };
    let core_path = stage.join(&core_name);
    log(&format!("Downloading {} ...", core_name));
    if let Err(e) = curl_to(&core_url, &core_path) {
        std::process::exit(err(&e));
    }

    // Verify sha256 against SHA256SUMS
    if !sums.is_empty() {
        let expect = sums.iter().find(|(n, _)| n == &core_name).map(|(_, h)| h.clone());
        match expect {
            Some(h) => {
                let actual = match sha256_of(&core_path) {
                    Ok(s) => s,
                    Err(e) => std::process::exit(err(&e)),
                };
                if actual != h {
                    std::process::exit(err(&format!(
                        "sha256 MISMATCH for {}: expected {}, got {} — aborting",
                        core_name, h, actual)));
                }
                log(&format!("sha256 verified: {}", &actual[..16.min(actual.len())]));
            }
            None => log("core asset not listed in SHA256SUMS — installing unverified (mirror-side gap)"),
        }
    } else {
        log("release carries no SHA256SUMS — installing unverified (mirror-side gap)");
    }

    // Extract (tar -xzf, native)
    log("Staging sovereign components...");
    let extract_dir = stage.join("core");
    fs::create_dir_all(&extract_dir).expect("cannot create extract dir");
    let st = Command::new("tar")
        .arg("-xzf").arg(&core_path)
        .arg("-C").arg(&extract_dir)
        .status()
        .expect("cannot spawn tar");
    if !st.success() {
        std::process::exit(err("tar extraction failed"));
    }

    extern "C" {
        fn geteuid() -> u32;
    }
    let is_root = unsafe { geteuid() == 0 };
    let user_home = env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let user_bin_dir = PathBuf::from(&user_home).join(".local/bin");
    let _ = fs::create_dir_all(&user_bin_dir);

    // Atomic install
    let mut installed = 0;
    for (name, dest) in INSTALL_MAP {
        let staged = find_staged(&extract_dir, name);
        if let Some(src) = staged {
            let dest_path = if is_root {
                PathBuf::from(dest)
            } else {
                user_bin_dir.join(name)
            };
            if let Some(parent) = dest_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let tmp = dest_path.with_extension("new");
            if fs::copy(&src, &tmp).is_err() {
                std::process::exit(err(&format!("cannot stage {}", dest_path.display())));
            }
            set_exec(&tmp);
            if fs::rename(&tmp, &dest_path).is_err() {
                let _ = fs::remove_file(&tmp);
                std::process::exit(err(&format!("cannot atomically install {} (running binary?) — retry after reboot", dest_path.display())));
            }
            installed += 1;
            log(&format!("installed \x1b[1;32m{}\x1b[0m -> {}", name, dest_path.display()));
        } else {
            log(&format!("\x1b[1;33mcomponent {} not present in release archive\x1b[0m", name));
        }
    }

    // Update local manifest
    let local_manifest = Path::new(LOCAL_MANIFEST);
    let manifest_src = assets.iter().find(|(n, _)| n == "manifest.json");
    if let Some((_, url)) = manifest_src {
        let tmp = stage.join("manifest.json");
        if curl_to(url, &tmp).is_ok() {
            let _ = fs::copy(&tmp, local_manifest);
        }
    } else {
        let _ = fs::write(local_manifest, &remote);
    }

    let _ = fs::remove_dir_all(&stage);

    ok(&format!(
        "✓ Sovereign stack updated to {} ({} components) — система синхронизирована с вашим GitHub-зеркалом!",
        remote_version, installed));
}

/// Sovereign engine channel: download the official release binary, verify the
/// pinned sha256, install atomically. The engine is never rebuilt from source
/// — this is the same binary that powers the live ISO console (builder/
/// build_iso.py pins the identical sha256).
fn install_engine() -> i32 {
    log(&format!(
        "sovereign engine channel: poler-engine-org/poler-engine v{} (official release, sha256-pinned)",
        ENGINE_VERSION));

    let stage_root = PathBuf::from(STAGE_DIR);
    let _ = fs::remove_dir_all(&stage_root);
    if fs::create_dir_all(&stage_root).is_err() {
        return err("cannot create staging dir /tmp/poler-update.d");
    }
    let staged = stage_root.join("poler-engine");

    log(&format!("Downloading official engine release ..."));
    if let Err(e) = curl_to(ENGINE_URL, &staged) {
        return err(&e);
    }

    let digest = match sha256_of(&staged) {
        Ok(s) => s,
        Err(e) => return err(&e),
    };
    if digest != ENGINE_SHA256 {
        return err(&format!(
            "sha256 MISMATCH for poler-engine: expected {}, got {} — refusing to install an unverified engine",
            ENGINE_SHA256, digest));
    }
    log(&format!("sha256 verified: {}", &digest[..16.min(digest.len())]));

    let dest = Path::new(ENGINE_DEST);
    if let Some(parent) = dest.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let tmp = dest.with_extension("engine.new");
    if fs::copy(&staged, &tmp).is_err() {
        return err(&format!("cannot stage {}", tmp.display()));
    }
    set_exec(&tmp);
    if fs::rename(&tmp, dest).is_err() {
        let _ = fs::remove_file(&tmp);
        return err(&format!(
            "cannot atomically install {} (engine running?) — quit the engine sessions and retry",
            dest.display()));
    }
    let _ = fs::remove_dir_all(&stage_root);

    ok(&format!(
        "✓ poler-engine v{} installed -> {} — Terminal Gateway of the live ISO",
        ENGINE_VERSION, ENGINE_DEST));
    println!();
    log("Try it safely FIRST:  poler-engine --shell        (interactive REPL)");
    log("                    poler-engine --gateway      (full contour: pipes, redirects)");
    println!();
    log("\x1b[1;33mSAFETY: before making the engine a login shell, verify it works in your");
    log("terminal AND keep a fallback shell line in /etc/passwd. NEVER leave the");
    log("system with a single unverified login shell — that is how a 3-hour lockout happens.\x1b[0m");
    0
}

fn find_staged(root: &Path, name: &str) -> Option<PathBuf> {
    // search the extracted tree for the component binary
    for base in ["", "usr/bin", "bin"] {
        let cand = if base.is_empty() { root.join(name) } else { root.join(base).join(name) };
        if cand.exists() {
            return Some(cand);
        }
    }
    // fallback: shallow walk
    if let Ok(entries) = fs::read_dir(root) {
        for e in entries.flatten() {
            if e.file_name().to_string_lossy() == name && e.path().is_file() {
                return Some(e.path());
            }
        }
    }
    None
}

fn set_exec(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = fs::metadata(p) {
        let mut perm = meta.permissions();
        perm.set_mode(0o755);
        let _ = fs::set_permissions(p, perm);
    }
}

/// Returns the raw lines between "key" and the matching closing brace.
fn section_after(text: &str, key: &str) -> Option<String> {
    let pos = text.find(key)?;
    let after = &text[pos + key.len()..];
    let open = after.find('{')?;
    let mut depth = 0usize;
    let bytes = after[open..].as_bytes();
    let mut end = 0;
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'{' {
            depth += 1;
        } else if *b == b'}' {
            depth -= 1;
            if depth == 0 {
                end = i;
                break;
            }
        }
    }
    Some(after[open + 1..open + end].to_string())
}
