<script lang="ts">
  import * as Input from '$lib/components/ui/input'
  import { GitBranch, Tag, Plus } from '@lucide/svelte'
  import { app } from '$lib/stores/app.svelte'
</script>

<div class="mb-3 flex flex-wrap items-center gap-2">
  <Input.Root class="w-40" placeholder="bookmark name" bind:value={app.newBookmarkName} />
  <Input.Root class="w-44" placeholder="from (empty=head)" bind:value={app.newBookmarkFrom} />
  <button class="g-btn tiny primary" onclick={() => app.doCreateBookmark()}><Plus class="size-3.5" /> Bookmark</button>
  <span class="mx-2 g-muted">|</span>
  <Input.Root class="w-36" placeholder="tag name" bind:value={app.newTagName} />
  <Input.Root class="w-36" placeholder="from" bind:value={app.newTagFrom} />
  <button class="g-btn tiny" onclick={() => app.doCreateTag()}><Tag class="size-3.5" /> Tag</button>
</div>
<h3 class="mb-2 text-xs font-semibold g-muted">Bookmarks</h3>
<div class="g-card overflow-hidden">
  {#each app.bookmarks as b (b.name)}
    <div class="flex w-full items-center gap-3 border-b border-[var(--gitea-secondary)] px-4 py-2.5 last:border-b-0">
      <a href={`#/${encodeURIComponent(app.route.org!)}/${encodeURIComponent(app.route.repo!)}`} onclick={() => { app.bookmark = b.name; void app.loadTree() }}
         class="flex min-w-0 flex-1 items-center gap-3">
        <GitBranch class="size-4 shrink-0 g-muted" />
        <span class="truncate font-mono text-xs">{b.name}</span>
        <span class="shrink-0 font-mono text-[11px]" style="color:var(--primary)">{b.sha.slice(0, 8)}</span>
      </a>
      <button class="shrink-0 font-mono text-[11px] hover:underline" style="color:var(--destructive)" onclick={() => void app.doDeleteBookmark(b.name)}>delete</button>
    </div>
  {/each}
</div>
<h3 class="mb-2 mt-6 text-xs font-semibold g-muted">Tags</h3>
<div class="g-card overflow-hidden">
  {#if app.tags.length === 0}
    <p class="p-4 text-xs g-subtle">No tags</p>
  {:else}
    {#each app.tags as t (t.name)}
      <div class="flex w-full items-center gap-3 border-b border-[var(--gitea-secondary)] px-4 py-2.5 last:border-b-0">
        <a href={`#/${encodeURIComponent(app.route.org!)}/${encodeURIComponent(app.route.repo!)}/commit/${encodeURIComponent(t.sha)}`} class="flex min-w-0 flex-1 items-center gap-3">
          <Tag class="size-4 shrink-0 g-muted" />
          <span class="truncate font-mono text-xs">{t.name}</span>
          <span class="shrink-0 font-mono text-[11px]" style="color:var(--primary)">{t.sha.slice(0, 8)}</span>
        </a>
        <button class="shrink-0 font-mono text-[11px] hover:underline" style="color:var(--destructive)" onclick={() => void app.doDeleteTag(t.name)}>delete</button>
      </div>
    {/each}
  {/if}
</div>