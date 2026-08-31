<script lang="ts">
  import { AlertTriangle } from '@lucide/svelte'
  import { app } from '$lib/stores/app.svelte'
</script>

<h2 class="mb-3 text-sm font-semibold">Changes (jj-native)</h2>

{#if app.conflicts.length > 0}
  <div class="mb-4 border p-3" style="border-color:var(--destructive);background:color-mix(in srgb, var(--destructive) 10%, transparent)">
    <div class="flex items-center gap-2 text-sm font-semibold" style="color:var(--destructive)"><AlertTriangle class="size-4" /> {app.conflicts.length} conflicted path{app.conflicts.length === 1 ? '' : 's'}</div>
    {#each app.conflicts as cf (cf.id)}
      <div class="mt-1 font-mono text-xs">{cf.path}</div>
    {/each}
  </div>
{/if}

<div class="g-card overflow-hidden">
  {#if app.changesList.length === 0}
    <p class="p-4 text-xs g-subtle">No changes</p>
  {:else}
    {#each app.changesList as c (c.change_id)}
      <a href={`#/${encodeURIComponent(app.route.org!)}/${encodeURIComponent(app.route.repo!)}/commit/${encodeURIComponent(c.commit_id)}`} class="flex w-full items-center gap-3 border-b border-[var(--gitea-secondary)] px-4 py-2.5 hover:bg-[var(--gitea-hover-opaque)] last:border-b-0">
        <span class="shrink-0 font-mono text-xs" style="color:var(--primary)">change {c.change_id.slice(0, 10)}</span>
        <span class="shrink-0 font-mono text-[11px] g-subtle">{c.commit_id.slice(0, 10)}</span>
        <span class="min-w-0 flex-1 truncate text-xs">{c.description || '(empty)'}</span>
        <span class="shrink-0 text-[11px] g-subtle">{c.author}</span>
      </a>
    {/each}
  {/if}
</div>