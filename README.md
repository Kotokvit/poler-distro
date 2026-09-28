# POLER CachyOS Sovereign Edition

**Суверенный live-дистрибутив на базе CachyOS x86_64: ядро linux-cachyos, PID 1 — poler-init, консольная оболочка — poler-engine Terminal Gateway (официальный бинарник v0.61.0 из poler-engine-org/poler-engine), резервная/аварийная — poler-sh. Bash, systemd, mkinitcpio и pacman полностью вырезаны.**

Это настоящий загрузочный ISO (гибрид BIOS+UEFI), собираемый CI из реальных репозиториев CachyOS и POLER — без муляжей и заглушек.

> **v1.2.0 — инженерный урок инцидента с блокировкой системы.**
> В v1.1.0 оболочкой консолей был standalone poler-sh 0.2.0. Теперь на консолях —
> **настоящий терминал из poler-engine** (Terminal Gateway v0.22.0: двойной контур
> исполнения, пайпы, редиректы, native retrieval — то, что реально работает),
> poler-sh остаётся аварийным фолбэком, а `poler.rescue=1` гарантирует консоль
> даже если движок не стартует. Подробности: [docs/SAFETY.md](docs/SAFETY.md).

---

## ⚡ Состав

| Компонент | Роль | Источник |
|-----------|------|----------|
| `linux-cachyos` | ядро x86_64 | зеркала CachyOS (`mirror.cachyos.org`) |
| `poler-init` v0.3.0 | PID 1: initramfs-стадия (модули ядра, поиск носителя, squashfs+loop, switch_root) + системная (супервизия сессий poler-engine Terminal Gateway с фолбэком на poler-sh, tmpfs для записываемых областей, marker-управление питанием, rescue-режим) | `sovereign/poler-init` этого репо |
| `poler-engine` v0.61.0 | **главная консольная оболочка**: Terminal Gateway (`poler-engine --gateway`, cwd=`/`) — двойной контур исполнения (engine-native + host proxy), пайпы, редиректы, native retrieval. Официальный релизный бинарник, sha256-пinned | [poler-engine-org/poler-engine](https://github.com/poler-engine-org/poler-engine) |
| `poler-sh` v0.2.0 | аварийная/совместимая оболочка: AST-calc, физический конвертер `=`, hw-аудит, JSON-шлюз; login-shell root в `/etc/passwd`; принудительно включается через `poler.rescue=1` | [Kotokvit/poler-sh](https://github.com/Kotokvit/poler-sh) |
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
sudo python3 builder/build_iso.py --version 1.2.0 --qemu
```

Что делает конвейер:

1. Скачивает настоящий Arch bootstrap rootfs;
2. Настраивает суверенные зеркала CachyOS (cachyos-core/extra + cachyos-keyring);
3. Синхронизирует пакеты (ядро `linux-cachyos`, core-юзерленд, curl, glibc-базу для движка);
4. Вырезает bash/systemd/mkinitcpio/pacman, ставит симлинки на POLER;
5. Скачивает официальный `poler-engine` v0.61.0 с sha256-верификацией и проверяет его glibc-зависимости в rootfs;
6. Инъектирует суверенный стек (собранный из исходников);
7. Пишет `os-release` и `VERIFICATION.txt`;
8. `mksquashfs` → `poler/live.squashfs`;
9. Собирает `poler-initramfs.cpio.gz` (poler-init + модули ядра для доступа к носителю);
10. `grub-mkrescue` → гибридный BIOS+UEFI ISO (+ rescue-запись `poler.rescue=1`);
11. Загружает ISO в QEMU и **доказывает** цепочку: poler-init stage 1 → stage 2 → poler-engine Terminal Gateway.

## 📦 Артефакты релиза

* `poler-cachyos-x86_64-<version>.iso` — загрузочный образ
* `poler-core-x86_64.tar.gz` — суверенный стек для `poler-update --apply` (движок не включён: он ставится своим каналом `--install engine`)
* `manifest.json` + `SHA256SUMS`

## 🖥 Загрузка ISO

BIOS и UEFI: грузится с CD/USB (dd или Rufus/Ventoy). Ядро: `console=ttyS0` уже включён для serial-консоли.

Сессии poler-engine Terminal Gateway поднимаются на `/dev/console` и `tty2..tty4`; PID 1 (`poler-init`) перезапускает их при выходе (с фолбэком на poler-sh). Питание: `reboot`/`poweroff`/`halt` (poler-powerctl) работают изнутри гейтвея; escape-код `exit 42` → reboot, `exit 43` → poweroff.
