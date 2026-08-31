<script lang="ts">
  import * as Skeleton from '$lib/components/ui/skeleton'
  import { ChevronRight, Folder, Download, Plus } from '@lucide/svelte'
  import { app } from '$lib/stores/app.svelte'
</script>

<div class="space-y-4">
  <div class="flex items-center justify-between">
    <div>
      <h1 class="text-xl font-semibold">Explore</h1>
      <p class="mt-1 text-sm g-subtle">Organizations on this instance.</p>
    </div>
    <div class="flex gap-2">
      <button class="g-btn small" onclick={() => (app.importOpen = true)}><Download class="size-3.5" /> Import</button>
      <button class="g-btn small primary" onclick={() => (app.createOpen = true)}><Plus class="size-3.5" /> New repository</button>
    </div>
  </div>

  {#if app.reposLoading}
    <div class="space-y-2">{#each Array(4) as _}<Skeleton.Root class="h-9 w-full" />{/each}</div>
  {:else if app.orgs.length === 0}
    <div class="g-card flex flex-col items-center gap-2 p-12 text-center">
      <Folder class="size-10 text-muted-foreground/40" />
      <p class="text-sm g-subtle">No organizations yet — create one by making a repository.</p>
    </div>
  {:else}
    <div class="g-card overflow-hidden">
      {#each app.orgs as o (o.org)}
        <a href={`#/${encodeURIComponent(o.org)}`} class="flex items-center gap-3 border-b border-[var(--gitea-secondary)] px-4 py-3 last:border-b-0 hover:bg-[var(--gitea-hover-opaque)]">
          <div class="flex size-8 items-center justify-center rounded bg-[var(--gitea-box-header)] text-primary"><Folder class="size-4" /></div>
          <div class="min-w-0 flex-1">
            <div class="text-base font-semibold hover:text-primary">{o.org}</div>
          </div>
          <span class="g-subtle">{o.repos.length} repositor{o.repos.length === 1 ? 'y' : 'ies'}</span>
          <ChevronRight class="size-4 g-muted" />
        </a>
      {/each}
    </div>
  {/if}
</div>