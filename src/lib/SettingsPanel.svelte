<script lang="ts">
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { api, formatBytes, isMac, shortcutFromEvent, type SettingsView, type UpdateStatus } from "./api";
  import Icon from "./Icon.svelte";

  let { onclose, onchange }: { onclose: () => void; onchange: (v: SettingsView) => void } = $props();

  let view = $state<SettingsView | null>(null);
  let recording = $state(false);
  let ignoredText = $state("");
  let confirmClear = $state(false);
  let error = $state<string | null>(null);

  let update = $state<UpdateStatus | null>(null);

  onMount(() => {
    let unlisten: (() => void) | undefined;
    listen<UpdateStatus>("update-status", (e) => (update = e.payload)).then((u) => (unlisten = u));
    api.updateStatus().then((s) => (update = s));
    api.getSettings().then((v) => {
      view = v;
      ignoredText = v.settings.ignoredApps.join("\n");
    });
    return () => unlisten?.();
  });

  function updateText(u: UpdateStatus | null): string {
    switch (u?.phase) {
      case "checking": return "Checking…";
      case "downloading": return `Downloading ${u.version}…`;
      case "upToDate": return "You have the latest version.";
      case "ready": return `Version ${u.version} is ready to install.`;
      case "error": return `Update check failed: ${u.message}`;
      default: return "Updates are checked automatically.";
    }
  }

  async function save() {
    if (!view) return;
    try {
      view = await api.saveSettings({
        ...view.settings,
        ignoredApps: ignoredText.split("\n"),
      });
      error = null;
      onchange(view);
    } catch (e) {
      error = String(e);
    }
  }

  async function startRecording() {
    recording = true;
    await api.suspendHotkey(true);
  }

  async function stopRecording(hotkey: string | null) {
    recording = false;
    if (hotkey && view) {
      view.settings.hotkey = hotkey;
      await save();
    } else {
      await api.suspendHotkey(false);
      view = await api.getSettings();
    }
  }

  function onRecordKey(e: KeyboardEvent) {
    if (!recording) return;
    e.preventDefault();
    e.stopPropagation();
    if (e.key === "Escape") return void stopRecording(null);
    const combo = shortcutFromEvent(e);
    if (combo) stopRecording(combo);
  }

  async function clear() {
    if (!confirmClear) {
      confirmClear = true;
      setTimeout(() => (confirmClear = false), 4000);
      return;
    }
    confirmClear = false;
    await api.clearHistory(true);
    view = await api.getSettings();
    if (view) onchange(view);
  }
</script>

<svelte:window onkeydowncapture={onRecordKey} />

<div class="settings">
  <header>
    <button class="icon" onclick={onclose} title="Back (Esc)"><Icon name="back" /></button>
    <h1>Settings</h1>
  </header>

  {#if view}
    <div class="body">
      <section>
        <div class="row">
          <div class="label">
            <strong>Open shortcut</strong>
            <span>Press it anywhere to open vrun.clip.</span>
          </div>
          <button class="hotkey" class:recording onclick={startRecording}>
            {recording ? "Press keys… (Esc cancels)" : view.settings.hotkey}
          </button>
        </div>
        {#if view.hotkeyError}
          <p class="warn">{view.hotkeyError}</p>
        {/if}

        <label class="row">
          <div class="label">
            <strong>{isMac ? "Start at login" : "Start with Windows"}</strong>
            <span>Runs quietly in the {isMac ? "menu bar" : "tray"}.</span>
          </div>
          <input type="checkbox" class="switch" bind:checked={view.settings.autostart} onchange={save} />
        </label>

        <label class="row">
          <div class="label">
            <strong>Pause recording</strong>
            <span>Nothing new is saved while paused.</span>
          </div>
          <input type="checkbox" class="switch" bind:checked={view.settings.paused} onchange={save} />
        </label>
      </section>

      <section>
        <label class="row">
          <div class="label"><strong>Keep last</strong><span>Pinned items are always kept.</span></div>
          <span class="num"><input type="number" min="10" bind:value={view.settings.maxItems} onchange={save} /> items</span>
        </label>
        <label class="row">
          <div class="label"><strong>Image storage limit</strong><span>Oldest images are removed first.</span></div>
          <span class="num"><input type="number" min="10" bind:value={view.settings.maxImageMb} onchange={save} /> MB</span>
        </label>
        <label class="row">
          <div class="label"><strong>Skip clips larger than</strong><span></span></div>
          <span class="num"><input type="number" min="1" bind:value={view.settings.maxItemMb} onchange={save} /> MB</span>
        </label>
        <label class="col">
          <div class="label">
            <strong>Never record from these apps</strong>
            <span>One executable name per line. Password managers that mark their clips private are skipped automatically.</span>
          </div>
          <textarea rows="3" bind:value={ignoredText} onchange={save} spellcheck="false"></textarea>
        </label>
      </section>

      <section>
        <div class="row">
          <div class="label">
            <strong>History</strong>
            <span>{view.stats.items} items · {view.stats.pinned} pinned · {formatBytes(view.stats.imageBytes)} of images</span>
          </div>
          <div class="buttons">
            <button onclick={() => api.openDataDir()}>Open folder</button>
            <button class="danger" onclick={clear}>{confirmClear ? "Click again to confirm" : "Clear history"}</button>
          </div>
        </div>
        <p class="hint">Clearing keeps pinned items. Data is stored locally in {view.stats.dataDir}</p>
      </section>

      <section>
        <div class="row">
          <div class="label">
            <strong>vrun.clip {view.version}</strong>
            <span>{updateText(update)}</span>
          </div>
          {#if update?.phase === "ready"}
            <button class="primary" onclick={() => api.installUpdate().catch((e) => (error = String(e)))}>Restart to update</button>
          {:else}
            <button disabled={update?.phase === "checking" || update?.phase === "downloading"} onclick={() => api.checkUpdate()}>Check for updates</button>
          {/if}
        </div>
      </section>

      {#if error}<p class="warn">{error}</p>{/if}
    </div>
  {/if}
</div>

<style>
  .settings { display: flex; flex-direction: column; height: 100%; min-height: 0; }
  header { display: flex; align-items: center; gap: 8px; padding: 10px 12px; border-bottom: 1px solid var(--border); }
  h1 { font-size: 15px; font-weight: 600; margin: 0; }
  .body { overflow-y: auto; padding: 8px 16px 16px; }
  section { border-bottom: 1px solid var(--border); padding: 6px 0; }
  section:last-of-type { border-bottom: 0; }
  .row { display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 8px 0; }
  .col { display: flex; flex-direction: column; gap: 6px; padding: 8px 0; }
  .label { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
  .label strong { font-weight: 600; font-size: 13px; }
  .label span, .hint { color: var(--muted); font-size: 12px; }
  .hint { margin: 0 0 6px; word-break: break-all; }
  .warn { color: var(--danger); font-size: 12px; margin: 4px 0; }
  .num { display: flex; align-items: center; gap: 6px; color: var(--muted); font-size: 12px; white-space: nowrap; }
  input[type="number"] { width: 84px; }
  textarea { width: 100%; resize: none; font-family: var(--mono); font-size: 12px; }
  .buttons { display: flex; gap: 8px; }
  .hotkey { font-family: var(--mono); min-width: 140px; }
  .hotkey.recording { border-color: var(--accent); color: var(--accent); }
  .danger { color: var(--danger); }
  .switch { appearance: none; width: 34px; height: 20px; border-radius: 10px; background: var(--border-strong); position: relative; cursor: pointer; flex: none; transition: background 0.15s; }
  .switch::after { content: ""; position: absolute; top: 3px; left: 3px; width: 14px; height: 14px; border-radius: 50%; background: #fff; transition: transform 0.15s; }
  .switch:checked { background: var(--accent); }
  .switch:checked::after { transform: translateX(14px); }
</style>
