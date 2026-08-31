<script lang="ts">
  import { onMount } from 'svelte'
  import * as Input from '$lib/components/ui/input'
  import * as Dialog from '$lib/components/ui/dialog'
  import ThemeMenu from '$lib/components/ThemeMenu.svelte'
  import TokenMenu from '$lib/components/TokenMenu.svelte'
  import { ChevronRight, Copy, Check, Download } from '@lucide/svelte'
  import { archiveUrl, cloneUrls } from '$lib/api'
  import { TABS } from '$lib/route'
  import { app } from '$lib/stores/app.svelte'
  import Explore from '$lib/views/Explore.svelte'
  import Org from '$lib/views/Org.svelte'
  import FilesTab from '$lib/views/repo/FilesTab.svelte'
  import CommitsTab from '$lib/views/repo/CommitsTab.svelte'
  import BranchesTab from '$lib/views/repo/BranchesTab.svelte'
  import GraphTab from '$lib/views/repo/GraphTab.svelte'
  import ChangesTab from '$lib/views/repo/ChangesTab.svelte'
  import OpLogTab from '$lib/views/repo/OpLogTab.svelte'
  import PullsTab from '$lib/views/repo/PullsTab.svelte'
  import ReleasesTab from '$lib/views/repo/ReleasesTab.svelte'
  import ActionsTab from '$lib/views/repo/ActionsTab.svelte'
  import SettingsTab from '$lib/views/repo/SettingsTab.svelte'

  onMount(() => {
    const stop = app.init()
    return () => stop()
  })
</script>

<div class="flex min-h-screen flex-col">
  <nav class="g-nav sticky top-0 z-40">
    <div class="g-nav-left">
      <a href="#/" class="g-nav-item gap-2" style="font-weight:600;color:var(--gitea-text-dark)">
        <div class="flex size-6 items-center justify-center rounded bg-primary text-[11px] font-bold text-primary-foreground">jj</div>
        <span class="text-sm">jjlab</span>
      </a>
      {#if app.route.org}
        <span class="g-nav-item gap-1" style="color:var(--gitea-text-light-2)">
          <a href="#/" class="hover:underline" style="color:inherit">orgs</a>
          <ChevronRight class="size-3" />
          {#if app.route.repo}
            <a href={`#/${encodeURIComponent(app.route.org)}`} class="hover:underline" style="color:inherit">{app.route.org}</a>
            <ChevronRight class="size-3" />
            <span style="color:var(--gitea-text-dark);font-weight:500">{app.route.repo}</span>
          {:else}
            <span style="color:var(--gitea-text-dark);font-weight:500">{app.route.org}</span>
          {/if}
        </span>
      {/if}
    </div>

    <div class="g-nav-right">
      {#if app.route.org && app.route.repo}
        <button class="g-btn small" onclick={() => app.copyClone(cloneUrls(app.route.org!, app.route.repo!).http)}>
          {#if app.cloneCopied}<Check class="size-3.5" /> Copied{:else}<Copy class="size-3.5" /> Clone{/if}
        </button>
        <a class="g-btn small" href={archiveUrl(app.route.org!, app.route.repo!, app.branch || 'main')} title="Download .tar.gz">
          <Download class="size-3.5" />
        </a>
      {/if}

      <ThemeMenu />

      <TokenMenu />
    </div>
  </nav>

  <main class="mx-auto w-full max-w-[1280px] flex-1 px-4 py-4">
    {#if app.route.org === null}
      <Explore />
    {:else if app.route.repo === null}
      <Org />
    {:else}
      <div class="g-repo-header">
        <div class="min-w-0">
          <div class="g-repo-title">
            <a class="muted hover:underline" href={`#/${encodeURIComponent(app.route.org!)}`}>{app.route.org}</a>
            <span class="g-muted mx-1">/</span>
            <a class="hover:underline" href={`#/${encodeURIComponent(app.route.org!)}/${encodeURIComponent(app.route.repo!)}`}>{app.route.repo}</a>
          </div>
        </div>
        <div class="g-repo-bucket">
          <button class="g-btn small" onclick={() => app.copyClone(cloneUrls(app.route.org!, app.route.repo!).http)}>
            {#if app.cloneCopied}<Check class="size-3.5" /> Copied{:else}<Copy class="size-3.5" /> Clone{/if}
          </button>
          <a class="g-btn small" href={archiveUrl(app.route.org!, app.route.repo!, app.branch || 'main')} title="Download source (.tar.gz)">
            <Download class="size-3.5" />
          </a>
        </div>
      </div>

      <nav class="g-tabs">
        {#each TABS as t (t.id)}
          <a href={`#/${encodeURIComponent(app.route.org!)}/${encodeURIComponent(app.route.repo!)}${t.id === 'files' ? '' : '/' + t.id}`}
             class={`g-tab ${app.route.tab === t.id ? 'active' : ''}`} aria-current={app.route.tab === t.id ? 'page' : undefined}>
            {t.label}
            {#if t.id === 'pulls' && app.mrs.length > 0}<span class="g-count">{app.mrs.length}</span>{/if}
          </a>
        {/each}
      </nav>

      <div class="mb-4">
        {#if app.route.tab === 'files'}
          <FilesTab />
        {:else if app.route.tab === 'commits'}
          <CommitsTab />
        {:else if app.route.tab === 'branches'}
          <BranchesTab />
        {:else if app.route.tab === 'graph'}
          <GraphTab />
        {:else if app.route.tab === 'changes'}
          <ChangesTab />
        {:else if app.route.tab === 'op-log'}
          <OpLogTab />
        {:else if app.route.tab === 'pulls'}
          <PullsTab />
        {:else if app.route.tab === 'releases'}
          <ReleasesTab />
        {:else if app.route.tab === 'actions'}
          <ActionsTab />
        {:else if app.route.tab === 'settings'}
          <SettingsTab />
        {/if}
      </div>
    {/if}
  </main>

  {#if app.error}
    <div class="fixed bottom-4 right-4 z-50 flex max-w-sm items-start gap-2 border p-3 text-xs shadow-md" style="border-color:var(--destructive);background:var(--gitea-overlay-backdrop);color:var(--destructive)">
      <span class="flex-1">{app.error}</span>
      <button class="shrink-0 underline" onclick={() => (app.error = null)}>Dismiss</button>
    </div>
  {/if}
  {#if app.notice}
    <div class="fixed bottom-4 left-4 z-50 flex max-w-sm items-start gap-2 border p-3 text-xs shadow-md" style="border-color:var(--success);background:var(--gitea-menu);color:var(--success)">
      <span class="flex-1">{app.notice}</span>
    </div>
  {/if}

  <!-- create repo dialog -->
  <Dialog.Root open={app.createOpen} onOpenChange={(o) => (app.createOpen = o)}>
    <Dialog.Content class="sm:max-w-md">
      <Dialog.Header>
        <Dialog.Title>New repository</Dialog.Title>
        <Dialog.Description>Create an empty repository (initialized with a README).</Dialog.Description>
      </Dialog.Header>
      <div class="space-y-3 p-4">
        <div>
          <label for="cr-org" class="mb-1 block text-xs font-medium">Organization</label>
          <Input.Root id="cr-org" placeholder="org" bind:value={app.createOrg} />
        </div>
        <div>
          <label for="cr-repo" class="mb-1 block text-xs font-medium">Repository name</label>
          <Input.Root id="cr-repo" placeholder="repo" bind:value={app.createRepoName} />
        </div>
      </div>
      <Dialog.Footer>
        <button class="g-btn tiny" onclick={() => (app.createOpen = false)}>Cancel</button>
        <button class="g-btn tiny primary" disabled={app.working || !app.createOrg || !app.createRepoName} onclick={app.doCreateRepo}>Create</button>
      </Dialog.Footer>
    </Dialog.Content>
  </Dialog.Root>

  <!-- import dialog -->
  <Dialog.Root open={app.importOpen} onOpenChange={(o) => (app.importOpen = o)}>
    <Dialog.Content class="sm:max-w-md">
      <Dialog.Header>
        <Dialog.Title>Import repository</Dialog.Title>
        <Dialog.Description>Clone an external Git URL into a new jjlab repository.</Dialog.Description>
      </Dialog.Header>
      <div class="space-y-3 p-4">
        <Input.Root placeholder="org" bind:value={app.importOrg} />
        <Input.Root placeholder="repo name" bind:value={app.importRepoName} />
        <Input.Root placeholder="git url" bind:value={app.importUrl} oninput={(e) => { const v = (e.target as HTMLInputElement).value; if (!app.importRepoName) app.importRepoName = app.deriveName(v) }} />
        <Input.Root placeholder="branch (optional)" bind:value={app.importBranch} />
      </div>
      <Dialog.Footer>
        <button class="g-btn tiny" onclick={() => (app.importOpen = false)}>Cancel</button>
        <button class="g-btn tiny primary" disabled={app.working || !app.importOrg || !app.importRepoName || !app.importUrl} onclick={app.doImport}>Import</button>
      </Dialog.Footer>
    </Dialog.Content>
  </Dialog.Root>
</div>
