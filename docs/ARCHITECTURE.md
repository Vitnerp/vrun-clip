# vrun.clip architecture

## Stack

| Layer | Choice | Why |
| --- | --- | --- |
| App shell | **Tauri 2** | Native Rust core, system WebView (WebView2 is part of Windows 11), 2.4 MB installer. Same codebase can target macOS, Linux, Android and iOS. |
| Core | **Rust** | Raw Win32 clipboard access with no runtime/GC pauses; safe threading. |
| Storage | **SQLite** (`rusqlite`, bundled) + **FTS5 trigram** index | Substring search over hundreds of thousands of items in milliseconds; case and diacritics insensitive. |
| Images | PNG files on disk + small thumbnails | Keeps the DB small; thumbnails make the list fast. |
| UI | **Svelte 5** + TypeScript (SvelteKit static SPA) | Tiny bundle, fast startup. |
| Installer | NSIS, per-user | No admin rights; built by GitHub Actions on every `v*` tag. |

## Source map

```
src-tauri/src/
  lib.rs              app setup: tray, global hotkey, popup placement, commands, image protocol
  model.rs            shared data types
  settings.rs         settings.json (hotkey, limits, ignore list…)
  db.rs               SQLite schema, search, de-duplication, pruning
  capture.rs          raw clipboard data -> stored item (DIB/PNG decode, thumbnails, hashing)
  platform/mod.rs     OS abstraction (one module per OS)
  platform/windows.rs clipboard listener, read/write formats, paste keystroke
src/
  routes/+page.svelte popup: search, list, preview, keyboard handling
  lib/SettingsPanel.svelte
  lib/api.ts          typed wrappers around Tauri commands
```

## Capture pipeline (and why it does not block the Snipping Tool)

```
WM_CLIPBOARDUPDATE ─► debounce 120 ms ─► OpenClipboard ─► copy bytes of needed formats ─► CloseClipboard
   (listener thread, own message loop)                       (milliseconds)
                                                                   │ channel
                                                                   ▼
                                    capture worker: hash ─► dedupe ─► decode/encode PNG ─► thumbnail ─► SQLite ─► UI event
```

* `AddClipboardFormatListener` instead of the legacy clipboard-viewer chain.
* **Debounce**: apps like Snipping Tool and Office write the clipboard in several steps; we only read after they are done, so we never hold the clipboard while they still need it.
* **Minimal reads**: only the formats we store. If text or files are present, image formats are not requested at all (Excel and others render bitmaps lazily, which is slow).
* **Hold time**: the clipboard is open only while bytes are copied. Measured: after copying a 4K image, another app could read the clipboard 35 ms later.
* Content marked `ExcludeClipboardContentFromMonitorProcessing`, `Clipboard Viewer Ignore` or `CanIncludeInClipboardHistory = 0` is skipped (password managers use these).
* Our own writes carry a private `VrunClip.Internal` format and are ignored by the listener.
* Identical content (blake3 hash) is not stored twice; it moves to the top.

## Paste

On the hotkey we remember the foreground window. Selecting an item writes all its formats to the clipboard (PNG decode is done *before* opening the clipboard), hides the popup, restores focus to that window and sends `Ctrl+V` with `SendInput`.

## Data model (sync ready)

`items(id, uuid, kind, hash, text, preview, source_app, created_at, used_at, updated_at, pinned, size, image_w, image_h, files)`, plus `item_data(html, rtf)` and the `items_fts` virtual table kept in sync by triggers. `uuid`, `hash` and `updated_at` already exist so a future sync layer can identify and merge items across devices. Schema changes go through `PRAGMA user_version` migrations in `db.rs`.

## Cross-platform plan

`platform/` is the only OS-specific code. To add an OS, implement `start_watcher`, `write_clipboard`, `foreground_window`, `paste_into`, `message_box`:

* **macOS**: poll `NSPasteboard.changeCount` (there is no change notification), paste with a `CGEvent` Cmd+V (needs the Accessibility permission).
* **Linux**: Wayland `wlr-data-control` / `ext-data-control` (via `wl-clipboard-rs`), X11 XFixes selection events; paste via `xdotool`/`ydotool`-like input.
* **Android/iOS**: no background clipboard access is allowed. Mobile would be a *sync client* (share sheet / manual paste) rather than a watcher.

The release workflow already has commented-out macOS/Linux matrix entries.

---

## Future: shared clipboard across devices (design only, NOT implemented)

Goal: copy on one device, paste on another; **nobody but your own devices can read the data**, including the server operator.

### Recommended approach: end-to-end encrypted, "blind" relay

```
 Device A ──encrypt(VK)──► relay (stores opaque blobs) ──► Device B ──decrypt(VK)
             ▲                                                  ▲
             └──────── vault key VK exists only on devices ─────┘
```

1. **Vault key (VK)**: a random 256-bit key created on the first device and stored in the OS keystore (Windows Credential Manager/DPAPI, macOS Keychain). It is never sent to the server.
2. **Items** are encrypted with **XChaCha20-Poly1305** (random 192-bit nonce per item, AAD = item uuid + version). Images are encrypted in chunks. Sizes are padded to buckets so the server learns little from lengths.
3. **Device pairing**: the new device shows a QR code or 6-word code. The two devices run a PAKE (**SPAKE2/CPace**, as in Magic Wormhole) to agree on a secret, and VK is sent through that channel, device to device. The relay only forwards opaque pairing messages.
4. **Server authentication**: each device has an **Ed25519** key pair. The server stores `vault_id → device public keys` and verifies signed requests. Even a stolen server database contains only ciphertext and public keys.
5. **Removing a device**: rotate VK (new key for new items; old items are re-encrypted lazily) and revoke the device's public key on the server.
6. **Conflict handling**: items are immutable and identified by uuid. Only `pinned`/`deleted` flags change (last-writer-wins by `updated_at` + device id), with tombstones for deletes.

Libraries: RustCrypto `chacha20poly1305`, `ed25519-dalek`, `x25519-dalek`, `spake2`; `keyring` for OS key storage. All audited, pure Rust, and they work on mobile.

### Where to host the relay

| Option | Pros | Cons |
| --- | --- | --- |
| **Self-hosted relay** (small Rust `axum` + SQLite service in Docker on your server) | Full control, simple | You maintain uptime, TLS, backups |
| **Cloudflare Workers + Durable Objects + R2** | No server to maintain, WebSocket push, global, free tier usually enough | Third-party host (acceptable: it only ever sees ciphertext) |
| **Peer-to-peer** (e.g. `iroh` over QUIC; works great over Tailscale/WireGuard) | No server at all | Both devices must be online at the same time; harder for mobile |

**Recommendation**: build the blind-relay protocol once, behind a small `SyncTransport` trait, and run it first on your own server (or a Worker). Because encryption is end-to-end, the host does not need to be trusted. Only availability matters.

### Before enabling sync: security checklist

- [ ] Threat model written (lost device, malicious server, network attacker)
- [ ] Optional local at-rest encryption of `clips.db` (SQLCipher, key in DPAPI)
- [ ] Per-kind and per-app sync filters (e.g. never sync from password managers, opt-in for images)
- [ ] Pairing protocol reviewed; no VK in logs, crash reports or URLs
- [ ] Independent review of the crypto code before release
