// Settings window. Rust owns all state; this is a view over it.
//
// Status and the assertion list are polled rather than pushed. For a 1Hz settings
// window that is simpler than event plumbing and cannot drift out of sync.

const { invoke } = window.__TAURI__.core;

const el = (id) => document.getElementById(id);
const STATUS_MS = 1000;
const ASSERTIONS_MS = 4000;

/** `8:04:11` / `43:09` / `0:07` — mirrors the Rust-side formatter. */
function formatRemaining(total) {
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const pad = (n) => String(n).padStart(2, "0");
  return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`;
}

// --- status ---------------------------------------------------------------

let busy = false;

async function refreshStatus() {
  let st;
  try {
    st = await invoke("get_status");
  } catch (err) {
    el("status-title").textContent = "Unavailable";
    el("status-sub").textContent = String(err);
    return;
  }

  el("status").classList.toggle("awake", st.active);
  el("status-title").textContent = st.active ? "Awake" : "Asleep as usual";

  if (!st.active) {
    el("status-sub").textContent = st.keep_display
      ? "Display will sleep and lock normally"
      : "System will sleep normally";
  } else if (st.indefinite) {
    el("status-sub").textContent = st.keep_display
      ? "Indefinitely, display staying on"
      : "Indefinitely, display may sleep";
  } else {
    el("status-sub").textContent = `${formatRemaining(st.remaining_secs ?? 0)} remaining`;
  }

  // Don't stomp the label mid-click.
  if (!busy) {
    el("toggle").textContent = st.active ? "Turn Off" : "Turn On";
    el("toggle").disabled = false;
  }
}

el("toggle").addEventListener("click", async () => {
  busy = true;
  el("toggle").disabled = true;
  try {
    const st = await invoke("get_status");
    if (st.active) {
      await invoke("stop_session");
    } else {
      const raw = el("default-duration").value;
      await invoke("start_session", { secs: raw === "inf" ? null : Number(raw) });
    }
  } catch (err) {
    console.error("toggle failed", err);
  } finally {
    busy = false;
    await refreshStatus();
  }
});

// --- settings -------------------------------------------------------------

async function saveSettings() {
  const raw = el("default-duration").value;
  try {
    await invoke("set_settings", {
      new: {
        keep_display: el("keep-display").checked,
        default_duration_secs: raw === "inf" ? null : Number(raw),
      },
    });
  } catch (err) {
    console.error("could not save settings", err);
  }
  await refreshStatus();
}

el("keep-display").addEventListener("change", saveSettings);
el("default-duration").addEventListener("change", saveSettings);

el("autostart").addEventListener("change", async (e) => {
  const wanted = e.target.checked;
  try {
    await invoke("set_autostart", { enabled: wanted });
  } catch (err) {
    console.error("could not change autostart", err);
  }
  // Reflect what the OS actually reports, not what was asked for.
  e.target.checked = await invoke("get_autostart");
});

el("donate").addEventListener("click", () => invoke("open_donate"));

// --- updates ---------------------------------------------------------------

const DEFAULT_ABOUT = "Keeps your Mac awake using IOKit power assertions.";

/** Offer the install, once a version is known to be available. */
function offerUpdate(version) {
  el("update-status").textContent = `Version ${version} is available.`;
  const btn = el("update");
  btn.textContent = `Install ${version}`;
  btn.classList.add("primary");
  btn.onclick = async () => {
    btn.disabled = true;
    btn.textContent = "Installing…";
    try {
      // Succeeds by relaunching, so nothing after this runs on the happy path.
      await invoke("install_update");
    } catch (err) {
      el("update-status").textContent = `Install failed: ${err}`;
      btn.disabled = false;
      btn.textContent = `Install ${version}`;
    }
  };
}

async function checkForUpdate() {
  const btn = el("update");
  btn.disabled = true;
  el("update-status").textContent = "Checking…";
  try {
    const version = await invoke("check_for_update");
    if (version) {
      offerUpdate(version);
    } else {
      el("update-status").textContent = "no-afk is up to date.";
    }
  } catch (err) {
    // Expected before the first release exists: the manifest 404s until then.
    el("update-status").textContent = `Could not check for updates: ${err}`;
  } finally {
    btn.disabled = false;
  }
}

el("update").addEventListener("click", checkForUpdate);

// --- assertions -----------------------------------------------------------

function assertionRow(a) {
  const row = document.createElement("div");
  row.className = a.ours ? "assertion ours" : "assertion";

  const top = document.createElement("div");
  top.className = "assertion-top";

  const proc = document.createElement("span");
  proc.className = "proc";
  proc.textContent = a.ours ? `${a.process} (this app)` : a.process;

  const kind = document.createElement("span");
  kind.className = "kind";
  kind.textContent = a.kind;

  top.append(proc, kind);

  const detail = document.createElement("div");
  detail.className = "detail";
  detail.textContent = a.name ? `pid ${a.pid} · ${a.name}` : `pid ${a.pid}`;
  detail.title = a.name;

  row.append(top, detail);
  return row;
}

async function refreshAssertions() {
  const host = el("assertions");
  let list;
  try {
    list = await invoke("list_assertions");
  } catch (err) {
    host.replaceChildren(
      Object.assign(document.createElement("p"), {
        className: "error",
        textContent: `Could not read power assertions: ${err}`,
      }),
    );
    return;
  }

  if (list.length === 0) {
    host.replaceChildren(
      Object.assign(document.createElement("p"), {
        className: "empty",
        textContent: "Nothing is holding this Mac awake.",
      }),
    );
    return;
  }

  // Rebuilding the rows resets scroll position; restore whatever the user had so a
  // periodic refresh doesn't yank the list out from under them (and stays at the top
  // when they haven't scrolled at all).
  const scroll = host.scrollTop;
  host.replaceChildren(...list.map(assertionRow));
  host.scrollTop = scroll;
}

el("refresh").addEventListener("click", refreshAssertions);

// --- init -----------------------------------------------------------------

async function init() {
  // Durations come from Rust so the window and the tray menu can never disagree.
  const presets = await invoke("presets");
  const select = el("default-duration");
  select.replaceChildren(
    ...presets.map((p) => {
      const o = document.createElement("option");
      o.value = p.secs === null ? "inf" : String(p.secs);
      o.textContent = p.label;
      return o;
    }),
  );

  const settings = await invoke("get_settings");
  el("keep-display").checked = settings.keep_display;
  select.value =
    settings.default_duration_secs === null ? "inf" : String(settings.default_duration_secs);

  el("autostart").checked = await invoke("get_autostart");
  el("version").textContent = await invoke("app_version");
  el("update-status").textContent = DEFAULT_ABOUT;

  // Surface whatever the startup background check already found, without spending
  // another network round trip on opening the window.
  const already = await invoke("pending_update");
  if (already) offerUpdate(already);

  await Promise.all([refreshStatus(), refreshAssertions()]);
  setInterval(refreshStatus, STATUS_MS);
  setInterval(refreshAssertions, ASSERTIONS_MS);
}

init().catch((err) => {
  el("status-title").textContent = "Failed to load";
  el("status-sub").textContent = String(err);
  console.error(err);
});
