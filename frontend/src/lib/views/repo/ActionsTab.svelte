<script lang="ts">
  import { Play } from '@lucide/svelte'
  import { app } from '$lib/stores/app.svelte'
</script>

<h2 class="mb-3 text-sm font-semibold">Workflows</h2>
<div class="g-card overflow-hidden">
  {#if app.workflows.length === 0}
    <p class="p-4 text-xs g-subtle">No workflows defined</p>
  {:else}
    {#each app.workflows as w (w.id)}
      <div class="flex w-full items-center gap-3 border-b border-[var(--gitea-secondary)] px-4 py-2.5 last:border-b-0">
        <Play class="size-4 shrink-0 g-muted" />
        <span class="min-w-0 flex-1 font-mono text-xs">{w.name}</span>
        <span class="font-mono text-[10px] g-subtle">{w.path}</span>
        <button class="g-btn tiny" onclick={() => void app.doDispatch(w)}>Run</button>
      </div>
    {/each}
  {/if}
</div>

<h2 class="mb-3 mt-6 text-sm font-semibold">Runs</h2>
<div class="grid grid-cols-1 gap-4 lg:grid-cols-2">
  <div class="g-card overflow-hidden">
    {#if app.runs.length === 0}
      <p class="p-4 text-xs g-subtle">No runs</p>
    {:else}
      {#each app.runs as r (r.id)}
        <button class="flex w-full items-center gap-3 border-b border-[var(--gitea-secondary)] px-4 py-2.5 text-left hover:bg-[var(--gitea-hover-opaque)] last:border-b-0" onclick={() => void app.openRun(r.id)}>
          <span class="font-mono text-xs" style="color:var(--primary)">#{r.id}</span>
          <span class="min-w-0 flex-1 truncate text-xs">{r.trigger_ref || 'manual'}</span>
          <span class="rounded px-1.5 py-0.5 text-[10px]" style={r.status === 'success' ? 'background:var(--success);color:#fff' : r.status === 'failed' ? 'background:var(--destructive);color:#fff' : 'background:var(--gitea-light-border)'}>{r.status}</span>
        </button>
      {/each}
    {/if}
  </div>
  <div class="space-y-3">
    {#if app.activeRun !== null}
      <div class="g-card p-4">
        <h3 class="mb-2 text-sm font-semibold">Run #{app.activeRun} jobs</h3>
        {#each app.runJobs as j (j.id)}
          <div class="flex items-center gap-2 border-b border-[var(--gitea-secondary)] py-2">
            <span class="min-w-0 flex-1 font-mono text-xs">{j.name}</span>
            <span class="rounded px-1.5 py-0.5 text-[10px]" style="background:var(--gitea-light-border)">{j.status}</span>
            <button class="g-btn tiny basic" onclick={() => void app.openJobLog(j.id)}>logs</button>
          </div>
        {/each}
      </div>
    {/if}
    {#if app.jobLogText}
      <div class="g-card p-4">
        <h3 class="mb-2 text-sm font-semibold">Logs</h3>
        <pre class="max-h-96 overflow-auto whitespace-pre-wrap font-mono text-xs">{app.jobLogText}</pre>
      </div>
    {/if}
  </div>
</div>