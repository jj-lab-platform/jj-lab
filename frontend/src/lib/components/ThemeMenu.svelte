<script lang="ts">
  import { DropdownMenu } from '$lib/components/ui/dropdown-menu'
  import { Check, Moon, Palette, Sun } from '@lucide/svelte'
  import {
    ALL_THEMES,
    applyTheme,
    getSavedMode,
    getSavedTheme,
    isDarkOnlyTheme,
  } from '$lib/stores/theme.svelte'
  import { themeData } from '$lib/stores/themes-data'

  const themeLabels: Record<string, string> = {
    opencode: 'OpenCode',
    tokyonight: 'Tokyo Night',
    everforest: 'Everforest',
    catppuccin: 'Catppuccin',
    'catppuccin-frappe': 'Catppuccin Frappé',
    'catppuccin-macchiato': 'Catppuccin Macchiato',
    ayu: 'Ayu',
    aura: 'Aura',
    nord: 'Nord',
    gruvbox: 'Gruvbox',
    kanagawa: 'Kanagawa',
    matrix: 'Matrix',
    'one-dark': 'One Dark',
    carbonfox: 'Carbonfox',
    cobalt2: 'Cobalt2',
    cursor: 'Cursor',
    dracula: 'Dracula',
    flexoki: 'Flexoki',
    github: 'GitHub',
    'lucent-orng': 'Lucent Orange',
    material: 'Material',
    mercury: 'Mercury',
    monokai: 'Monokai',
    nightowl: 'Night Owl',
    orng: 'Orange',
    'osaka-jade': 'Osaka Jade',
    palenight: 'Palenight',
    rosepine: 'Rose Pine',
    solarized: 'Solarized',
    synthwave84: 'Synthwave 84',
    vercel: 'Vercel',
    vesper: 'Vesper',
    zenburn: 'Zenburn',
  }

  let currentTheme = $state(
    typeof localStorage !== 'undefined' ? getSavedTheme() : 'opencode',
  )
  let themeMode = $state<'dark' | 'light'>(
    typeof localStorage !== 'undefined' ? getSavedMode() : 'dark',
  )
  const darkOnly = $derived(isDarkOnlyTheme(currentTheme))

  function pickTheme(tid: string): void {
    currentTheme = tid
    applyTheme(tid, themeMode)
  }

  function toggleMode(): void {
    themeMode = themeMode === 'dark' ? 'light' : 'dark'
    applyTheme(currentTheme, themeMode)
  }
</script>

<DropdownMenu.Root>
  <DropdownMenu.Trigger>
    {#snippet child({ props })}
      <button {...props} class="g-nav-item" title="Theme">
        <Palette class="size-4" />
      </button>
    {/snippet}
  </DropdownMenu.Trigger>
  <DropdownMenu.Content align="end" class="w-72 p-2">
    <DropdownMenu.Label>Theme</DropdownMenu.Label>
    <div class="flex gap-2 px-2 py-1">
      <select
        class="h-8 flex-1 rounded border border-[var(--gitea-input-border)] bg-[var(--gitea-input-bg)] px-2 text-xs"
        value={currentTheme}
        onchange={(e) => pickTheme(e.currentTarget.value)}
      >
        {#each ALL_THEMES as tid (tid)}
          <option value={tid}>{themeLabels[tid] ?? tid}</option>
        {/each}
      </select>
      <button
        class="g-btn small"
        disabled={darkOnly}
        onclick={toggleMode}
        title={darkOnly ? 'Theme is dark-only' : 'Toggle light/dark'}
      >
        {#if themeMode === 'dark'}<Sun class="size-3.5" />{:else}<Moon class="size-3.5" />{/if}
      </button>
    </div>
    <div class="mt-2 max-h-64 overflow-y-auto px-2 pb-1">
      {#each ALL_THEMES as tid (tid)}
        <button
          type="button"
          class={`flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-xs hover:bg-muted ${tid === currentTheme ? 'ring-1 ring-primary/40' : ''}`}
          onclick={() => pickTheme(tid)}
        >
          <span
            class="size-3 shrink-0 rounded-full border border-border"
            style={`background:${themeData[tid]?.dark?.primary ?? '#888'}`}
          ></span>
          <span class="truncate">{themeLabels[tid] ?? tid}</span>
          {#if tid === currentTheme}<Check class="ml-auto size-3.5 shrink-0" />{/if}
        </button>
      {/each}
    </div>
  </DropdownMenu.Content>
</DropdownMenu.Root>