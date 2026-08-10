const invoke = window.__TAURI__.core.invoke;

let state = { worlds: [], filter: "all", search: "", running: [] };

function humanSize(bytes) {
  const units = ["B", "KiB", "MiB", "GiB"];
  let v = bytes, u = 0;
  while (v >= 1024 && u < units.length - 1) { v /= 1024; u++; }
  return u === 0 ? `${bytes} B` : `${v.toFixed(1)} ${units[u]}`;
}

function fmtTime(unix) {
  if (!unix) return "never";
  return new Date(unix * 1000).toLocaleString(undefined, {
    year: "numeric", month: "short", day: "numeric", hour: "2-digit", minute: "2-digit",
  });
}

function banner(message, kind) {
  const el = document.getElementById("banner");
  el.textContent = message;
  el.className = `banner ${kind || ""}`;
}

function clearBanner() {
  document.getElementById("banner").className = "banner hidden";
}

function visibleWorlds() {
  const q = state.search.toLowerCase();
  return state.worlds.filter((w) => {
    if (state.filter !== "all" && w.state !== state.filter) return false;
    return !q || w.name.toLowerCase().includes(q);
  });
}

function card(w) {
  const el = document.createElement("article");
  el.className = "card";

  const head = document.createElement("div");
  head.className = "card-head";
  const title = document.createElement("h2");
  title.textContent = w.name;
  const badge = document.createElement("span");
  badge.className = `badge ${w.state}`;
  badge.textContent = w.state === "active" ? "In game" : "Vault";
  head.append(title, badge);

  const meta = document.createElement("div");
  meta.className = "meta";
  for (const text of [w.mode, `v${w.version}`, humanSize(w.size_bytes), fmtTime(w.last_played)]) {
    const span = document.createElement("span");
    span.textContent = text;
    meta.append(span);
  }

  el.append(head, meta);

  if (w.packs.length) {
    const packs = document.createElement("div");
    packs.className = "packs";
    const missing = w.packs.filter((p) => p === "missing content").length;
    const named = w.packs.filter((p) => p !== "missing content");
    packs.textContent = named.length ? `Packs: ${named.join(", ")}` : "";
    if (missing) {
      const warn = document.createElement("span");
      warn.className = "missing";
      warn.textContent = `${packs.textContent ? " · " : "Packs: "}${missing} not installed here`;
      packs.append(warn);
    }
    el.append(packs);
  }

  if (w.error) {
    const err = document.createElement("div");
    err.className = "card-error";
    err.textContent = w.error;
    el.append(err);
  }

  const actions = document.createElement("div");
  actions.className = "actions";
  const blocked = state.running.length > 0;

  if (w.state === "active") {
    actions.append(
      button("Archive", blocked, () => run("archive", { folder: w.id })),
      button("Back up", blocked, () => run("backup", { folder: w.id })),
    );
  } else {
    actions.append(button("Activate", blocked, () => run("activate", { id: w.id })));
  }
  el.append(actions);
  return el;
}

function button(label, disabled, onClick) {
  const b = document.createElement("button");
  b.textContent = label;
  b.disabled = disabled;
  b.title = disabled ? "Close Minecraft first" : "";
  b.addEventListener("click", onClick);
  return b;
}

async function run(cmd, args) {
  try {
    banner(`Working…`, "");
    const message = await invoke(cmd, args);
    banner(message, "ok");
    await load();
  } catch (e) {
    banner(String(e), "error");
  }
}

function render() {
  const grid = document.getElementById("grid");
  grid.textContent = "";
  const worlds = visibleWorlds();
  if (!worlds.length) {
    const empty = document.createElement("div");
    empty.className = "empty";
    empty.textContent = "No worlds match.";
    grid.append(empty);
    return;
  }
  for (const w of worlds) grid.append(card(w));
}

async function load() {
  const data = await invoke("overview");
  state.worlds = data.worlds;
  state.running = data.game_running;

  document.getElementById("status").textContent =
    data.game_running.length
      ? `Minecraft is running — operations blocked`
      : `Vault: ${data.vault_root}`;

  const active = data.worlds.filter((w) => w.state === "active").length;
  const library = data.worlds.length - active;
  const total = data.worlds.reduce((n, w) => n + w.size_bytes, 0);
  document.getElementById("footer").textContent =
    `${active} in game · ${library} in vault · ${humanSize(total)} total` +
    (data.store_summary.length ? ` · Store content — ${data.store_summary.join(", ")}` : "");

  if (data.game_running.length) {
    banner("Minecraft is running. Close the game to move world data safely.", "");
  } else {
    clearBanner();
  }
  render();
}

document.querySelectorAll(".tab").forEach((tab) => {
  tab.addEventListener("click", () => {
    document.querySelectorAll(".tab").forEach((t) => t.classList.remove("active"));
    tab.classList.add("active");
    state.filter = tab.dataset.filter;
    render();
  });
});

document.getElementById("search").addEventListener("input", (e) => {
  state.search = e.target.value;
  render();
});

document.getElementById("refresh").addEventListener("click", () => load());

load().catch((e) => banner(String(e), "error"));
