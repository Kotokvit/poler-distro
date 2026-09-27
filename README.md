# POLER CachyOS Sovereign Edition

**Суверенный дистрибутив на базе высокопроизводительного ядра CachyOS с полностью вырезанным Bash и legacy GNU-клеем**

---

## ⚡ Особенности

* **База:** Ядро Linux CachyOS (`linux-cachyos` x86_64) с оптимизациями под современное железо.
* **PID 1:** `poler-init` — мгновенный холодный старт (**0.535 мс**) без SysVinit и systemd shell-генераторов.
* **Суверенная оболочка:** `poler-sh` — нативный терминальный шлюз (отклик 3.3 мс, встроенный AST math `calc`, физический конвертер `=`, аппаратный аудит `hw`, Unicode grep без bash-обвязок).
* **Собственное зеркало обновлений:** Обновление всей системы и компонентов выполняется напрямую из этого GitHub-репозитория (`Kotokvit/poler-distro`).

---

## 🔄 Как обновиться

В терминале `poler-sh` достаточно вызвать:

```bash
poler-update
```

Или в одну строку:
```bash
curl -fsSL https://raw.githubusercontent.com/Kotokvit/poler-distro/main/tools/poler-update.py | python3
```

Апдейтер запрашивает манифест с вашего GitHub, проверяет контрольные суммы BLAKE3 и атомарно накатывает обновления.

---

## 🛠️ Сборка локального образа

```bash
python3 builder/build_distro.py
```

Результат:
* `out/poler-cachyos-initramfs.cpio.gz` — загрузочный образ initramfs.
* `out/poler-cachyos-base.poler` — суверенный контейнер полного rootfs.
