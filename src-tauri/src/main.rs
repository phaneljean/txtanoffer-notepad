// TxtAnOffer Notepad — a desk notepad for your deals.
//
// The pad is the product: a small window that sits on the member's desk,
// opens instantly, and takes notes with NO network and NO sign-in. Notes
// autosave to this computer on every change (app config dir, deals.json).
//
// Linking a TxtAnOffer account is a layer on top. The member pastes a
// signed dashboard link once (they text DASHBOARD to (833) 897-0333 to
// get one); the app stores the phone/expires/sig trio and then speaks to
// the SAME /notepad routes the website itself uses — form/JSON POSTs with
// the trio in the query string — to push notes up and pull Deal Sheets
// down. The deep views (Deal Sheet detail, action items, TC Check) open
// in a separate "web" window inside the app; capturing a note never
// depends on them, and links off txtanoffer.com open in the real browser.
//
// The pad also lives in the system tray and answers to a global hotkey
// (Ctrl+Shift+N, or Cmd+Shift+N on macOS): it is meant to be summoned,
// scribbled in, and dismissed — not to be another browser tab.

// Prevents an additional console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::HashMap;
use std::fs;
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{
    menu::{AboutMetadata, CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, Url, WebviewUrl, WebviewWindowBuilder,
};
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};
use tauri_plugin_opener::OpenerExt;
use uuid::Uuid;

const SITE: &str = "https://txtanoffer.com";
const PAD_LABEL: &str = "main";
const WEB_LABEL: &str = "web";
const AOT_ID: &str = "always_on_top";
const TRAY_AOT_ID: &str = "tray_always_on_top";

// ---------------------------------------------------------------------------
// Local data: settings, the account link, and the deals store — three
// small JSON files in the app config dir. Notes are the member's work
// product, so writes go through a temp file + rename.
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Default, Clone, Copy)]
struct Settings {
    always_on_top: bool,
    start_in_tray: bool,
}

#[derive(Serialize, Deserialize, Clone)]
struct Link {
    phone: String,
    expires: String,
    sig: String,
}

#[derive(Serialize, Deserialize, Clone)]
struct LocalDeal {
    id: String,
    title: String,
    /// "TX" or "CO" — the form family this deal drafts on.
    state: String,
    note: String,
    /// TxtAnOffer transaction id once the deal exists server-side.
    server_id: Option<i64>,
    /// The note text as last pushed; the server keeps append-only
    /// versions, so sync = append when the text moved on.
    last_pushed: String,
    /// Deal Sheet pulled from the server (window.NP payload), if any:
    /// { "fields": {...}, "defs": [[key, label], ...] }.
    sheet: Option<serde_json::Value>,
    updated_at: String,
}

#[derive(Serialize, Deserialize, Default, Clone)]
struct Store {
    deals: Vec<LocalDeal>,
}

struct AppState {
    settings: Mutex<Settings>,
    link: Mutex<Option<Link>>,
    store: Mutex<Store>,
    aot_items: Mutex<Vec<CheckMenuItem<tauri::Wry>>>,
    http: reqwest::Client,
}

fn config_file(app: &AppHandle, name: &str) -> Option<std::path::PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join(name))
}

fn read_json<T: for<'de> Deserialize<'de> + Default>(app: &AppHandle, name: &str) -> T {
    config_file(app, name)
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn write_json<T: Serialize>(app: &AppHandle, name: &str, value: &T) {
    if let Some(path) = config_file(app, name) {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(text) = serde_json::to_string_pretty(value) {
            let tmp = path.with_extension("json.tmp");
            if fs::write(&tmp, text).is_ok() {
                let _ = fs::rename(tmp, path);
            }
        }
    }
}

fn save_settings(app: &AppHandle, s: &Settings) {
    write_json(app, "settings.json", s);
}
fn save_link(app: &AppHandle, l: &Option<Link>) {
    write_json(app, "link.json", l);
}
fn save_store(app: &AppHandle, s: &Store) {
    write_json(app, "deals.json", s);
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

// ---------------------------------------------------------------------------
// The signed trio: every /notepad route takes ?phone&expires&sig, the
// same parameters the website's own signed links carry.
// ---------------------------------------------------------------------------

impl Link {
    fn trio(&self) -> [(&str, &str); 3] {
        [
            ("phone", self.phone.as_str()),
            ("expires", self.expires.as_str()),
            ("sig", self.sig.as_str()),
        ]
    }
    fn signed_url(&self, path: &str) -> Url {
        let mut url = Url::parse(&format!("{SITE}{path}")).expect("static base parses");
        {
            let mut q = url.query_pairs_mut();
            for (k, v) in self.trio() {
                q.append_pair(k, v);
            }
        }
        url
    }
}

/// Extract and validate the trio from a pasted link. Accepts any
/// txtanoffer.com URL that carries all three parameters (a dashboard
/// link, a notepad link — the signature scheme is the same).
fn parse_link(raw: &str) -> Result<Link, String> {
    let url = Url::parse(raw.trim())
        .map_err(|_| "That doesn't look like a link. Copy the full address and try again.".to_string())?;
    if url.scheme() != "https" {
        return Err("The link must start with https://".to_string());
    }
    match url.host_str() {
        Some(h) if h == "txtanoffer.com" || h.ends_with(".txtanoffer.com") => {}
        _ => return Err("That link isn't on txtanoffer.com, so the app won't use it.".to_string()),
    }
    let params: HashMap<String, String> = url.query_pairs().into_owned().collect();
    let get = |k: &str| params.get(k).cloned().filter(|v| !v.is_empty());
    match (get("phone"), get("expires"), get("sig")) {
        (Some(phone), Some(expires), Some(sig)) => Ok(Link { phone, expires, sig }),
        _ => Err(
            "That link is missing its signature pieces. Open the link from your texts in a \
             browser on this computer and copy the full address from the address bar."
                .to_string(),
        ),
    }
}

// ---------------------------------------------------------------------------
// Link routing for WEB windows only (the pad itself is local content):
// txtanoffer.com stays in the app; anything else goes to the real browser
// or the OS handler (mailto:/tel:/sms:).
// ---------------------------------------------------------------------------

fn is_app_url(url: &Url) -> bool {
    match url.scheme() {
        "about" | "data" | "blob" => true,
        "http" | "https" => match url.host_str() {
            Some(host) => host == "txtanoffer.com" || host.ends_with(".txtanoffer.com"),
            None => false,
        },
        _ => false,
    }
}

fn open_externally(app: &AppHandle, url: &Url) {
    match url.scheme() {
        "http" | "https" | "mailto" | "tel" | "sms" => {
            let _ = app.opener().open_url(url.as_str(), None::<&str>);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Sync: the app speaks to the website's existing /notepad routes exactly
// the way the website's own pages do — trio in the query string, form
// POST to create a deal (the route answers 302 -> /notepad/<id>), JSON
// POST {"raw_text": ...} to append a note (the server keeps append-only
// versions), and plain GETs of the list/detail pages to pull deals and
// Deal Sheets down (the detail page embeds its sheet as window.NP JSON
// and the current note in its textarea; the app reads those, nothing
// more exotic exists server-side).
// ---------------------------------------------------------------------------

const EXPIRED_MARKER: &str = "expired or is invalid";
const UPSELL_MARKER: &str = "part of the Agent plan";

struct SyncErr(String);
type SyncResult<T> = Result<T, SyncErr>;

fn err<T>(msg: impl Into<String>) -> SyncResult<T> {
    Err(SyncErr(msg.into()))
}

fn check_page(body: &str) -> SyncResult<()> {
    if body.contains(EXPIRED_MARKER) {
        return err(
            "Your TxtAnOffer link has expired. Text DASHBOARD to (833) 897-0333 for a fresh \
             link, then link your account again.",
        );
    }
    Ok(())
}

async fn get_page(state: &AppState, link: &Link, path: &str) -> SyncResult<String> {
    let resp = state
        .http
        .get(link.signed_url(path))
        .send()
        .await
        .map_err(|e| SyncErr(format!("Couldn't reach txtanoffer.com: {e}")))?;
    let body = resp
        .text()
        .await
        .map_err(|e| SyncErr(format!("Couldn't read txtanoffer.com's reply: {e}")))?;
    check_page(&body)?;
    Ok(body)
}

/// Create the deal server-side; returns the new transaction id.
async fn server_create_deal(state: &AppState, link: &Link, deal: &LocalDeal) -> SyncResult<i64> {
    let resp = state
        .http
        .post(link.signed_url("/notepad"))
        .form(&[("title", deal.title.as_str()), ("state", deal.state.as_str())])
        .send()
        .await
        .map_err(|e| SyncErr(format!("Couldn't reach txtanoffer.com: {e}")))?;
    if let Some(loc) = resp.headers().get(reqwest::header::LOCATION) {
        let loc = loc.to_str().unwrap_or_default();
        if let Some(id) = loc
            .split('/')
            .filter(|s| !s.is_empty())
            .last()
            .and_then(|tail| tail.split('?').next())
            .and_then(|n| n.parse::<i64>().ok())
        {
            if loc.contains("/notepad/") {
                return Ok(id);
            }
        }
    }
    let body = resp.text().await.unwrap_or_default();
    check_page(&body)?;
    if body.contains(UPSELL_MARKER) {
        return err(
            "This account doesn't have the Deal Notepad yet — it's part of the Agent plan, \
             so the website won't take new deals from it.",
        );
    }
    err("The website didn't create the deal. Try Sync again, or open your notepad on the web to check the account.")
}

/// Append the current note text as a new version on the server deal.
async fn server_append_note(
    state: &AppState,
    link: &Link,
    server_id: i64,
    text: &str,
) -> SyncResult<()> {
    let path = format!("/notepad/{server_id}/notes");
    let resp = state
        .http
        .post(link.signed_url(&path))
        .json(&serde_json::json!({ "raw_text": text }))
        .send()
        .await
        .map_err(|e| SyncErr(format!("Couldn't reach txtanoffer.com: {e}")))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if status.is_success() && body.contains("\"ok\"") {
        return Ok(());
    }
    check_page(&body)?;
    if status.as_u16() == 403 || body.contains(UPSELL_MARKER) {
        return err("This account doesn't have the Deal Notepad (Agent plan), so the note couldn't be sent.");
    }
    if status.as_u16() == 429 {
        return err("The website asked us to slow down. Wait a moment and Sync again.");
    }
    err(format!("The website didn't take the note (HTTP {status}). Your words are still saved on this computer."))
}

// --- Pull parsing (the list and detail pages are server-rendered HTML) ---

fn html_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let semi = tail.find(';').filter(|&j| j <= 10);
        match semi {
            Some(j) => {
                let entity = &tail[1..j];
                let decoded: Option<String> = match entity {
                    "amp" => Some("&".into()),
                    "lt" => Some("<".into()),
                    "gt" => Some(">".into()),
                    "quot" => Some("\"".into()),
                    "apos" => Some("'".into()),
                    "nbsp" => Some(" ".into()),
                    "middot" => Some("·".into()),
                    "hellip" => Some("…".into()),
                    _ if entity.starts_with('#') => entity[1..]
                        .parse::<u32>()
                        .ok()
                        .and_then(char::from_u32)
                        .map(|c| c.to_string()),
                    _ => None,
                };
                match decoded {
                    Some(d) => {
                        out.push_str(&d);
                        rest = &tail[j + 1..];
                    }
                    None => {
                        out.push('&');
                        rest = &tail[1..];
                    }
                }
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

struct ServerDeal {
    id: i64,
    title: String,
    state: String,
}

/// One card per deal on GET /notepad: an <a href="/notepad/<id>?...">
/// wrapping a .tx-title div and a .tx-meta line ("TX · updated ...").
fn parse_deal_list(html: &str) -> Vec<ServerDeal> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(pos) = rest.find("/notepad/") {
        let after = &rest[pos + "/notepad/".len()..];
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            rest = after;
            continue;
        }
        // Only card links are followed by a tx-title within a short span.
        let window = &after[..after.len().min(1200)];
        let title = find_between(window, "tx-title\">", "</div>")
            .map(|t| html_unescape(t.trim()))
            .filter(|t| !t.is_empty());
        if let Some(title) = title {
            let meta = find_between(window, "tx-meta\">", "</div>").unwrap_or_default();
            let state = if meta.trim_start().starts_with("CO") { "CO" } else { "TX" }.to_string();
            if let Ok(id) = digits.parse::<i64>() {
                if !out.iter().any(|d: &ServerDeal| d.id == id) {
                    out.push(ServerDeal { id, title, state });
                }
            }
        }
        rest = &after[digits.len()..];
    }
    out
}

fn find_between<'a>(hay: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let i = hay.find(start)? + start.len();
    let j = hay[i..].find(end)? + i;
    Some(&hay[i..j])
}

/// The detail page's current note + Deal Sheet payload.
fn parse_detail(html: &str) -> (String, Option<serde_json::Value>) {
    let note = find_between(html, "id=\"npNote\"", "</textarea>")
        .and_then(|chunk| chunk.find('>').map(|i| &chunk[i + 1..]))
        .map(|t| html_unescape(t))
        .unwrap_or_default();
    let sheet = find_between(html, "window.NP=", "</script>")
        .and_then(|json| {
            let json = json.trim().trim_end_matches(';');
            serde_json::from_str::<serde_json::Value>(json).ok()
        })
        .and_then(|np| {
            let fields = np.get("fields")?.clone();
            let defs = np.get("defs")?.clone();
            Some(serde_json::json!({ "fields": fields, "defs": defs }))
        });
    (note, sheet)
}

// ---------------------------------------------------------------------------
// Commands the pad UI calls. Local operations never touch the network;
// sync operations say plainly what happened.
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct AppView {
    deals: Vec<LocalDeal>,
    linked: bool,
    settings: Settings,
}

fn view(state: &AppState) -> AppView {
    AppView {
        deals: state.store.lock().expect("store lock").deals.clone(),
        linked: state.link.lock().expect("link lock").is_some(),
        settings: *state.settings.lock().expect("settings lock"),
    }
}

#[tauri::command]
fn get_state(app: AppHandle) -> AppView {
    view(&app.state::<AppState>())
}

#[tauri::command]
fn create_deal(app: AppHandle, title: String, deal_state: String) -> AppView {
    let state = app.state::<AppState>();
    let deal = LocalDeal {
        id: Uuid::new_v4().to_string(),
        title: if title.trim().is_empty() { "Untitled deal".into() } else { title.trim().to_string() },
        state: if deal_state == "CO" { "CO".into() } else { "TX".into() },
        note: String::new(),
        server_id: None,
        last_pushed: String::new(),
        sheet: None,
        updated_at: now_iso(),
    };
    {
        let mut store = state.store.lock().expect("store lock");
        store.deals.insert(0, deal);
        save_store(&app, &store);
    }
    view(&state)
}

#[tauri::command]
fn update_note(app: AppHandle, deal_id: String, text: String) -> AppView {
    let state = app.state::<AppState>();
    {
        let mut store = state.store.lock().expect("store lock");
        if let Some(d) = store.deals.iter_mut().find(|d| d.id == deal_id) {
            d.note = text.chars().take(20000).collect();
            d.updated_at = now_iso();
        }
        save_store(&app, &store);
    }
    view(&state)
}

#[tauri::command]
fn rename_deal(app: AppHandle, deal_id: String, title: String) -> AppView {
    let state = app.state::<AppState>();
    {
        let mut store = state.store.lock().expect("store lock");
        if let Some(d) = store.deals.iter_mut().find(|d| d.id == deal_id) {
            d.title = title.trim().chars().take(120).collect();
            d.updated_at = now_iso();
        }
        save_store(&app, &store);
    }
    view(&state)
}

#[tauri::command]
fn delete_deal(app: AppHandle, deal_id: String) -> AppView {
    let state = app.state::<AppState>();
    {
        let mut store = state.store.lock().expect("store lock");
        store.deals.retain(|d| d.id != deal_id);
        save_store(&app, &store);
    }
    view(&state)
}

#[tauri::command]
fn set_settings(app: AppHandle, always_on_top: Option<bool>, start_in_tray: Option<bool>) -> AppView {
    let state = app.state::<AppState>();
    let current = {
        let mut settings = state.settings.lock().expect("settings lock");
        if let Some(v) = always_on_top {
            settings.always_on_top = v;
        }
        if let Some(v) = start_in_tray {
            settings.start_in_tray = v;
        }
        save_settings(&app, &settings);
        *settings
    };
    if always_on_top.is_some() {
        apply_always_on_top(&app, current.always_on_top);
    }
    view(&state)
}

#[tauri::command]
async fn link_account(app: AppHandle, url: String) -> Result<AppView, String> {
    let link = parse_link(&url)?;
    let state = app.state::<AppState>();
    // Prove the link works before storing it: the list page must render.
    let body = get_page(&state, &link, "/notepad")
        .await
        .map_err(|e| e.0)?;
    if body.contains(UPSELL_MARKER) && !body.contains("tx-card") {
        return Err(
            "This account doesn't have the Deal Notepad yet — it's part of the Agent plan. \
             The pad still works on its own; linking needs the plan."
                .to_string(),
        );
    }
    {
        let mut stored = state.link.lock().expect("link lock");
        *stored = Some(link);
        save_link(&app, &stored);
    }
    Ok(view(&state))
}

#[tauri::command]
fn unlink_account(app: AppHandle) -> AppView {
    let state = app.state::<AppState>();
    {
        let mut stored = state.link.lock().expect("link lock");
        *stored = None;
        save_link(&app, &stored);
    }
    view(&state)
}

/// Push one deal: create it server-side if needed, then append the note
/// if it changed since the last push. Returns a human status line.
async fn push_deal(app: &AppHandle, state: &AppState, link: &Link, deal_id: &str) -> Result<String, String> {
    let mut deal = {
        let store = state.store.lock().expect("store lock");
        store.deals.iter().find(|d| d.id == deal_id).cloned()
    }
    .ok_or_else(|| "That deal is gone.".to_string())?;

    if deal.server_id.is_none() {
        let id = server_create_deal(state, link, &deal).await.map_err(|e| e.0)?;
        deal.server_id = Some(id);
    }
    let server_id = deal.server_id.expect("set above");
    let mut pushed = false;
    if !deal.note.trim().is_empty() && deal.note != deal.last_pushed {
        server_append_note(state, link, server_id, &deal.note)
            .await
            .map_err(|e| e.0)?;
        deal.last_pushed = deal.note.clone();
        pushed = true;
    }
    deal.updated_at = now_iso();
    {
        let mut store = state.store.lock().expect("store lock");
        if let Some(d) = store.deals.iter_mut().find(|d| d.id == deal_id) {
            d.server_id = deal.server_id;
            d.last_pushed = deal.last_pushed.clone();
            d.updated_at = deal.updated_at.clone();
        }
        save_store(app, &store);
    }
    Ok(if pushed {
        format!("\"{}\" synced to TxtAnOffer.", deal.title)
    } else {
        format!("\"{}\" is already in sync.", deal.title)
    })
}

#[tauri::command]
async fn sync_deal(app: AppHandle, deal_id: String) -> Result<String, String> {
    let state = app.state::<AppState>();
    let link = state
        .link
        .lock()
        .expect("link lock")
        .clone()
        .ok_or_else(|| "Link your TxtAnOffer account first.".to_string())?;
    let msg = push_deal(&app, &state, &link, &deal_id).await?;
    Ok(msg)
}

#[tauri::command]
async fn sync_all(app: AppHandle) -> Result<String, String> {
    let state = app.state::<AppState>();
    let link = state
        .link
        .lock()
        .expect("link lock")
        .clone()
        .ok_or_else(|| "Link your TxtAnOffer account first.".to_string())?;
    let ids: Vec<String> = {
        let store = state.store.lock().expect("store lock");
        store.deals.iter().map(|d| d.id.clone()).collect()
    };
    let mut synced = 0usize;
    for id in &ids {
        push_deal(&app, &state, &link, id).await?;
        synced += 1;
    }
    Ok(format!("{synced} deal(s) synced."))
}

/// Pull the account's deals down: match by server id, refresh titles, and
/// for each, fetch the detail page for the current note + Deal Sheet.
/// A local note with unsent edits is never overwritten — sync it first.
#[tauri::command]
async fn pull_deals(app: AppHandle) -> Result<String, String> {
    let state = app.state::<AppState>();
    let link = state
        .link
        .lock()
        .expect("link lock")
        .clone()
        .ok_or_else(|| "Link your TxtAnOffer account first.".to_string())?;
    let list_html = get_page(&state, &link, "/notepad").await.map_err(|e| e.0)?;
    let server_deals = parse_deal_list(&list_html);
    let mut pulled = 0usize;
    for sd in &server_deals {
        let detail = get_page(&state, &link, &format!("/notepad/{}", sd.id))
            .await
            .map_err(|e| e.0)?;
        let (server_note, sheet) = parse_detail(&detail);
        {
            let mut store = state.store.lock().expect("store lock");
            match store.deals.iter_mut().find(|d| d.server_id == Some(sd.id)) {
                Some(local) => {
                    local.title = sd.title.clone();
                    local.state = sd.state.clone();
                    if local.note == local.last_pushed {
                        local.note = server_note.clone();
                        local.last_pushed = server_note;
                    }
                    if sheet.is_some() {
                        local.sheet = sheet;
                    }
                    local.updated_at = now_iso();
                }
                None => {
                    store.deals.push(LocalDeal {
                        id: Uuid::new_v4().to_string(),
                        title: sd.title.clone(),
                        state: sd.state.clone(),
                        note: server_note.clone(),
                        server_id: Some(sd.id),
                        last_pushed: server_note,
                        sheet,
                        updated_at: now_iso(),
                    });
                }
            }
            save_store(&app, &store);
        }
        pulled += 1;
    }
    Ok(format!("{pulled} deal(s) pulled from TxtAnOffer."))
}

// ---------------------------------------------------------------------------
// The "web" window: deep views (a deal's Deal Sheet page, the deals list)
// from the member's linked account. Capture never happens here — this is
// purely where Deal Sheets, action items and TC Check live. Links off
// txtanoffer.com leave the app for the real browser.
// ---------------------------------------------------------------------------

fn open_web(app: &AppHandle, url: Url, title: &str) {
    if let Some(w) = app.get_webview_window(WEB_LABEL) {
        let _ = w.navigate(url);
        let _ = w.set_title(title);
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    let nav = app.clone();
    let new_win = app.clone();
    let _ = WebviewWindowBuilder::new(app, WEB_LABEL, WebviewUrl::External(url))
        .title(title)
        .inner_size(900.0, 840.0)
        .min_inner_size(420.0, 560.0)
        .on_navigation(move |url| {
            if is_app_url(url) {
                true
            } else {
                open_externally(&nav, url);
                false
            }
        })
        .on_new_window(move |url, _features| {
            open_externally(&new_win, &url);
            tauri::webview::NewWindowResponse::Deny
        })
        .build();
}

#[tauri::command]
fn open_deal_sheet(app: AppHandle, deal_id: String) -> Result<(), String> {
    let state = app.state::<AppState>();
    let link = state
        .link
        .lock()
        .expect("link lock")
        .clone()
        .ok_or_else(|| "Link your TxtAnOffer account first.".to_string())?;
    let deal = {
        let store = state.store.lock().expect("store lock");
        store.deals.iter().find(|d| d.id == deal_id).cloned()
    }
    .ok_or_else(|| "That deal is gone.".to_string())?;
    let server_id = deal.server_id.ok_or_else(|| {
        "Sync this deal first — it doesn't exist on TxtAnOffer yet.".to_string()
    })?;
    open_web(&app, link.signed_url(&format!("/notepad/{server_id}")), "Deal Sheet — TxtAnOffer");
    Ok(())
}

#[tauri::command]
fn open_deals_web(app: AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let link = state
        .link
        .lock()
        .expect("link lock")
        .clone()
        .ok_or_else(|| "Link your TxtAnOffer account first.".to_string())?;
    open_web(&app, link.signed_url("/notepad"), "My Deals — TxtAnOffer");
    Ok(())
}

// ---------------------------------------------------------------------------
// Desk behavior: summon/dismiss, always-on-top, tray.
// ---------------------------------------------------------------------------

fn toggle_pad(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(PAD_LABEL) {
        if w.is_visible().unwrap_or(false) && w.is_focused().unwrap_or(false) {
            let _ = w.hide();
        } else {
            let _ = w.show();
            let _ = w.set_focus();
        }
    }
}

fn apply_always_on_top(app: &AppHandle, value: bool) {
    if let Some(w) = app.get_webview_window(PAD_LABEL) {
        let _ = w.set_always_on_top(value);
    }
    let state = app.state::<AppState>();
    for item in state.aot_items.lock().expect("aot lock").iter() {
        let _ = item.set_checked(value);
    }
}

fn toggle_always_on_top(app: &AppHandle) {
    let state = app.state::<AppState>();
    let value = {
        let mut settings = state.settings.lock().expect("settings lock");
        settings.always_on_top = !settings.always_on_top;
        save_settings(app, &settings);
        settings.always_on_top
    };
    apply_always_on_top(app, value);
}

fn show_link_panel(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(PAD_LABEL) {
        let _ = w.show();
        let _ = w.set_focus();
        let _ = w.emit("open-link-panel", ());
    }
}

// ---------------------------------------------------------------------------
// Menus: the native app menu (About, account actions, Edit so the pad's
// text fields get their usual shortcuts) and the tray menu.
// ---------------------------------------------------------------------------

fn build_app_menu(app: &AppHandle, aot_checked: bool) -> tauri::Result<(Menu<tauri::Wry>, CheckMenuItem<tauri::Wry>)> {
    let about = PredefinedMenuItem::about(
        app,
        Some("About TxtAnOffer Notepad"),
        Some(AboutMetadata {
            name: Some("TxtAnOffer Notepad".to_string()),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            comments: Some(
                "A desk notepad for your deals. Notes save on this computer as you type; \
                 link a TxtAnOffer account to turn them into Deal Sheets, drafts and TC Checks."
                    .to_string(),
            ),
            website: Some("https://txtanoffer.com".to_string()),
            website_label: Some("txtanoffer.com".to_string()),
            ..Default::default()
        }),
    )?;
    let link_item = MenuItem::with_id(app, "menu_link", "Link My TxtAnOffer Account…", true, None::<&str>)?;
    let browser_item = MenuItem::with_id(app, "menu_browser", "Open TxtAnOffer in Browser", true, None::<&str>)?;
    let aot = CheckMenuItem::with_id(app, AOT_ID, "Always on Top", true, aot_checked, Some("CmdOrCtrl+Shift+T"))?;
    let quit = PredefinedMenuItem::quit(app, Some("Quit TxtAnOffer Notepad"))?;
    let app_menu = Submenu::with_items(
        app,
        "TxtAnOffer Notepad",
        true,
        &[
            &about,
            &PredefinedMenuItem::separator(app)?,
            &link_item,
            &browser_item,
            &PredefinedMenuItem::separator(app)?,
            &aot,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;
    let edit_menu = Submenu::with_items(
        app,
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(app, None)?,
            &PredefinedMenuItem::redo(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::cut(app, None)?,
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
        ],
    )?;
    Ok((Menu::with_items(app, &[&app_menu, &edit_menu])?, aot))
}

fn build_tray(app: &AppHandle, aot_checked: bool) -> tauri::Result<CheckMenuItem<tauri::Wry>> {
    let toggle = MenuItem::with_id(app, "tray_toggle", "Show / Hide Pad", true, None::<&str>)?;
    let aot = CheckMenuItem::with_id(app, TRAY_AOT_ID, "Always on Top", true, aot_checked, None::<&str>)?;
    let web = MenuItem::with_id(app, "tray_web", "Open My Deals (TxtAnOffer)", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "tray_quit", "Quit TxtAnOffer Notepad", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&toggle, &aot, &web, &PredefinedMenuItem::separator(app)?, &quit])?;
    let icon = app
        .default_window_icon()
        .cloned()
        .expect("app icon is configured");
    TrayIconBuilder::with_id("tray")
        .icon(icon)
        .tooltip("TxtAnOffer Notepad")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "tray_toggle" => toggle_pad(app),
            TRAY_AOT_ID => toggle_always_on_top(app),
            "tray_web" => {
                if open_deals_web(app.clone()).is_err() {
                    app.dialog()
                        .message("Link your TxtAnOffer account first — open the pad and choose \"Link my TxtAnOffer account\".")
                        .title("TxtAnOffer Notepad")
                        .kind(MessageDialogKind::Info)
                        .show(|_| {});
                    toggle_pad(app);
                }
            }
            "tray_quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_pad(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(aot)
}

// ---------------------------------------------------------------------------

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state == ShortcutState::Pressed {
                        toggle_pad(app);
                    }
                })
                .build(),
        )
        .setup(|app| {
            let handle = app.handle().clone();
            let settings: Settings = read_json(&handle, "settings.json");
            let link: Option<Link> = read_json(&handle, "link.json");
            let store: Store = read_json(&handle, "deals.json");
            let http = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(20))
                .user_agent("TxtAnOfferNotepad/0.1 (desktop notepad; +https://txtanoffer.com)")
                .build()
                .expect("http client builds");
            app.manage(AppState {
                settings: Mutex::new(settings),
                link: Mutex::new(link),
                store: Mutex::new(store),
                aot_items: Mutex::new(Vec::new()),
                http,
            });

            let (menu, menu_aot) = build_app_menu(&handle, settings.always_on_top)?;
            app.set_menu(menu)?;
            let tray_aot = build_tray(&handle, settings.always_on_top)?;
            {
                let state = app.state::<AppState>();
                let mut items = state.aot_items.lock().expect("aot lock");
                items.push(menu_aot);
                items.push(tray_aot);
            }

            // Desk hotkey: Ctrl+Shift+N (and Cmd+Shift+N on macOS).
            let _ = handle.global_shortcut().register(
                Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyN),
            );
            let _ = handle.global_shortcut().register(
                Shortcut::new(Some(Modifiers::SUPER | Modifiers::SHIFT), Code::KeyN),
            );

            let pad = WebviewWindowBuilder::new(app, PAD_LABEL, WebviewUrl::App("index.html".into()))
                .title("TxtAnOffer Notepad")
                .inner_size(420.0, 760.0)
                .min_inner_size(360.0, 520.0)
                .resizable(true)
                .always_on_top(settings.always_on_top)
                .visible(!settings.start_in_tray)
                .build()?;
            // Closing the window parks the pad in the tray; Quit is Quit.
            let pad_clone = pad.clone();
            pad.on_window_event(move |event| {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = pad_clone.hide();
                }
            });
            Ok(())
        })
        .on_menu_event(|app, event| match event.id().as_ref() {
            "menu_link" => show_link_panel(app),
            "menu_browser" => {
                if let Ok(url) = Url::parse(SITE) {
                    open_externally(app, &url);
                }
            }
            AOT_ID => toggle_always_on_top(app),
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            create_deal,
            update_note,
            rename_deal,
            delete_deal,
            set_settings,
            link_account,
            unlink_account,
            sync_deal,
            sync_all,
            pull_deals,
            open_deal_sheet,
            open_deals_web
        ])
        .run(tauri::generate_context!())
        .expect("error while running TxtAnOffer Notepad");
}
