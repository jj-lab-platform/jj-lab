<script lang="ts">
  import * as Input from '$lib/components/ui/input'
  import * as Textarea from '$lib/components/ui/textarea'
  import Markdown from '$lib/components/Markdown.svelte'
  import { Tag } from '@lucide/svelte'
  import { app } from '$lib/stores/app.svelte'
</script>

<h2 class="mb-3 text-sm font-semibold">Releases</h2>
<div class="g-card mb-4 space-y-2 p-4">
  <div class="flex gap-2">
    <Input.Root class="w-40" placeholder="tag (e.g. v1.0)" bind:value={app.relTag} />
    <Input.Root class="flex-1" placeholder="title" bind:value={app.relName} />
  </div>
  <Textarea.Root placeholder="release notes" bind:value={app.relBody} />
  <div class="flex items-center gap-3">
    <label class="flex items-center gap-2 text-xs"><input type="checkbox" bind:checked={app.relPre} /> pre-release</label>
    <button class="g-btn tiny primary" onclick={() => app.doCreateRelease()}>Publish release</button>
  </div>
</div>
<div class="space-y-3">
  {#if app.releases.length === 0}
    <div class="g-card p-8 text-center text-sm g-subtle">No releases</div>
  {:else}
    {#each app.releases as r (r.id)}
      <div class="g-card p-4">
        <div class="flex items-center gap-2">
          <Tag class="size-4" style="color:var(--primary)" />
          <span class="font-mono text-sm font-semibold">{r.tag_name}</span>
          {#if r.prerelease}<span class="rounded px-1.5 py-0.5 text-[10px]" style="background:var(--gitea-light-border)">pre-release</span>{/if}
          <button class="ml-auto font-mono text-[11px] hover:underline" style="color:var(--destructive)" onclick={() => void app.doDeleteRelease(r.tag_name)}>delete</button>
        </div>
        {#if r.body}<Markdown content={r.body} />{/if}
        {#if r.assets.length > 0}
          <div class="mt-2 flex flex-wrap gap-2">
            {#each r.assets as a (a.name)}
              <a href={a.browser_download_url} class="g-btn tiny">{a.name} · {a.size}</a>
            {/each}
          </div>
        {/if}
      </div>
    {/each}
  {/if}
</div>