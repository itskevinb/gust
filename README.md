# gust

A small fan controller for Apple Silicon Macs, written in Rust.

You draw a fan curve, and a tiny root daemon applies it, with fail-safes that hand the fans back to macOS if anything goes wrong. A menu bar item shows temperatures and fan speed and opens a visual curve editor.

- **`gustd`:** the daemon. It's the only part that writes to the SMC (the chip that runs the fans), and only to the fan keys.
- **`gust-bar`:** the menu bar readout and curve editor. Read-only: it never touches the fans.
- **`gust`:** a CLI for reading fans and sensors and dry-running a curve.

No Electron, no Qt. The menu bar app uses about 1% CPU. Apart from the menu bar app's UI crates, it's plain IOKit calls.

> **Use at your own risk.** gust takes the fans away from macOS's own thermal control. It has fail-safes (below), but a fan controller that goes wrong can let a Mac run hot. Linux's own Apple SMC driver marks manual fan control unsafe for the same reason. gust has been tested on one machine: a 16" MacBook Pro with M1 Max. Other Apple Silicon Macs should work, but they're untested. Intel Macs are refused.

## Install

You need an Apple Silicon Mac and Rust (`curl https://sh.rustup.rs -sSf | sh`).

```sh
git clone https://github.com/itskevinb/gust.git
cd gust
./install.sh
```

The script:
- builds everything
- writes a starter curve to `~/.config/gust/curve.conf` (an existing one is kept)
- installs `gustd` as a launchd daemon; this step asks for your password
- installs the menu bar app as a login item

Quit any other fan app first: Macs Fan Control, TG Pro, or Stats' fan control. Two fan controllers fight over the fans.

To update, `git pull` and run `./install.sh` again. To remove everything, run `./uninstall.sh`; the fans go straight back to macOS.

## The curve

`~/.config/gust/curve.conf` is a plain text file. Edit it by hand or with **Curve editor…** in the menu. gustd reloads it within 2 seconds of a save. A broken edit is refused: gustd keeps running the last good curve, and the menu shows why it refused the edit.

```
sensors = Tp0* TC1* TCDX Tg* TC2* TC3*   # hottest of these drives the fans
point 50 auto                             # below/at 50°C: macOS decides
point 60 0%                               # fan minimum
point 95 max                              # full speed
shape = linear                            # linear | log | exp
average = 15                              # follow the 15 s rolling average (0 = off)
ramp_up = 100                             # RPM per second, at most
ramp_down = 50
```

- **Points** are `point <°C> <speed>`. The speed is `auto`, `max`, an RPM, or a percent of each fan's own min..max range. Speeds are always clamped to what the fan supports.
- **shape** bends the line between points. `log` rises fast and then levels off; `exp` stays low and climbs steeply near the top.
- **average** makes the curve follow a rolling average, so short spikes don't move the fans.
- **ramp_up / ramp_down** limit how fast the fans change speed.
- **sensors** picks which temperature sensors count. `gust temps` lists your Mac's sensors, grouped by name. Sensor names differ between chips; the defaults were measured on an M1 Max.

`gust curve --table` prints the curve as a temperature to RPM table, and `gust curve` shows what it would do right now. Neither writes anything.

## Fail-safes

gustd hands the fans back to macOS:
- when it starts, in case a previous run was killed
- when it's stopped (any of SIGTERM, SIGINT, SIGHUP or SIGQUIT), and on panic
- after the Mac wakes from sleep
- when it can't read the sensors for 3 checks in a row
- when SMC writes keep failing (it then exits, and launchd restarts it)

**At 100°C on any curve sensor, every fan goes to max**, whatever the curve says. This uses the live reading, not the average, and ignores ramp limits.

SIGKILL can't be caught. If gustd is force-killed, launchd restarts it straight away and the restart resets the fans first.

It refuses to write to any SMC key except the fan mode and target keys (and `Ftst` on Macs that have it). It also refuses to run on a Mac whose fan keys aren't the Apple Silicon types.

## Commands

```
gust fans            fan RPM (actual, min, max, target) and mode
gust temps           every temperature sensor, grouped by key prefix
gust status          what gustd is doing right now
gust curve [--table] dry run of your curve
gust read KEY...     raw SMC keys, e.g. gust read F0Ac F0Md
```

The log is at `/var/log/gustd.log`. Status is in `/var/run/gust.json`, for anything else that wants to read it.

## How it works

On Apple Silicon each fan has a mode key, `F{n}Md` (0 is auto, 1 is manual), and a target, `F{n}Tg` (a float, in RPM). gustd sets the mode to manual, then writes the target every couple of seconds from the curve. On Macs whose SMC refuses that mode write, gustd first sets `Ftst` = 1, waits for macOS's thermal daemon to let go, and retries. It undoes all of this on exit.

That write sequence follows [Stats](https://github.com/exelban/stats) (`SMC/smc.swift`, MIT). gust doesn't use Stats' code, but it wouldn't exist without that project's work on the Apple Silicon fan keys.

## Licence

MIT
