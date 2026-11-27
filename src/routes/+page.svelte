<script lang="ts">
  import { onMount, tick } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { api, formatBytes, timeAgo, type Clip, type Detail, type PopupShown, type SettingsView, type UpdateStatus } from "$lib/api";
  import Icon from "$lib/Icon.svelte";
  import SettingsPanel from "$lib/SettingsPanel.svelte";

  const PAGE = 80;

  let query = $state("");
  let items = $state<Clip[]>([]);
  let selected = $state(0);
  let detail = $state<Detail | null>(null);
  let view = $state<"list" | "settings">("list");
  let welcome = $state(false);
  let canPaste = $state(true);
  let hotkey = $state("Ctrl+Alt+V");
  let paused = $state(false);
  let total = $state(0);
  let loaded = $state(false);
  let now = $state(Date.now());
  let error = $state<string | null>(null);
  let update = $state<UpdateStatus | null>(null);

  let searchEl = $state<HTMLInputElement>();
  let listEl = $state<HTMLElement>();
  let exhausted = false;
  let loadingMore = false;
  let requestSeq = 0;

  const current = $derived(items[selected] ?? null);

  async function load(reset: boolean) {
    const seq = ++requestSeq;
    try {
      const page = await api.search(query, reset ? 0 : items.length, PAGE);
      if (seq !== requestSeq) return;
      items = reset ? page : [...items, ...page];
      exhausted = page.length < PAGE;
      if (reset) selected = Math.min(selected, Math.max(0, items.length - 1));
      loaded = true;
      error = null;
    } catch (e) {
      error = String(e);
    }
  }

  async function loadMore() {
    if (exhausted || loadingMore) return;
    loadingMore = true;
    await load(false);
    loadingMore = false;
  }

  async function refreshSettings() {
    try {
      applySettingsView(await api.getSettings());
    } catch {}
  }

  function applySettingsView(v: SettingsView) {
    hotkey = v.settings.hotkey;
    paused = v.settings.paused;
    total = v.stats.items;
  }

  // Re-run the search shortly after typing stops.
  let searchTimer: ReturnType<typeof setTimeout>;
  function onInput() {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(() => {
      selected = 0;
      load(true);
      listEl?.scrollTo({ top: 0 });
    }, 40);
  }

  // Load the preview for the selected item.
  let detailTimer: ReturnType<typeof setTimeout>;
  $effect(() => {
    const item = current;
    clearTimeout(detailTimer);
    if (!item || item.kind === "image") {
      detail = null;
      return;
    }
    if (detail?.id === item.id) return;
    detailTimer = setTimeout(async () => {
      try {
        const d = await api.detail(item.id);
        if (current?.id === d.id) detail = d;
      } catch {}
    }, 30);
  });

  async function select(i: number) {
    if (items.length === 0) return;
    selected = Math.max(0, Math.min(items.length - 1, i));
    await tick();
    listEl?.querySelector(`[data-index="${selected}"]`)?.scrollIntoView({ block: "nearest" });
    if (selected > items.length - 10) loadMore();
  }

  async function activate(i: number, plain = false, paste = canPaste) {
    const item = items[i];
    if (!item) return;
    try {
      await api.activate(item.id, plain, paste);
    } catch (e) {
      error = String(e);
    }
  }

  async function remove(i: number) {
    const item = items[i];
    if (!item) return;
    await api.deleteItem(item.id);
    items = items.filter((x) => x.id !== item.id);
    total = Math.max(0, total - 1);
    selected = Math.min(selected, Math.max(0, items.length - 1));
  }

  async function pin(i: number) {
    const item = items[i];
    if (!item) return;
    item.pinned = await api.togglePin(item.id);
    await load(true);
  }

  function onKeydown(e: KeyboardEvent) {
    const ctrl = e.ctrlKey || e.metaKey;
    // Never let the webview reload, print, find or zoom.
    if (e.key === "F5" || (ctrl && ["r", "p", "f", "g", "j", "u", "+", "-", "="].includes(e.key.toLowerCase()))) {
      e.preventDefault();
    }
    if (welcome && (e.key === "Enter" || e.key === "Escape")) {
      e.preventDefault();
      welcome = false;
      return;
    }
    if (view === "settings") {
      if (e.key === "Escape") {
        e.preventDefault();
        view = "list";
        load(true);
        tick().then(() => searchEl?.focus());
      }
      return;
    }
    if (ctrl && e.key === ",") {
      e.preventDefault();
      view = "settings";
      return;
    }
    switch (e.key) {
      case "Escape":
        e.preventDefault();
        if (query) {
          query = "";
          onInput();
        } else {
          api.hide();
        }
        return;
      case "ArrowDown":
        e.preventDefault();
        select(selected + 1);
        return;
      case "ArrowUp":
        e.preventDefault();
        select(selected - 1);
        return;
      case "PageDown":
        e.preventDefault();
        select(selected + 8);
        return;
      case "PageUp":
        e.preventDefault();
        select(selected - 8);
        return;
      case "Enter":
        e.preventDefault();
        // Enter = paste, Shift+Enter = paste as plain text, Ctrl+Enter = copy only.
        activate(selected, e.shiftKey, canPaste && !ctrl);
        return;
      case "Delete": {
        const input = searchEl;
        const atEnd = !input || (input.selectionStart === input.value.length && input.selectionEnd === input.value.length);
        if (atEnd) {
          e.preventDefault();
          remove(selected);
        }
        return;
      }
    }
    if (ctrl && e.key.toLowerCase() === "p") {
      pin(selected);
      return;
    }
    if (ctrl && e.key.toLowerCase() === "c" && !window.getSelection()?.toString() && searchEl?.selectionStart === searchEl?.selectionEnd) {
      e.preventDefault();
      activate(selected, false, false);
      return;
    }
    if (ctrl && /^[1-9]$/.test(e.key)) {
      e.preventDefault();
      activate(Number(e.key) - 1, e.shiftKey);
      return;
    }
    // Typing anywhere goes to the search box.
    if (document.activeElement !== searchEl && e.key.length === 1 && !ctrl && !e.altKey) {
      searchEl?.focus();
    }
  }

  function onScroll() {
    if (!listEl) return;
    if (listEl.scrollTop + listEl.clientHeight > listEl.scrollHeight - 300) loadMore();
  }

  onMount(() => {
    const unlisten: Array<() => void> = [];
    listen<PopupShown>("popup-shown", async (e) => {
      welcome = e.payload.welcome;
      canPaste = e.payload.canPaste;
      view = e.payload.settings ? "settings" : "list";
      query = "";
      selected = 0;
      detail = null;
      error = null;
      now = Date.now();
      await load(true);
      refreshSettings();
      listEl?.scrollTo({ top: 0 });
      await tick();
      searchEl?.focus();
    }).then((u) => unlisten.push(u));
    listen<UpdateStatus>("update-status", (e) => (update = e.payload)).then((u) => unlisten.push(u));
    api.updateStatus().then((s) => (update = s)).catch(() => {});
    listen("clips-changed", () => {
      now = Date.now();
      load(true);
      refreshSettings();
    }).then((u) => unlisten.push(u));

    const noMenu = (e: MouseEvent) => {
      if (!(e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement)) e.preventDefault();
    };
    window.addEventListener("contextmenu", noMenu);
    load(true);
    refreshSettings();
    return () => {
      unlisten.forEach((u) => u());
      window.removeEventListener("contextmenu", noMenu);
    };
  });

  function kindLabel(c: Clip) {
    if (c.kind === "image") return `${c.imageWidth}×${c.imageHeight}`;
    if (c.kind === "files") return "files";
    return c.hasHtml || c.hasRtf ? "rich text" : "text";
  }
</script>

<svelte:window onkeydown={onKeydown} />

<main>
  {#if view === "settings"}
    <SettingsPanel
      onclose={() => {
        view = "list";
        load(true);
        tick().then(() => searchEl?.focus());
      }}
      onchange={applySettingsView}
    />
  {:else}
    <header>
      <span class="search-icon"><Icon name="search" /></span>
      <input
        bind:this={searchEl}
        bind:value={query}
        oninput={onInput}
        placeholder="Search clipboard history…"
        spellcheck="false"
        autocomplete="off"
      />
      <button class="icon" title="Settings (Ctrl+,)" onclick={() => (view = "settings")}><Icon name="gear" /></button>
      <button class="icon" title="Close (Esc)" onclick={() => api.hide()}><Icon name="close" /></button>
    </header>

    {#if paused}
      <div class="banner">
        <Icon name="pause" size={14} /> Recording is paused.
        <button
          class="link"
          onclick={async () => {
            const v = await api.getSettings();
            applySettingsView(await api.saveSettings({ ...v.settings, paused: false }));
          }}>Resume</button>
      </div>
    {/if}

    {#if update?.phase === "ready"}
      <div class="banner update">
        vrun.clip {update.version} is ready.
        <button class="link" onclick={() => api.installUpdate().catch((e) => (error = String(e)))}>Restart to update</button>
      </div>
    {/if}

    <div class="content">
      <ul class="list" bind:this={listEl} onscroll={onScroll} role="listbox" aria-label="Clipboard history">
        {#each items as item, i (item.id)}
          <li
            data-index={i}
            class:selected={i === selected}
            role="option"
            aria-selected={i === selected}
            onmousedown={() => select(i)}
            ondblclick={() => activate(i)}
          >
            <span class="num">{i < 9 ? i + 1 : ""}</span>
            {#if item.kind === "image" && item.thumbUrl}
              <img class="thumb" src={item.thumbUrl} alt="" loading="lazy" />
            {:else}
              <span class="kind"><Icon name={item.kind === "files" ? "file" : "text"} size={15} /></span>
            {/if}
            <div class="text">
              <div class="preview" class:mono={item.kind === "files"}>{item.preview || " "}</div>
              <div class="meta">
                {kindLabel(item)}{item.sourceApp ? ` · ${item.sourceApp.replace(/\.exe$/i, "")}` : ""} · {timeAgo(item.usedAt, now)}
              </div>
            </div>
            <div class="actions">
              <button class="icon small" class:on={item.pinned} title="Pin (Ctrl+P)" onmousedown={(e) => e.stopPropagation()} onclick={() => pin(i)}><Icon name="pin" size={14} /></button>
              <button class="icon small" title="Delete (Del)" onmousedown={(e) => e.stopPropagation()} onclick={() => remove(i)}><Icon name="trash" size={14} /></button>
            </div>
          </li>
        {/each}
        {#if loaded && items.length === 0}
          <li class="empty">
            {#if query}
              No matches for “{query}”.
            {:else}
              <strong>Nothing here yet.</strong>
              Copy some text or take a screenshot (Win+Shift+S),<br />then press <kbd>{hotkey}</kbd> anywhere.
            {/if}
          </li>
        {/if}
      </ul>

      <aside class="preview-pane">
        {#if current}
          {#if current.kind === "image" && current.imageUrl}
            <div class="img-wrap"><img src={current.imageUrl} alt="Clipboard content preview" /></div>
            <p class="pmeta">{current.imageWidth}×{current.imageHeight} · {formatBytes(current.size)}</p>
          {:else if detail && detail.id === current.id}
            {#if detail.kind === "files"}
              <ul class="files">
                {#each detail.files as f}<li>{f}</li>{/each}
              </ul>
            {:else}
              <pre>{detail.text}{detail.truncated ? "\n…" : ""}</pre>
            {/if}
            <p class="pmeta">
              {detail.kind === "files" ? `${detail.files.length} file(s)` : `${detail.text.length.toLocaleString()} chars`}{current.hasHtml ? " · HTML" : ""}{current.hasRtf ? " · RTF" : ""}
            </p>
          {/if}
        {/if}
      </aside>
    </div>

    <footer>
      {#if error}
        <span class="err">{error}</span>
      {:else}
        <span><kbd>↵</kbd> {canPaste ? "paste" : "copy"}</span>
        <span><kbd>⇧↵</kbd> plain text</span>
        <span><kbd>Ctrl+P</kbd> pin</span>
        <span><kbd>Del</kbd> delete</span>
        <span class="grow"></span>
        <span class="count">{total} items</span>
      {/if}
    </footer>
  {/if}

  {#if welcome}
    <div class="welcome" role="dialog" aria-label="Welcome">
      <div class="card">
        <h2>vrun.clip is running</h2>
        <p>It lives in the system tray and remembers everything you copy: text, images, screenshots and files.</p>
        <ol>
          <li>Copy something as usual.</li>
          <li>Press <kbd>{hotkey}</kbd> anywhere.</li>
          <li>Type to search, <kbd>↑</kbd><kbd>↓</kbd> to choose, <kbd>Enter</kbd> to paste.</li>
        </ol>
        <button class="primary" onclick={() => { welcome = false; searchEl?.focus(); }}>Got it</button>
      </div>
    </div>
  {/if}
</main>

<style>
  main { position: relative; display: flex; flex-direction: column; height: 100vh; }
  header { display: flex; align-items: center; gap: 6px; padding: 8px 8px 8px 12px; border-bottom: 1px solid var(--border); }
  .search-icon { color: var(--muted); display: flex; }
  header input { flex: 1; border: 0; background: transparent; font-size: 15px; padding: 6px 4px; outline: none; box-shadow: none; }
  .banner { display: flex; align-items: center; gap: 6px; padding: 6px 12px; font-size: 12px; background: var(--warn-bg); color: var(--warn-fg); }
  .banner.update { background: var(--selected); color: var(--fg); }
  .content { flex: 1; display: flex; min-height: 0; }
  .list { list-style: none; margin: 0; padding: 4px; width: 58%; overflow-y: auto; border-right: 1px solid var(--border); }
  .list li { display: flex; align-items: center; gap: 8px; padding: 6px 8px; border-radius: 6px; cursor: default; min-height: 44px; }
  .list li.selected { background: var(--selected); }
  .list li:not(.selected):hover { background: var(--hover); }
  .num { width: 10px; font-size: 11px; color: var(--muted); text-align: right; flex: none; font-variant-numeric: tabular-nums; }
  .kind { width: 44px; height: 32px; display: flex; align-items: center; justify-content: center; color: var(--muted); flex: none; }
  .thumb { width: 44px; height: 32px; object-fit: cover; border-radius: 4px; flex: none; background: var(--checker); border: 1px solid var(--border); }
  .text { flex: 1; min-width: 0; }
  .preview { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; font-size: 13px; }
  .preview.mono { font-family: var(--mono); font-size: 12px; }
  .meta { font-size: 11px; color: var(--muted); margin-top: 2px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .actions { display: flex; gap: 2px; opacity: 0; flex: none; }
  li:hover .actions, li.selected .actions, .actions:has(.on) { opacity: 1; }
  .icon.on { color: var(--accent); }
  .empty { flex-direction: column; justify-content: center; text-align: center; color: var(--muted); font-size: 13px; padding: 48px 16px !important; line-height: 1.7; }
  .empty strong { color: var(--fg); }
  .preview-pane { width: 42%; padding: 12px; overflow: auto; display: flex; flex-direction: column; min-width: 0; }
  .preview-pane pre { margin: 0; flex: 1; white-space: pre-wrap; word-break: break-word; font-family: var(--mono); font-size: 12px; line-height: 1.5; user-select: text; }
  .img-wrap { flex: 1; display: flex; align-items: center; justify-content: center; min-height: 0; }
  .img-wrap img { max-width: 100%; max-height: 100%; object-fit: contain; border-radius: 4px; background: var(--checker); }
  .files { margin: 0; padding-left: 16px; font-family: var(--mono); font-size: 12px; word-break: break-all; flex: 1; }
  .pmeta { color: var(--muted); font-size: 11px; margin: 8px 0 0; }
  footer { display: flex; gap: 14px; align-items: center; padding: 6px 12px; border-top: 1px solid var(--border); font-size: 11px; color: var(--muted); }
  footer .grow { flex: 1; }
  .err { color: var(--danger); }
  .link { border: 0; background: none; color: inherit; text-decoration: underline; padding: 0 2px; font-size: 12px; cursor: pointer; }
  .welcome { position: absolute; inset: 0; background: var(--overlay); display: flex; align-items: center; justify-content: center; }
  .card { background: var(--bg); border: 1px solid var(--border); border-radius: 10px; padding: 20px 24px; max-width: 420px; box-shadow: 0 8px 32px rgba(0, 0, 0, 0.25); }
  .card h2 { margin: 0 0 8px; font-size: 17px; }
  .card p, .card ol { font-size: 13px; line-height: 1.6; margin: 0 0 12px; }
  .card ol { padding-left: 20px; }
</style>
