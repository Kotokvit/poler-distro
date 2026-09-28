# POLER RECOVERY — возвращение контроля над системой

Пошаговые пути восстановления для сценариев, встречавшихся на практике:
заблокированная консоль после смены оболочки, emergency mode без `/bin/sh`,
повреждённая GPT, отказ SATA-кабеля, тёмный экран / зацикливание при
загрузке live-ISO с Ventoy на реальном железе.

## Сценарий 0: live-ISO «зациклился» / тёмный экран на реальном ПК (Ventoy)

Симптомы (реальный случай на v1.1.0/v1.2.0): после GRUB экран гаснет или
вечно что-то сканируется; на мониторе ничего интерактивного.

Что было на самом деле — ТРИ наложенные причины:
1. **Ventoy ≠ dd.** Ventoy держит ISO как ФАЙЛ на exFAT-флешке; сканер
   v1.2.0 монтировал только разделы и искал `/poler/live.squashfs` в корне —
   файла там нет → «boot medium not found».
2. **Аварийного шелла в initramfs не было.** emergency_shell звал
   `/usr/bin/poler-sh`, которого в initramfs физически нет → тёмный цикл.
3. **/dev/console = serial.** cmdline `console=tty0 console=ttyS0` делает
   `/dev/console` serial-портом; интерактивные сессии ждали ввода на COM-порту,
   а монитор получал только kernel log.

Лечение — всё это исправлено в **v1.3.0** (ISO-file скан, poler-sh в
initramfs, tty1-first). Действия на старых версиях:
* перезапишите флешку через `dd`/Rufus-DD — прямой носитель работает и на
  v1.2.0;
* либо в GRUB добавьте к строке ядра `poler.findiso=1` (если ядро новее);
* либо подключите serial-кабель/включите serial-порт VM — там живёт шелл.

## Сценарий 1: «удалил Bash — после перезагрузки не могу войти»

Симптомы: getty/sulogin не стартуют («root account locked», «/bin/sh: No such
file or directory»), emergency mode недоступен, TTY пустые.

Причина: login-shell из `/etc/passwd` указывает на несуществующий/битый
интерпретатор. До перезагрузки работало, потому что старый shell уже был в
памяти.

### Путь А — live-ISO POLER (рекомендуется)

1. Загрузите `poler-cachyos-x86_64-<version>.iso` (BIOS или UEFI, USB/CD).
2. На консоли — poler-engine Terminal Gateway; если движок не поднялся, poler-init
   сам подаст poler-sh (или выберите в GRUB «POLER Sovereign Rescue»).
3. Смонтируйте корень установленной системы:
   ```
   mount /dev/sdXN /mnt        # корневой раздел установленной системы
   ```
4. Почините login-shell (фолбэк!):
   ```
   # в /mnt/etc/passwd: root:...:/usr/bin/poler-sh   (или /bin/bash, если вернули)
   ```
5. Проверьте, что целевой бинарник существует и исполняется в chroot:
   ```
   chroot /mnt /usr/bin/poler-sh -c "pwd; echo alive"
   ```
6. `umount /mnt`, перезагрузка.

### Путь B — любой Linux live-USB (Arch/CachyOS/что угодно)

1. `mount /dev/sdXN /mnt`
2. `arch-chroot /mnt` (или `chroot`)
3. Восстановите shell root и/или установите Bash: `pacman -S bash` (если pacman жив)
   либо просто скопируйте бинарник shell с live-системы.
4. Выйдите, размонтируйте, перезагрузитесь.
5. Урок: дублируйте shell, а не удаляйте (см. [SAFETY.md](SAFETY.md)).

### Путь C — нет live-USB, но есть GRUB

1. В GRUB нажмите `e` на записи загрузки.
2. Добавьте к строке ядра: `init=/usr/bin/poler-sh` (или `init=/bin/bash`,
   если binary вернули).
3. `Ctrl+X` — загрузка сразу в shell как PID 1.
4. Почините `/etc/passwd`, проверьте бинарник, перезагрузитесь.

## Сценарий 2: emergency mode / root account locked

`sulogin` требует `/bin/sh`; если его нет — «root account locked».
Пути А–C выше подходят; ключ — вернуть существующий shell для root.
Политики `passwd -u root` не помогут, пока интерпретатор отсутствует.

## Сценарий 3: повреждена primary GPT (из реального случая)

Симптомы: ядро не видит разделы, BIOS не находит загрузчик, `fdisk -l`
пустой или ругается на таблицу.

1. Загрузитесь с live-ISO.
2. **Сначала образ диска, потом инструменты**:
   ```
   poler a /mnt/backup/sda.poler /dev/sda      # или dd of=...bs=4M status=progress
   ```
3. Восстановление таблицы:
   ```
   gdisk /dev/sdX        # r → b (rebuild backup GPT), либо двойная реконструкция
   testdisk              # интерактивный поиск потерянных разделов
   ```
4. Если загрузчик не находится — переустановите его в chroot:
   ```
   arch-chroot /mnt; grub-install /dev/sdX; grub-mkconfig -o /boot/grub/grub.cfg
   ```
5. При `SCSI parity error` / случайных I/O ошибках — **сначала замените
   SATA-кабель** (в реальном инциденте именно он был источником порчи GPT;
   таблица чинилась, кабель портил снова).

## Сценарий 4: poler-engine Gateway не поднимается на консолях

1. GRUB → «POLER Sovereign Rescue» (`poler.rescue=1`): консоль = poler-sh,
   движок полностью обойдён.
2. В rescue-сессии обновите движок с официального канала:
   ```
   poler-update --install engine
   ```
3. Проверьте бинарник вручную: `poler-engine --shell` (REPL), затем
   `poler-engine --gateway` (полный контур).
4. Перезагрузка — poler-init снова попробует движок, при неудаче сам
   уйдёт на poler-sh (лог: «supervising N terminal session(s)»).

## Сценарий 5: система на read-only squashfs, «нечего записать»

Это норма live-ISO: корень read-only. poler-init монтирует tmpfs на
`/run`, `/tmp`, `/var/tmp`, `/root/.cache`. Если нужна запись в «корень» —
используйте эти каталоги либо монтируйте реальный диск:
```
mount /dev/sdXN /run/mnt
```

## Золотое правило

> Прежде чем менять то, что запускается при загрузке, запишите на бумаге
> путь обратно. Если путь обратно не помещается на бумагу — вы ещё не готовы
> к изменению. (SAFETY.md → «Чек-лист перед изменением загрузочной цепочки»)
