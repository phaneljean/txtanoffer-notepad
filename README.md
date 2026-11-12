# TxtAnOffer Notepad (desktop)

A **desk notepad for your deals**. It sits on your screen — in the tray, one
hotkey away — ready to take notes the second a call ends. It is *not* the
TxtAnOffer website in a window: the pad is bundled with the app, opens
instantly, and works with **no network and no sign-in**. Notes autosave to
your computer on every keystroke.

Linking your TxtAnOffer account is a layer on top of the pad, never a gate
in front of it:

- **Unlinked**, the app is a complete standalone notepad: one pad per deal,
  everything stored locally.
- **Linked**, the same notes sync to TxtAnOffer, where they become Deal
  Sheets, drafts and TC Checks. The app pushes notes up and pulls Deal
  Sheets down; the deep views (Deal Sheet detail, action items, TC Check)
  open inside the app in a separate window, straight from your account.

## How a member uses it

1. Install the app and launch it. Start writing — that's it. No account,
   no page load. Create one pad per deal with **+ New deal**.
2. Summon or dismiss the pad any time with **Ctrl+Shift+N**
   (Cmd+Shift+N on macOS), click the tray icon, or use the tray menu.
   Closing the window parks the pad in the tray instead of quitting;
   **Always on Top** (menu, tray, or Ctrl+Shift+T) keeps it floating over
   your transaction platform and email while you work.
3. Optional — link your account: choose **Link my account**, then
   1. text **DASHBOARD** to **(833) 897-0333**,
   2. open the link you get back *on this computer* and copy the full
      address from your browser,
   3. paste it into the app and choose **Link account**.
   
   The app verifies the link, stores it on this computer only, and pulls
   your existing deals down. From then on, **Sync** pushes a deal's notes
   to TxtAnOffer (creating the deal there the first time), **Pull my
   deals** refreshes notes and Deal Sheets, and **Deal Sheet ↗** opens
   the deal's full page — Deal Sheet, action items, deadlines, TC Check —
   in the app's web window. Links that leave txtanoffer.com open in your
   real browser, not in the app.
4. When the link expires (TxtAnOffer links are time-limited), sync will
   say so plainly. Text DASHBOARD again and re-link. Your local notes are
   never affected.

There is **no custom protocol handler** and no password anywhere in this
flow: the pasted signed link *is* the credential, the same one the
website itself issues.

## What's in the box

- **The pad** (`ui/`): vanilla HTML/CSS/JS, no build step. A deal
  switcher, a ruled writing pad, local autosave, a read-only Deal Sheet
  preview for linked deals, and the linking panel.
- **The shell** (`src-tauri/`, Rust + Tauri 2): window, tray icon, global
  hotkey, native menus, settings persistence, and the sync client. Sync
  speaks to the website's existing `/notepad` routes exactly the way the
  website's own pages do — the signed `phone/expires/sig` trio in the
  query string, a form POST to create a deal (the route answers
  `302 → /notepad/<id>`), a JSON POST `{"raw_text": …}` to append a note
  (the server keeps append-only versions, so sync = append when the text
  changed), and GETs of the list/detail pages to pull deals and Deal
  Sheets down (the detail page embeds its sheet as `window.NP` JSON; the
  app reads that and the page's note field). Nothing was added
  server-side for this app.
- **Local data** lives in the app config dir (`settings.json`,
  `link.json`, `deals.json`), written via temp-file + rename so a crash
  mid-keystroke can't corrupt the store. The stored link is a capability
  URL — treat the machine as trusted, and unlink from the pad before
  giving the computer away.

## Why Tauri (and not Electron)

For a solo founder shipping a shell whose real product is a small local
UI plus a sync client:

- **Installer size.** Tauri renders with the OS webview (WebView2 on
  Windows, WKWebView on macOS, WebKitGTK on Linux): the Windows, macOS
  and .deb installers land in the ~5–15 MB range (the first Linux build
  produced here: a 4.6 MB .deb). Electron ships a whole Chromium per app
  (~80–150 MB installers) — a heavy download to ask of a TC on a brokerage
  laptop, and a heavier update every time. The one exception is the
  portable Linux AppImage (~80 MB), which carries its WebKit runtime
  with it — that's the price of running anywhere with no install.
- **Maintenance.** One small Rust crate, no Node runtime in the shipped
  app, no Chromium security-update treadmill. The attack surface a solo
  maintainer must watch is mostly the website itself.
- **Cross-build story.** Neither stack cross-compiles installers well;
  both need per-OS runners. Tauri's bundler targets (NSIS, DMG, DEB,
  AppImage) cover everything from one config, driven by GitHub Actions
  (see below).
- **Costs accepted.** The OS webviews differ subtly (that's why the pad
  UI is deliberately simple and framework-free), WebView2 is a Windows
  prerequisite (the NSIS config uses the download bootstrapper, so a
  machine without it fetches it during install), and the Linux build
  needs the WebKitGTK dev stack on the build machine.

## Building

Prerequisites: stable Rust, Node 20 (for the Tauri CLI, pinned by
`package-lock.json`), and on Linux:

```sh
sudo apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev \
  libayatana-appindicator3-dev librsvg2-dev libxdo-dev patchelf
```

```sh
npm ci
npm run dev     # run the app locally (tauri dev)
npm run build   # build this platform's installers into src-tauri/target/release/bundle/
```

`npm run icon` regenerates `src-tauri/icons/` from
`src-tauri/app-icon.png` (the brand tile; the shipped icon set was
produced from the site's `static/logo.png`).

### Releases: GitHub Actions

`.github/workflows/build.yml` builds all three platforms on GitHub
runners — the only sane way to produce Windows and macOS installers from
a Linux desk. Run it from the Actions tab (**Build installers → Run
workflow**) or push a version tag (`v0.1.0`); each job uploads its
installers as artifacts (`txtanoffer-notepad-windows` — NSIS `.exe`,
`txtanoffer-notepad-macos` — `.dmg`, `txtanoffer-notepad-linux` — `.deb`
+ `.AppImage`). Pushing this project to a GitHub repo is all it takes;
no secrets are needed until signing (below).

## Signing, notarization, and getting to "download it from txtanoffer.com"

What exists today produces **unsigned** installers. Between here and a
member downloading the app from the site:

1. **Push this folder to a GitHub repo** and run the Actions workflow —
   that's where the real Windows/macOS installers come from (this dev
   machine is Linux and only produces the Linux artifacts).
2. **macOS signing + notarization** — required for a Mac install without
   scary warnings: Apple Developer Program, **$99/year**. Then add the
   Developer ID certificate + notarization credentials as Actions
   secrets (`APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`,
   `APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_PASSWORD`,
   `APPLE_TEAM_ID`) and the Tauri bundler signs and notarizes the DMG.
3. **Windows code signing** — without it, SmartScreen warns on first run
   ("Windows protected your PC"). An OV code-signing certificate runs
   roughly **$200–400/year** from a public CA (EV certs cost more and
   clear SmartScreen reputation faster). Wire it into the workflow via
   `TAURI_SIGNING_*`/certificate secrets when purchased.
4. **A download page** on txtanoffer.com linking the three installers
   (and release hosting — GitHub Releases is fine to start). That page is
   a website change, deliberately not part of this project.
5. **Real-device testing**: first installs on an actual Windows laptop
   and an actual Mac — webview behavior, the tray, and the hotkey can
   only be signed off there. The global hotkey is X11-oriented on Linux;
   some Wayland compositors restrict global shortcuts.

## Deliberately not in v1

- **No auto-update.** The config has no updater block; members reinstall
  to update. When the app has real users, add Tauri's updater plugin
  with a signed update feed as a focused step.
- **No telemetry.** The app phones home only when the member syncs.
- **No protocol handler** (`txtanoffer://`) — considered and skipped;
  copy-paste linking is honest, visible, and enough.
- **Enhance / fields editing in the pad.** Deal Sheet field editing,
  action items and TC Check live in the web window on purpose: the pad
  is for capture, the account is for processing.
