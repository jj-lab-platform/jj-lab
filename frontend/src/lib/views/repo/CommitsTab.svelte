<script lang="ts">
  import DiffView from '$lib/components/DiffView.svelte'
  import { app } from '$lib/stores/app.svelte'
</script>

{#if app.route.sub === 'commit' && app.commitDetail}
  <div class="space-y-4">
    <div class="g-breadcrumb">
      <a href={`#/${encodeURIComponent(app.route.org!)}/${encodeURIComponent(app.route.repo!)}/commits`}>Commits</a>
      <span class="divider">/</span>
      <span class="active font-mono">{app.commitDetail.sha.slice(0, 10)}</span>
    </div>
    <div class="g-card p-4">
      <div class="flex flex-wrap items-center gap-2">
        <span class="font-mono text-sm" style="color:var(--primary)">{app.commitDetail.sha}</span>
        <span class="g-subtle font-mono">change {app.commitDetail.change_id.slice(0, 10)}</span>
        <span class="ml-auto g-subtle">{app.commitDetail.author}</span>
      </div>
      <div class="mt-2 whitespace-pre-wrap">{app.commitDetail.description}</div>
    </div>
    <div class="g-card overflow-hidden">
      <div class="border-b border-[var(--gitea-secondary)] px-3 py-2 text-xs font-semibold g-muted">Changes</div>
      <DiffView diffText={app.commitDiffText} />
    </div>
  </div>
{:else}
  <div class="g-card overflow-hidden">
    {#if app.commits.length === 0}
      <p class="p-4 text-xs g-subtle">No commits</p>
    {:else}
      <div class="overflow-x-auto">
        <table class="g-commit-table">
          <thead>
            <tr><th class="w-40">Author</th><th class="w-24">SHA</th><th>Message</th><th class="w-28 text-right">Date</th></tr>
          </thead>
          <tbody>
            {#each app.commits as c (c.sha)}
              <tr>
                <td class="g-subtle" title={c.author}>{c.author.split(' <')[0]}</td>
                <td><a class="sha" href={`#/${encodeURIComponent(app.route.org!)}/${encodeURIComponent(app.route.repo!)}/commit/${encodeURIComponent(c.sha)}`}>{c.sha.slice(0, 10)}</a></td>
                <td class="msg">
                  <a href={`#/${encodeURIComponent(app.route.org!)}/${encodeURIComponent(app.route.repo!)}/commit/${encodeURIComponent(c.sha)}`} style="color:inherit">
                    {c.description.trim() || '(empty)'}
                  </a>
                  <span class="g-subtle font-mono" style="margin-left:6px">change {c.change_id.slice(0, 8)}</span>
                </td>
                <td class="text-right g-subtle">{c.committer || c.author}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
      <div class="flex items-center justify-between border-t border-[var(--gitea-secondary)] px-4 py-2">
        <span class="g-subtle">{app.commitTotal} total</span>
        <div class="flex gap-1">
          <button class="g-btn tiny" disabled={app.commitPage <= 1} onclick={() => { void app.loadCommits(app.commitPage - 1) }}>Prev</button>
          <button class="g-btn tiny" disabled={app.commitPage * app.pageSize >= app.commitTotal} onclick={() => { void app.loadCommits(app.commitPage + 1) }}>Next</button>
        </div>
      </div>
    {/if}
  </div>
{/if}