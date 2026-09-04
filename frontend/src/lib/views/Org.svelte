<script lang="ts">
  import { ChevronRight, BookOpen, Plus } from '@lucide/svelte'
  import { app } from '$lib/stores/app.svelte'
</script>

<div class="space-y-4">
  <div class="g-breadcrumb">
    <a href="#/">Explore</a>
    <span class="divider">/</span>
    <span class="active">{app.route.org}</span>
    <div class="ml-auto flex gap-2">
      <button class="g-btn small" onclick={() => (app.importOpen = true)}>Import</button>
      <button class="g-btn small primary" onclick={() => (app.createOpen = true)}><Plus class="size-3.5" /> New</button>
    </div>
  </div>
  <div class="g-card overflow-hidden">
    {#each (app.orgs.find(o => o.org === app.route.org)?.repos ?? []) as r (r.repo)}
      <a href={`#/${encodeURIComponent(app.route.org!)}/${encodeURIComponent(r.repo)}`} class="flex items-center gap-3 border-b border-[var(--gitea-secondary)] px-4 py-3 last:border-b-0 hover:bg-[var(--gitea-hover-opaque)]">
        <div class="flex size-8 items-center justify-center rounded bg-[var(--gitea-box-header)] text-[var(--gitea-text)]"><BookOpen class="size-4" /></div>
        <div class="min-w-0 flex-1">
          <div class="text-base font-semibold hover:text-primary">{r.repo}</div>
          <div class="g-subtle">default bookmark: {r.default_bookmark}</div>
        </div>
        <ChevronRight class="size-4 g-muted" />
      </a>
    {/each}
  </div>
</div>