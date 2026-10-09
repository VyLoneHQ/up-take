<script lang="ts">
import { invoke } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { onMount } from 'svelte';
import { type LicenceFile, licenceFile, TITLE_KEY } from '$lib/licence-view';
import { isLanguage, type Language, text } from '$lib/strings';

// A licence file, shown and never editable (I-443). The founder asked for this
// on his rig pass of #130: the files had opened in an editor, where a stray
// keystroke and a save changed them. The text is an ordinary <pre>: it can be
// selected and copied, and nothing on this page can change it.

let language = $state<Language>('en');
let which = $state<LicenceFile | null>(null);
let body = $state('');
let unreadable = $state(false);

onMount(() => {
  void (async () => {
    const chosen = await invoke('overlay_language');
    if (isLanguage(chosen)) language = chosen;
    which = licenceFile(window.location.search);
    if (which) {
      try {
        body = await invoke<string>('licence_text', { which });
      } catch {
        unreadable = true;
      }
    } else {
      unreadable = true;
    }
    // Shown only now, as the settings window is: created hidden so nobody sees
    // an unpainted white frame before the dark page lands.
    await getCurrentWindow().show();
  })();
});
</script>

<svelte:head>
  <title>{text(language, which ? TITLE_KEY[which] : 'licence.title.own')}</title>
</svelte:head>

<main>
  {#if unreadable}
    <p class="unreadable">{text(language, 'licence.unreadable')}</p>
  {:else}
    <pre>{body}</pre>
  {/if}
</main>

<style>
/* The overlay's palette (UI-UX.md §2), as the settings window uses it, opaque:
   this window is for reading long text, and a translucent panel over whatever
   is behind it would make that harder rather than easier. */
:global(html),
:global(body) {
  margin: 0;
  height: 100%;
  background: rgb(24, 28, 36);
}

main {
  box-sizing: border-box;
  height: 100vh;
  overflow: auto;
  padding: 16px 20px;
  color: rgba(235, 240, 250, 0.95);
  background: rgb(24, 28, 36);
}

pre {
  margin: 0;
  /* The files are plain text laid out in fixed columns, so a monospace face;
     long lines wrap rather than scroll sideways. */
  font: 12.5px/1.45 Consolas, 'Cascadia Mono', monospace;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
  user-select: text;
}

.unreadable {
  font: 13px/1.4 system-ui, sans-serif;
  color: rgba(235, 240, 250, 0.55);
}
</style>
