const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;
const openDialog = window.__TAURI__.dialog.open;

const state = {
  data: null,
  realms: null,
  realmDetail: {},   // realm id -> slots/players, fetched on demand
  packs: null,
  packsLoading: false,
  profile: null,
  section: "live",
  search: "",
  busy: false,
  login: null,       // device code sign-in in progress
  loginTimer: null,
};

/// Buttons are coloured by where the action lands, so the destination is
/// obvious before clicking. Orange overrides the rest for anything that
/// destroys or disables something.
const TOUCHES = {
  minecraft: "green",
  vault: "red",
  realm: "purple",
  warning: "orange",
};

const EXPLAIN = {
  live: "These are the worlds Minecraft can see right now. Put one away to tidy up the in-game list — it stays safe in the vault.",
  vault: "Every world you own lives here. Press Send to Minecraft to put one into the game. Nothing here is lost when you tidy up your game.",
  backups: "Older copies of your worlds, kept automatically. If a world goes wrong, restore a copy from before it happened.",
  packs: "Everything from the Marketplace that is installed on this PC, and which of your worlds uses it.",
  realms: "Your Realm and what is on it. Copy a Realm's world into your vault at any time.",
};

/// Wallpapers bundled with the app; one is picked at random each launch.
const WALLPAPERS = [
  "beach", "cherry", "fireplace", "mangrove_swamp", "snow", "warm_ocean",
];

function pickWallpaper() {
  const choice = WALLPAPERS[Math.floor(Math.random() * WALLPAPERS.length)];
  document.getElementById("wallpaper").style.backgroundImage =
    `url("backgrounds/${choice}.jpg")`;
}

/// Ctrl+wheel and Ctrl+plus/minus scale the whole window, as in a browser.
///
/// Uses the CSS `zoom` property rather than a transform: it reflows the layout,
/// so panels re-wrap to the window instead of being scaled and clipped. The
/// chosen level is remembered between launches.
const ZOOM = { min: 0.6, max: 2.0, step: 0.1, key: "bedrock-vault-zoom" };

function applyZoom(level) {
  const clamped = Math.min(ZOOM.max, Math.max(ZOOM.min, Math.round(level * 100) / 100));
  document.body.style.zoom = clamped;
  try {
    localStorage.setItem(ZOOM.key, String(clamped));
  } catch {
    // Private mode or a locked-down webview: zoom still works this session.
  }
  return clamped;
}

function currentZoom() {
  return parseFloat(document.body.style.zoom) || 1;
}

function nudgeZoom(direction) {
  const level = applyZoom(currentZoom() + direction * ZOOM.step);
  banner(level === 1 ? "Size reset" : `Size ${Math.round(level * 100)}%`, "ok");
  clearTimeout(nudgeZoom.timer);
  nudgeZoom.timer = setTimeout(() => {
    // Do not sit on top of a real message.
    const b = document.getElementById("banner");
    if (b.textContent.startsWith("Size")) clearBanner();
  }, 1200);
}

function setUpZoom() {
  let saved = 1;
  try {
    saved = parseFloat(localStorage.getItem(ZOOM.key)) || 1;
  } catch { /* fall back to 100% */ }
  applyZoom(saved);

  window.addEventListener("wheel", (e) => {
    if (!e.ctrlKey) return;
    e.preventDefault();
    nudgeZoom(e.deltaY < 0 ? 1 : -1);
  }, { passive: false });

  window.addEventListener("keydown", (e) => {
    if (!e.ctrlKey) return;
    if (e.key === "+" || e.key === "=") { e.preventDefault(); nudgeZoom(1); }
    else if (e.key === "-") { e.preventDefault(); nudgeZoom(-1); }
    else if (e.key === "0") { e.preventDefault(); applyZoom(1); nudgeZoom(0); }
  });
}

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
  const b = el("button", `mc-btn small ${opts.className || ""}`, label);
  b.disabled = !!opts.disabled || state.busy;
  b.title = b.disabled && !state.busy ? (opts.disabledReason || "") : (opts.help || "");
  b.addEventListener("click", opts.onClick);
  return b;
}

function matches(name) {
  return !state.search || name.toLowerCase().includes(state.search.toLowerCase());
}

function thumb(icon) {
  if (icon) {
    const img = el("img", "thumb");
    img.src = icon;
    img.alt = "";
    return img;
  }
  return el("div", "thumb placeholder", "🌍");
}

/// One world row: thumbnail, name, chips, then its actions on the right.
function worldRow({ icon, name, chips, meta, actions }) {
  const row = el("div", "row");
  row.append(thumb(icon));

  const main = el("div", "row-main");
  main.append(el("div", "row-name", name));
  const chipRow = el("div", "chips");
  for (const c of chips) {
    if (c) chipRow.append(el("span", `chip ${c.kind || ""}`, c.text));
  }
  main.append(chipRow);
  if (meta) main.append(el("div", "row-meta", meta));
  row.append(main);

  const act = el("div", "row-actions");
  for (const a of actions) act.append(a);
  row.append(act);
  return row;
}

function liveRows() {
  const blocked = state.data.game_running.length > 0;
  const reason = "Close Minecraft first";
  return state.data.live.filter((w) => matches(w.name)).map((w) => {
    const actions = [];
    if (!w.saved) {
      actions.push(button(w.protection === "stale" ? "Save" : "Save to vault", {
        className: TOUCHES.vault, disabled: blocked, disabledReason: reason,
        help: "Copy this world into the vault. It stays in Minecraft.",
        onClick: () => run("save_to_vault", { folder: w.folder }),
      }));
    }
    actions.push(button("Put away", {
      className: TOUCHES.minecraft, disabled: blocked, disabledReason: reason,
      help: "Save it to the vault and take it out of Minecraft's world list.",
      onClick: () => confirmRun(
        `Put "${w.name}" away?`,
        "It goes into the vault and disappears from Minecraft's world list. Press Send to Minecraft in the Vault to bring it back whenever you like.",
        "Yes, put it away", "put_away", { folder: w.folder }),
    }));

    return worldRow({
      icon: w.icon,
      name: w.name,
      chips: [
        { text: w.mode, kind: "mode" },
        { text: w.saved_label, kind: w.saved ? "saved" : "unsaved" },
        w.missing_packs ? { text: `${w.missing_packs} add-on${w.missing_packs > 1 ? "s" : ""} missing`, kind: "warn" } : null,
      ],
      meta: `${humanSize(w.size_bytes)} · ${fmtTime(w.last_played)} · v${w.version}` +
        (w.packs.length ? ` · ${w.packs.join(", ")}` : ""),
      actions,
    });
  });
}

function vaultRows() {
  const blocked = state.data.game_running.length > 0;
  const reason = "Close Minecraft first";
  return state.data.vault.filter((w) => matches(w.name)).map((w) => {
    const actions = [];
    if (!w.in_game) {
      actions.push(button("Send to Minecraft", {
        className: TOUCHES.minecraft, disabled: blocked, disabledReason: reason,
        help: "Copy this world into Minecraft on this PC, so it appears in your single-player world list.",
        onClick: () => run("play", { id: w.id }),
      }));
    }
    if (state.realmTargets && state.realmTargets.length) {
      actions.push(button("Send to Realm", {
        className: TOUCHES.realm,
        help: "Put this world onto one of your Realms, so everyone can play it.",
        onClick: () => chooseRealm(w),
      }));
    }
    actions.push(button("Back up", {
      className: TOUCHES.vault,
      help: "Save an extra copy in Backups that you can go back to later.",
      onClick: () => run("back_up", { id: w.id }),
    }));
    actions.push(button("Share", {
      className: TOUCHES.vault,
      help: "Make a .mcworld file you can send to a friend or copy to another device.",
      onClick: () => run("export", { id: w.id }),
    }));
    actions.push(button("Delete", {
      className: TOUCHES.warning,
      help: "Remove it from the vault. A backup is kept, so it can still be restored.",
      onClick: () => confirmRun(
        `Delete "${w.name}" from the vault?`,
        "A backup is kept in the Backups tab, so you can still get it back." +
        (w.in_game ? " The copy inside Minecraft is not touched." : ""),
        "Yes, delete it", "delete", { id: w.id }),
    }));

    return worldRow({
      icon: w.icon,
      name: w.name,
      chips: [
        { text: w.mode, kind: "mode" },
        w.in_game ? { text: "In Minecraft now", kind: "live" } : null,
        w.backup_count ? { text: `${w.backup_count} backup${w.backup_count > 1 ? "s" : ""}` } : null,
        w.missing_packs ? { text: `${w.missing_packs} add-on${w.missing_packs > 1 ? "s" : ""} missing`, kind: "warn" } : null,
      ],
      meta: `${humanSize(w.size_bytes)} · ${fmtTime(w.last_played)} · v${w.version}` +
        (w.packs.length ? ` · ${w.packs.join(", ")}` : ""),
      actions,
    });
  });
}

function backupRows() {
  return state.data.backups.filter((g) => matches(g.name)).map((g) => {
    const row = el("div", "row");
    const main = el("div", "row-main");
    main.append(el("div", "row-name", g.name));
    main.append(el("div", "row-meta",
      `${g.backups.length} backup${g.backups.length > 1 ? "s" : ""} · ${humanSize(g.total_bytes)}`));

    const list = el("div", "backup-list");
    for (const b of g.backups) {
      const line = el("div", "backup-row");
      line.append(el("span", "backup-when", b.label));
      line.append(el("span", "backup-size", humanSize(b.size_bytes)));
      line.append(button("Restore", {
        className: TOUCHES.vault,
        help: "Put this older copy back into the vault as a separate world. Nothing you have now is changed.",
        onClick: () => confirmRun(
          `Restore "${g.name}"?`,
          `The copy from ${b.label} is added to the vault as a separate world, so nothing you have now is changed or lost.`,
          "Yes, restore it", "restore", { path: b.path }),
      }));
      line.append(button("Delete", {
        className: TOUCHES.warning,
        help: "Throw this older copy away. The world itself is not touched.",
        onClick: () => confirmRun(
          `Delete the copy of "${g.name}" from ${b.label}?`,
          "This copy is thrown away for good and cannot be brought back." +
          (g.backups.length === 1
            ? ` It is the last copy of "${g.name}" kept in Backups.`
            : " The other copies are not touched."),
          "Yes, delete it", "delete_backup", { path: b.path }),
      }));
      list.append(line);
    }
    main.append(list);
    row.append(main);
    return row;
  });
}

/// A Realm shown in full: slots, what is on them, packs, players.
function realmHero(realm) {
  const card = el("div", "hero");

  const head = el("div", "hero-head");
  const title = el("div", "hero-title");
  title.append(el("h2", null, realm.name));
  const sub = el("div", "hero-sub");
  sub.append(el("span", `chip ${realm.state === "OPEN" ? "mode" : ""}`, realm.state === "OPEN" ? "Open" : "Closed"));
  sub.append(el("span", `chip ${realm.expired ? "warn" : "saved"}`, realm.subscription));
  sub.append(el("span", "chip", `Up to ${realm.max_players ?? "?"} players`));
  title.append(sub);
  head.append(title);

  const actions = el("div", "row-actions");
  if (realm.can_download) {
    // Reading from a Realm changes nothing there; the world lands in the vault.
    actions.append(button("Copy world to vault", {
      className: TOUCHES.vault,
      help: "Download this Realm's current world into your vault. Nothing on the Realm changes.",
      onClick: () => confirmRun(
        `Copy "${realm.name}" into your vault?`,
        "The Realm's current world is downloaded and added to your vault as a new world. Nothing on the Realm itself is changed.",
        "Yes, copy it", "realm_download",
        { realmId: realm.id, slot: realm.active_slot, name: realm.name }),
    }));
    actions.append(button("Put a vault world here", {
      className: TOUCHES.realm,
      help: "Replace this Realm's world with one from your vault.",
      onClick: () => chooseVaultWorld(realm),
    }));
  }
  head.append(actions);
  card.append(head);

  const detail = state.realmDetail[realm.id];
  if (!detail) {
    card.append(el("div", "hero-loading", "Loading worlds…"));
    loadRealmDetail(realm.id);
    return card;
  }
  if (detail.error) {
    card.append(el("div", "hero-loading", detail.error));
    return card;
  }

  const slots = el("div", "slots");
  for (const slot of detail.slots) {
    const s = el("div", `slot ${slot.active ? "live" : ""} ${slot.empty ? "empty" : ""}`);
    const shead = el("div", "slot-head");
    shead.append(el("span", "slot-num", `Slot ${slot.slot_id}`));
    if (slot.active) shead.append(el("span", "chip live", "Playing now"));
    s.append(shead);

    // An unused slot: offer to fill it rather than showing a blank card.
    if (slot.empty) {
      s.append(el("div", "slot-name empty-name", "No world here yet"));
      s.append(el("div", "slot-rules",
        "Put one of your vault worlds here to use this slot."));
      if (realm.can_download) {
        const emptyActions = el("div", "row-actions");
        emptyActions.append(button("Put a world here", {
          className: TOUCHES.realm,
          help: "Send a world from your vault to this slot.",
          onClick: () => chooseVaultWorld(realm, slot.slot_id),
        }));
        s.append(emptyActions);
      }
      slots.append(s);
      continue;
    }

    s.append(el("div", "slot-name", slot.name));

    const facts = el("div", "chips");
    for (const t of [slot.game_mode, slot.difficulty, slot.hardcore ? "Hardcore" : null]) {
      if (t) facts.append(el("span", "chip mode", t));
    }
    s.append(facts);

    if (slot.packs.length) {
      const packs = el("div", "slot-packs");
      for (const p of slot.packs) {
        const chipEl = el("div", `pack-chip ${p.installed ? "" : "warn"}`);
        if (p.icon) {
          const img = el("img", "pack-icon");
          img.src = p.icon;
          chipEl.append(img);
        }
        chipEl.append(el("span", null, p.name));
        packs.append(chipEl);
      }
      s.append(packs);
    }

    if (slot.rules.length) {
      s.append(el("div", "slot-rules",
        slot.rules.map(([k, v]) => `${k}: ${v}`).join(" · ")));
    }
    if (slot.seed) s.append(el("div", "slot-rules", `Seed ${slot.seed}`));
    if (!slot.can_backup) {
      s.append(el("div", "slot-warn",
        "Minecraft has no saved copy of this world yet, so it cannot be copied to your vault."));
    }

    if (realm.can_download) {
      const slotActions = el("div", "row-actions");
      if (!slot.active) {
        slotActions.append(button("Play this one", {
          className: TOUCHES.realm,
          help: "Make this the world the Realm runs.",
          onClick: () => confirmRun(
            `Switch the Realm to "${slot.name}"?`,
            "Everyone on the Realm moves to this world. The other slots keep their worlds, and you can switch back at any time.",
            "Yes, switch", "realm_switch_slot", { realmId: realm.id, slot: slot.slot_id }),
        }));
      }
      slotActions.append(button("Replace", {
        className: TOUCHES.realm,
        help: "Put a different world from your vault on this slot.",
        onClick: () => chooseVaultWorld(realm, slot.slot_id),
      }));
      slotActions.append(button("Rename", {
        className: TOUCHES.realm,
        help: "Change the world's name as shown in Minecraft.",
        onClick: () => askText(
          `Rename the world on slot ${slot.slot_id}`,
          "This is the name Minecraft shows for the world on this Realm.",
          slot.name === "Unnamed world" ? "" : slot.name,
          (name) => run("realm_rename_slot", { realmId: realm.id, slot: slot.slot_id, name })),
      }));
      if (slot.can_backup) {
        slotActions.append(button("Copy to vault", {
          className: TOUCHES.vault,
          help: "Download this slot's world into your vault. Nothing on the Realm changes.",
          onClick: () => confirmRun(
            `Copy "${slot.name}" into your vault?`,
            "The world is downloaded and added to your vault. Nothing on the Realm is changed.",
            "Yes, copy it", "realm_download",
            { realmId: realm.id, slot: slot.slot_id, name: slot.name }),
        }));
      }
      if (slot.packs.length) {
        slotActions.append(button("Turn off add-ons", {
          className: TOUCHES.warning,
          help: "Remove every add-on from this world on the Realm.",
          onClick: () => confirmRun(
            `Turn off the add-ons on slot ${slot.slot_id}?`,
            "The world keeps everything already built with them, but the add-ons stop being applied. " +
            "You can put them back from inside Minecraft.",
            "Yes, turn them off", "realm_clear_packs",
            { realmId: realm.id, slot: slot.slot_id }),
        }));
      }
      s.append(slotActions);
    }
    slots.append(s);
  }
  card.append(slots);

  if (detail.players && detail.players.length) {
    const people = el("div", "players");
    people.append(el("span", "players-label", "Players:"));
    for (const p of detail.players) {
      people.append(el("span", `chip ${p.online ? "saved" : ""}`,
        `${p.name}${p.role ? ` (${p.role})` : ""}`));
    }
    card.append(people);
  }
  return card;
}

async function loadRealmDetail(id) {
  if (state.realmDetail[id]) return;
  state.realmDetail[id] = { loading: true };
  try {
    state.realmDetail[id] = await invoke("realm_detail", { realmId: id });
  } catch (e) {
    state.realmDetail[id] = { error: String(e) };
  }
  if (state.section === "realms") render();
}

function realmSummaryRow(realm) {
  const actions = [];
  if (realm.can_download) {
    actions.push(button("Copy to vault", {
      className: TOUCHES.vault,
      help: "Download this Realm's world into your vault.",
      onClick: () => confirmRun(
        `Copy "${realm.name}" into your vault?`,
        "The Realm's world is downloaded and added to your vault. Nothing on the Realm is changed.",
        "Yes, copy it", "realm_download",
        { realmId: realm.id, slot: realm.active_slot, name: realm.name }),
    }));
  }
  return worldRow({
    icon: null,
    name: realm.name,
    chips: [
      { text: realm.state === "OPEN" ? "Open" : "Closed", kind: realm.state === "OPEN" ? "mode" : "" },
      { text: realm.subscription, kind: realm.expired ? "warn" : "saved" },
      { text: realm.role === "yours" ? "Yours" : "Joined", kind: realm.role === "yours" ? "live" : "" },
    ],
    meta: `Slot ${realm.active_slot ?? "?"} · up to ${realm.max_players ?? "?"} players` +
      (realm.can_download ? "" : " · only the owner can copy this world"),
    actions,
  });
}

function heading(text, note) {
  const h = el("div", "section-head");
  h.append(el("span", "section-title", text));
  if (note) h.append(el("span", "section-note", note));
  return h;
}

function realmRows() {
  const r = state.realms;

  // Sign-in in progress: show the code the user must type.
  if (state.login) {
    const panel = el("div", "signin");
    panel.append(el("h2", null, "Finish signing in"));
    panel.append(el("p", null,
      "Go to the Microsoft sign-in page and enter this code. This window will notice as soon as you are done."));
    panel.append(el("div", "big-code", state.login.user_code));
    const actions = el("div", "signin-actions");
    actions.append(button("Open the sign-in page", {
      className: TOUCHES.realm,
      onClick: () => invoke("open_url", { url: state.login.verification_uri }).catch(() => {}),
    }));
    actions.append(button("Cancel", { onClick: cancelLogin }));
    panel.append(actions);
    panel.append(el("p", null, state.login.verification_uri));
    return [panel];
  }

  if (!r) return [el("div", "empty", "Checking…")];

  if (!r.signed_in) {
    const panel = el("div", "signin");
    panel.append(el("h2", null, "Sign in to see your Realms"));
    panel.append(el("p", null,
      "Use the same Microsoft account you use for Minecraft. You will get a short code to type on Microsoft's website — Bedrock Vault never sees your password."));
    const actions = el("div", "signin-actions");
    actions.append(button("Sign in with Microsoft", { className: TOUCHES.realm, onClick: beginLogin }));
    panel.append(actions);
    return [panel];
  }

  const rows = [];
  if (r.error) rows.push(el("div", "banner error", r.error));

  const mine = r.mine.filter((x) => matches(x.name));
  const expired = r.expired.filter((x) => matches(x.name));
  const joined = r.joined.filter((x) => matches(x.name));

  // Your own live Realms get the whole treatment; everything else is a list.
  if (mine.length) {
    for (const realm of mine) rows.push(realmHero(realm));
  } else if (!state.search) {
    rows.push(el("div", "empty",
      "You have no Realm running at the moment. Expired Realms are below — their worlds can still be copied into your vault."));
  }

  if (joined.length) {
    rows.push(heading("Realms you have joined",
      "Only a Realm's owner can copy or replace its world"));
    for (const realm of joined) rows.push(realmSummaryRow(realm));
  }

  if (expired.length) {
    rows.push(heading("Your past Realms",
      "The subscriptions lapsed, but Minecraft still holds the worlds — copy them into your vault"));
    for (const realm of expired) rows.push(realmSummaryRow(realm));
  }

  if (!mine.length && !joined.length && !expired.length) {
    rows.push(el("div", "empty", state.search ? "No Realm matches that name." : "No Realms on this account."));
  }
  return rows;
}

function packRows() {
  const p = state.packs;
  if (!p) {
    loadPacks();
    return [el("div", "empty", "Reading your marketplace content…")];
  }

  const rows = [];
  const shown = p.packs.filter((x) => matches(x.name));
  let category = null;
  for (const pack of shown) {
    if (pack.category !== category) {
      category = pack.category;
      rows.push(heading(category, null));
    }
    const row = el("div", "row");
    if (pack.icon) {
      const img = el("img", "thumb pack");
      img.src = pack.icon;
      row.append(img);
    } else {
      row.append(el("div", "thumb pack placeholder", "📦"));
    }

    const main = el("div", "row-main");
    main.append(el("div", "row-name", pack.name));
    if (pack.description) main.append(el("div", "row-meta", pack.description));
    const chips = el("div", "chips");
    chips.append(el("span", "chip", `v${pack.version}`));
    chips.append(el("span", "chip", humanSize(pack.size_bytes)));
    if (pack.used_by.length) {
      chips.append(el("span", "chip live", `Used by ${pack.used_by.join(", ")}`));
    }
    main.append(chips);
    row.append(main);
    rows.push(row);
  }

  if (!shown.length) {
    rows.push(el("div", "empty",
      state.search ? "No pack matches that name." : "No marketplace content is installed on this PC."));
  }
  if (p.persona_count && !state.search) {
    rows.push(heading("Character creator",
      `${p.persona_count} items owned — shown in game, not here`));
  }
  if (p.missing.length && !state.search) {
    rows.push(heading("Not installed on this PC",
      `${p.missing.length} add-on${p.missing.length > 1 ? "s are" : " is"} used by a world but not downloaded here`));
  }
  return rows;
}

async function loadPacks() {
  if (state.packsLoading) return;
  state.packsLoading = true;
  try {
    state.packs = await invoke("packs_overview");
    const n = state.packs.packs.length;
    document.getElementById("count-packs").textContent = `${n} item${n === 1 ? "" : "s"}`;
    if (state.section === "packs") render();
  } catch (e) {
    banner(String(e), "error");
  } finally {
    state.packsLoading = false;
  }
}

async function beginLogin() {
  try {
    state.login = await invoke("realms_begin_login");
    render();
    // Poll at the interval Microsoft asked for, until the user finishes.
    const wait = Math.max(2, state.login.interval_secs || 5) * 1000;
    state.loginTimer = setInterval(pollLogin, wait);
  } catch (e) {
    banner(String(e), "error");
  }
}

async function pollLogin() {
  if (!state.login) return;
  try {
    const who = await invoke("realms_poll_login");
    if (!who) return;
    stopLoginTimer();
    state.login = null;
    banner(`Signed in as ${who}`, "ok");
    await loadProfile();
    await loadRealms();
    render();
  } catch (e) {
    stopLoginTimer();
    state.login = null;
    banner(String(e), "error");
    render();
  }
}

function stopLoginTimer() {
  if (state.loginTimer) clearInterval(state.loginTimer);
  state.loginTimer = null;
}

async function cancelLogin() {
  stopLoginTimer();
  state.login = null;
  await invoke("realms_cancel_login").catch(() => {});
  render();
}

/// The signed-in player, shown top-right on every page.
function renderAccount() {
  const box = document.getElementById("account");
  const p = state.profile;
  if (!p) {
    box.className = "account hidden";
    return;
  }
  box.className = "account";
  const pic = document.getElementById("account-pic");
  if (p.picture) {
    pic.src = p.picture;
    pic.style.display = "";
  } else {
    pic.style.display = "none";
  }
  document.getElementById("account-name").textContent = p.gamertag;
  document.getElementById("account-sub").textContent =
    p.gamerscore ? `${Number(p.gamerscore).toLocaleString()} gamerscore` : "Signed in";
}

async function loadProfile() {
  try {
    state.profile = await invoke("profile");
  } catch {
    state.profile = null;
  }
  renderAccount();
}

async function loadRealmTargets() {
  try {
    state.realmTargets = await invoke("realm_targets");
  } catch {
    state.realmTargets = [];
  }
}

/// Ask the Realms service what is on each Realm now.
///
/// Nothing here is cached between calls, so this is also the refresh: the
/// per-Realm detail is dropped so each hero fetches its slots again.
async function loadRealms() {
  try {
    state.realms = await invoke("realms_overview");
    state.realmDetail = {};
    const r = state.realms;
    const total = r.mine.length + r.expired.length + r.joined.length;
    document.getElementById("count-realms").textContent = r.signed_in
      ? (r.mine.length ? `${r.mine.length} active of ${total}` : `${total} Realm${total === 1 ? "" : "s"}`)
      : "not signed in";
    state.profile = r.profile || state.profile;
    state.realmTargets = r.mine;
    renderAccount();
    return true;
  } catch (e) {
    banner(String(e), "error");
    return false;
  }
}

/// The Refresh button on the Realms page.
///
/// A Realm changes while the app is open — someone loads a different world on
/// it from inside Minecraft, or makes a new one — and nothing tells the app
/// that happened, so this is how the page catches up without a restart.
async function refreshRealms() {
  if (state.busy) return;
  state.busy = true;
  render();
  showProgress("Checking your Realms…", 0, 0);
  const ok = await loadRealms();
  state.busy = false;
  hideProgress();
  // A Realm that answered but reported a problem shows it in the list itself.
  if (ok && !state.realms?.error) banner("Your Realms are up to date", "ok");
  render();
}

function render() {
  const grid = document.getElementById("grid");
  grid.textContent = "";
  document.getElementById("explain").textContent = EXPLAIN[state.section];

  const rows = {
    live: liveRows, vault: vaultRows, backups: backupRows,
    packs: packRows, realms: realmRows,
  }[state.section]();
  if (!rows.length) {
    const empty = {
      live: "No worlds in Minecraft right now. Press Send to Minecraft on a vault world to add one.",
      vault: "The vault is empty. Save a world from Minecraft, or import a .mcworld file.",
      backups: "No backups yet. They are made automatically whenever a world is saved.",
      packs: "No marketplace content is installed on this PC.",
      realms: "Nothing to show.",
    }[state.section];
    grid.append(el("div", "empty", state.search ? "Nothing matches that name." : empty));
  }
  for (const r of rows) grid.append(r);

  // Only the Realms page has something to re-read from a server; the Packs
  // page carries its own Refresh as the main action.
  const refresh = document.getElementById("refresh-action");
  const canRefresh = state.section === "realms" && state.realms?.signed_in && !state.login;
  refresh.style.display = canRefresh ? "" : "none";
  refresh.disabled = state.busy;
  refresh.onclick = refreshRealms;

  const main = document.getElementById("main-action");
  main.style.display = "";
  if (state.section === "live") {
    const n = state.data.unsaved;
    // Lands in the vault, so it is a vault action even on the Minecraft tab.
    main.className = `mc-btn ${TOUCHES.vault}`;
    main.textContent = n ? `Save ${n} world${n > 1 ? "s" : ""} to the vault` : "Everything is saved";
    main.disabled = !n || state.busy || state.data.game_running.length > 0;
    main.onclick = () => run("save_all", {});
  } else if (state.section === "vault") {
    main.className = `mc-btn ${TOUCHES.vault}`;
    main.textContent = "Import a world";
    main.disabled = state.busy;
    main.onclick = importWorld;
  } else if (state.section === "backups") {
    main.className = `mc-btn ${TOUCHES.vault}`;
    main.textContent = "Open backups folder";
    main.disabled = state.busy;
    main.onclick = () => run("open_folder", { which: "backups" });
  } else if (state.section === "packs") {
    main.className = "mc-btn";
    main.textContent = "Refresh";
    main.disabled = state.busy;
    main.onclick = () => { state.packs = null; loadPacks(); render(); };
  } else if (state.realms && state.realms.signed_in && !state.login) {
    main.textContent = "Sign out";
    main.className = `mc-btn ${TOUCHES.realm}`;
    main.disabled = state.busy;
    main.onclick = () => confirmRun("Sign out?",
      "The saved sign-in is deleted from this PC. You can sign in again at any time.",
      "Yes, sign out", "realms_sign_out", {});
  } else {
    // Signed out or mid sign-in: the panel carries its own buttons.
    main.style.display = "none";
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
    restore: "Restoring", delete: "Deleting", delete_backup: "Deleting that copy",
    import: "Importing",
    set_vault_location: "Moving the vault", open_folder: "Opening",
    realms_sign_out: "Signing out", realm_download: "Downloading from the Realm",
    realm_upload: "Sending to the Realm", realm_rename_slot: "Renaming",
    realm_clear_packs: "Updating the Realm", realm_switch_slot: "Switching world",
  }[cmd] || "Working";
  showProgress(`${label}…`, 0, 0);
  try {
    const message = await invoke(cmd, args);
    state.busy = false;
    showProgress("Refreshing…", 0, 0);
    if (cmd === "realms_sign_out") {
      state.profile = null;
      renderAccount();
      await loadRealms();
    } else {
      await load();
      if (cmd.startsWith("realm_")) await loadRealms();
    }
    if (message) banner(message, "ok");
  } catch (e) {
    banner(String(e), "error");
  } finally {
    state.busy = false;
    hideProgress();
    if (state.data) render();
  }
}

/// Ask for a line of text, in the app's own dialog.
function askText(title, body, initial, onDone) {
  const modal = document.getElementById("modal");
  document.getElementById("modal-title").textContent = title;
  document.getElementById("modal-body").textContent = body;
  const ok = document.getElementById("modal-ok");
  const cancel = document.getElementById("modal-cancel");
  ok.textContent = "Save";
  // Only used for renaming a Realm's world.
  ok.className = `mc-btn ${TOUCHES.realm}`;
  ok.style.display = "";

  const field = el("input", "text-field");
  field.type = "text";
  field.value = initial || "";
  field.maxLength = 64;
  const box = document.querySelector(".modal-actions");
  box.parentElement.insertBefore(field, box);

  const close = () => {
    modal.className = "modal hidden";
    field.remove();
    ok.onclick = null;
    cancel.onclick = null;
    document.onkeydown = null;
  };
  const submit = () => {
    const value = field.value.trim();
    close();
    if (value) onDone(value);
  };
  ok.onclick = submit;
  cancel.onclick = close;
  document.onkeydown = (e) => {
    if (e.key === "Escape") close();
    if (e.key === "Enter") submit();
  };
  modal.className = "modal";
  field.focus();
  field.select();
}

/// Pick which vault world to put on a Realm slot.
function chooseVaultWorld(realm, slotId) {
  const worlds = (state.data?.vault || []).slice(0, 40);
  if (!worlds.length) {
    banner("Your vault has no worlds to send yet.", "");
    return;
  }
  const slot = slotId ?? realm.active_slot;
  const detail = state.realmDetail[realm.id];
  const target = detail?.slots?.find((s) => s.slot_id === slot);

  const modal = document.getElementById("modal");
  document.getElementById("modal-title").textContent =
    `Put which world on slot ${slot}?`;
  let explanation;
  if (target?.empty) {
    explanation = "This slot is empty, so nothing will be replaced. " +
      "The Realm closes while the world is sent and reopens afterwards.";
  } else if (target && !target.can_backup) {
    explanation = `Careful: Minecraft has no saved copy of "${target.name}" yet, so it cannot be kept — ` +
      "replacing it will lose it. Minecraft only keeps a copy once it has made its own backup, " +
      "which usually means playing the world for a while first.";
  } else {
    explanation = "The world already on this slot is downloaded into your vault first, so nothing is lost. " +
      "The Realm closes while the swap happens and reopens afterwards.";
  }
  document.getElementById("modal-body").textContent = explanation;
  const ok = document.getElementById("modal-ok");
  const cancel = document.getElementById("modal-cancel");
  ok.style.display = "none";

  const list = el("div", "realm-choices scroll");
  for (const world of worlds) {
    list.append(button(`${world.name}  ·  ${humanSize(world.size_bytes)}`, {
      onClick: () => {
        close();
        run("realm_upload", { realmId: realm.id, slot, id: world.id });
      },
    }));
  }
  const box = document.querySelector(".modal-actions");
  box.parentElement.insertBefore(list, box);

  const close = () => {
    modal.className = "modal hidden";
    list.remove();
    ok.style.display = "";
    cancel.onclick = null;
    document.onkeydown = null;
  };
  cancel.onclick = close;
  document.onkeydown = (e) => { if (e.key === "Escape") close(); };
  modal.className = "modal";
}

/// Pick which Realm a vault world goes onto, then confirm the replacement.
function chooseRealm(world) {
  const targets = state.realmTargets || [];
  if (!targets.length) {
    banner("You have no Realm of your own running to send a world to.", "");
    return;
  }
  if (targets.length === 1) {
    const realm = targets[0];
    confirmRun(
      `Put "${world.name}" on ${realm.name}?`,
      `This replaces the world currently on ${realm.name}. That world is downloaded into your vault first, ` +
      "so nothing is lost, and the Realm is closed while the swap happens and reopened afterwards.",
      "Yes, send it", "realm_upload", { realmId: realm.id, slot: realm.active_slot, id: world.id });
    return;
  }

  // More than one Realm: ask which.
  const modal = document.getElementById("modal");
  document.getElementById("modal-title").textContent = `Send "${world.name}" to which Realm?`;
  document.getElementById("modal-body").textContent =
    "The world already on it is downloaded into your vault first, so nothing is lost.";
  const ok = document.getElementById("modal-ok");
  const cancel = document.getElementById("modal-cancel");
  ok.style.display = "none";

  const list = el("div", "realm-choices");
  for (const realm of targets) {
    list.append(button(realm.name, {
      className: TOUCHES.realm,
      onClick: () => {
        close();
        run("realm_upload", { realmId: realm.id, slot: realm.active_slot, id: world.id });
      },
    }));
  }
  const box = document.querySelector(".modal-actions");
  box.parentElement.insertBefore(list, box);

  const close = () => {
    modal.className = "modal hidden";
    list.remove();
    ok.style.display = "";
    cancel.onclick = null;
    document.onkeydown = null;
  };
  cancel.onclick = close;
  document.onkeydown = (e) => { if (e.key === "Escape") close(); };
  modal.className = "modal";
}

function confirmRun(title, body, okLabel, cmd, args) {
  const modal = document.getElementById("modal");
  document.getElementById("modal-title").textContent = title;
  document.getElementById("modal-body").textContent = body;
  const ok = document.getElementById("modal-ok");
  const cancel = document.getElementById("modal-cancel");
  ok.textContent = okLabel;
  // The confirm button matches where the action lands, so the colour the user
  // clicked on the row is the colour they confirm with.
  ok.className = `mc-btn ${
    cmd.startsWith("realm_") || cmd.startsWith("realms_") ? TOUCHES.realm
    : cmd === "delete" || cmd === "delete_backup" || cmd === "realm_clear_packs" ? TOUCHES.warning
    : cmd === "put_away" || cmd === "play" ? TOUCHES.minecraft
    : TOUCHES.vault}`;
  ok.style.display = "";

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
    `Minecraft ${humanSize(data.live_bytes)} · Vault ${humanSize(data.vault_bytes)} · Backups ${humanSize(data.backup_bytes)}`;
  document.getElementById("vault-path").textContent = `Vault folder: ${data.vault_root}`;

  if (data.game_running.length) {
    banner("Minecraft is open. Close the game before moving worlds around, so nothing gets damaged.", "");
  } else if (data.unsaved) {
    banner(`${data.unsaved} world${data.unsaved > 1 ? "s are" : " is"} not saved in the vault yet.`, "");
  } else {
    clearBanner();
  }
  render();
}

document.querySelectorAll(".tab").forEach((tab) => {
  tab.addEventListener("click", () => {
    document.querySelectorAll(".tab").forEach((t) => t.classList.remove("active"));
    tab.classList.add("active");
    state.section = tab.dataset.section;
    if (state.section === "realms" && !state.realms) loadRealms().then(render);
    if (state.section === "packs" && !state.packs) loadPacks();
    render();
  });
});

document.getElementById("search").addEventListener("input", (e) => {
  state.search = e.target.value;
  render();
});

document.getElementById("open-vault").addEventListener("click", () => run("open_folder", { which: "root" }));

document.getElementById("change-location").addEventListener("click", async () => {
  try {
    const folder = await openDialog({ directory: true, multiple: false });
    if (!folder) return;
    const path = typeof folder === "string" ? folder : folder.path;
    confirmRun(
      "Move the vault here?",
      `Everything in the vault — worlds and backups — will be moved to ${path}. ` +
      "The old copy is only removed once every file has arrived safely. " +
      "If that folder already holds a vault, the app will just start using it instead.",
      "Yes, use this folder", "set_vault_location", { path });
  } catch (e) {
    banner(String(e), "error");
  }
});

listen("progress", (event) => {
  const { done, total, current } = event.payload;
  const verb = state.section === "realms" ? "Downloading" : "Saving";
  showProgress(current ? `${verb} "${current}"…` : "Finishing…", done, total);
});

/// Watch for Minecraft opening or closing. Only drives the on-screen warning:
/// every operation re-checks the guard in the backend, so this is deliberately
/// lazy — a slow timer plus a check when the user returns to the window.
async function watchGame() {
  if (state.busy || !state.data || document.hidden) return;
  try {
    const running = await invoke("game_status");
    const was = state.data.game_running.length > 0;
    const now = running.length > 0;
    if (was === now) return;
    state.data.game_running = running;
    if (now) {
      banner("Minecraft is open. Close the game before moving worlds around, so nothing gets damaged.", "");
      document.getElementById("status").textContent = "Minecraft is open — close it to move worlds";
      document.getElementById("status").className = "status warn";
      render();
    } else {
      await load();
    }
  } catch (e) {
    console.error("game poll failed", e);
  }
}

setInterval(watchGame, 30000);
window.addEventListener("focus", watchGame);

pickWallpaper();
setUpZoom();

load()
  .then(loadProfile)
  .then(loadRealms)
  .then(loadPacks)
  .catch((e) => banner(String(e), "error"));
