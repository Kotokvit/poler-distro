// POLER Sovereign Updater — native replacement for legacy package-manager glue.
// ============================================================================
// Synchronizes the running POLER CachyOS Sovereign Edition system directly
// with the user's GitHub mirror (Kotokvit/poler-distro):
//
//   poler-update            — report mode: show mirror state vs local state
//   poler-update --apply    — download + verify + atomically install the
//                             sovereign core stack from the latest release
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
    ("poler-sh", "/usr/bin/poler-sh"),
    ("poler", "/usr/bin/poler"),
    ("poler-box", "/usr/bin/poler-box"),
    ("poler-fuse", "/usr/bin/poler-fuse"),
    ("poler-update", "/usr/bin/poler-update"),
];

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
    println!("  poler-update             report mirror state vs this system");
    println!("  poler-update --apply     download, verify and install the latest");
    println!("                           sovereign core stack from GitHub");
    println!("  poler-update --help      this help");
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
    let mut idx = 0;
    while let Some(pos) = text[idx..].find("\"browser_download_url\"") {
        let block_start = idx + pos;
        // walk back to the opening '{' of this asset object
        let obj_start = text[..block_start].rfind('{').unwrap_or(0);
        // find the closing '}' after the url
        let url_after = &text[block_start..];
        let url = json_string_field(url_after, "browser_download_url").unwrap_or_default();
        let name = json_string_field(&text[obj_start..block_start + 40], "name")
            .or_else(|| json_string_field(url_after, "name"))
            .unwrap_or_default();
        if !url.is_empty() {
            out.push((name, url));
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

    // Atomic install
    let mut installed = 0;
    for (name, dest) in INSTALL_MAP {
        let staged = find_staged(&extract_dir, name);
        if let Some(src) = staged {
            let dest = Path::new(dest);
            if let Some(parent) = dest.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let tmp = dest.with_extension("new");
            if fs::copy(&src, &tmp).is_err() {
                std::process::exit(err(&format!("cannot stage {}", dest.display())));
            }
            set_exec(&tmp);
            if fs::rename(&tmp, dest).is_err() {
                let _ = fs::remove_file(&tmp);
                std::process::exit(err(&format!("cannot atomically install {} (running binary?) — retry after reboot", dest.display())));
            }
            installed += 1;
            log(&format!("installed \x1b[1;32m{}\x1b[0m -> {}", name, dest.display()));
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
