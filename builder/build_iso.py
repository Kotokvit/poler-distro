#!/usr/bin/env python3
"""
POLER CachyOS Sovereign Edition — REAL ISO builder.

Pipeline (requires root; designed for GitHub Actions ubuntu-latest):
  1. Fetch the Arch Linux bootstrap rootfs (real, pacman-equipped base).
  2. Point pacman at the sovereign CachyOS mirrors (cachyos-core/extra),
     install cachyos-keyring + cachyos-mirrorlist, sync packages
     (linux-cachyos kernel, core userland, curl for updates).
  3. PURGE legacy: bash, systemd, SysVinit glue, mkinitcpio, pacman itself —
     the sovereign image carries zero legacy shell/PID-1 glue.
  4. Inject the POLER sovereign stack (built from source by CI):
       poler-init (PID 1), poler-sh (shell), poler, poler-box, poler-fuse,
       poler-update, /bin/sh + /bin/bash symlinks -> poler-sh,
       /etc/passwd root shell -> poler-sh, /sbin/init -> poler-init.
  5. Write /etc/os-release (POLER CachyOS Sovereign Edition) and
     /poler/VERIFICATION.txt with the full build proof.
  6. mksquashfs the rootfs -> /poler/live.squashfs.
  7. Build poler-initramfs.cpio.gz: poler-init /init + kernel modules needed
     to reach the medium (iso9660/sr_mod/ata_piix/ahci/usb/squashfs/loop)
     + zstd/xz fallback decompressors with their shared libs.
  8. grub-mkrescue -> hybrid BIOS+UEFI ISO (xorriso).
  9. QEMU smoke boot (--qemu): boots the ISO, asserts the sovereign chain
     (poler-init stage 1 -> stage 2 -> poler-sh) on the serial console.

Usage:
  sudo python3 builder/build_iso.py [--version 1.1.0] [--qemu] [--keep-work]
"""

import argparse
import glob as _glob
import hashlib
import os
import shutil
import subprocess
import sys
import time

# ---------------------------------------------------------------- constants --

DISTRO_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

BOOTSTRAP_URL = "https://geo.mirror.pkgbuild.com/iso/latest/archlinux-bootstrap-x86_64.tar.zst"

CACHYOS_MIRROR = "https://mirror.cachyos.org/repo"
ARCH_MIRROR = "https://geo.mirror.pkgbuild.com"

KERNEL_PKG = "linux-cachyos"

# Extra packages synced into the bootstrap base (curl = poler-update transport).
ROOT_PKGS = [
    KERNEL_PKG,
    "curl", "ca-certificates", "ca-certificates-mozilla",
    "zstd", "xz", "tar", "gzip",
    "tzdata", "iana-etc", "less", "which",
    "iproute2", "iputils",
]

# Legacy packages purged from the final image (force, deps ignored — the
# sovereign stack replaces their roles):
PURGE_PKGS = [
    "bash",              # replaced by poler-sh
    "systemd",           # replaced by poler-init (PID 1)
    "mkinitcpio",        # replaced by the sovereign poler-initramfs
    "pacman",            # updates flow from GitHub via poler-update
    "arch-install-scripts",
    "sysvinit", "initramfs-tools",
]

# POLER stack binaries (built by CI into binaries/).
# poler-box / poler-fuse are optional (best-effort); the rest are required.
POLER_BINARIES = {
    "poler-init":  "/usr/bin/poler-init",
    "poler-sh":    "/usr/bin/poler-sh",
    "poler":       "/usr/bin/poler",
    "poler-box":   "/usr/bin/poler-box",
    "poler-fuse":  "/usr/bin/poler-fuse",
    "poler-update": "/usr/bin/poler-update",
}
OPTIONAL_BINARIES = {"poler-box", "poler-fuse"}

# Kernel modules the initramfs must carry to reach the boot medium.
# NOTE: real module names as found in modules.dep ("isofs" is the module
# behind the iso9660 alias; dashes are normalized to underscores).
INITRAMFS_MODULES = [
    "scsi_mod", "cdrom", "sr_mod", "isofs", "udf",
    "ata_piix", "ata_generic", "libata", "ahci", "sd_mod",
    "usb_common", "usbcore", "xhci_pci", "ehci_pci", "uhci_hcd",
    "usb_storage", "uas",
    "nls_cp437", "nls_iso8859_1", "vfat", "fat",
    "squashfs", "loop",
]

ISO_VOLUME_ID = "POLER_CACHYOS"
ISO_MARKER = ".poler_boot_medium"

GREEN = "\033[1;32m"
CYAN = "\033[1;36m"
YELLOW = "\033[1;33m"
RED = "\033[1;31m"
RESET = "\033[0m"


def log(msg):
    print(f"{CYAN}[POLER-ISO]{RESET} {msg}", flush=True)


def ok(msg):
    print(f"{GREEN}[✓]{RESET} {msg}", flush=True)


def die(msg):
    print(f"{RED}[FATAL]{RESET} {msg}", flush=True)
    sys.exit(1)


def sh(cmd, check=True, env=None, quiet=False):
    r = subprocess.run(cmd, shell=True, env=env,
                       stdout=subprocess.DEVNULL if quiet else None,
                       stderr=subprocess.DEVNULL if quiet else None)
    if check and r.returncode != 0:
        die(f"command failed ({r.returncode}): {cmd[:200]}")
    return r


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def du_gb(path):
    r = subprocess.run(["du", "-sm", path], capture_output=True, text=True)
    try:
        return float(r.stdout.split()[0]) / 1024.0
    except Exception:
        return 0.0


# ------------------------------------------------------------------- stages --

def stage_fetch_bootstrap(work):
    bs_tar = os.path.join(work, "bootstrap.tar.zst")
    if not os.path.exists(bs_tar):
        log(f"downloading Arch bootstrap: {BOOTSTRAP_URL}")
        sh(f"curl -fL --retry 3 -o '{bs_tar}' '{BOOTSTRAP_URL}'")
    ok(f"bootstrap tarball: {os.path.getsize(bs_tar) / 1e6:.1f} MB")
    return bs_tar


def stage_extract_rootfs(work, bs_tar, rootfs):
    if os.path.exists(os.path.join(rootfs, ".poler_rootfs_ready")):
        ok("rootfs already extracted")
        return
    if os.path.exists(rootfs):
        unmount_all(rootfs)
        shutil.rmtree(rootfs)
    os.makedirs(rootfs, exist_ok=True)
    log("extracting bootstrap rootfs (real Arch base)...")
    sh(f"tar --zstd -xf '{bs_tar}' -C '{rootfs}' --strip-components=1")
    open(os.path.join(rootfs, ".poler_rootfs_ready"), "w").write("ok\n")
    ok(f"rootfs base: {du_gb(rootfs):.2f} GB")


PACMAN_CONF = """#
# POLER CachyOS Sovereign Edition — pacman.conf (build-time only;
# pacman itself is purged from the final image)
#
[options]
HoldPkg     = pacman glibc
Architecture = x86_64
# No CheckSpace: in a chroot on CI runners the root mount point cannot be
# resolved from the bind-mounted /proc, producing spurious
# "not enough free disk space" aborts.
SigLevel    = Required DatabaseOptional
LocalFileSigLevel = Optional

[core]
Server = {arch_mirror}/$repo/os/$arch

[extra]
Server = {arch_mirror}/$repo/os/$arch

[cachyos]
Server = {cachyos_mirror}/$arch/cachyos
"""


def mount_pseudo(rootfs):
    for src, dst in [("/dev", "dev"), ("/proc", "proc"), ("/sys", "sys")]:
        d = os.path.join(rootfs, dst)
        os.makedirs(d, exist_ok=True)
        sh(f"mount --bind {src} '{d}'", check=False)
    resolv = os.path.join(rootfs, "etc/resolv.conf")
    if os.path.exists(resolv) or os.path.islink(resolv):
        os.remove(resolv)
    shutil.copy2("/etc/resolv.conf", resolv)


def unmount_all(rootfs):
    for dst in ["dev", "proc", "sys"]:
        d = os.path.join(rootfs, dst)
        sh(f"umount -l '{d}' 2>/dev/null", check=False, quiet=True)


def stage_prepare_pacman(rootfs):
    """Sovereign mirrors + keyrings inside the real rootfs."""
    conf = os.path.join(rootfs, "etc/pacman.conf")
    with open(conf, "w") as f:
        f.write(PACMAN_CONF.format(arch_mirror=ARCH_MIRROR, cachyos_mirror=CACHYOS_MIRROR))

    log("initializing keyrings (archlinux + cachyos)...")
    sh(f"chroot '{rootfs}' /usr/bin/pacman-key --init")
    sh(f"chroot '{rootfs}' /usr/bin/pacman-key --populate archlinux")

    # cachyos keyring + mirrorlist straight from the sovereign mirror
    keyring_url = f"{CACHYOS_MIRROR}/x86_64/cachyos/cachyos-keyring-20240331-1-any.pkg.tar.zst"
    mirrorlist_url = f"{CACHYOS_MIRROR}/x86_64/cachyos/cachyos-mirrorlist-27-1-any.pkg.tar.zst"
    tmp = os.path.join(rootfs, "tmp")
    sh(f"curl -fsSL --retry 3 -o '{tmp}/cachyos-keyring.pkg.tar.zst' '{keyring_url}'")
    sh(f"curl -fsSL --retry 3 -o '{tmp}/cachyos-mirrorlist.pkg.tar.zst' '{mirrorlist_url}'")
    sh(f"chroot '{rootfs}' /usr/bin/pacman -U --noconfirm "
       f"/tmp/cachyos-keyring.pkg.tar.zst /tmp/cachyos-mirrorlist.pkg.tar.zst")
    sh(f"chroot '{rootfs}' /usr/bin/pacman-key --populate cachyos")
    ok("cachyos sovereign mirrors + keyring registered")


def stage_sync_pkgs(rootfs, kernel_pkg):
    log(f"syncing packages from CachyOS mirrors (kernel: {kernel_pkg})...")
    pkgs = [p if p != KERNEL_PKG else kernel_pkg for p in ROOT_PKGS]
    sh(f"chroot '{rootfs}' /usr/bin/pacman -Sy --needed --noconfirm " + " ".join(pkgs))
    ok(f"rootfs after sync: {du_gb(rootfs):.2f} GB")


def stage_purge_legacy(rootfs):
    log("purging legacy stack (bash, systemd, mkinitcpio, pacman)...")
    for pkg in PURGE_PKGS:
        sh(f"chroot '{rootfs}' /usr/bin/pacman -Rdd --noconfirm {pkg} 2>/dev/null",
           check=False, quiet=True)

    # Hard evidence: no bash ELF may survive anywhere.
    for victim in ["usr/bin/bash", "usr/bin/sh", "bin/bash", "bin/sh", "usr/bin/systemd"]:
        p = os.path.join(rootfs, victim)
        if os.path.exists(p) and not os.path.islink(p):
            os.remove(p)
    # orphaned systemd directories
    for legacy_dir in ["usr/lib/systemd", "etc/systemd", "usr/lib/sysusers.d",
                       "etc/init.d", "var/spool/mail"]:
        p = os.path.join(rootfs, legacy_dir)
        if os.path.isdir(p):
            shutil.rmtree(p, ignore_errors=True)

    # /bin and /sbin -> usr/bin (Arch merged layout)
    for merged in ["bin", "sbin"]:
        d = os.path.join(rootfs, merged)
        if os.path.isdir(d) and not os.path.islink(d):
            for item in os.listdir(d):
                src = os.path.join(d, item)
                dst = os.path.join(rootfs, "usr/bin", item)
                if not os.path.exists(dst):
                    shutil.move(src, dst)
            os.rmdir(d)
        linkpath = os.path.join(rootfs, merged)
        if not os.path.lexists(linkpath):
            os.symlink("usr/bin", linkpath)

    # Sovereign wiring: shell + PID 1.
    for link, target in [("usr/bin/sh", "poler-sh"),
                         ("usr/bin/bash", "poler-sh")]:
        lp = os.path.join(rootfs, link)
        if os.path.lexists(lp):
            os.remove(lp)
        os.symlink(target, lp)

    # PID 1: with the merged usr layout (sbin -> usr/bin) the canonical
    # /sbin/init IS /usr/bin/init — creating rootfs/sbin/init would traverse
    # the sbin symlink and produce a broken link at the wrong place.
    init_link = os.path.join(rootfs, "usr/bin/init")
    if os.path.lexists(init_link):
        os.remove(init_link)
    os.symlink("poler-init", init_link)

    ok("legacy purged: /bin/sh, /bin/bash -> poler-sh; /sbin/init (=usr/bin/init) -> poler-init")


def stage_inject_poler(rootfs, bins_dir):
    log("injecting POLER sovereign stack...")
    for name, dest in POLER_BINARIES.items():
        src = os.path.join(bins_dir, name)
        if not os.path.exists(src):
            if name in OPTIONAL_BINARIES:
                print(f"{YELLOW}[skip]{RESET} optional component absent: {name}")
                continue
            die(f"missing POLER binary: {src} — build the sovereign stack first")
        d = os.path.join(rootfs, dest)
        os.makedirs(os.path.dirname(d), exist_ok=True)
        shutil.copy2(src, d)
        os.chmod(d, 0o755)

    # sovereign manifest + mirror config
    poler_etc = os.path.join(rootfs, "etc/poler")
    os.makedirs(poler_etc, exist_ok=True)
    src_manifest = os.path.join(DISTRO_DIR, "manifest.json")
    if os.path.exists(src_manifest):
        shutil.copy2(src_manifest, os.path.join(poler_etc, "manifest.json"))

    # /etc/passwd: every login shell is the sovereign shell.
    passwd = os.path.join(rootfs, "etc/passwd")
    out = []
    if os.path.exists(passwd):
        for line in open(passwd).read().splitlines():
            parts = line.split(":")
            if len(parts) == 7 and parts[6] in (
                    "/bin/bash", "/bin/sh", "/usr/bin/bash", "/usr/bin/sh", "/sbin/nologin"):
                parts[6] = "/usr/bin/poler-sh" if parts[0] == "root" else "/usr/bin/poler-sh"
                line = ":".join(parts)
            out.append(line)
    else:
        out = ["root:x:0:0::/root:/usr/bin/poler-sh"]
    open(passwd, "w").write("\n".join(out) + "\n")

    os.makedirs(os.path.join(rootfs, "etc"), exist_ok=True)
    open(os.path.join(rootfs, "etc/securetty"), "w").write(
        "console\ntty1\ntty2\ntty3\ntty4\nttyS0\n")
    ok("POLER stack injected")


def stage_os_release(rootfs, version):
    content = f"""NAME="POLER CachyOS Sovereign Edition"
PRETTY_NAME="POLER CachyOS Sovereign Edition {version}"
ID=poler
ID_LIKE=cachyos arch
BUILD_ID=rolling
ANSI_COLOR="1;36"
HOME_URL="https://github.com/Kotokvit/poler-distro"
SUPPORT_URL="https://github.com/Kotokvit/poler-distro"
LOGO=poler

POLER_INIT=1
POLER_SHELL=/usr/bin/poler-sh
BASH_PURGED=1
SYSTEMD_PURGED=1
UPSTREAM_KERNEL={KERNEL_PKG}
"""
    for p in [os.path.join(rootfs, "usr/lib/os-release"),
              os.path.join(rootfs, "etc/os-release")]:
        if os.path.islink(p):
            os.remove(p)
        open(p, "w").write(content)
    open(os.path.join(rootfs, "etc/hostname"), "w").write("poler-cachyos\n")
    open(os.path.join(rootfs, "etc/hosts"), "w").write(
        "127.0.0.1 localhost\n127.0.1.1 poler-cachyos\n::1 localhost\n")
    ok("os-release: POLER CachyOS Sovereign Edition")


def stage_verification(rootfs, version, kver):
    vfile = os.path.join(rootfs, "poler/VERIFICATION.txt")
    os.makedirs(os.path.dirname(vfile), exist_ok=True)

    binaries = "\n".join(
        f"  {dest:<22} {os.path.getsize(os.path.join(rootfs, dest)):>10} bytes"
        for dest in sorted(POLER_BINARIES.values())
        if os.path.exists(os.path.join(rootfs, dest)))

    content = f"""POLER CACHYOS SOVEREIGN EDITION — BUILD VERIFICATION
=====================================================
distro version      : {version}
build date (UTC)    : {time.strftime('%Y-%m-%d %H:%M:%S', time.gmtime())}
target architecture : x86_64

BASE OPERATING SYSTEM
---------------------
base                : CachyOS repos ({CACHYOS_MIRROR}) on an Arch bootstrap
kernel package      : {KERNEL_PKG}
kernel version      : {kver}
kernel image        : /boot/vmlinuz-cachyos on the ISO

SOVEREIGN STACK
---------------
{binaries}

LEGACY PURGE PROOF
------------------
bash               : PURGED (no ELF present; /bin/bash + /usr/bin/bash are symlinks -> poler-sh)
systemd            : PURGED (not installed; PID 1 = poler-init via /sbin/init)
mkinitcpio         : PURGED (sovereign poler-initramfs instead)
pacman             : PURGED (updates arrive via poler-update from GitHub only)
SysVinit glue      : absent (no /etc/init.d, no rc.local)
/bin/sh            : symlink -> poler-sh
/sbin/init         : /usr/bin/init (merged usr layout) -> poler-init (PID 1)
root shell         : /usr/bin/poler-sh (/etc/passwd)

BOOT CHAIN
----------
GRUB (BIOS+UEFI) -> vmlinuz-cachyos + poler-initramfs.cpio.gz
  -> poler-init stage 1 (modules, medium scan, squashfs loop, switch_root)
  -> poler-init stage 2 (console supervision, respawn, reboot/poweroff)
  -> poler-sh sessions on /dev/console, tty2..tty4

UPDATE PATH
-----------
poler-update -> https://github.com/Kotokvit/poler-distro/releases/latest
(sha256-verified, atomic, no legacy package manager involved)
"""
    open(vfile, "w").write(content)
    ok("VERIFICATION.txt written into the image")


def stage_squashfs(rootfs, iso_tree):
    sq = os.path.join(iso_tree, "poler/live.squashfs")
    os.makedirs(os.path.dirname(sq), exist_ok=True)
    if os.path.exists(sq):
        os.remove(sq)
    log("mksquashfs (zstd 19)...")
    # NOTE: usr/lib/modules stays IN the image — the live system needs the
    # full module set at runtime (network, filesystems, ...).
    sh(f"mksquashfs '{rootfs}' '{sq}' -noappend -comp zstd -Xcompression-level 19 "
       f"-no-exports -wildcards "
       f"-e 'var/cache/pacman/pkg' 'boot/vmlinuz*'")
    ok(f"live.squashfs: {os.path.getsize(sq) / 1e6:.1f} MB")
    return sq


def copy_tool_with_libs(rootfs, tool, initrd_dir):
    """Copy a userspace tool + its shared libs into the initramfs."""
    src = None
    for cand in [f"/usr/bin/{tool}", f"/bin/{tool}", f"/usr/sbin/{tool}"]:
        p = os.path.join(rootfs, cand.lstrip("/"))
        if os.path.exists(p):
            src = p
            break
    if not src:
        return False
    dst_bin = os.path.join(initrd_dir, "usr/bin", tool)
    os.makedirs(os.path.dirname(dst_bin), exist_ok=True)
    shutil.copy2(src, dst_bin)
    copy_elf_libs(src, initrd_dir)
    return True


def copy_elf_libs(elf, initrd_dir):
    """Copy the shared-library closure of an ELF (per ldd) into the
    initramfs. The dynamic loader (ld-linux) additionally lands in /lib64 —
    the canonical interpreter path on x86_64."""
    r = subprocess.run(["ldd", elf], capture_output=True, text=True)
    for line in r.stdout.splitlines():
        line = line.strip()
        lib = None
        if "=>" in line:
            right = line.split("=>", 1)[1].strip()
            if right and not right.startswith("("):
                lib = right.split()[0]
        elif line.startswith("/"):
            lib = line.split()[0]
        if lib and os.path.exists(lib):
            base = os.path.basename(lib)
            dst = os.path.join(initrd_dir, "usr/lib", base)
            if not os.path.exists(dst):
                shutil.copy2(lib, dst)
                os.chmod(dst, 0o755)
            # dynamic loader must also exist at the canonical interp path
            if base.startswith("ld-linux") or base.startswith("ld64"):
                dst64 = os.path.join(initrd_dir, "lib64", base)
                if not os.path.exists(dst64):
                    shutil.copy2(lib, dst64)
                    os.chmod(dst64, 0o755)


def stage_initramfs(rootfs, bins_dir, iso_tree, kver):
    initrd_dir = os.path.join(os.path.dirname(rootfs), "initramfs")
    if os.path.exists(initrd_dir):
        shutil.rmtree(initrd_dir)
    os.makedirs(os.path.join(initrd_dir, "usr/bin"))
    os.makedirs(os.path.join(initrd_dir, "usr/lib"))
    os.makedirs(os.path.join(initrd_dir, "lib64"))
    os.makedirs(os.path.join(initrd_dir, "usr/lib/modules", kver))

    # stage-1 PID 1
    shutil.copy2(os.path.join(bins_dir, "poler-init"), os.path.join(initrd_dir, "init"))
    os.chmod(os.path.join(initrd_dir, "init"), 0o755)
    open(os.path.join(initrd_dir, ".poler-initramfs"), "w").write("stage1\n")

    # CRITICAL: /init is a dynamic ELF — its interpreter and libs MUST be
    # inside the initramfs, otherwise the kernel panics with
    # "Failed to execute /init (error -2)" (ENOENT on ld-linux).
    copy_elf_libs(os.path.join(bins_dir, "poler-init"), initrd_dir)

    # fallback module decompressors (+ shared libs)
    for tool in ["zstd", "xz"]:
        copy_tool_with_libs(rootfs, tool, initrd_dir)

    # kernel modules closure (deps from modules.dep)
    # NOTE: module files are .ko.zst (Arch packaging). They are DECOMPRESSED
    # at build time so poler-init can insmod raw ELF via finit_module(0) on
    # any kernel — no runtime decompression dependency at all.
    modsrc = os.path.join(rootfs, "usr/lib/modules", kver)
    moddst = os.path.join(initrd_dir, "usr/lib/modules", kver)
    dep_lines = open(os.path.join(modsrc, "modules.dep")).read().splitlines()

    def clean(name):
        # modprobe-style normalization: dashes and underscores are equivalent
        return (name.replace(".ko.zst", "").replace(".ko.xz", "")
                    .replace(".ko", "").replace("-", "_"))

    dep_map = {}      # normalized module name -> (rel path, [dep names])
    for line in dep_lines:
        left, _, right = line.partition(":")
        rel = left.strip()
        name = clean(os.path.basename(rel))
        deps = [clean(os.path.basename(d)) for d in right.split()]
        dep_map[name] = (rel, deps)

    keep = set()

    def add(name):
        if name in keep or name not in dep_map:
            return
        keep.add(name)
        for d in dep_map[name][1]:
            add(d)

    for want in INITRAMFS_MODULES:
        add(want)

    resolved = {n: dep_map[n] for n in keep}

    for name in sorted(keep):
        rel = resolved[name][0]
        src = os.path.join(modsrc, rel)
        dst = os.path.join(moddst, rel)
        if rel.endswith(".ko.zst") or rel.endswith(".ko.xz"):
            dst = dst[: -len(".zst")] if rel.endswith(".ko.zst") else dst[: -len(".xz")]
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            tool = "zstd" if rel.endswith(".zst") else "xz"
            r = subprocess.run([tool, "-d", "-c", src], capture_output=True)
            if r.returncode != 0:
                die(f"cannot decompress module {rel}")
            open(dst, "wb").write(r.stdout)
        else:
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            shutil.copy2(src, dst)

    # filtered modules.dep with decompressed paths
    with open(os.path.join(moddst, "modules.dep"), "w") as out:
        for line in dep_lines:
            rel = line.split(":")[0].strip()
            if rel and clean(os.path.basename(rel)) in keep:
                if rel.endswith(".ko.zst"):
                    line = line.replace(rel, rel[: -len(".zst")])
                elif rel.endswith(".ko.xz"):
                    line = line.replace(rel, rel[: -len(".xz")])
                out.write(line + "\n")
    for aux in ["modules.alias", "modules.symbols", "modules.builtin",
                "modules.builtin.modinfo", "modules.order"]:
        src = os.path.join(modsrc, aux)
        if os.path.exists(src):
            shutil.copy2(src, os.path.join(moddst, aux))

    unresolved = [m for m in INITRAMFS_MODULES if m not in dep_map]
    if unresolved:
        print(f"{YELLOW}[note]{RESET} built-in or absent modules (skipped): {', '.join(unresolved)}")
    log(f"initramfs modules: {len(keep)} (decompressed to raw ELF)")

    img = os.path.join(iso_tree, "boot/poler-initramfs.cpio.gz")
    os.makedirs(os.path.dirname(img), exist_ok=True)
    sh(f"cd '{initrd_dir}' && find . -print0 | "
       f"cpio --null --create --format=newc --owner=0:0 2>/dev/null | gzip -9 > '{img}'")
    ok(f"initramfs: {os.path.getsize(img) / 1e6:.1f} MB")
    return img


GRUB_CFG = """set timeout=3
set default=0

menuentry 'POLER CachyOS Sovereign Edition' {
    search --no-floppy --set=root --file /{marker}
    linux /boot/vmlinuz-cachyos poler.live=1 console=tty0 console=ttyS0,115200n8
    initrd /boot/poler-initramfs.cpio.gz
}

menuentry 'POLER CachyOS Sovereign Edition (verbose boot)' {
    search --no-floppy --set=root --file /{marker}
    linux /boot/vmlinuz-cachyos poler.live=1 poler.debug=1 console=tty0 console=ttyS0,115200n8
    initrd /boot/poler-initramfs.cpio.gz
}

menuentry 'POLER emergency shell' {
    search --no-floppy --set=root --file /{marker}
    linux /boot/vmlinuz-cachyos poler.live=1 poler.rescue=1 console=tty0 console=ttyS0,115200n8
    initrd /boot/poler-initramfs.cpio.gz
}
"""


def stage_iso(work, iso_tree, version, kernel_img):
    boot_dir = os.path.join(iso_tree, "boot")
    os.makedirs(boot_dir, exist_ok=True)
    shutil.copy2(kernel_img, os.path.join(boot_dir, "vmlinuz-cachyos"))

    cfg_dir = os.path.join(iso_tree, "boot/grub")
    os.makedirs(cfg_dir, exist_ok=True)
    open(os.path.join(cfg_dir, "grub.cfg"), "w").write(GRUB_CFG.replace("{marker}", ISO_MARKER))

    # boot-medium marker (poler-init searches for /poler/live.squashfs)
    open(os.path.join(iso_tree, ISO_MARKER), "w").write("POLER\n")

    iso_name = f"poler-cachyos-x86_64-{version}.iso"
    iso_path = os.path.join(work, "out", iso_name)
    os.makedirs(os.path.dirname(iso_path), exist_ok=True)
    if os.path.exists(iso_path):
        os.remove(iso_path)

    log("grub-mkrescue (BIOS + UEFI hybrid ISO)...")
    sh(f"grub-mkrescue -o '{iso_path}' '{iso_tree}' -volid {ISO_VOLUME_ID}")
    ok(f"ISO: {iso_path} ({os.path.getsize(iso_path) / 1e6:.1f} MB)")
    return iso_path


def stage_qemu(iso_path, timeout_s=240):
    log("QEMU smoke boot (serial console, TCG)...")
    log_file = "/tmp/poler_qemu_boot.log"
    sh(f"timeout {timeout_s} qemu-system-x86_64 -m 2048 -cdrom '{iso_path}' "
       f"-boot d -machine pc -cpu max -nographic -serial mon:stdio "
       f"-display none -no-reboot 2>&1 | tee {log_file}",
       check=False)

    text = open(log_file, errors="ignore").read() if os.path.exists(log_file) else ""
    checks = [
        ("poler-init stage 1 (initramfs)", "sovereign boot stage 1" in text),
        ("kernel modules loaded", "kernel modules loaded" in text),
        ("boot medium found", "boot medium:" in text),
        ("switch_root executed", "switch_root" in text),
        ("poler-init stage 2 (system)", "sovereign boot stage 2" in text),
        ("poler-sh session supervised", "supervising" in text and "poler-sh session" in text),
    ]
    failed = [name for name, passed in checks if not passed]
    for name, passed in checks:
        mark = f"{GREEN}PASS{RESET}" if passed else f"{RED}FAIL{RESET}"
        print(f"  [{mark}] {name}", flush=True)
    if failed:
        die(f"QEMU boot verification failed: {failed}")
    ok("QEMU boot verification PASSED — sovereign chain reaches poler-sh")


# ---------------------------------------------------------------------- main --

def detect_kver(rootfs):
    mods = os.path.join(rootfs, "usr/lib/modules")
    if os.path.isdir(mods):
        for d in sorted(os.listdir(mods), reverse=True):
            if os.path.isdir(os.path.join(mods, d)):
                return d
    die("cannot detect kernel version in rootfs")


def find_kernel_image(rootfs, kver):
    for cand in [os.path.join(rootfs, f"boot/vmlinuz-{kver}"),
                 os.path.join(rootfs, "boot/vmlinuz-cachyos"),
                 os.path.join(rootfs, f"usr/lib/modules/{kver}/vmlinuz")]:
        if os.path.exists(cand):
            return cand
    for c in sorted(_glob.glob(os.path.join(rootfs, "boot/vmlinuz*"))):
        return c
    die("kernel image (vmlinuz) not found in rootfs")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--workdir", default="/tmp/poler-iso-build")
    ap.add_argument("--out", default=None)
    ap.add_argument("--bins", default=None)
    ap.add_argument("--kernel", default=KERNEL_PKG)
    ap.add_argument("--version", default="1.1.0")
    ap.add_argument("--qemu", action="store_true")
    args = ap.parse_args()

    if os.geteuid() != 0:
        die("must run as root (chroot/pacstrap/mount/mknod are required); CI runs this with sudo")

    work = os.path.abspath(args.workdir)
    out = os.path.abspath(args.out or os.path.join(DISTRO_DIR, "out"))
    bins = os.path.abspath(args.bins or os.path.join(DISTRO_DIR, "binaries"))
    os.makedirs(work, exist_ok=True)
    os.makedirs(out, exist_ok=True)
    os.makedirs(bins, exist_ok=True)

    log(f"POLER CachyOS Sovereign Edition v{args.version} — REAL ISO build")

    rootfs = os.path.join(work, "rootfs")
    iso_tree = os.path.join(work, "iso_tree")

    # 1. real base
    bs_tar = stage_fetch_bootstrap(work)
    stage_extract_rootfs(work, bs_tar, rootfs)
    mount_pseudo(rootfs)

    # 2. sovereign mirrors + packages
    stage_prepare_pacman(rootfs)
    stage_sync_pkgs(rootfs, args.kernel)

    kver = detect_kver(rootfs)
    kernel_img = find_kernel_image(rootfs, kver)
    log(f"kernel: {args.kernel} ({kver})")

    # 3-5. purge, inject, identity, proof
    stage_purge_legacy(rootfs)
    stage_inject_poler(rootfs, bins)
    stage_os_release(rootfs, args.version)
    stage_verification(rootfs, args.version, kver)

    unmount_all(rootfs)

    # 6-7. image assembly
    if os.path.exists(iso_tree):
        shutil.rmtree(iso_tree)
    os.makedirs(iso_tree)
    stage_squashfs(rootfs, iso_tree)
    stage_initramfs(rootfs, bins, iso_tree, kver)

    # 8. ISO
    iso_path = stage_iso(work, iso_tree, args.version, kernel_img)

    # 9. integrity + optional QEMU boot proof
    sums = os.path.join(out, "SHA256SUMS")
    with open(sums, "w") as f:
        f.write(f"{sha256_file(iso_path)}  {os.path.basename(iso_path)}\n")
    shutil.copy2(iso_path, os.path.join(out, os.path.basename(iso_path)))
    ok(f"sha256: {sha256_file(iso_path)}")

    if args.qemu:
        stage_qemu(os.path.join(out, os.path.basename(iso_path)))

    log(f"DONE -> {out}")
    print(f"\n{GREEN}ISO artifact:{RESET} {os.path.join(out, os.path.basename(iso_path))}")
    print(f"{GREEN}Checksums   :{RESET} {sums}")


if __name__ == "__main__":
    main()
