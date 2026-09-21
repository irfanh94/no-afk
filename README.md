# no-afk

A menu bar utility that stops macOS sleeping, blanking the display, or locking the
screen. Like Amphetamine, Caffeine or CoffeeTea.

Uses IOKit power assertions — the same public API every video player holds while you
watch a film. **No permissions, no Accessibility grant, no helper daemon.**

> **Status: early.** The core is done and tested; the menu bar shell runs. Windows and
> Linux backends are stubbed with implementation notes but not written.

## Why another one

Most apps in this category conflate three unrelated OS subsystems. no-afk treats them
separately, and is honest about which it can actually do:

| | What it means | Status |
|---|---|---|
| **L1** System sleep | The machine suspends | ✅ works |
| **L2** Display sleep, screensaver, lock | Screen blanks and locks | ✅ works — including an MDM-managed screensaver timeout |
| **L3** Presence / "AFK" | Slack and Teams marking you Away | ❌ not implemented, and **not possible** with power assertions |

That last row is the one every competitor is vague about. Slack and Teams read the HID
idle counter directly, and no power assertion resets it — measured, not assumed:
`IOPMAssertionDeclareUserActivity` returns success while the counter climbs straight
through the call. Defeating it would need synthetic input injection and an Accessibility
grant, so it is a deliberate decision rather than something that falls out of L1/L2.
Undecided for now.

## Layout

```
crates/awake-core/     platform-agnostic keep-awake primitives — no UI, no Tauri
  src/lib.rs           Flags / Request / Guard / Backend trait
  src/macos.rs         IOKit power assertions          (implemented)
  src/windows.rs       PowerCreateRequest              (stub + implementation notes)
  src/linux.rs         logind / Wayland / ScreenSaver  (stub + implementation notes)
  src/fake.rs          in-memory backend for tests
  src/session.rs       durations, countdown, auto-end
src-tauri/             menu bar shell
  src/main.rs          tray, menu, commands, tick
  src/settings.rs      persisted settings (JSON in the OS config dir)
src/                   settings window (plain HTML/CSS/JS, no bundler)
tools/gen_icons.py     regenerates all icons from code
```

The split is deliberate: `awake-core` has no dependency on Tauri or any UI, so the
Windows and Linux ports are a backend file each rather than a rewrite.

## Why is my Mac awake?

The settings window lists **every** power assertion held on the system, by any
process, with ours highlighted — so "why won't my Mac sleep?" has an answer in the app
instead of needing `pmset -g assertions` in a terminal. In practice the culprit is
usually Chrome, Zoom or an Electron app rather than a keep-awake utility.

## Build

Needs Rust and Node.

```bash
cargo test -p awake-core
```

```bash
cargo run -p no-afk
```

Hold a session from the command line and watch it in the OS:

```bash
cargo run -p awake-core --example hold -- 30
```

```bash
pmset -g assertions | grep no-afk
```

Regenerate icons after editing `tools/gen_icons.py`:

```bash
python3 tools/gen_icons.py
```

## Design notes

**Leaked assertions, narrowly.** A crash is safe: powerd releases a dead process's
assertions, verified by holding two with a 24-hour timeout and `SIGKILL`ing the owner —
they vanished at once. The leak that does matter is dropping the handle while still
running, so `Guard` releases on `Drop`, `Drop` never panics, a partial acquire rolls
back, and every test asserts `live_count() == 0`.

**Timed sessions hand the deadline to the kernel** via `kIOPMAssertionTimeoutKey`
rather than relying on an app-side timer, so the deadline still fires if the process is
alive but no longer ticking — suspended or deadlocked.

**No shelling out to `caffeinate`.** It's a subprocess to babysit, it's blocked under
App Sandbox, and it's a thin wrapper over the same IOKit calls.

## Support

If no-afk is useful to you: [ko-fi.com/irfanhodzic](https://ko-fi.com/irfanhodzic)

## Licence

MIT
