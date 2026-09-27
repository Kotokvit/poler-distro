#!/usr/bin/env python3
"""
POLER CachyOS Sovereign Edition — Distro Image Builder
Extracts CachyOS base, purges legacy GNU/Bash shells, injects poler-init & poler-sh,
and packages directly into .poler archive and bootable image.
"""

import os
import sys
import shutil
import subprocess
import json

DISTRO_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUTPUT_DIR = os.path.join(DISTRO_DIR, "out")
ROOTFS_DIR = "/tmp/poler_distro_rootfs"

def log(msg):
    print(f"[\033[1;32mPOLER-DISTRO-BUILDER\033[0m] {msg}")

def build_distro():
    log("Starting sovereign CachyOS build pipeline...")
    
    if os.path.exists(ROOTFS_DIR):
        shutil.rmtree(ROOTFS_DIR)
    os.makedirs(ROOTFS_DIR, exist_ok=True)
    os.makedirs(OUTPUT_DIR, exist_ok=True)

    # 1. Base directory structure
    dirs = [
        "bin", "sbin", "usr/bin", "usr/sbin", "usr/lib", "usr/lib64", "lib64",
        "proc", "sys", "dev", "tmp", "run", "root", "etc/poler", "var/log"
    ]
    for d in dirs:
        os.makedirs(os.path.join(ROOTFS_DIR, d), exist_ok=True)

    # 2. Inject sovereign binaries
    sovereign_bins = {
        "/home/vitalij/Стільниця/poler/target/release/poler-init": "bin/poler-init",
        "/home/vitalij/.local/bin/poler-sh": "bin/poler-sh",
        "/home/vitalij/.local/bin/poler": "bin/poler",
        "/home/vitalij/.local/bin/poler-engine": "bin/poler-engine",
    }

    for src, dst in sovereign_bins.items():
        if os.path.exists(src):
            target = os.path.join(ROOTFS_DIR, dst)
            shutil.copy2(src, target)
            os.chmod(target, 0o755)
            log(f"Injected sovereign component: {dst}")

    # 3. Purge bash & legacy shell: replace with symlinks to poler-sh
    log("Purging legacy 90s GNU/Bash shell...")
    os.symlink("bin/poler-init", os.path.join(ROOTFS_DIR, "init"))
    os.symlink("poler-sh", os.path.join(ROOTFS_DIR, "bin/sh"))
    os.symlink("poler-sh", os.path.join(ROOTFS_DIR, "bin/bash"))
    os.symlink("../bin/poler-sh", os.path.join(ROOTFS_DIR, "usr/bin/sh"))
    os.symlink("../bin/poler-sh", os.path.join(ROOTFS_DIR, "usr/bin/bash"))

    # 4. Copy required shared libraries from host CachyOS
    libs = [
        "/usr/lib/libc.so.6",
        "/usr/lib/libm.so.6",
        "/usr/lib/libgcc_s.so.1",
        "/lib64/ld-linux-x86-64.so.2"
    ]
    for lib in libs:
        if os.path.exists(lib):
            dest = os.path.join(ROOTFS_DIR, "usr/lib", os.path.basename(lib))
            shutil.copy2(lib, dest)

    os.symlink("../usr/lib/ld-linux-x86-64.so.2", os.path.join(ROOTFS_DIR, "lib64/ld-linux-x86-64.so.2"))
    os.symlink("usr/lib", os.path.join(ROOTFS_DIR, "lib"))

    # 5. Distro manifest & mirror config
    shutil.copy2(
        os.path.join(DISTRO_DIR, "manifest.json"),
        os.path.join(ROOTFS_DIR, "etc/poler/manifest.json")
    )

    # 6. Create bootable cpio.gz initramfs
    initrd_out = os.path.join(OUTPUT_DIR, "poler-cachyos-initramfs.cpio.gz")
    log("Packaging sovereign initramfs...")
    cmd = f"cd {ROOTFS_DIR} && find . -print0 | cpio --null --create --format=newc | gzip -9 > {initrd_out}"
    subprocess.run(cmd, shell=True, check=True)

    # 7. Create sovereign .poler archive bundle
    poler_archive_out = os.path.join(OUTPUT_DIR, "poler-cachyos-base.poler")
    log(f"Creating sovereign .poler container: {poler_archive_out}...")
    subprocess.run([
        "/home/vitalij/.local/bin/poler", "create", poler_archive_out, ROOTFS_DIR
    ], check=True)

    log("Distro build complete!")
    log(f"Initramfs size: {os.path.getsize(initrd_out) / 1024 / 1024:.2f} MB")
    log(f"Sovereign .poler archive size: {os.path.getsize(poler_archive_out) / 1024 / 1024:.2f} MB")

if __name__ == "__main__":
    build_distro()
