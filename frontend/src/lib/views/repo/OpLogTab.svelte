<script lang="ts">
  import { History, AlertTriangle } from '@lucide/svelte'
  import { app } from '$lib/stores/app.svelte'

  function payloadChangeId(op: { payload: string }): string | undefined {
    try {
      const changeId = JSON.parse(op.payload).change_id
      return typeof changeId === 'string' ? changeId : undefined
    } catch {
      return undefined
    }
  }
</script>

<h2 class="mb-3 flex items-center gap-2 text-sm font-semibold"><History class="size-4" /> Operation log</h2>
{#if app.conflicts.length > 0}
  <div class="mb-4 border p-3" style="border-color:var(--destructive);background:color-mix(in srgb, var(--destructive) 10%, transparent)">
    <div class="flex items-center gap-2 text-sm font-semibold" style="color:var(--destructive)"><AlertTriangle class="size-4" /> {app.conflicts.length} conflicted path{app.conflicts.length === 1 ? '' : 's'}</div>
    {#each app.conflicts as cf (cf.id)}
      <div class="mt-1 font-mono text-xs">{cf.path}</div>
    {/each}
  </div>
{/if}
<div class="g-card overflow-hidden">
  {#if app.ops.length === 0}
    <p class="p-4 text-xs g-subtle">No operations recorded</p>
  {:else}
    {#each app.ops as op (op.id)}
      {@const changeId = payloadChangeId(op)}
      <div class="flex w-full items-center gap-3 border-b border-[var(--gitea-secondary)] px-4 py-2.5 last:border-b-0">
        <span class="shrink-0 font-mono text-[11px]" style="color:var(--primary)">{op.id.slice(0, 10)}</span>
        <span class="shrink-0 rounded px-1.5 py-0.5 font-mono text-[10px]" style="background:var(--gitea-light-border)">{op.op_type}</span>
        {#if op.undo_of}<span class="shrink-0 font-mono text-[10px] g-subtle">undoes {op.undo_of.slice(0, 8)}</span>{/if}
        {#if (op.op_type === 'write' || op.op_type === 'delete') && changeId}
          <button class="shrink-0 font-mono text-[11px]" style="color:var(--primary)" onclick={() => { void app.loadChangeDetail(changeId) }}>{changeId.slice(0, 10)}</button>
        {/if}
        <span class="ml-auto shrink-0">
          {#if !op.undo_of}
            <button class="g-btn tiny basic" onclick={() => void app.doUndo(op.id)}>undo</button>
          {/if}
        </span>
      </div>
    {/each}
  {/if}
</div>