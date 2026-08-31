<script lang="ts">
  import * as Input from '$lib/components/ui/input'
  import { archiveUrl, cloneUrls } from '$lib/api'
  import { app } from '$lib/stores/app.svelte'
</script>

<h2 class="mb-3 text-sm font-semibold">Repository settings</h2>
<div class="g-card space-y-4 p-4">
  <div>
    <div class="mb-1 text-xs font-semibold g-muted">Clone URL</div>
    <div class="flex items-center gap-2">
      <code class="min-w-0 flex-1 rounded border border-[var(--gitea-secondary)] bg-[var(--gitea-box-header)] px-2 py-1.5 font-mono text-xs">{cloneUrls(app.route.org!, app.route.repo!).http}</code>
      <button class="g-btn tiny" onclick={() => void app.copyClone(cloneUrls(app.route.org!, app.route.repo!).http)}>{app.cloneCopied ? 'Copied' : 'Copy'}</button>
    </div>
  </div>
  <div>
    <div class="mb-1 text-xs font-semibold g-muted">Download</div>
    <a href={archiveUrl(app.route.org!, app.route.repo!, app.branch || 'main')} class="text-xs hover:underline" style="color:var(--primary)">source (tar.gz)</a>
  </div>
  <div class="border-t border-[var(--gitea-secondary)] pt-4">
    <div class="mb-1 text-xs font-semibold" style="color:var(--destructive)">Danger zone</div>
    <div class="flex items-center gap-2">
      <Input.Root class="max-w-xs" placeholder="confirm repo name" bind:value={app.renameName} />
      <button class="g-btn tiny red" disabled={app.renameName !== app.route.repo} onclick={app.doDeleteRepo}>Delete repository</button>
    </div>
    <p class="mt-1 text-[11px] g-subtle">Type the repository name to enable deletion.</p>
  </div>
</div>