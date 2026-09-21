//! Linux backend — **not yet implemented**.
//!
//! There is no single mechanism on Linux. Implement a **cascade**: try each in order,
//! keep *every* one that succeeds, and only fail if all of them fail. Desktop
//! environments disagree about which one is authoritative, and holding several is
//! harmless.
//!
//! 1. **`org.freedesktop.login1.Manager.Inhibit`** (systemd-logind) — the system-sleep
//!    layer. `what="idle:sleep"`, `mode="block"`. Returns a file descriptor; **hold it
//!    open** — closing it releases the lock. Store the fd in [`Handle::ids`].
//!    logind speaks D-Bus only; it knows nothing about Wayland.
//!
//! 2. **Wayland `zwp_idle_inhibit_manager_v1`** — `create_inhibitor(surface)`. The
//!    compositor-native path and the most correct one under Wayland. Needs a
//!    `wl_surface`; a 1×1 invisible surface is enough for a tray app.
//!
//! 3. **`org.freedesktop.ScreenSaver.Inhibit`** → cookie. Implemented by GNOME, KDE,
//!    XFCE and Cinnamon; the de-facto portable path. On bare compositors it may need a
//!    bridge (cf. `wscreensaver-bridge`).
//!
//! 4. **`org.freedesktop.portal.Inhibit`** (xdg-desktop-portal) — required under
//!    Flatpak/Snap confinement.
//!
//! 5. **X11 fallback** — `XScreenSaverSuspend`, or `xset s off` + `xset -dpms`.
//!
//! Suggested crates: `zbus` for 1/3/4, `wayland-client` for 2. Prior art worth reading
//! before starting: `wakepy`, `keepawake-rs`, `caffeine-ng`.
//!
//! Note the flag mapping is lossy here: [`Flags::display`] maps to the screensaver/idle
//! inhibitors (2–5) and [`Flags::system`] to logind (1). [`Flags::disk`] has no Linux
//! equivalent and should be ignored rather than treated as an error.

use crate::{Backend, Error, Handle, Request, Result};

#[derive(Debug, Default)]
pub struct DbusBackend;

impl Backend for DbusBackend {
    fn name(&self) -> &'static str {
        "linux/dbus"
    }

    fn acquire(&self, _req: &Request) -> Result<Handle> {
        Err(Error::Unsupported("keep-awake is not implemented on Linux yet"))
    }

    fn release(&self, _handle: &Handle) -> Result<()> {
        Err(Error::Unsupported("keep-awake is not implemented on Linux yet"))
    }
}
