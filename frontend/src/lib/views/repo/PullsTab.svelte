<script lang="ts">
  import * as Input from '$lib/components/ui/input'
  import * as Textarea from '$lib/components/ui/textarea'
  import * as NativeSelect from '$lib/components/ui/native-select'
  import DiffView from '$lib/components/DiffView.svelte'
  import { GitPullRequest, Plus } from '@lucide/svelte'
  import { app } from '$lib/stores/app.svelte'
</script>

{#if app.route.sub !== '' && app.route.sub !== 'new' && app.mrDetail}
  <div class="space-y-4">
    <div class="g-breadcrumb">
      <a href={`#/${encodeURIComponent(app.route.org!)}/${encodeURIComponent(app.route.repo!)}/pulls`}>Pull requests</a>
      <span class="divider">/</span>
      <span class="active">#{app.mrDetail.number}</span>
    </div>
    <div class="g-card p-4">
      <div class="flex flex-wrap items-center gap-2">
        <span class="text-lg font-semibold">#{app.mrDetail.number} {app.mrDetail.title}</span>
        <span class="rounded px-1.5 py-0.5 text-[11px]" style={app.mrDetail.state === 'open' ? 'background:var(--success);color:#fff' : 'background:var(--gitea-secondary)'}>{app.mrDetail.state}</span>
        <span class="rounded px-1.5 py-0.5 text-[11px]" style="background:var(--gitea-light-border)">{app.mrDetail.review_state}</span>
      </div>
      <div class="mt-1 g-subtle">{app.mrDetail.author} wants to merge {app.mrDetail.head_change_id.slice(0, 10)} into {app.mrDetail.base}</div>
      {#if app.mrDetail.body}<div class="mt-3 text-sm">{app.mrDetail.body}</div>{/if}
      {#if app.mrDetail.state === 'open'}
        <div class="mt-3">
          <button class="g-btn tiny red" onclick={() => void app.closeMr()}>Close pull</button>
        </div>
      {/if}
    </div>
    <div class="g-card overflow-hidden">
      <div class="border-b border-[var(--gitea-secondary)] px-4 py-2 text-xs font-semibold g-muted">Diff</div>
      <DiffView diffText={app.mrDiffText} />
    </div>
    <div class="grid grid-cols-1 gap-4 lg:grid-cols-2">
      <div class="g-card p-4">
        <h3 class="mb-2 text-sm font-semibold">Reviews</h3>
        {#each app.mrReviews as r (r.reviewer + r.state + r.body)}
          <div class="border-b border-[var(--gitea-secondary)] py-2">
            <div class="flex items-center gap-2 text-xs"><span class="font-medium">{r.reviewer}</span><span class="rounded px-1.5 py-0.5 text-[10px]" style="background:var(--gitea-light-border)">{r.state}</span></div>
            {#if r.body}<p class="mt-1 text-xs">{r.body}</p>{/if}
          </div>
        {/each}
        <div class="mt-3 flex gap-2">
          <NativeSelect.Root value={app.reviewState} onchange={(e) => (app.reviewState = (e.target as HTMLSelectElement).value)} class="h-8 rounded border border-[var(--gitea-input-border)] bg-[var(--gitea-input-bg)] px-2 text-xs">
            <NativeSelect.Option value="comment">Comment</NativeSelect.Option>
            <NativeSelect.Option value="approved">Approve</NativeSelect.Option>
            <NativeSelect.Option value="request_changes">Request changes</NativeSelect.Option>
          </NativeSelect.Root>
          <Input.Root class="flex-1" placeholder="review body" bind:value={app.reviewBody} />
          <button class="g-btn tiny primary" onclick={() => app.doReview()}>Submit</button>
        </div>
      </div>
      <div class="g-card p-4">
        <h3 class="mb-2 text-sm font-semibold">Comments</h3>
        {#each app.mrComments as c (c.author + c.body)}
          <div class="border-b border-[var(--gitea-secondary)] py-2">
            <div class="text-xs font-medium">{c.author}</div>
            <p class="mt-1 text-xs">{c.body}</p>
          </div>
        {/each}
        <div class="mt-3 flex gap-2">
          <Input.Root class="flex-1" placeholder="comment" bind:value={app.commentBody} />
          <button class="g-btn tiny" onclick={() => app.doComment()}>Add</button>
        </div>
      </div>
    </div>
  </div>
{:else}
  <div class="mb-3 flex items-center justify-between">
    <h2 class="text-sm font-semibold">Pull requests</h2>
    <button class="g-btn small primary" onclick={() => app.nav(`#/${encodeURIComponent(app.route.org!)}/${encodeURIComponent(app.route.repo!)}/pulls/new`)}><Plus class="size-3.5" /> New</button>
  </div>
  {#if app.route.sub === 'new'}
    <div class="g-card mb-4 space-y-2 p-4">
      <Input.Root placeholder="title" bind:value={app.newMrTitle} />
      <div class="flex gap-2">
        <Input.Root class="flex-1" placeholder="head branch" bind:value={app.newMrHead} />
        <Input.Root class="flex-1" placeholder="base (main)" bind:value={app.newMrBase} />
      </div>
      <Textarea.Root placeholder="description" bind:value={app.newMrBody} />
      <button class="g-btn tiny primary" onclick={() => app.doCreateMr()}>Create pull request</button>
    </div>
  {/if}
  <div class="g-card overflow-hidden">
    {#if app.mrs.length === 0}
      <p class="p-4 text-xs g-subtle">No pull requests</p>
    {:else}
      {#each app.mrs as m (m.number)}
        <a href={`#/${encodeURIComponent(app.route.org!)}/${encodeURIComponent(app.route.repo!)}/pulls/${m.number}`} class="flex w-full items-center gap-3 border-b border-[var(--gitea-secondary)] px-4 py-2.5 hover:bg-[var(--gitea-hover-opaque)] last:border-b-0">
          <GitPullRequest class="size-4 shrink-0" style="color:var(--success)" />
          <span class="font-mono text-xs g-muted">#{m.number}</span>
          <span class="min-w-0 flex-1 truncate text-xs">{m.title}</span>
          <span class="rounded px-1.5 py-0.5 text-[10px]" style={m.state === 'open' ? 'background:var(--success);color:#fff' : 'background:var(--gitea-secondary)'}>{m.state}</span>
          <span class="shrink-0 text-[11px] g-subtle">{m.author}</span>
        </a>
      {/each}
    {/if}
  </div>
{/if}