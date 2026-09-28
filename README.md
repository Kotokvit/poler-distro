# POLER CachyOS Sovereign Edition

**Суверенный live-дистрибутив на базе CachyOS x86_64: ядро linux-cachyos, PID 1 — poler-init, консольная оболочка — poler-engine Terminal Gateway (официальный бинарник v0.61.0 из poler-engine-org/poler-engine), резервная/аварийная — poler-sh. Bash, systemd, mkinitcpio и pacman полностью вырезаны.**

Это настоящий загрузочный ISO (гибрид BIOS+UEFI), собираемый CI из реальных репозиториев CachyOS и POLER — без муляжей и заглушек.

> **v1.3.0 — реальное железо.** v1.2.0 запускалась на виртуалках, но на
> физическом ПК через Ventoy уходила в тёмный цикл: сканер не умел видеть
> ISO-файл на флешке Ventoy, аварийного шелла в initramfs физически не было,
> а консоль висела на serial-порту. v1.3.0 чинит всё: Ventoy-скан (ISO как
> файл → loop-mount), poler-sh внутри initramfs (живая аварийная консоль с
> `exit 44` rescan), tty1-first консоли для монитора + клавиатуры, NVMe/
> USB3/USB-клавиатуры/exFAT/NTFS модули, linux-firmware. Каждый релиз
> доказывается в QEMU ДВАЖДЫ: классическая загрузка И симуляция Ventoy.

---

## ⚡ Состав

| Компонент | Роль | Источник |
|-----------|------|----------|
| `linux-cachyos` | ядро x86_64 | зеркала CachyOS (`mirror.cachyos.org`) |
| `poler-init` v0.4.0 | PID 1: initramfs-стадия (модули реального железа: NVMe/USB3/USB-клавиатуры; прямой носитель ИЛИ ISO-файл Ventoy → loop-mount) + системная (супервизия сессий poler-engine Terminal Gateway с фолбэком на poler-sh, tmpfs, marker-управление питанием, rescue-режим) | `sovereign/poler-init` этого репо |
| `poler-engine` v0.61.0 | **главная консольная оболочка**: Terminal Gateway (`poler-engine --gateway`, cwd=`/`) — двойной контур исполнения (engine-native + host proxy), пайпы, редиректы, native retrieval. Официальный релизный бинарник, sha256-пinned | [poler-engine-org/poler-engine](https://github.com/poler-engine-org/poler-engine) |
| `poler-sh` v0.2.0 | аварийная/совместимая оболочка: AST-calc, физический конвертер `=`, hw-аудит, JSON-шлюз; login-shell root в `/etc/passwd`; принудительно — через `poler.rescue=1`; **внутри initramfs — живая аварийная консоль** (`exit 44` = повторить загрузку/rescan USB) | [Kotokvit/poler-sh](https://github.com/Kotokvit/poler-sh) |
| `poler-powerctl` | суверенные `reboot`/`poweroff`/`halt`: marker-IPC к PID 1 — работает из любого контура, включая песочницу движка | `sovereign/poler-init` (bin) |
| `poler` | архиватор .poler: FastCDC + BLAKE3 | [Kotokvit/poler](https://github.com/Kotokvit/poler) |
| `poler-box` | песочница: userns + seccomp | [Kotokvit/poler](https://github.com/Kotokvit/poler) |
| `poler-fuse` | монтирование .poler без распаковки | [Kotokvit/poler](https://github.com/Kotokvit/poler) |
| `poler-update` v1.2.0 | обновление напрямую с GitHub-зеркала: curl, sha256, атомарная установка; суверенный канал движка `--install engine` | `sovereign/poler-update` этого репо |

Опциональные модули (в разработке): `poler-mesh`, `poler-git`, `poler-edit`.

## 🚫 Что вырезано

* **bash** — ELF отсутствует; `/bin/sh`, `/bin/bash`, `/usr/bin/bash` → симлинки на `poler-sh`
* **systemd** — не установлен; PID 1 = `poler-init` (через `/sbin/init`)
* **mkinitcpio** — вместо него суверенный `poler-initramfs.cpio.gz`
* **pacman** — обновления приходят только с GitHub через `poler-update`
* **SysVinit-клей** — `/etc/init.d` отсутствует

Доказательство каждого пункта печатается в `/poler/VERIFICATION.txt` внутри образа.

## 🛟 Спасение (rescue)

* GRUB-меню содержит запись **POLER Sovereign Rescue** (kernel cmdline `poler.rescue=1`): движок обходится целиком, консоль — чистый `poler-sh`.
* poler-init всегда держит фолбэк: если `poler-engine` не смог запуститься, сессия автоматически падает на `poler-sh`.
* Полная инструкция по разблокировке системы: [docs/RECOVERY.md](docs/RECOVERY.md).

## 🔄 Обновление и установка движка

```
poler-update                    # состояние зеркала vs системы
poler-update --apply            # скачать, проверить sha256, атомарно установить
poler-update --install engine   # официальный poler-engine v0.61.0 (sha256-pinned)
                                # на ЛЮБУЮ систему — Terminal Gateway как оболочка
```

Обновления идут строго с `https://github.com/Kotokvit/poler-distro/releases/latest`; движок — только с официального релиза `poler-engine-org/poler-engine` (никогда не пересобирается из исходников).

## 🛠 Сборка ISO

Полный конвейер (требует root — запускается в CI):

```
sudo python3 builder/build_iso.py --version 1.3.0 --qemu
```

Что делает конвейер:

1. Скачивает настоящий Arch bootstrap rootfs;
2. Настраивает суверенные зеркала CachyOS (cachyos-core/extra + cachyos-keyring);
3. Синхронизирует пакеты (ядро `linux-cachyos`, `linux-firmware` для реального железа, core-юзерленд, curl, glibc-базу для движка);
4. Вырезает bash/systemd/mkinitcpio/pacman (и GPU-блобы прошивок — мёртвый груз без графического стека), ставит симлинки на POLER;
5. Скачивает официальный `poler-engine` v0.61.0 с sha256-верификацией и проверяет его glibc-зависимости в rootfs;
6. Инъектирует суверенный стек (собранный из исходников);
7. Пишет `os-release` и `VERIFICATION.txt`;
8. `mksquashfs` → `poler/live.squashfs`;
9. Собирает `poler-initramfs.cpio.gz` (poler-init + poler-sh аварийный шелл + модули реального железа: NVMe/USB3/клавиатуры/exFAT/NTFS/virtio);
10. `grub-mkrescue` → гибридный BIOS+UEFI ISO (+ rescue-запись `poler.rescue=1` + ISO-file-scan запись `poler.findiso=1`);
11. Загружает ISO в QEMU и **доказывает** цепочку ДВАЖДЫ: классическая загрузка (poler-init stage 1 → stage 2 → poler-engine Terminal Gateway) И симуляция Ventoy (ISO как файл на vfat-диске → iso-scan → loop-mount → Terminal Gateway).

## 📦 Артефакты релиза

* `poler-cachyos-x86_64-<version>.iso` — загрузочный образ
* `poler-core-x86_64.tar.gz` — суверенный стек для `poler-update --apply` (движок не включён: он ставится своим каналом `--install engine`)
* `manifest.json` + `SHA256SUMS`

## 🖥 Загрузка на реальном железе (v1.3.0)

Поддерживаются все три способа:

* **Ventoy** — просто положите `.iso` файл на флешку (exFAT/NTFS/vfat/FAT32).
  poler-init сам найдёт ISO-файл, loop-примонтирует его и загрузит squashfs
  (GRUB-запись «ISO-file scan» / `poler.findiso=1` принудительно).
* **dd / Rufus (режим DD) / OptiDrive** — посекторная запись: прямое
  монтирование носителя.
* **CD/DVD** — классический iso9660.

Консоли: интерактивный poler-engine Terminal Gateway поднимается на
**`/dev/tty1` — монитор + клавиатура** (это главный терминал на реальном
железе), плюс serial (`/dev/console`) и `tty2`/`tty3` (Alt+F2/F3).

Если носитель не найден — вы НЕ в тёмном цикле: на мониторе появится
красный баннер POLER EMERGENCY MODE и интерактивный poler-sh прямо из
initramfs. `exit 44` — повторить загрузку (rescan USB), `exit 42`/`43` —
перезагрузка/выключение.

### Про размер: почему не 3 ГБ, как оригинальный CachyOS

Оригинальный образ CachyOS (~3 ГБ) = GPU-прошивки nvidia/amdgpu/i915
(~1,5–2 ГБ) + графический стек Mesa/KDE/Calamares (~1 ГБ) + база (~400 МБ).
В суверенной редакции нет X/Wayland/Mesa — текстовая консоль работает на
VGA/EFI-фреймбуфере БЕЗ GPU-прошивок, поэтому GPU-блобы вырезаны как мёртвый
груз. Всё, что нужно реальному железу для загрузки и работы, — внутри:
linux-firmware (WiFi/BT/сетевые/аудио), NVMe/USB3/USB-клавиатуры/exFAT/NTFS.
Итог ~1 ГБ — это осознанная инженерия, а не «порезанный» образ.

## 🖥 Загрузка ISO (виртуалки)

BIOS и UEFI: грузится с CD/USB (dd или Rufus/Ventoy). Ядро: `console=ttyS0` включён для serial-консоли (QEMU/CI).

Сессии poler-engine Terminal Gateway: `tty1` (PRIMARY, монитор+клавиатура), `/dev/console` (serial) и `tty2`/`tty3`; PID 1 перезапускает их при выходе (с фолбэком на poler-sh). Питание: `reboot`/`poweroff`/`halt` (poler-powerctl) работают изнутри гейтвея; escape-код `exit 42` → reboot, `exit 43` → poweroff.
