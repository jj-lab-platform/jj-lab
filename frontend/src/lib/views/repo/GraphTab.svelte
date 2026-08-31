<script lang="ts">
  import { LANE_COLORS } from '$lib/graph-layout'
  import { app } from '$lib/stores/app.svelte'
</script>

<h2 class="mb-3 text-sm font-semibold">Change graph</h2>
<div class="g-card overflow-hidden">
  {#if app.graph.length === 0}
    <p class="p-4 text-xs g-subtle">No commits</p>
  {:else}
    {@const lay = app.commitGraph}
    <div class="flex flex-col divide-y divide-[var(--gitea-secondary)]">
      {#each lay.rows as row (row.node.commit_id)}
        {@const color = LANE_COLORS[row.lane % LANE_COLORS.length]}
        <div class="flex items-center gap-3 px-4 py-2.5">
          <svg width="20" height="20" class="shrink-0">
            <circle cx="10" cy="10" r="5" fill={color} stroke={row.node.is_head ? 'var(--gitea-text-dark)' : 'rgba(127,127,127,0.4)'} stroke-width="1.5" />
          </svg>
          <a href={`#/${encodeURIComponent(app.route.org!)}/${encodeURIComponent(app.route.repo!)}/commit/${encodeURIComponent(row.node.commit_id)}`} class="flex min-w-0 flex-1 items-center gap-2">
            <span class="shrink-0 font-mono text-xs" style="color:var(--primary)">change {row.node.change_id.slice(0, 8)}</span>
            <span class="shrink-0 font-mono text-[11px] g-subtle">{row.node.commit_id.slice(0, 8)}</span>
            <span class="truncate text-xs">{row.node.message || '(empty)'}</span>
          </a>
          <span class="shrink-0 text-[11px] g-subtle">{row.node.author}</span>
          {#if row.node.is_head}<span class="shrink-0 rounded px-1 text-[10px]" style="background:var(--gitea-light-border);color:var(--primary)">head</span>{/if}
        </div>
      {/each}
    </div>
  {/if}
</div>