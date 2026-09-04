<script lang="ts">
  import * as Input from '$lib/components/ui/input'
  import * as Textarea from '$lib/components/ui/textarea'
  import * as Skeleton from '$lib/components/ui/skeleton'
  import * as NativeSelect from '$lib/components/ui/native-select'
  import CodeView from '$lib/components/CodeView.svelte'
  import Markdown from '$lib/components/Markdown.svelte'
  import TreeNode from '$lib/components/TreeNode.svelte'
  import { GitBranch, Folder, File, BookOpen } from '@lucide/svelte'
  import { app } from '$lib/stores/app.svelte'
</script>

<div class="mb-3 flex items-center gap-2">
  <NativeSelect.Root value={app.bookmark} onchange={(e) => { app.bookmark = (e.target as HTMLSelectElement).value; app.selectedPath = null; app.fileData = null; void app.loadTree(); void app.loadReadme() }}
    class="flex h-8 w-52 items-center rounded border border-[var(--gitea-input-border)] bg-[var(--gitea-input-bg)] px-2 text-xs font-mono text-[var(--gitea-text)]">
    <NativeSelect.Option disabled>bookmark</NativeSelect.Option>
    {#each app.bookmarks as b (b.name)}
      <NativeSelect.Option value={b.name}> <GitBranch class="size-3 inline" /> {b.name}</NativeSelect.Option>
    {/each}
  </NativeSelect.Root>
  <span class="g-subtle">{app.bookmarks.length} bookmark{app.bookmarks.length === 1 ? '' : 's'} · {app.tags.length} tags</span>
</div>

{#if app.route.sub === 'blob' && app.selectedPath}
  <!-- file view -->
  <div class="g-card overflow-hidden">
    <div class="flex items-center gap-2 border-b border-[var(--gitea-secondary)] px-3 py-2">
      <div class="g-breadcrumb min-w-0">
        {#each app.selectedPath.split('/') as seg, i (i)}
          {#if i > 0}<span class="divider">/</span>{/if}
          <span class={i === app.selectedPath.split('/').length - 1 ? 'active' : ''}>{seg}</span>
        {/each}
      </div>
      <div class="ml-auto flex gap-1.5">
        <button class="g-btn tiny basic" onclick={() => app.loadFileLog()}>History</button>
        {#if app.editing}
          <button class="g-btn tiny primary" onclick={() => app.saveFile()}>Save</button>
          <button class="g-btn tiny" onclick={() => { app.editing = false; app.editContent = app.fileData?.content ?? '' }}>Cancel</button>
        {:else}
          <button class="g-btn tiny basic" onclick={() => app.startEdit()}>Edit</button>
          <button class="g-btn tiny red" onclick={() => app.removeFile()}>Delete</button>
        {/if}
      </div>
    </div>
    {#if app.editing}
      <div class="p-3">
        <Textarea.Root class="h-[60vh] w-full resize-none font-mono text-xs" bind:value={app.editContent} />
        <div class="mt-2 flex items-center gap-2">
          <Input.Root class="max-w-md flex-1" placeholder="commit message" bind:value={app.editMessage} />
          <label class="flex items-center gap-1 text-[11px] g-muted">
            <input type="checkbox" bind:checked={app.editAmend} />
            amend head change
          </label>
          <button class="g-btn tiny primary" onclick={() => app.saveFile()}>Commit</button>
        </div>
      </div>
    {:else if app.fileData}
      <CodeView code={app.fileData.content} filepath={app.selectedPath} />
    {:else}
      <div class="p-8 text-center text-sm g-subtle">Loading…</div>
    {/if}
  </div>
{:else}
  <!-- repo view: left tree sidebar + right content (Gitea layout) -->
  <div class="g-repo-view">
    <aside class="g-tree-sidebar hidden md:block">
      <div class="g-tree-sidebar-head"><GitBranch class="size-3.5" /> Files</div>
      {#if app.tree.length === 0}
        <p class="p-3 text-xs g-subtle">Empty repository</p>
      {:else}
        <TreeNode entries={app.tree} expanded={app.expandedDirs} selectedPath={null} onToggle={(p) => app.toggleDir(p)} onOpen={(p) => { app.selectedPath = p; void app.openFile(p); app.nav(`#/${encodeURIComponent(app.route.org!)}/${encodeURIComponent(app.route.repo!)}/blob/${p.split('/').map(encodeURIComponent).join('/')}`) }} />
      {/if}
    </aside>
    <div class="g-repo-content">
      {#if app.loading}
        <div class="space-y-1">{#each Array(6) as _}<Skeleton.Root class="h-8 w-full" />{/each}</div>
      {:else}
        <div class="g-files">
          <div class="g-files-row">
            <div class="g-files-cell g-files-head">
              <GitBranch class="size-3.5 g-muted" />
              <span class="g-muted text-xs">{app.bookmark || 'main'}</span>
            </div>
          </div>
          {#each app.tree as entry (entry.path)}
            {@const isDir = entry.is_dir}
            <div class="g-files-row">
              <div class="g-files-cell name">
                <span class="g-muted">{#if isDir}<Folder class="size-3.5" />{:else}<File class="size-3.5" />{/if}</span>
                {#if isDir}
                  <button class="g-files-name" onclick={() => app.toggleDir(entry.path)}>{entry.name}</button>
                {:else}
                  <a class="g-files-name" href={`#/${encodeURIComponent(app.route.org!)}/${encodeURIComponent(app.route.repo!)}/blob/${entry.path.split('/').map(encodeURIComponent).join('/')}`} onclick={() => { app.selectedPath = entry.path; void app.openFile(entry.path) }}>{entry.name}</a>
                {/if}
              </div>
              <div class="g-files-cell">
                <span class="g-files-msg" title={entry.path}>{entry.path}</span>
              </div>
              <div class="g-files-cell">
                <span class="g-files-age">{entry.size ? entry.size + ' B' : ''}</span>
              </div>
            </div>
          {/each}
        </div>

        {#if app.readmeLabel() && app.readmeText !== null}
          <div class="g-card mt-4 p-5">
            <h3 class="mb-3 border-b border-[var(--gitea-secondary)] pb-2 text-sm font-semibold">
              <BookOpen class="mr-1.5 inline size-4 g-muted" />README.md
            </h3>
            <Markdown content={app.readmeText} />
          </div>
        {/if}
      {/if}
    </div>
  </div>
{/if}