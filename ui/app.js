"use strict";
const { invoke } = window.__TAURI__.core;
const listen = window.__TAURI__.event.listen;

const $ = (id) => document.getElementById(id);
const els = {
  linkPill: $("linkPill"), linkPanel: $("linkPanel"), linkInput: $("linkInput"),
  linkBtn: $("linkBtn"), linkCancel: $("linkCancel"), unlinkBtn: $("unlinkBtn"),
  linkMsg: $("linkMsg"), dealSelect: $("dealSelect"), newState: $("newState"),
  newDeal: $("newDeal"), padArea: $("padArea"), dealTitle: $("dealTitle"),
  pad: $("pad"), sheetBox: $("sheetBox"), sheetRows: $("sheetRows"),
  syncBtn: $("syncBtn"), pullBtn: $("pullBtn"), webBtn: $("webBtn"),
  deleteBtn: $("deleteBtn"), welcome: $("welcome"), firstDeal: $("firstDeal"),
  welcomeLink: $("welcomeLink"), saveState: $("saveState"), syncState: $("syncState"),
  aotToggle: $("aotToggle"), trayToggle: $("trayToggle"),
};

let view = null;
let currentId = null;
let saveTimer = null;
let deleteArmed = false;

function current() {
  if (!view) return null;
  return view.deals.find((d) => d.id === currentId) || view.deals[0] || null;
}

function fmtTime(iso) {
  try {
    return new Date(iso).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  } catch (_) { return ""; }
}

function fmtValue(key, v) {
  if (v === null || v === undefined || v === "") return "";
  if (typeof v === "number") {
    // Percentage fields store fractions (0.2 = 20%) and must be handled
    // before the money branch: pct keys contain "down"/"earnest", so the
    // money regex below would claim them ("$0.2"). The pulled sheet carries
    // no field kinds (the Rust sync layer keeps only fields+defs from
    // window.NP), so the _pct suffix is the signal; the server's FIELD_KINDS
    // marks exactly two pct fields, down_payment_pct and earnest_pct.
    if (key.endsWith("_pct")) {
      // Mirror format_field_value in notepad.py: f"{value * 100:g}%"
      // (%g = up to 6 significant digits, trailing zeros stripped).
      return parseFloat((v * 100).toPrecision(6)) + "%";
    }
    // _pct keys are excluded here too, so they can never fall into the
    // money branch even if the branches above are ever reordered.
    const money = !key.endsWith("_pct") &&
      /price|amount|fee|earnest|down|credit|concession/.test(key);
    return (money ? "$" : "") + v.toLocaleString("en-US");
  }
  return String(v);
}

function renderSheet(deal) {
  if (!deal || !deal.sheet || !deal.sheet.fields) {
    els.sheetBox.hidden = true;
    els.sheetRows.innerHTML = "";
    return;
  }
  const { fields, defs } = deal.sheet;
  els.sheetRows.innerHTML = "";
  // Built with DOM calls (never innerHTML) so note-derived text stays inert.
  for (const [key, label] of defs || []) {
    const f = fields[key];
    if (!f || f.value === null || f.value === undefined || f.value === "") continue;
    const div = document.createElement("div");
    div.className = "sheetrow";
    const k = document.createElement("span");
    k.className = "k";
    k.textContent = label;
    const v = document.createElement("span");
    v.className = "v";
    v.textContent = fmtValue(key, f.value);
    const chip = document.createElement("span");
    const isConfirm = f.state === "confirm";
    chip.className = "chip " + (isConfirm ? "confirm" : "found");
    chip.textContent = isConfirm ? "Confirm" : "Found";
    v.appendChild(chip);
    div.appendChild(k);
    div.appendChild(v);
    els.sheetRows.appendChild(div);
  }
  els.sheetBox.hidden = els.sheetRows.children.length === 0;
}

function render() {
  if (!view) return;
  const deal = current();
  currentId = deal ? deal.id : null;

  // link pill + sync affordances
  els.linkPill.textContent = view.linked ? "Linked to TxtAnOffer ✓" : "Link my account";
  els.linkPill.classList.toggle("linked", view.linked);
  els.unlinkBtn.hidden = !view.linked;
  els.syncBtn.hidden = !view.linked;
  els.pullBtn.hidden = !view.linked;
  els.webBtn.hidden = !view.linked;

  // settings
  els.aotToggle.checked = !!view.settings.always_on_top;
  els.trayToggle.checked = !!view.settings.start_in_tray;

  // deal switcher
  els.dealSelect.innerHTML = "";
  for (const d of view.deals) {
    const opt = document.createElement("option");
    opt.value = d.id;
    opt.textContent = (d.title || "Untitled deal") + (d.server_id ? "" : " · local");
    els.dealSelect.appendChild(opt);
  }
  if (deal) els.dealSelect.value = deal.id;

  const has = !!deal;
  els.padArea.style.display = has ? "" : "none";
  els.welcome.hidden = has;
  els.dealSelect.disabled = !has;
  if (!has) return;

  if (document.activeElement !== els.dealTitle) els.dealTitle.value = deal.title || "";
  if (document.activeElement !== els.pad) els.pad.value = deal.note || "";
  els.saveState.textContent = deal.updated_at
    ? "Saved on this computer · " + fmtTime(deal.updated_at)
    : "Saved on this computer";
  els.syncState.textContent = !view.linked
    ? ""
    : deal.server_id
      ? (deal.note === deal.last_pushed ? "In sync with TxtAnOffer" : "Edits not synced yet")
      : "Not on TxtAnOffer yet — Sync creates it";
  renderSheet(deal);
}

// --- pad editing: autosave locally on every pause in typing ---
els.pad.addEventListener("input", () => {
  const deal = current();
  if (!deal) return;
  deal.note = els.pad.value;
  els.saveState.textContent = "Saving…";
  clearTimeout(saveTimer);
  saveTimer = setTimeout(async () => {
    view = await invoke("update_note", { dealId: deal.id, text: els.pad.value });
    const d = current();
    els.saveState.textContent = "Saved on this computer · " + fmtTime(d ? d.updated_at : null);
    if (view.linked) render();
  }, 300);
});

els.dealTitle.addEventListener("change", async () => {
  const deal = current();
  if (!deal) return;
  view = await invoke("rename_deal", { dealId: deal.id, title: els.dealTitle.value });
  render();
});

els.dealSelect.addEventListener("change", () => {
  currentId = els.dealSelect.value;
  deleteArmed = false;
  els.deleteBtn.textContent = "Delete deal";
  render();
});

async function newDeal() {
  view = await invoke("create_deal", { title: "", dealState: els.newState.value });
  currentId = view.deals.length ? view.deals[0].id : null;
  render();
  els.dealTitle.focus();
}
els.newDeal.addEventListener("click", newDeal);
els.firstDeal.addEventListener("click", newDeal);

els.deleteBtn.addEventListener("click", async () => {
  const deal = current();
  if (!deal) return;
  if (!deleteArmed) {
    deleteArmed = true;
    els.deleteBtn.textContent = "Confirm delete?";
    setTimeout(() => { deleteArmed = false; els.deleteBtn.textContent = "Delete deal"; }, 3000);
    return;
  }
  view = await invoke("delete_deal", { dealId: deal.id });
  currentId = null;
  deleteArmed = false;
  els.deleteBtn.textContent = "Delete deal";
  render();
});

// --- linking ---
function showLinkPanel(show) {
  els.linkPanel.hidden = !show;
  if (show) { els.linkMsg.textContent = ""; els.linkMsg.className = "msg"; els.linkInput.focus(); }
}
els.linkPill.addEventListener("click", () => showLinkPanel(els.linkPanel.hidden));
els.linkCancel.addEventListener("click", () => showLinkPanel(false));
els.welcomeLink.addEventListener("click", (e) => { e.preventDefault(); showLinkPanel(true); });
listen("open-link-panel", () => showLinkPanel(true));

els.linkBtn.addEventListener("click", async () => {
  els.linkBtn.disabled = true;
  els.linkMsg.className = "msg";
  els.linkMsg.textContent = "Checking the link…";
  try {
    view = await invoke("link_account", { url: els.linkInput.value });
    els.linkMsg.className = "msg ok";
    els.linkMsg.textContent = "Linked. Pulling your deals…";
    render();
    try {
      const msg = await invoke("pull_deals");
      els.syncState.textContent = msg;
    } catch (e) {
      els.syncState.textContent = String(e);
    }
    setTimeout(() => showLinkPanel(false), 900);
  } catch (e) {
    els.linkMsg.className = "msg error";
    els.linkMsg.textContent = String(e);
  } finally {
    els.linkBtn.disabled = false;
  }
});

els.unlinkBtn.addEventListener("click", async () => {
  view = await invoke("unlink_account");
  els.linkMsg.className = "msg ok";
  els.linkMsg.textContent = "Unlinked. Your notes stay on this computer.";
  render();
});

// --- sync + web views ---
els.syncBtn.addEventListener("click", async () => {
  const deal = current();
  if (!deal) return;
  els.syncBtn.disabled = true;
  try {
    const msg = await invoke("sync_deal", { dealId: deal.id });
    view = await invoke("get_state");
    els.syncState.textContent = msg;
    render();
  } catch (e) {
    els.syncState.textContent = String(e);
  } finally {
    els.syncBtn.disabled = false;
  }
});

els.pullBtn.addEventListener("click", async () => {
  els.pullBtn.disabled = true;
  try {
    const msg = await invoke("pull_deals");
    view = await invoke("get_state");
    els.syncState.textContent = msg;
    render();
  } catch (e) {
    els.syncState.textContent = String(e);
  } finally {
    els.pullBtn.disabled = false;
  }
});

els.webBtn.addEventListener("click", async () => {
  const deal = current();
  if (!deal) return;
  try {
    if (deal.server_id) {
      await invoke("open_deal_sheet", { dealId: deal.id });
    } else {
      await invoke("open_deals_web");
    }
  } catch (e) {
    els.syncState.textContent = String(e);
  }
});

// --- settings ---
els.aotToggle.addEventListener("change", async () => {
  view = await invoke("set_settings", { alwaysOnTop: els.aotToggle.checked });
  render();
});
els.trayToggle.addEventListener("change", async () => {
  view = await invoke("set_settings", { startInTray: els.trayToggle.checked });
  render();
});

// --- boot ---
(async () => {
  view = await invoke("get_state");
  if (view.deals.length) currentId = view.deals[0].id;
  render();
})();
