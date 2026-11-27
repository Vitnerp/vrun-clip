# vrun.clip

Fast, searchable clipboard history for Windows and macOS. Press **Ctrl+Shift+V** (macOS: **⌘⇧V**) anywhere, type a few letters, hit **Enter** and it is pasted.

- Remembers text, rich text (HTML/RTF), images and screenshots (4K too), and files copied in Explorer
- Instant full-text search: substring, case- and diacritics-insensitive (`zlutou` finds `žluťoučký`)
- Never blocks other apps: the clipboard is held for milliseconds, heavy work happens in the background (the Snipping Tool problem some clipboard managers have)
- Skips content that apps mark as private (password managers) plus a configurable ignore list
- Lives in the tray / menu bar, starts at login, small installer, no admin rights needed
- Everything stays on your computer (`%LOCALAPPDATA%\com.vrun.clip`, macOS: `~/Library/Application Support/com.vrun.clip`)

## Install

Download `vrun.clip_<version>_x64-setup.exe` from the [Releases](../../releases) page and run it.

> Windows SmartScreen may say *"Windows protected your PC"* because the installer is not code-signed yet.
> Click **More info → Run anyway**.

### macOS (11 Big Sur or newer, Apple Silicon and Intel)

1. Download `vrun.clip_<version>_universal.dmg`, open it and drag **vrun.clip** into **Applications**.
2. Start it. Because the app is not notarized by Apple yet, macOS blocks the first launch:
   open **System Settings → Privacy & Security**, scroll down and click **Open Anyway**.
3. vrun.clip appears in the menu bar (no Dock icon). Press **⌘⇧V** anywhere.
4. The first time you paste, macOS asks to allow vrun.clip under **Privacy & Security → Accessibility**.
   This is needed to send ⌘V to the app you were in. Without it, items are only copied.

> Until the app is signed with an Apple Developer ID, macOS may ask for the Accessibility permission again after an update.

## Use

| Key | Action |
| --- | --- |
| `Ctrl+Shift+V` | Open / close the popup (change it in Settings) |
| type | Search |
| `↑` `↓` `PgUp` `PgDn` | Move |
| `Enter` / double-click | Paste into the app you were in |
| `Shift+Enter` | Paste as plain text |
| `Ctrl+Enter` / `Ctrl+C` | Copy only (don't paste) |
| `Ctrl+1` … `Ctrl+9` | Paste item 1–9 |
| `Ctrl+P` | Pin / unpin (pinned items are never deleted) |
| `Del` | Delete item |
| `Ctrl+,` | Settings |
| `Esc` | Clear search, then close |

On macOS use ⌘ wherever the table says Ctrl.

Ctrl+Shift+V (⌘⇧V on macOS) is "paste as plain text" in some apps (browsers, Slack, VS Code). vrun.clip takes it over globally; use **Shift+Enter** in the popup for plain text, or pick another shortcut in Settings.

The tray / menu bar icon has *Pause recording*, *Start with Windows* (*Start at login* on macOS), *Settings*, *Check for updates* and *Quit*.

> Pasting into apps that run **as administrator** only works when vrun.clip also runs as administrator (a Windows security rule). The item is still copied, so you can press Ctrl+V yourself.

## Develop

Requirements: [Rust](https://rustup.rs), Node.js 22+, pnpm, and on Windows the Visual Studio C++ Build Tools (WebView2 ships with Windows 11).

```powershell
pnpm install
pnpm tauri dev          # run with hot reload
pnpm tauri build        # installer in src-tauri/target/release/bundle/nsis/
cd src-tauri; cargo test                                  # unit tests
cd src-tauri; cargo test -- --ignored clipboard_roundtrip # touches the real clipboard
```

Project layout and design decisions: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Release

GitHub Actions builds and publishes releases automatically:

1. Bump `version` in `src-tauri/tauri.conf.json` (keep `package.json` and `src-tauri/Cargo.toml` in sync).
2. Commit, then tag and push:
   ```powershell
   git tag v0.2.0
   git push --follow-tags
   ```
3. The **Release** workflow builds `vrun.clip_0.2.0_x64-setup.exe` and attaches it to a new GitHub Release.

### Auto-update

The app checks `https://github.com/Vitnerp/vrun-clip/releases/latest/download/latest.json` 20 s after start and then every 6 hours. It downloads updates in the background and offers **Restart to update** in the popup, the tray menu and Settings. Each update is verified against the public key in `tauri.conf.json`, so only builds signed with the project's private key are accepted.

One-time key setup (the maintainer does this; the private key never goes into the repo):

1. `pnpm tauri signer generate -w "$HOME\.tauri\vrun-clip.key"` and choose a password.
2. Back up `vrun-clip.key` and the password (password manager). **If they are lost, installed copies can never be updated again.**
3. Upload them as GitHub Actions secrets:
   ```powershell
   Get-Content -Raw "$HOME\.tauri\vrun-clip.key" | gh secret set TAURI_SIGNING_PRIVATE_KEY --repo Vitnerp/vrun-clip
   gh secret set TAURI_SIGNING_PRIVATE_KEY_PASSWORD --repo Vitnerp/vrun-clip   # prompts for the password
   ```
4. Put the contents of `vrun-clip.key.pub` into `plugins.updater.pubkey` in `src-tauri/tauri.conf.json`.

Local `pnpm tauri build` then needs the key in the environment: `$env:TAURI_SIGNING_PRIVATE_KEY = Get-Content -Raw "$HOME\.tauri\vrun-clip.key"` (plus `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`).

### Code signing (removing the SmartScreen warning)

Planned via the free SignPath Foundation program for open-source projects. See [docs/SIGNING.md](docs/SIGNING.md).
