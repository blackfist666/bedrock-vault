const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;
const openDialog = window.__TAURI__.dialog.open;

const state = { data: null, section: "live", search: "", busy: false };

/// One plain-English sentence per section, shown above the cards.
const EXPLAIN = {
  live: "These are the worlds Minecraft can see right now. Put one away to tidy up the in-game list — it stays safe in the vault.",
  vault: "Every world you own lives here. Press Play to put one into Minecraft. Nothing here is ever lost when you tidy up your game.",
  backups: "Older copies of your worlds, kept automatically. If a world goes wrong, restore a copy from before it happened.",
};

function humanSize(bytes) {
  const units = ["B", "KB", "MB", "GB"];
  let v = bytes, u = 0;
  while (v >= 1024 && u < units.length - 1) { v /= 1024; u++; }
  return u === 0 ? `${bytes} B` : `${v.toFixed(v < 10 && u > 1 ? 1 : 0)} ${units[u]}`;
}

function fmtTime(unix) {
  if (!unix) return "never played";
  const then = new Date(unix * 1000);
  const days = Math.floor((Date.now() - then) / 86400000);
  if (days === 0) return "played today";
  if (days === 1) return "played yesterday";
  if (days < 30) return `played ${days} days ago`;
  return `played ${then.toLocaleDateString(undefined, { month: "short", year: "numeric" })}`;
}

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function banner(message, kind) {
  const b = document.getElementById("banner");
  b.textContent = message;
  b.className = `banner ${kind || ""}`;
}

function clearBanner() {
  document.getElementById("banner").className = "banner hidden";
}

function showProgress(label, done, total) {
  document.getElementById("progress").className = "progress";
  document.getElementById("progress-label").textContent = label;
  document.getElementById("progress-count").textContent = total ? `${done} of ${total}` : "";
  document.getElementById("progress-fill").style.width = total ? `${Math.round((done / total) * 100)}%` : "0%";
}

function hideProgress() {
  document.getElementById("progress").className = "progress hidden";
}

function button(label, opts = {}) {
  const b = el("button", opts.className, label);
  b.disabled = !!opts.disabled || state.busy;
  b.title = b.disabled && !state.busy ? (opts.disabledReason || "") : (opts.help || "");
  b.addEventListener("click", opts.onClick);
  return b;
}

function matches(name) {
  return !state.search || name.toLowerCase().includes(state.search.toLowerCase());
}

function worldCard(title, subtitle, facts, packs, missingPacks, actions, extraClass) {
  const node = el("article", `card ${extraClass || ""}`);
  node.append(el("h2", null, title));
  if (subtitle) node.append(subtitle);
  const meta = el("div", "meta");
  for (const f of facts) meta.append(el("span", null, f));
  node.append(meta);

  if (packs && packs.length) {
    node.append(el("div", "packs", `Uses: ${packs.join(", ")}`));
  }
  if (missingPacks) {
    node.append(el("div", "packs missing",
      `${missingPacks} add-on${missingPacks > 1 ? "s" : ""} not installed on this PC`));
  }

  const row = el("div", "actions");
  for (const a of actions) row.append(a);
  node.append(row);
  return node;
}

function liveCards() {
  const blocked = state.data.game_running.length > 0;
  const reason = "Close Minecraft first";
  return state.data.live.filter((w) => matches(w.name)).map((w) => {
    const saved = el("div", `saved ${w.saved ? "yes" : "no"}`);
    saved.append(el("span", "dot"));
    saved.append(el("span", null, w.saved_label));

    const actions = [];
    if (!w.saved) {
      actions.push(button("Save to vault", {
        className: "primary", disabled: blocked, disabledReason: reason,
        help: "Copy this world into the vault. It stays in Minecraft.",
        onClick: () => run("save_to_vault", { folder: w.folder }),
      }));
    }
    actions.push(button("Put away", {
      disabled: blocked, disabledReason: reason,
      help: "Save it to the vault and take it out of Minecraft's world list.",
      onClick: () => confirmRun(
        `Put "${w.name}" away?`,
        "It goes into the vault and disappears from Minecraft's world list. Press Play in the Vault to bring it back whenever you like.",
        "Yes, put it away", "put_away", { folder: w.folder }),
    }));

    return worldCard(w.name, saved,
      [w.mode, humanSize(w.size_bytes), fmtTime(w.last_played)],
      w.packs, w.missing_packs, actions);
  });
}

function vaultCards() {
  const blocked = state.data.game_running.length > 0;
  const reason = "Close Minecraft first";
  return state.data.vault.filter((w) => matches(w.name)).map((w) => {
    let subtitle = null;
    if (w.in_game) {
      subtitle = el("div", "saved yes");
      subtitle.append(el("span", "dot"));
      subtitle.append(el("span", null, "In Minecraft now"));
    }

    const actions = [];
    if (!w.in_game) {
      actions.push(button("Play", {
        className: "primary", disabled: blocked, disabledReason: reason,
        help: "Put this world into Minecraft so you can play it.",
        onClick: () => run("play", { id: w.id }),
      }));
    }
    actions.push(button("Back up", {
      help: "Save an extra copy in Backups that you can go back to later.",
      onClick: () => run("back_up", { id: w.id }),
    }));
    actions.push(button("Share", {
      help: "Make a .mcworld file you can send to a friend or copy to another device.",
      onClick: () => run("export", { id: w.id }),
    }));
    actions.push(button("Delete", {
      className: "danger",
      help: "Remove it from the vault. A backup is kept, so it can still be restored.",
      onClick: () => confirmRun(
        `Delete "${w.name}" from the vault?`,
        "A backup is kept in the Backups section, so you can still get it back." +
        (w.in_game ? " The copy inside Minecraft is not touched." : ""),
        "Yes, delete it", "delete", { id: w.id }),
    }));

    const facts = [w.mode, humanSize(w.size_bytes), fmtTime(w.last_played)];
    if (w.backup_count) facts.push(`${w.backup_count} backup${w.backup_count > 1 ? "s" : ""}`);
    return worldCard(w.name, subtitle, facts, w.packs, w.missing_packs, actions);
  });
}

function backupCards() {
  return state.data.backups.filter((g) => matches(g.name)).map((g) => {
    const node = el("article", "card");
    node.append(el("h2", null, g.name));
    node.append(el("div", "meta",
      `${g.backups.length} backup${g.backups.length > 1 ? "s" : ""} · ${humanSize(g.total_bytes)}`));

    const list = el("div", "snapshots");
    for (const b of g.backups) {
      const row = el("div", "snap-row");
      row.append(el("span", "snap-when", b.label));
      row.append(el("span", "snap-size", humanSize(b.size_bytes)));
      row.append(button("Restore", {
        className: "link",
        help: "Put this older copy back into the vault as a separate world. Nothing you have now is changed.",
        onClick: () => confirmRun(
          `Restore "${g.name}"?`,
          `The copy from ${b.label} is added to the vault as a separate world, so nothing you have now is changed or lost.`,
          "Yes, restore it", "restore", { path: b.path }),
      }));
      list.append(row);
    }
    node.append(list);
    return node;
  });
}

function render() {
  const grid = document.getElementById("grid");
  grid.textContent = "";
  document.getElementById("explain").textContent = EXPLAIN[state.section];

  const cards = { live: liveCards, vault: vaultCards, backups: backupCards }[state.section]();
  if (!cards.length) {
    const empty = {
      live: "No worlds in Minecraft right now. Press Play on a vault world to add one.",
      vault: "The vault is empty. Save a world from Minecraft, or import a .mcworld file.",
      backups: "No backups yet. They are made automatically whenever a world is saved.",
    }[state.section];
    grid.append(el("div", "empty", state.search ? "Nothing matches that name." : empty));
  }
  for (const c of cards) grid.append(c);

  // The one big button changes with the section it belongs to.
  const main = document.getElementById("main-action");
  main.className = "big";
  if (state.section === "live") {
    const n = state.data.unsaved;
    main.textContent = n ? `Save ${n} world${n > 1 ? "s" : ""} to the vault` : "Everything is saved";
    main.disabled = !n || state.busy || state.data.game_running.length > 0;
    main.onclick = () => run("save_all", {});
  } else if (state.section === "vault") {
    main.textContent = "Import a world…";
    main.disabled = state.busy;
    main.onclick = importWorld;
  } else {
    main.textContent = "Open backups folder";
    main.disabled = state.busy;
    main.onclick = () => run("open_folder", { which: "backups" });
  }
}

async function importWorld() {
  try {
    const file = await openDialog({
      multiple: false,
      filters: [{ name: "Minecraft world", extensions: ["mcworld", "zip"] }],
    });
    if (file) await run("import", { path: typeof file === "string" ? file : file.path });
  } catch (e) {
    banner(String(e), "error");
  }
}

async function run(cmd, args) {
  if (state.busy) return;
  state.busy = true;
  render();
  const label = {
    save_all: "Saving worlds", save_to_vault: "Saving to the vault", put_away: "Putting away",
    play: "Getting it ready", back_up: "Backing up", export: "Making a copy",
    restore: "Restoring", delete: "Deleting", import: "Importing",
  }[cmd] || "Working";
  showProgress(`${label}…`, 0, 0);
  try {
    const message = await invoke(cmd, args);
    state.busy = false;
    showProgress("Refreshing…", 0, 0);
    await load();
    banner(message, "ok");
  } catch (e) {
    banner(String(e), "error");
  } finally {
    state.busy = false;
    hideProgress();
    if (state.data) render();
  }
}

/// In-app confirmation.
///
/// Deliberately not `window.confirm()`: that blocks the webview's thread, which
/// can wedge the window against Tauri's messaging, and it cannot be worded or
/// sized for a child.
function confirmRun(title, body, okLabel, cmd, args) {
  const modal = document.getElementById("modal");
  document.getElementById("modal-title").textContent = title;
  document.getElementById("modal-body").textContent = body;
  const ok = document.getElementById("modal-ok");
  const cancel = document.getElementById("modal-cancel");
  ok.textContent = okLabel;

  const close = () => {
    modal.className = "modal hidden";
    ok.onclick = null;
    cancel.onclick = null;
    document.onkeydown = null;
  };
  ok.onclick = () => { close(); run(cmd, args); };
  cancel.onclick = close;
  document.onkeydown = (e) => { if (e.key === "Escape") close(); };
  modal.className = "modal";
  ok.focus();
}

async function load() {
  const data = await invoke("overview");
  state.data = data;

  document.getElementById("count-live").textContent =
    `${data.live.length} world${data.live.length === 1 ? "" : "s"}`;
  document.getElementById("count-vault").textContent =
    `${data.vault.length} world${data.vault.length === 1 ? "" : "s"}`;
  const backupCount = data.backups.reduce((n, g) => n + g.backups.length, 0);
  document.getElementById("count-backups").textContent =
    `${backupCount} cop${backupCount === 1 ? "y" : "ies"}`;

  document.getElementById("status").textContent = data.game_running.length
    ? "Minecraft is open — close it to move worlds"
    : "Ready";
  document.getElementById("status").className = `status ${data.game_running.length ? "warn" : ""}`;

  document.getElementById("footer").textContent =
    `Minecraft ${humanSize(data.live_bytes)} · Vault ${humanSize(data.vault_bytes)} · ` +
    `Backups ${humanSize(data.backup_bytes)} · Stored in ${data.vault_root}`;

  if (data.game_running.length) {
    banner("Minecraft is open. Close the game before moving worlds around, so nothing gets damaged.", "");
  } else if (data.unsaved) {
    banner(`${data.unsaved} world${data.unsaved > 1 ? "s are" : " is"} not saved in the vault yet.`, "");
  } else {
    clearBanner();
  }
  render();
}

document.querySelectorAll(".step").forEach((step) => {
  step.addEventListener("click", () => {
    document.querySelectorAll(".step").forEach((s) => s.classList.remove("active"));
    step.classList.add("active");
    state.section = step.dataset.section;
    render();
  });
});

document.getElementById("search").addEventListener("input", (e) => {
  state.search = e.target.value;
  render();
});

listen("progress", (event) => {
  const { done, total, current } = event.payload;
  showProgress(current ? `Saving "${current}"…` : "Finishing…", done, total);
});

load().catch((e) => banner(String(e), "error"));
