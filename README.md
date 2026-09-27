# POLER CachyOS Sovereign Edition

**Суверенный live-дистрибутив на базе CachyOS x86_64: ядро linux-cachyos, PID 1 — poler-init, оболочка — poler-sh. Bash, systemd, mkinitcpio и pacman полностью вырезаны.**

Это настоящий загрузочный ISO (гибрид BIOS+UEFI), собираемый CI из реальных репозиториев CachyOS и POLER — без муляжей и заглушек.

---

## ⚡ Состав

| Компонент | Роль | Источник |
|-----------|------|----------|
| `linux-cachyos` | ядро x86_64 | зеркала CachyOS (`mirror.cachyos.org`) |
| `poler-init` v0.2.0 | PID 1: initramfs-стадия (модули ядра, поиск носителя, squashfs+loop, switch_root) + системная (консоли poler-sh, respawn, reboot/poweroff) | `sovereign/poler-init` этого репо |
| `poler-sh` v0.2.0 | суверенная оболочка: AST-calc, физический конвертер `=`, hw-аудит, JSON-шлюз | [Kotokvit/poler-sh](https://github.com/Kotokvit/poler-sh) |
| `poler` | архиватор .poler: FastCDC + BLAKE3 | [Kotokvit/poler](https://github.com/Kotokvit/poler) |
| `poler-box` | песочница: userns + seccomp | [Kotokvit/poler](https://github.com/Kotokvit/poler) |
| `poler-fuse` | монтирование .poler без распаковки | [Kotokvit/poler](https://github.com/Kotokvit/poler) |
| `poler-update` v1.1.0 | обновление напрямую с GitHub-зеркала: curl, sha256, атомарная установка | `sovereign/poler-update` этого репо |

Опциональные модули (не в ISO, ставятся через `poler-update --apply`): `poler-engine`, `poler-mesh`, `poler-git`, `poler-edit`.

## 🚫 Что вырезано

* **bash** — ELF отсутствует; `/bin/sh`, `/bin/bash`, `/usr/bin/bash` → симлинки на `poler-sh`
* **systemd** — не установлен; PID 1 = `poler-init` (через `/sbin/init`)
* **mkinitcpio** — вместо него суверенный `poler-initramfs.cpio.gz`
* **pacman** — обновления приходят только с GitHub через `poler-update`
* **SysVinit-клей** — `/etc/init.d` отсутствует

Доказательство каждого пункта печатается в `/poler/VERIFICATION.txt` внутри образа.

## 🔄 Обновление

В терминале `poler-sh`:

```
poler-update            # состояние зеркала vs системы
poler-update --apply    # скачать, проверить sha256, атомарно установить
```

Обновления идут строго с `https://github.com/Kotokvit/poler-distro/releases/latest`.

## 🛠 Сборка ISO

Полный конвейер (требует root — запускается в CI):

```
sudo python3 builder/build_iso.py --version 1.1.0 --qemu
```

Что делает конвейер:

1. Скачивает настоящий Arch bootstrap rootfs;
2. Настраивает суверенные зеркала CachyOS (cachyos-core/extra + cachyos-keyring);
3. Синхронизирует пакеты (ядро `linux-cachyos`, core-юзерленд, curl);
4. Вырезает bash/systemd/mkinitcpio/pacman, ставит симлинки на POLER;
5. Инъектирует суверенный стек (собранный из исходников);
6. Пишет `os-release` и `VERIFICATION.txt`;
7. `mksquashfs` → `poler/live.squashfs`;
8. Собирает `poler-initramfs.cpio.gz` (poler-init + модули ядра для доступа к носителю);
9. `grub-mkrescue` → гибридный BIOS+UEFI ISO;
10. Загружает ISO в QEMU и **доказывает** цепочку: poler-init stage 1 → stage 2 → poler-sh.

## 📦 Артефакты релиза

* `poler-cachyos-x86_64-<version>.iso` — загрузочный образ
* `poler-core-x86_64.tar.gz` — суверенный стек для `poler-update --apply`
* `manifest.json` + `SHA256SUMS`

## 🖥 Загрузка ISO

BIOS и UEFI: грузится с CD/USB (dd или Rufus/Ventoy). Ядро: `console=ttyS0` уже включён для serial-консоли.

Сессии `poler-sh` поднимаются на `/dev/console` и `tty2..tty4`; PID 1 (`poler-init`) перезапускает их при выходе. `exit 42` → reboot, `exit 43` → poweroff.
