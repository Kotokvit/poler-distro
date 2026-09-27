// POLER-INIT — sovereign PID 1 for POLER CachyOS Sovereign Edition
// ============================================================================
// Stage 1 (initramfs, argv[0]==/init): mount pseudo-fs, load kernel modules
//   from modules.dep, scan block devices for the POLER boot medium (marker
//   file /poler/live.squashfs), loop-mount the squashfs, move mounts and
//   switch_root into the live system. Emergency poler-sh REPL on failure.
//
// Stage 2 (live system, /sbin/init): supervise poler-sh sessions on the
//   console (+ tty2..tty4 when present), respawn on exit, honor exit codes:
//     42 -> reboot    43 -> poweroff
//   Signals: SIGINT/SIGTERM/SIGUSR1 -> reboot; SIGUSR2/SIGQUIT -> poweroff.
//
// Exit codes of poler-sh consumed here match sovereign/poler-sh-core.
// No shell scripts, no systemd, no SysVinit glue: pure native Rust.
// ============================================================================

use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::thread;
use std::time::Duration;

const EXIT_REBOOT: i32 = 42;
const EXIT_POWEROFF: i32 = 43;
const RB_AUTOBOOT: libc::c_int = 0x01234567;
const RB_POWER_OFF: libc::c_int = 0x4321fedc;
const SYS_FINIT_MODULE: libc::c_long = 313; // x86_64
// ioctl request type differs between libc targets (glibc: c_ulong, musl: c_int)
#[cfg(target_env = "musl")]
type IoctlReq = libc::c_int;
#[cfg(not(target_env = "musl"))]
type IoctlReq = libc::c_ulong;
const LOOP_CTL_GET_FREE: IoctlReq = 0x4C82;
const LOOP_SET_FD: IoctlReq = 0x4C00;
const INITRAMFS_MARKER: &str = "/.poler-initramfs";

static PENDING_ACTION: AtomicI32 = AtomicI32::new(0); // 0 none, 1 reboot, 2 poweroff

unsafe extern "C" fn on_signal(sig: libc::c_int) {
    match sig {
        libc::SIGUSR2 | libc::SIGQUIT => PENDING_ACTION.store(2, Ordering::SeqCst),
        _ => PENDING_ACTION.store(1, Ordering::SeqCst),
    }
}

fn main() {
    // PID 1 always; initramfs stage detected by marker file in the rootfs.
    if Path::new(INITRAMFS_MARKER).exists() {
        initramfs_stage();
    } else {
        system_stage();
    }
}

fn log(msg: &str) {
    // Best-effort logging to the kernel console.
    if let Ok(mut c) = OpenOptions::new().write(true).open("/dev/console") {
        let _ = writeln!(c, "\x1b[1;36mpoler-init\x1b[0m: {}", msg);
    }
    let _ = writeln!(std::io::stderr(), "poler-init: {}", msg);
}

// ============================ Stage 1: initramfs ============================

fn initramfs_stage() -> ! {
    log("sovereign boot stage 1 (initramfs)");

    mount_early_pseudo_fs();

    let cmdline = fs::read_to_string("/proc/cmdline").unwrap_or_default();
    let debug = cmdline.contains("poler.debug");
    if debug {
        log(format!("kernel cmdline: {}", cmdline.trim()).as_str());
    }

    let kver = kernel_version();
    log(format!("kernel: {}", kver).as_str());

    load_boot_modules(&kver, debug);

    // Find the boot medium that carries /poler/live.squashfs.
    let boot_dir = match find_boot_medium(&cmdline, debug) {
        Some(d) => d,
        None => {
            log("\x1b[1;31mFATAL: boot medium with /poler/live.squashfs not found\x1b[0m");
            emergency_shell("boot medium not found");
        }
    };
    let squashfs = boot_dir.join("poler/live.squashfs");
    log(format!("boot medium: {}", boot_dir.display()).as_str());

    // Loop-mount the squashfs onto /mnt.
    if !mount_squashfs_loop(&squashfs, "/mnt") {
        log("\x1b[1;31mFATAL: cannot loop-mount live.squashfs\x1b[0m");
        emergency_shell("squashfs mount failed");
    }

    // Move pseudo-filesystems into the new root.
    for (src, dst) in [("/dev", "/mnt/dev"), ("/proc", "/mnt/proc"), ("/sys", "/mnt/sys"), ("/run", "/mnt/run")] {
        ensure_dir(dst);
        // /run may be a fresh tmpfs in initramfs; move only what exists mounted.
        if Path::new(src).exists() {
            let _ = unsafe { mount_move(src, dst) };
        }
    }

    // Free initramfs memory: delete everything except the mount target tree.
    delete_initramfs_files();

    // switch_root
    unsafe {
        if chdir_c("/mnt").is_err() {
            log("chdir /mnt failed");
            emergency_shell("switch_root failed");
        }
        if mount_move(".", "/").is_err() {
            log("MS_MOVE . -> / failed");
            emergency_shell("switch_root failed");
        }
        if chroot_c(".").is_err() {
            log("chroot failed");
            emergency_shell("switch_root failed");
        }
        let _ = chdir_c("/");
    }
    log("switch_root -> /sbin/init (poler-init stage 2)");

    // Diagnostics: prove what the new root actually contains before exec.
    for probe in ["/sbin/init", "/usr/bin/init", "/usr/bin/poler-init",
                  "/usr/bin/poler-sh", "/lib64/ld-linux-x86-64.so.2",
                  "/usr/lib/ld-linux-x86-64.so.2"] {
        let is_link = fs::symlink_metadata(probe)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);
        match fs::metadata(probe) {
            Ok(m) => log(format!("probe {} : present ({} bytes, {})", probe, m.len(),
                                 if is_link { "symlink" } else { "file" }).as_str()),
            Err(e) => log(format!("probe {} : {} — errno {}", probe, e, e.raw_os_error().unwrap_or(0)).as_str()),
        }
    }

    // exec chain with fallbacks — PID 1 handoff must never depend on a
    // single symlink resolving correctly.
    for cand in ["/sbin/init", "/usr/bin/init", "/usr/bin/poler-init"] {
        match exec_as_init(cand) {
            Ok(()) => unreachable!("execve does not return on success"),
            Err(errno) => {
                log(format!("execve({}) failed: errno {} ({})", cand, errno,
                            std::io::Error::from_raw_os_error(errno)).as_str());
            }
        }
    }
    emergency_shell("exec init failed");
}

fn exec_as_init(path: &str) -> Result<(), i32> {
    let init = CString::new(path).map_err(|_| 22)?;
    let arg1 = CString::new("poler-init").map_err(|_| 22)?;
    let argv = [init.clone(), arg1];
    let env = [
        CString::new("HOME=/root").unwrap(),
        CString::new("PATH=/usr/bin:/bin:/usr/sbin:/sbin").unwrap(),
        CString::new("TERM=linux").unwrap(),
    ];
    unsafe {
        libc::execve(
            init.as_ptr(),
            [argv[0].as_ptr(), argv[1].as_ptr(), std::ptr::null()].as_ptr(),
            [env[0].as_ptr(), env[1].as_ptr(), env[2].as_ptr(), std::ptr::null()].as_ptr(),
        );
    }
    Err(unsafe { *libc::__errno_location() })
}

fn mount_early_pseudo_fs() {
    ensure_dir("/dev");
    ensure_dir("/dev/pts");
    ensure_dir("/proc");
    ensure_dir("/sys");
    ensure_dir("/run");
    unsafe {
        // devtmpfs (kernel auto-populates device nodes)
        libc::mount(b"devtmpfs\0".as_ptr() as *const libc::c_char,
                    b"/dev\0".as_ptr() as *const libc::c_char,
                    b"devtmpfs\0".as_ptr() as *const libc::c_char, 0,
                    std::ptr::null());
        libc::mount(b"proc\0".as_ptr() as *const libc::c_char,
                    b"/proc\0".as_ptr() as *const libc::c_char,
                    b"proc\0".as_ptr() as *const libc::c_char, 0,
                    std::ptr::null());
        libc::mount(b"sysfs\0".as_ptr() as *const libc::c_char,
                    b"/sys\0".as_ptr() as *const libc::c_char,
                    b"sysfs\0".as_ptr() as *const libc::c_char, 0,
                    std::ptr::null());
        libc::mount(b"tmpfs\0".as_ptr() as *const libc::c_char,
                    b"/run\0".as_ptr() as *const libc::c_char,
                    b"tmpfs\0".as_ptr() as *const libc::c_char, 0,
                    b"mode=0755\0".as_ptr() as *const libc::c_void);
        libc::mount(b"devpts\0".as_ptr() as *const libc::c_char,
                    b"/dev/pts\0".as_ptr() as *const libc::c_char,
                    b"devpts\0".as_ptr() as *const libc::c_char, 0,
                    b"mode=0620\0".as_ptr() as *const libc::c_void);
    }
}

fn kernel_version() -> String {
    if let Ok(v) = fs::read_to_string("/proc/version") {
        // "Linux version 7.2.7-1-cachyos (user@...) ..."
        let mut it = v.split_whitespace();
        let _ = it.next(); // Linux
        let _ = it.next(); // version
        if let Some(kver) = it.next() {
            return kver.to_string();
        }
    }
    "unknown".to_string()
}

// ------------------------- kernel module loading ----------------------------

/// Static priority list of modules required to reach the boot medium.
/// Names are REAL module names (as in modules.dep, dashes normalized to
/// underscores). "isofs" is the module behind the iso9660 filesystem alias.
const BOOT_MODULES: &[&str] = &[
    // SCSI core + CD-ROM + ISO9660/UDF (optical boot, incl. QEMU IDE cdrom)
    "scsi_mod", "cdrom", "sr_mod", "isofs", "udf",
    // SATA/ATA (QEMU -cdrom default IDE: ata_piix + deps)
    "ata_piix", "ata_generic", "libata", "ahci", "sd_mod",
    // USB mass storage chain
    "usb_common", "usbcore", "xhci_pci", "ehci_pci", "uhci_hcd",
    "usb_storage", "uas",
    // FAT for USB sticks formatted vfat
    "nls_cp437", "nls_iso8859_1", "vfat", "fat",
    // Live root
    "squashfs", "loop",
];

/// userspace aliases -> real module names (modprobe-style resolution).
const MODULE_ALIASES: &[(&str, &str)] = &[("iso9660", "isofs")];

fn resolve_alias(name: &str) -> &str {
    for (alias, real) in MODULE_ALIASES {
        if *alias == name {
            return real;
        }
    }
    name
}

struct ModulesDep {
    // basename (no .ko, no path) -> (relative path from module dir, deps)
    entries: Vec<(String, String, Vec<String>)>,
}

fn parse_modules_dep(kver: &str) -> ModulesDep {
    let mut entries = Vec::new();
    let base = format!("/usr/lib/modules/{}", kver);
    let path = format!("{}/modules.dep", base);
    if let Ok(content) = fs::read_to_string(&path) {
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let (left, right) = match line.split_once(':') {
                Some(x) => x,
                None => continue,
            };
            let rel = left.trim().to_string();
            let name = module_basename(&rel);
            let deps: Vec<String> = right
                .split_whitespace()
                .filter(|d| !d.is_empty())
                .map(|d| module_basename(d))
                .collect();
            entries.push((name, rel, deps));
        }
    }
    ModulesDep { entries }
}

fn module_basename(rel: &str) -> String {
    // modprobe normalization: file names may use dashes or underscores
    // interchangeably; underscores are the canonical module-name form.
    let base = rel.rsplit('/').next().unwrap_or(rel);
    let stem = base
        .strip_suffix(".ko.zst")
        .or_else(|| base.strip_suffix(".ko.xz"))
        .or_else(|| base.strip_suffix(".ko"))
        .unwrap_or(base);
    stem.replace('-', "_")
}

fn load_boot_modules(kver: &str, debug: bool) {
    let dep = parse_modules_dep(kver);
    let base = format!("/usr/lib/modules/{}", kver);
    let mut loaded: Vec<String> = Vec::new();
    let mut missing: Vec<&str> = Vec::new();

    for want in BOOT_MODULES {
        let real = resolve_alias(want);
        let before = loaded.len();
        load_module_tree(real, &dep, &base, &mut loaded, debug, 0);
        if loaded.len() == before && !dep.entries.iter().any(|(n, _, _)| n == real) {
            missing.push(want);
        }
    }
    log(format!("{} kernel modules loaded", loaded.len()).as_str());
    if !missing.is_empty() {
        // Not fatal: some of these may be built into the kernel (=y).
        log(format!("not in modules.dep (built-in or absent): {:?}", missing).as_str());
    }
}

fn load_module_tree(want: &str, dep: &ModulesDep, base: &str, loaded: &mut Vec<String>, debug: bool, depth: usize) {
    if depth > 16 || loaded.iter().any(|m| m == want) {
        return;
    }
    let entry = dep.entries.iter().find(|(name, _, _)| name == want);
    let (rel, deps) = match entry {
        Some((_, rel, deps)) => (rel.clone(), deps.clone()),
        None => {
            if debug {
                log(format!("module '{}' not in modules.dep (may be built-in)", want).as_str());
            }
            return;
        }
    };
    for d in &deps {
        load_module_tree(d, dep, base, loaded, debug, depth + 1);
    }
    let path = format!("{}/{}", base, rel);
    if !Path::new(&path).exists() {
        if debug {
            log(format!("module file missing: {}", path).as_str());
        }
        return;
    }
    if insmod_compressed(&path) {
        loaded.push(want.to_string());
        if debug {
            log(format!("insmod {} ({})", want, rel).as_str());
        }
    } else if debug {
        log(format!("insmod FAILED: {}", path).as_str());
    }
}

/// insmod via finit_module syscall; decompresses .ko.zst/.ko.xz into memory first.
fn insmod_compressed(path: &str) -> bool {
    let mut data = Vec::new();
    match File::open(path) {
        Ok(mut f) => {
            if f.read_to_end(&mut data).is_err() {
                return false;
            }
        }
        Err(_) => return false,
    }
    let cpath = match CString::new(path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    // Raw ELF: finit_module directly with O_RDONLY fd.
    if data.starts_with(b"\x7fELF") {
        let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
        if fd < 0 {
            return false;
        }
        let rc = unsafe {
            libc::syscall(SYS_FINIT_MODULE, fd, b"\0".as_ptr() as *const libc::c_char, 0)
        };
        unsafe { libc::close(fd) };
        return rc == 0;
    }
    // Compressed module (.ko.zst / .ko.xz — Arch-style packaging):
    // kernel >= 6.1 accepts MODULE_INIT_COMPRESSED_FILE for in-kernel
    // decompression; fall back to userspace zstd/xz otherwise.
    let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
    if fd >= 0 {
        let flags: libc::c_uint = 4; // MODULE_INIT_COMPRESSED_FILE (since 6.1)
        let rc = unsafe {
            libc::syscall(SYS_FINIT_MODULE, fd, b"\0".as_ptr() as *const libc::c_char, flags)
        };
        unsafe { libc::close(fd) };
        if rc == 0 {
            return true;
        }
    }
    decompressed_insmod(path)
}

/// Fallback: decompress via zstd/xz binaries present in the initramfs
/// (absolute paths — PID 1 has no meaningful PATH), then init_module from
/// the decompressed buffer.
fn decompressed_insmod(path: &str) -> bool {
    let out = if path.ends_with(".zst") {
        Command::new("/usr/bin/zstd").arg("-d").arg("-c").arg(path).output()
    } else if path.ends_with(".xz") {
        Command::new("/usr/bin/xz").arg("-d").arg("-c").arg(path).output()
    } else {
        return false;
    };
    let decompressed = match out {
        Ok(o) if o.status.success() => o.stdout,
        _ => return false,
    };
    if decompressed.is_empty() || !decompressed.starts_with(b"\x7fELF") {
        return false;
    }
    unsafe {
        let rc = libc::syscall(
            libc::SYS_init_module,
            decompressed.as_ptr() as *const libc::c_void,
            decompressed.len(),
            b"\0".as_ptr() as *const libc::c_char,
        );
        rc == 0
    }
}

// ------------------------- boot medium discovery ----------------------------

fn find_boot_medium(cmdline: &str, debug: bool) -> Option<PathBuf> {
    // poler.root=/dev/xxx — explicit override.
    for token in cmdline.split_whitespace() {
        if let Some(dev) = token.strip_prefix("poler.root=") {
            let dev = dev.trim();
            if !dev.is_empty() {
                log(format!("explicit poler.root={}", dev).as_str());
                if let Some(d) = try_mount_medium(dev, debug) {
                    return Some(d);
                }
                return None;
            }
        }
    }
    // Scan block devices for up to ~15 seconds (USB enumeration can be slow).
    for attempt in 0..30 {
        let devices = list_block_devices();
        if attempt == 0 {
            log(format!("scanning {} block device node(s) for POLER boot medium...", devices.len()).as_str());
        }
        if !devices.is_empty() {
            for dev in devices {
                if let Some(d) = try_mount_medium(&dev, debug) {
                    return Some(d);
                }
            }
        }
        thread::sleep(Duration::from_millis(500));
    }
    None
}

fn list_block_devices() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir("/sys/block") {
        let mut names: Vec<String> = entries
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        for name in names {
            // Whole disk/cdrom device node
            out.push(format!("/dev/{}", name));
            // Partitions: /sys/block/<name>/<name>N
            if let Ok(parts) = fs::read_dir(format!("/sys/block/{}", name)) {
                let mut part_names: Vec<String> = parts
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .filter(|n| n.starts_with(&name) && n.len() > name.len())
                    .collect();
                part_names.sort();
                for p in part_names {
                    out.push(format!("/dev/{}", p));
                }
            }
        }
    }
    out
}

/// Try to mount `dev` read-only with likely filesystems and look for the
/// POLER marker. Returns the mountpoint on success.
fn try_mount_medium(dev: &str, debug: bool) -> Option<PathBuf> {
    if !Path::new(dev).exists() {
        return None;
    }
    let mnt = "/run/medium";
    ensure_dir(mnt);
    // Already mounted? Unmount to retry cleanly.
    unsafe {
        libc::umount2(cstr(mnt).as_ptr(), libc::MNT_DETACH);
    }
    for fs_type in ["iso9660", "udf", "vfat", "exfat", "ext4", "btrfs"] {
        let fst = CString::new(fs_type).unwrap();
        let rc = unsafe {
            libc::mount(cstr(dev).as_ptr(), cstr(mnt).as_ptr(), fst.as_ptr(),
                        libc::MS_RDONLY | libc::MS_NOSUID, std::ptr::null())
        };
        if rc == 0 {
            let marker = Path::new(mnt).join("poler/live.squashfs");
            if marker.exists() {
                return Some(PathBuf::from(mnt));
            }
            unsafe {
                libc::umount2(cstr(mnt).as_ptr(), 0);
            }
            return None; // mounted fine but not our medium
        }
        if debug {
            log(format!("mount {} as {} failed (errno {})", dev, fs_type, unsafe { *libc::__errno_location() }).as_str());
        }
    }
    None
}

// ------------------------------ loop + squashfs -----------------------------

fn mount_squashfs_loop(squashfs: &Path, target: &str) -> bool {
    // Get a free loop device via /dev/loop-control.
    ensure_dir(target);
    let ctrl_path = "/dev/loop-control";
    if !Path::new(ctrl_path).exists() {
        // misc device 10:237
        unsafe {
            let c = cstr(ctrl_path);
            libc::mknod(c.as_ptr(), libc::S_IFCHR | 0o600, libc::makedev(10, 237));
        }
    }
    let loop_dev: Option<String> = (|| {
        let f = File::open(ctrl_path).ok()?;
        let idx = unsafe { libc::ioctl(f.as_raw_fd(), LOOP_CTL_GET_FREE) };
        if idx < 0 {
            return None;
        }
        Some(format!("/dev/loop{}", idx))
    })();

    let sq = match File::open(squashfs) {
        Ok(f) => f,
        Err(_) => return false,
    };

    let dev_name = match loop_dev {
        Some(d) => d,
        None => {
            // Fallback: probe /dev/loop0../dev/loop7
            let mut found = None;
            for i in 0..8 {
                let cand = format!("/dev/loop{}", i);
                if let Ok(lf) = OpenOptions::new().read(true).write(true).open(&cand) {
                    let rc = unsafe { libc::ioctl(lf.as_raw_fd(), LOOP_SET_FD, sq.as_raw_fd()) };
                    if rc == 0 {
                        found = Some(cand);
                        break;
                    }
                }
            }
            match found {
                Some(d) => d,
                None => return false,
            }
        }
    };

    if let Ok(mut lf) = OpenOptions::new().read(true).write(true).open(&dev_name) {
        let rc = unsafe { libc::ioctl(lf.as_raw_fd(), LOOP_SET_FD, sq.as_raw_fd()) };
        if rc != 0 {
            // loop-control already bound it; only fatal if this is a fresh bind failure
            let errno = unsafe { *libc::__errno_location() };
            if errno != libc::EBUSY {
                return false;
            }
        }
        let _ = lf.seek(SeekFrom::Start(0));
    }

    let rc = unsafe {
        libc::mount(cstr(&dev_name).as_ptr(), cstr(target).as_ptr(),
                    b"squashfs\0".as_ptr() as *const libc::c_char,
                    libc::MS_RDONLY | libc::MS_NOSUID, std::ptr::null())
    };
    rc == 0
}

// ------------------------------ switch_root ---------------------------------

fn delete_initramfs_files() {
    // Free ramfs memory the busybox switch_root way: wipe everything except
    // the new root mount. Mountpoints (e.g. a failed MS_MOVE) are skipped —
    // deleting through them would destroy the moved filesystem's contents.
    let keep = ["mnt"];
    if let Ok(entries) = fs::read_dir("/") {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if keep.contains(&name.as_str()) {
                continue;
            }
            let path = format!("/{}", name);
            let meta = match fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.is_dir() && !meta.file_type().is_symlink() {
                if !is_mountpoint(&path) {
                    let _ = fs::remove_dir_all(&path);
                }
            } else {
                let _ = fs::remove_file(&path);
            }
        }
    }
}

fn is_mountpoint(path: &str) -> bool {
    use std::os::unix::fs::MetadataExt;
    let parent = match Path::new(path).parent() {
        Some(p) => p.to_string_lossy().to_string(),
        None => return true,
    };
    match (fs::metadata(&path), fs::metadata(&parent)) {
        (Ok(a), Ok(b)) => a.dev() != b.dev(),
        _ => true, // be conservative when in doubt
    }
}

// ============================ Stage 2: live system ===========================

fn system_stage() -> ! {
    log("sovereign boot stage 2 (live system supervisor)");

    // Defensive mounts (normally inherited via MS_MOVE from initramfs).
    if !Path::new("/proc/self").exists() {
        mount_early_pseudo_fs();
    }

    // Hostname
    let _ = fs::write("/proc/sys/kernel/hostname", "poler-cachyos\n");

    // /etc/mtab convenience symlink (some userspace tools)
    let _ = std::os::unix::fs::symlink("/proc/mounts", "/etc/mtab");

    banner();

    // Signals: SIGINT/SIGTERM/SIGUSR1 -> reboot; SIGUSR2/SIGQUIT -> poweroff.
    // NOTE: SIGCHLD stays at default — the supervision loop needs wait() to
    // collect the shell's exit code (42=reboot, 43=poweroff).
    unsafe {
        for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGUSR1, libc::SIGUSR2, libc::SIGQUIT] {
            libc::signal(sig, on_signal as *const () as usize);
        }
    }

    // Supervision loop on the console (and extra TTYs when present).
    let consoles = available_consoles();
    log(format!("supervising {} poler-sh session(s): {:?}", consoles.len(), consoles).as_str());

    let mut restarts: u32 = 0;
    loop {
        let action = PENDING_ACTION.load(Ordering::SeqCst);
        if action == 1 {
            do_reboot();
        }
        if action == 2 {
            do_poweroff();
        }

        // Spawn shell on first console synchronously; others as background respawners.
        let primary = consoles[0].clone();
        let code = spawn_shell_wait(&primary);
        match code {
            EXIT_REBOOT => do_reboot(),
            EXIT_POWEROFF => do_poweroff(),
            _ => {
                restarts += 1;
                if restarts > 200 {
                    log("shell crash-looping too fast, throttling to 1s");
                    thread::sleep(Duration::from_secs(1));
                } else {
                    thread::sleep(Duration::from_millis(80));
                }
            }
        }
    }
}

fn available_consoles() -> Vec<String> {
    let mut out = vec!["/dev/console".to_string()];
    for tty in ["/dev/tty2", "/dev/tty3", "/dev/tty4"] {
        if Path::new(tty).exists() {
            out.push(tty.to_string());
        }
    }
    out
}

fn banner() {
    if let Ok(mut c) = OpenOptions::new().write(true).open("/dev/console") {
        let _ = writeln!(c, "\x1b[1;36m╭─────────────────────────────────────────────────────╮\x1b[0m");
        let _ = writeln!(c, "\x1b[1;36m│\x1b[0m \x1b[1;37mPOLER CachyOS Sovereign Edition\x1b[0m              \x1b[1;36m│\x1b[0m");
        let _ = writeln!(c, "\x1b[1;36m│\x1b[0m \x1b[1;33mpoler-init PID 1 · poler-sh · bash purged\x1b[0m       \x1b[1;36m│\x1b[0m");
        let _ = writeln!(c, "\x1b[1;36m╰─────────────────────────────────────────────────────╯\x1b[0m");
    }
}

fn spawn_shell_wait(tty: &str) -> i32 {
    // Background TTYs: spawn detached respawner threads (best effort).
    if tty != "/dev/console" {
        let tty = tty.to_string();
        thread::spawn(move || loop {
            let _ = spawn_shell_once(&tty);
            thread::sleep(Duration::from_millis(300));
        });
        return 0;
    }
    spawn_shell_once(tty)
}

fn spawn_shell_once(tty: &str) -> i32 {
    let console = match OpenOptions::new().read(true).write(true).open(tty) {
        Ok(f) => f,
        Err(_) => {
            log(format!("cannot open {} — falling back to stdio", tty).as_str());
            return run_shell_stdio();
        }
    };
    let mut child = match Command::new("/usr/bin/poler-sh")
        .env("HOME", "/root")
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("TERM", "linux")
        .env("SHELL", "/usr/bin/poler-sh")
        .env("USER", "root")
        .env("POLER_INIT", "1")
        .stdin(Stdio::from(console.try_clone().unwrap()))
        .stdout(Stdio::from(console.try_clone().unwrap()))
        .stderr(Stdio::from(console))
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            log(format!("cannot spawn poler-sh: {} — stdio fallback", e).as_str());
            return run_shell_stdio();
        }
    };
    match child.wait() {
        Ok(status) => status.code().unwrap_or(0),
        Err(_) => 0,
    }
}

fn run_shell_stdio() -> i32 {
    match Command::new("/usr/bin/poler-sh")
        .env("HOME", "/root")
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("TERM", "linux")
        .status()
    {
        Ok(s) => s.code().unwrap_or(0),
        Err(_) => {
            log("\x1b[1;31mno shell available — system halted\x1b[0m");
            0
        }
    }
}

fn do_reboot() -> ! {
    log("REBOOT requested");
    unsafe {
        libc::sync();
        libc::reboot(RB_AUTOBOOT);
    }
    std::process::exit(0);
}

fn do_poweroff() -> ! {
    log("POWEROFF requested");
    unsafe {
        libc::sync();
        libc::reboot(RB_POWER_OFF);
    }
    std::process::exit(0);
}

// ------------------------------ emergency REPL ------------------------------

fn emergency_shell(reason: &str) -> ! {
    log(format!("EMERGENCY shell ({})", reason).as_str());
    if let Ok(mut c) = OpenOptions::new().write(true).open("/dev/console") {
        let _ = writeln!(c, "\x1b[1;31mPOLER EMERGENCY MODE: {}\x1b[0m", reason);
        let _ = writeln!(c, "Starting poler-sh on console. Fix the problem and type 'exit 44' to retry boot.\x1b[0m");
    }
    loop {
        let code = run_shell_stdio();
        if code == 44 {
            // retry the whole boot path
            log("retry requested — re-entering initramfs stage");
            initramfs_stage();
        }
        if code == EXIT_REBOOT {
            do_reboot();
        }
        if code == EXIT_POWEROFF {
            do_poweroff();
        }
        // no shell available (or it exited) — avoid a log flood
        thread::sleep(Duration::from_millis(700));
    }
}

// --------------------------------- helpers ----------------------------------

fn ensure_dir(path: &str) {
    let _ = fs::create_dir_all(path);
}

fn cstr(s: &str) -> CString {
    CString::new(s).unwrap()
}

unsafe fn chdir_c(path: &str) -> Result<(), ()> {
    let c = cstr(path);
    if libc::chdir(c.as_ptr()) == 0 {
        Ok(())
    } else {
        Err(())
    }
}

unsafe fn chroot_c(path: &str) -> Result<(), ()> {
    let c = cstr(path);
    if libc::chroot(c.as_ptr()) == 0 {
        Ok(())
    } else {
        Err(())
    }
}

unsafe fn mount_move(source: &str, target: &str) -> Result<(), ()> {
    let s = cstr(source);
    let t = cstr(target);
    if libc::mount(s.as_ptr(), t.as_ptr(), std::ptr::null(), libc::MS_MOVE, std::ptr::null()) == 0 {
        Ok(())
    } else {
        Err(())
    }
}
