<script lang="ts">
  import { GitBranch } from '@lucide/svelte'
  import { app } from '$lib/stores/app.svelte'
</script>

<h2 class="mb-3 text-sm font-semibold">Change-ids (jj-native)</h2>
<div class="grid grid-cols-1 gap-4 lg:grid-cols-2">
  <div class="g-card overflow-hidden">
    <div class="border-b border-[var(--gitea-secondary)] px-4 py-2 text-xs font-semibold g-muted">Bookmarks by change-id</div>
    {#if app.changesList.length === 0}
      <p class="p-4 text-xs g-subtle">No bookmarks</p>
    {:else}
      {#each app.changesList as b (b.name)}
        <button class="flex w-full items-center gap-3 border-b border-[var(--gitea-secondary)] px-4 py-2.5 text-left hover:bg-[var(--gitea-hover-opaque)] last:border-b-0" onclick={() => { void app.loadChangeDetail(b.change_id) }}>
          <GitBranch class="size-4 shrink-0 g-muted" />
          <span class="min-w-0 flex-1 truncate font-mono text-xs">{b.name}</span>
          <span class="shrink-0 font-mono text-[11px]" style="color:var(--primary)">{b.change_id.slice(0, 10)}</span>
        </button>
      {/each}
    {/if}
  </div>
  <div class="g-card p-4">
    {#if app.changeDetail}
      <div class="text-sm font-semibold">change {app.changeDetail.change_id}</div>
      <div class="mt-1 font-mono text-xs g-muted">commit {app.changeDetail.sha}</div>
      <div class="mt-2 text-xs">{app.changeDetail.description}</div>
      <div class="mt-2 text-[11px] g-subtle">{app.changeDetail.author}</div>
    {:else}
      <p class="text-xs g-subtle">Select a bookmark to inspect its change.</p>
    {/if}
  </div>
</div>