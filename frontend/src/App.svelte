<script lang="ts">
  import { onMount } from 'svelte'
  import * as Input from '$lib/components/ui/input'
  import * as Textarea from '$lib/components/ui/textarea'
  import * as Skeleton from '$lib/components/ui/skeleton'
  import * as Dialog from '$lib/components/ui/dialog'
  import * as NativeSelect from '$lib/components/ui/native-select'
  import CodeView from '$lib/components/CodeView.svelte'
  import DiffView from '$lib/components/DiffView.svelte'
  import Markdown from '$lib/components/Markdown.svelte'
  import TreeNode, { type TreeEntry } from '$lib/components/TreeNode.svelte'
  import ThemeMenu from '$lib/components/ThemeMenu.svelte'
  import TokenMenu from '$lib/components/TokenMenu.svelte'
  import {
    ChevronRight, GitBranch, BookOpen, Folder, Download,
    GitPullRequest, Tag, Play, Copy, Plus,
    History, AlertTriangle,
    File,
  } from '@lucide/svelte'
  import {
    fetchOrgs, fetchBranches, fetchTags, fetchCommits, fetchCommit, fetchChange,
    fetchTree, fetchFile, fetchRawFile, fetchDiff, fetchGraph, fetchFileLog,
    searchCode, fetchOpLog, fetchConflicts, fetchBookmarks, undoOp,
    createRepo, deleteRepo, writeFile, createFile, deleteFile, createBranch,
    deleteBranch, createTag, deleteTag, cloneRemote, fetchMrs, fetchMr, createMr,
    updateMrState, fetchReviews, addReview, fetchComments, addComment, fetchMrDiff,
    fetchReleases, createRelease, deleteRelease, fetchWorkflows, dispatchWorkflow,
    fetchRuns, fetchJobs, fetchJobLogs, archiveUrl, cloneUrls, decodeContent,
    type Org, type CommitInfo, type BranchInfo, type GraphNode, type Mr,
    type MrReview, type MrComment, type Release, type Workflow, type Run, type Job,
    type OpLogEntry, type Conflict, type DbBookmark, type SearchHit,
  } from '$lib/api'
  import { parseDiff } from '$lib/diff-parser'
  import { LANE_COLORS, layoutGraph } from '$lib/graph-layout'
  import { TABS, type Tab, type Route } from '$lib/route'

  // route state
  let route: Route = $state({ org: null, repo: null, tab: 'files', sub: '', sub2: '' })

  let error: string | null = $state(null)
  let notice: string | null = $state(null)

  function flash(msg: string): void {
    notice = msg
    setTimeout(() => { if (notice === msg) notice = null }, 4000)
  }

  function parseHash(): void {
    const h = window.location.hash.replace(/^#\/?/, '')
    const parts = h.split('/').filter(Boolean).map(decodeURIComponent)
    if (parts.length === 0) {
      route = { org: null, repo: null, tab: 'files', sub: '', sub2: '' }
      return
    }
    if (parts.length === 1) {
      route = { org: parts[0]!, repo: null, tab: 'files', sub: '', sub2: '' }
      return
    }
    const [org, repo] = parts
    // special sub-routes
    if (parts[2] === 'commit' && parts[3]) {
      route = { org: org!, repo: repo!, tab: 'commits', sub: 'commit', sub2: parts[3]! }
      return
    }
    if (parts[2] === 'pulls' && parts[3]) {
      route = { org: org!, repo: repo!, tab: 'pulls', sub: parts[3]! || '', sub2: '' }
      return
    }
    if (parts[2] === 'blob' && parts[3]) {
      const path = parts.slice(3).join('/')
      route = { org: org!, repo: repo!, tab: 'files', sub: 'blob', sub2: path }
      return
    }
    const tab = TABS.some(t => t.id === parts[2]) ? (parts[2] as Tab) : 'files'
    route = { org: org!, repo: repo!, tab, sub: '', sub2: '' }
  }

  function nav(hash: string): void {
    if (window.location.hash === hash) void applyRoute()
    else window.location.hash = hash
  }

  // ── app-level data ──
  let orgs: Org[] = $state([])
  let reposLoading = $state(false)

  async function loadOrgs(): Promise<void> {
    reposLoading = true
    try {
      orgs = await fetchOrgs()
    } catch (e) {
      error = e instanceof Error ? e.message : String(e)
    } finally {
      reposLoading = false
    }
  }

  // ── explore create repo ──
  let createOpen = $state(false)
  let createOrg = $state('')
  let createRepoName = $state('')
  let importOpen = $state(false)
  let importOrg = $state('')
  let importRepoName = $state('')
  let importUrl = $state('')
  let importBranch = $state('')
  let working = $state(false)

  async function doCreateRepo(): Promise<void> {
    if (!createOrg || !createRepoName) return
    working = true
    try {
      await createRepo(createOrg, createRepoName)
      createOpen = false
      createRepoName = ''
      await loadOrgs()
      flash(`Created ${createOrg}/${createRepoName}`)
      nav(`#/${encodeURIComponent(createOrg)}/${encodeURIComponent(createRepoName)}`)
    } catch (e) {
      error = e instanceof Error ? e.message : String(e)
    } finally {
      working = false
    }
  }

  async function doImport(): Promise<void> {
    if (!importOrg || !importRepoName || !importUrl) return
    working = true
    try {
      await cloneRemote(importOrg, importRepoName, importUrl, importBranch || undefined)
      importOpen = false
      await loadOrgs()
      flash(`Imported ${importOrg}/${importRepoName}`)
    } catch (e) {
      error = e instanceof Error ? e.message : String(e)
    } finally {
      working = false
    }
  }

  function deriveName(url: string): string {
    const m = url.trim().match(/[/:]([^/:]+?)(?:\.git)?(?:\/)?$/)
    return m ? m[1]! : ''
  }

  // ── repository context ──
  let branch = $state('')
  let branches: BranchInfo[] = $state([])
  let tags: BranchInfo[] = $state([])
  let tree: TreeEntry[] = $state([])
  let commits: CommitInfo[] = $state([])
  let commitTotal = $state(0)
  let commitPage = $state(1)
  let pageSize = 30
  let graph: GraphNode[] = $state([])
  let loading = $state(false)

  async function refreshRepo(): Promise<void> {
    const { org, repo } = route
    if (!org || !repo) return
    loading = true
    error = null
    try {
      const bs = await fetchBranches(org, repo)
      branches = bs
      if (branches.length && !branch) branch = branches.find(b => b.name === 'main')?.name ?? branches[0]!.name
      else if (branches.length === 0) branch = 'main'
      else if (!branches.some(b => b.name === branch)) branch = branches[0]!.name
      tags = []
      try {
        tags = await fetchTags(org, repo)
      } catch { tags = [] }
      await loadTree()
      await loadCommits(1)
    } catch (e) {
      error = e instanceof Error ? e.message : String(e)
    } finally {
      loading = false
    }
  }

  async function loadTree(): Promise<void> {
    const { org, repo } = route
    if (!org || !repo || !branch) return
    try {
      const raw = await fetchTree(org, repo, branch || 'main')
      tree = raw.map(e => ({
        name: e.path.split('/').pop() ?? e.path,
        path: e.path,
        is_dir: e.kind === 'tree',
        size: e.size,
      }))
      await loadReadme()
    } catch (e) {
      tree = []
    }
  }

  async function loadCommits(page: number): Promise<void> {
    const { org, repo } = route
    if (!org || !repo) return
    try {
      const res = await fetchCommits(org, repo, page || 1, pageSize)
      commits = res.commits
      commitTotal = res.total_count
      commitPage = page || 1
    } catch (e) {
      commits = []
    }
  }

  async function loadGraph(): Promise<void> {
    const { org, repo } = route
    if (!org || !repo) return
    try {
      graph = await fetchGraph(org, repo)
    } catch { graph = [] }
  }

  // ── files tab ──
  let expandedDirs = $state(new Set<string>(['']))
  let selectedPath = $state<string | null>(null)
  let fileData = $state<{ content: string; encoding: string; size: number } | null>(null)
  let editing = $state(false)
  let editContent = $state('')
  let editMessage = $state('')
  let editAmend = $state(true)
  let fileLog: CommitInfo[] = $state([])
  let readmeText = $state<string | null>(null)
  let readmeFailed = $state(false)

  function readmeLabel(): TreeEntry | null {
    return tree.find(e => e.path.toLowerCase() === 'readme.md') ?? null
  }

  async function loadReadme(): Promise<void> {
    readmeText = null
    readmeFailed = false
    const rm = readmeLabel()
    const { org, repo } = route
    if (!rm || !org || !repo) return
    try {
      readmeText = await fetchRawFile(org, repo, rm.path)
    } catch {
      readmeFailed = true
    }
  }

  async function openFile(path: string): Promise<void> {
    const { org, repo } = route
    if (!org || !repo) return
    selectedPath = path
    editing = false
    fileData = null
    try {
      const f = await fetchFile(org, repo, branch || 'main', path)
      fileData = { content: decodeContent(f), encoding: f.encoding, size: f.size }
    } catch (e) {
      error = e instanceof Error ? e.message : String(e)
    }
  }

  function startEdit(): void {
    editing = true
    editContent = fileData?.content ?? ''
    editMessage = ''
    editAmend = true
  }

  async function saveFile(): Promise<void> {
    const { org, repo } = route
    if (!org || !repo || !selectedPath) return
    try {
      await writeFile(org, repo, branch || 'main', selectedPath, editContent, editMessage || `update ${selectedPath}`, editAmend)
      fileData = { content: editContent, encoding: 'utf-8', size: editContent.length }
      editing = false
      await Promise.all([loadTree(), loadCommits(commitPage)])
      flash(`Saved ${selectedPath}`)
    } catch (e) {
      error = e instanceof Error ? e.message : String(e)
    }
  }

  async function removeFile(): Promise<void> {
    const { org, repo } = route
    if (!org || !repo || !selectedPath) return
    if (!confirm(`Delete ${selectedPath}?`)) return
    try {
      await deleteFile(org, repo, branch || 'main', selectedPath, `delete ${selectedPath}`)
      selectedPath = null
      fileData = null
      await Promise.all([loadTree(), loadCommits(commitPage)])
    } catch (e) {
      error = e instanceof Error ? e.message : String(e)
    }
  }

  async function loadFileLog(): Promise<void> {
    const { org, repo } = route
    if (!org || !repo || !selectedPath) return
    try {
      const res = await fetchFileLog(org, repo, selectedPath)
      fileLog = res.commits
    } catch { fileLog = [] }
  }

  function toggleDir(path: string): void {
    expandedDirs = new Set(expandedDirs)
    if (expandedDirs.has(path)) expandedDirs.delete(path)
    else expandedDirs.add(path)
  }

  // ── branches/tags tab ──
  let newBranchFrom = $state('')
  let newBranchName = $state('')
  let newTagFrom = $state('')
  let newTagName = $state('')

  async function doCreateBranch(): Promise<void> {
    const { org, repo } = route
    if (!org || !repo || !newBranchName) return
    try {
      await createBranch(org, repo, newBranchName, newBranchFrom || branch || 'main')
      newBranchName = ''
      newBranchFrom = ''
      await refreshRepo()
    } catch (e) {
      error = e instanceof Error ? e.message : String(e)
    }
  }

  async function doCreateTag(): Promise<void> {
    const { org, repo } = route
    if (!org || !repo || !newTagName) return
    try {
      await createTag(org, repo, newTagName, newTagFrom || branch || 'main')
      newTagName = ''
      newTagFrom = ''
      tags = await fetchTags(org, repo)
    } catch (e) {
      error = e instanceof Error ? e.message : String(e)
    }
  }

  async function doDeleteBranch(name: string): Promise<void> {
    const { org, repo } = route
    if (!org || !repo) return
    if (!confirm(`Delete branch ${name}?`)) return
    try {
      await deleteBranch(org, repo, name)
      await refreshRepo()
    } catch (e) {
      error = e instanceof Error ? e.message : String(e)
    }
  }

  async function doDeleteTag(name: string): Promise<void> {
    const { org, repo } = route
    if (!org || !repo) return
    if (!confirm(`Delete tag ${name}?`)) return
    try {
      await deleteTag(org, repo, name)
      tags = await fetchTags(org, repo)
    } catch (e) {
      error = e instanceof Error ? e.message : String(e)
    }
  }

  // ── commit detail ──
  let commitDetail = $state<CommitInfo | null>(null)
  let commitDiffText = $state('')

  async function loadCommitDetail(sha: string): Promise<void> {
    const { org, repo } = route
    if (!org || !repo) return
    commitDetail = null
    commitDiffText = ''
    try {
      const c = await fetchCommit(org, repo, sha)
      commitDetail = c
      const parent = c.parents[0]
      commitDiffText = await fetchDiff(org, repo, parent ?? 'root', sha)
    } catch (e) {
      error = e instanceof Error ? e.message : String(e)
    }
  }

  // ── changes tab (change-id addressed) ──
  let changesList: DbBookmark[] = $state([])
  let changeDetail = $state<CommitInfo | null>(null)
  let changeDiffText = $state('')

  async function loadChanges(): Promise<void> {
    const { org, repo } = route
    if (!org || !repo) return
    try {
      changesList = await fetchBookmarks(org, repo)
    } catch { changesList = [] }
  }

  async function loadChangeDetail(changeId: string): Promise<void> {
    const { org, repo } = route
    if (!org || !repo) return
    changeDetail = null
    try {
      changeDetail = await fetchChange(org, repo, changeId)
    } catch (e) {
      error = e instanceof Error ? e.message : String(e)
    }
  }

  // ── op-log tab ──
  let ops: OpLogEntry[] = $state([])
  let conflicts: Conflict[] = $state([])

  async function loadOps(): Promise<void> {
    const { org, repo } = route
    if (!org || !repo) return
    try {
      ops = await fetchOpLog(org, repo)
      conflicts = await fetchConflicts(org, repo)
    } catch (e) {
      error = e instanceof Error ? e.message : String(e)
    }
  }

  async function doUndo(opId: string): Promise<void> {
    const { org, repo } = route
    if (!org || !repo) return
    if (!confirm(`Undo operation ${opId.slice(0, 10)}? This creates a compensating change.`)) return
    try {
      await undoOp(org, repo, opId)
      await loadOps()
      flash('Undo recorded')
    } catch (e) {
      error = e instanceof Error ? e.message : String(e)
    }
  }

  // ── pulls tab ──
  let mrs: Mr[] = $state([])
  let mrDetail = $state<Mr | null>(null)
  let mrDiffText = $state('')
  let mrReviews: MrReview[] = $state([])
  let mrComments: MrComment[] = $state([])
  let newMrTitle = $state('')
  let newMrHead = $state('')
  let newMrBase = $state('main')
  let newMrBody = $state('')
  let reviewState = $state('comment')
  let reviewBody = $state('')
  let commentBody = $state('')

  async function loadMrs(): Promise<void> {
    const { org, repo } = route
    if (!org || !repo) return
    try {
      mrs = await fetchMrs(org, repo)
    } catch { mrs = [] }
  }

  async function loadMrDetail(num: number): Promise<void> {
    const { org, repo } = route
    if (!org || !repo) return
    try {
      mrDetail = await fetchMr(org, repo, num)
      mrDiffText = await fetchMrDiff(org, repo, num)
      mrReviews = await fetchReviews(org, repo, num)
      mrComments = await fetchComments(org, repo, num)
    } catch (e) {
      error = e instanceof Error ? e.message : String(e)
    }
  }

  async function doCreateMr(): Promise<void> {
    const { org, repo } = route
    if (!org || !repo || !newMrTitle || !newMrHead) return
    try {
      await createMr(org, repo, newMrTitle, newMrBody, newMrHead, newMrBase)
      newMrTitle = ''
      newMrHead = ''
      newMrBody = ''
      await loadMrs()
      flash('Pull request created')
    } catch (e) {
      error = e instanceof Error ? e.message : String(e)
    }
  }

  async function doReview(): Promise<void> {
    const { org, repo } = route
    if (!org || !repo || !mrDetail) return
    try {
      await addReview(org, repo, mrDetail.number, reviewState, reviewBody)
      reviewBody = ''
      await loadMrDetail(mrDetail.number)
    } catch (e) {
      error = e instanceof Error ? e.message : String(e)
    }
  }

  async function doComment(): Promise<void> {
    const { org, repo } = route
    if (!org || !repo || !mrDetail) return
    try {
      await addComment(org, repo, mrDetail.number, commentBody)
      commentBody = ''
      await loadMrDetail(mrDetail.number)
    } catch (e) {
      error = e instanceof Error ? e.message : String(e)
    }
  }

  // ── releases ──
  let releases: Release[] = $state([])
  let relTag = $state('')
  let relName = $state('')
  let relBody = $state('')
  let relPre = $state(false)

  async function loadReleases(): Promise<void> {
    const { org, repo } = route
    if (!org || !repo) return
    try {
      releases = await fetchReleases(org, repo)
    } catch { releases = [] }
  }

  async function doCreateRelease(): Promise<void> {
    const { org, repo } = route
    if (!org || !repo || !relTag) return
    try {
      await createRelease(org, repo, relTag, relName, relBody, relPre)
      relTag = ''
      relName = ''
      relBody = ''
      relPre = false
      await loadReleases()
    } catch (e) {
      error = e instanceof Error ? e.message : String(e)
    }
  }

  async function doDeleteRelease(tag: string): Promise<void> {
    const { org, repo } = route
    if (!org || !repo) return
    if (!confirm(`Delete release ${tag}?`)) return
    try {
      await deleteRelease(org, repo, tag)
      await loadReleases()
    } catch (e) {
      error = e instanceof Error ? e.message : String(e)
    }
  }

  // ── actions ──
  let workflows: Workflow[] = $state([])
  let runs: Run[] = $state([])
  let runJobs: Job[] = $state([])
  let activeRun = $state<number | null>(null)
  let jobLogText = $state('')

  async function loadActions(): Promise<void> {
    const { org, repo } = route
    if (!org || !repo) return
    try {
      workflows = await fetchWorkflows(org, repo)
      runs = await fetchRuns(org, repo)
    } catch (e) {
      error = e instanceof Error ? e.message : String(e)
    }
  }

  async function doDispatch(wf: Workflow): Promise<void> {
    const { org, repo } = route
    if (!org || !repo) return
    try {
      await dispatchWorkflow(org, repo, wf.id)
      await loadActions()
      flash(`Dispatched ${wf.name}`)
    } catch (e) {
      error = e instanceof Error ? e.message : String(e)
    }
  }

  async function openRun(runId: number): Promise<void> {
    const { org, repo } = route
    if (!org || !repo) return
    activeRun = runId
    runJobs = await fetchJobs(org, repo, runId)
  }

  async function openJobLog(jobId: number): Promise<void> {
    const { org, repo } = route
    if (!org || !repo) return
    jobLogText = await fetchJobLogs(org, repo, jobId)
  }

  // ── token ──
  let cloneCopied = $state(false)

  // ── settings tab ──
  let renameName = $state('')

  async function doRenameRepo(): Promise<void> {
    const { org, repo } = route
    if (!org || !repo || !renameName) return
    // rename is not exposed on the new REST surface; reuse clone as a proxy.
    flash('Rename not yet exposed over REST')
  }

  async function doDeleteRepo(): Promise<void> {
    const { org, repo } = route
    if (!org || !repo) return
    if (!confirm(`Delete ${org}/${repo}? This is irreversible.`)) return
    try {
      await deleteRepo(org, repo)
      nav(`#/${encodeURIComponent(org)}`)
      await loadOrgs()
    } catch (e) {
      error = e instanceof Error ? e.message : String(e)
    }
  }

  async function copyClone(url: string): Promise<void> {
    try {
      await navigator.clipboard.writeText(url)
      cloneCopied = true
      setTimeout(() => (cloneCopied = false), 1500)
    } catch {
      /* clipboard unavailable */
    }
  }

  // ── lifecycle ──
  function applyRoute(): void {
    parseHash()
  }

  $effect(() => {
    const { org, repo } = route
    if (!org || !repo) {
      branches = []
      tags = []
      tree = []
      commits = []
      graph = []
      return
    }
    branch = ''
    selectedPath = null
    fileData = null
    editing = false
    commitDetail = null
    mrDetail = null
    void refreshRepo()
  })

  onMount(() => {
    void loadOrgs()
    if (!window.location.hash) window.location.hash = '#/'
    window.addEventListener('hashchange', () => void applyRoute())
    void applyRoute()
  })

  // tab data loads
  $effect(() => {
    const { org, repo, tab, sub, sub2 } = route
    if (!org || !repo) return
    if (tab === 'graph') void loadGraph()
    else if (tab === 'changes') void loadChanges()
    else if (tab === 'op-log') void loadOps()
    else if (tab === 'pulls') {
      if (sub && sub !== 'new') void loadMrDetail(Number(sub))
      else void loadMrs()
    } else if (tab === 'releases') void loadReleases()
    else if (tab === 'actions') void loadActions()
    if (sub === 'commit' && sub2) void loadCommitDetail(sub2)
  })

  // deep-link into a blob
  $effect(() => {
    const { org, repo, tab, sub, sub2 } = route
    if (!org || !repo || tab !== 'files' || sub !== 'blob' || !sub2) return
    selectedPath = sub2
    void openFile(sub2)
  })

  let commitGraph = $derived(layoutGraph(graph))
</script>

<div class="flex min-h-screen flex-col">
  <nav class="g-nav sticky top-0 z-40">
    <div class="g-nav-left">
      <a href="#/" class="g-nav-item gap-2" style="font-weight:600;color:var(--gitea-text-dark)">
        <div class="flex size-6 items-center justify-center rounded bg-primary text-[11px] font-bold text-primary-foreground">jj</div>
        <span class="text-sm">jjlab</span>
      </a>
      {#if route.org}
        <span class="g-nav-item gap-1" style="color:var(--gitea-text-light-2)">
          <a href="#/" class="hover:underline" style="color:inherit">orgs</a>
          <ChevronRight class="size-3" />
          {#if route.repo}
            <a href={`#/${encodeURIComponent(route.org)}`} class="hover:underline" style="color:inherit">{route.org}</a>
            <ChevronRight class="size-3" />
            <span style="color:var(--gitea-text-dark);font-weight:500">{route.repo}</span>
          {:else}
            <span style="color:var(--gitea-text-dark);font-weight:500">{route.org}</span>
          {/if}
        </span>
      {/if}
    </div>

    <div class="g-nav-right">
      {#if route.org && route.repo}
        <button class="g-btn small" onclick={() => copyClone(cloneUrls(route.org!, route.repo!).http)}>
          {#if cloneCopied}<Check class="size-3.5" /> Copied{:else}<Copy class="size-3.5" /> Clone{/if}
        </button>
        <a class="g-btn small" href={archiveUrl(route.org!, route.repo!, branch || 'main')} title="Download .tar.gz">
          <Download class="size-3.5" />
        </a>
      {/if}

      <ThemeMenu />

      <TokenMenu />
    </div>
  </nav>

  <main class="mx-auto w-full max-w-[1280px] flex-1 px-4 py-4">
    {#if route.org === null}
      <!-- Explore -->
      <div class="space-y-4">
        <div class="flex items-center justify-between">
          <div>
            <h1 class="text-xl font-semibold">Explore</h1>
            <p class="mt-1 text-sm g-subtle">Organizations on this instance.</p>
          </div>
          <div class="flex gap-2">
            <button class="g-btn small" onclick={() => (importOpen = true)}><Download class="size-3.5" /> Import</button>
            <button class="g-btn small primary" onclick={() => (createOpen = true)}><Plus class="size-3.5" /> New repository</button>
          </div>
        </div>

        {#if reposLoading}
          <div class="space-y-2">{#each Array(4) as _}<Skeleton.Root class="h-9 w-full" />{/each}</div>
        {:else if orgs.length === 0}
          <div class="g-card flex flex-col items-center gap-2 p-12 text-center">
            <Folder class="size-10 text-muted-foreground/40" />
            <p class="text-sm g-subtle">No organizations yet — create one by making a repository.</p>
          </div>
        {:else}
          <div class="g-card overflow-hidden">
            {#each orgs as o (o.org)}
              <a href={`#/${encodeURIComponent(o.org)}`} class="flex items-center gap-3 border-b border-[var(--gitea-secondary)] px-4 py-3 last:border-b-0 hover:bg-[var(--gitea-hover-opaque)]">
                <div class="flex size-8 items-center justify-center rounded bg-[var(--gitea-box-header)] text-primary"><Folder class="size-4" /></div>
                <div class="min-w-0 flex-1">
                  <div class="text-base font-semibold hover:text-primary">{o.org}</div>
                </div>
                <span class="g-subtle">{o.repos.length} repositor{o.repos.length === 1 ? 'y' : 'ies'}</span>
                <ChevronRight class="size-4 g-muted" />
              </a>
            {/each}
          </div>
        {/if}
      </div>

    {:else if route.repo === null}
      <!-- Org page -->
      <div class="space-y-4">
        <div class="g-breadcrumb">
          <a href="#/">Explore</a>
          <span class="divider">/</span>
          <span class="active">{route.org}</span>
          <div class="ml-auto flex gap-2">
            <button class="g-btn small" onclick={() => (importOpen = true)}>Import</button>
            <button class="g-btn small primary" onclick={() => (createOpen = true)}><Plus class="size-3.5" /> New</button>
          </div>
        </div>
        <div class="g-card overflow-hidden">
          {#each (orgs.find(o => o.org === route.org)?.repos ?? []) as r (r.repo)}
            <a href={`#/${encodeURIComponent(route.org!)}/${encodeURIComponent(r.repo)}`} class="flex items-center gap-3 border-b border-[var(--gitea-secondary)] px-4 py-3 last:border-b-0 hover:bg-[var(--gitea-hover-opaque)]">
              <div class="flex size-8 items-center justify-center rounded bg-[var(--gitea-box-header)] text-[var(--gitea-text)]"><BookOpen class="size-4" /></div>
              <div class="min-w-0 flex-1">
                <div class="text-base font-semibold hover:text-primary">{r.repo}</div>
                <div class="g-subtle">default branch: {r.default_bookmark}</div>
              </div>
              <ChevronRight class="size-4 g-muted" />
            </a>
          {/each}
        </div>
      </div>

    {:else}
      <!-- Repo page -->
      <div class="g-repo-header">
        <div class="min-w-0">
          <div class="g-repo-title">
            <a class="muted hover:underline" href={`#/${encodeURIComponent(route.org!)}`}>{route.org}</a>
            <span class="g-muted mx-1">/</span>
            <a class="hover:underline" href={`#/${encodeURIComponent(route.org!)}/${encodeURIComponent(route.repo!)}`}>{route.repo}</a>
          </div>
        </div>
        <div class="g-repo-bucket">
          <button class="g-btn small" onclick={() => copyClone(cloneUrls(route.org!, route.repo!).http)}>
            {#if cloneCopied}<Check class="size-3.5" /> Copied{:else}<Copy class="size-3.5" /> Clone{/if}
          </button>
          <a class="g-btn small" href={archiveUrl(route.org!, route.repo!, branch || 'main')} title="Download source (.tar.gz)">
            <Download class="size-3.5" />
          </a>
        </div>
      </div>

      <!-- Gitea tab bar (secondary pointing menu) -->
      <nav class="g-tabs">
        {#each TABS as t (t.id)}
          <a href={`#/${encodeURIComponent(route.org!)}/${encodeURIComponent(route.repo!)}${t.id === 'files' ? '' : '/' + t.id}`}
             class={`g-tab ${route.tab === t.id ? 'active' : ''}`} aria-current={route.tab === t.id ? 'page' : undefined}>
            {t.label}
            {#if t.id === 'pulls' && mrs.length > 0}<span class="g-count">{mrs.length}</span>{/if}
          </a>
        {/each}
      </nav>

      <div class="mb-4">
        {#if route.tab === 'files'}
          <!-- branch selector row (Gitea repo-button-row) -->
          <div class="mb-3 flex items-center gap-2">
            <NativeSelect.Root value={branch} onchange={(e) => { branch = (e.target as HTMLSelectElement).value; selectedPath = null; fileData = null; void loadTree(); void loadReadme() }}
              class="flex h-8 w-52 items-center rounded border border-[var(--gitea-input-border)] bg-[var(--gitea-input-bg)] px-2 text-xs font-mono text-[var(--gitea-text)]">
              <NativeSelect.Option disabled>branch</NativeSelect.Option>
              {#each branches as b (b.name)}
                <NativeSelect.Option value={b.name}> <GitBranch class="size-3 inline" /> {b.name}</NativeSelect.Option>
              {/each}
            </NativeSelect.Root>
            <span class="g-subtle">{branches.length} branche{branches.length === 1 ? '' : 's'} · {tags.length} tags</span>
          </div>

          {#if route.sub === 'blob' && selectedPath}
            <!-- file view -->
            <div class="g-card overflow-hidden">
              <div class="flex items-center gap-2 border-b border-[var(--gitea-secondary)] px-3 py-2">
                <div class="g-breadcrumb min-w-0">
                  {#each selectedPath.split('/') as seg, i (i)}
                    {#if i > 0}<span class="divider">/</span>{/if}
                    <span class={i === selectedPath.split('/').length - 1 ? 'active' : ''}>{seg}</span>
                  {/each}
                </div>
                <div class="ml-auto flex gap-1.5">
                  <button class="g-btn tiny basic" onclick={loadFileLog}>History</button>
                  {#if editing}
                    <button class="g-btn tiny primary" onclick={saveFile}>Save</button>
                    <button class="g-btn tiny" onclick={() => { editing = false; editContent = fileData?.content ?? '' }}>Cancel</button>
                  {:else}
                    <button class="g-btn tiny basic" onclick={startEdit}>Edit</button>
                    <button class="g-btn tiny red" onclick={removeFile}>Delete</button>
                  {/if}
                </div>
              </div>
              {#if editing}
                <div class="p-3">
                  <Textarea.Root class="h-[60vh] w-full resize-none font-mono text-xs" bind:value={editContent} />
                  <div class="mt-2 flex items-center gap-2">
                    <Input.Root class="max-w-md flex-1" placeholder="commit message" bind:value={editMessage} />
                    <label class="flex items-center gap-1 text-[11px] g-muted">
                      <input type="checkbox" bind:checked={editAmend} />
                      amend head change
                    </label>
                    <button class="g-btn tiny primary" onclick={saveFile}>Commit</button>
                  </div>
                </div>
              {:else if fileData}
                <CodeView code={fileData.content} filepath={selectedPath} />
              {:else}
                <div class="p-8 text-center text-sm g-subtle">Loading…</div>
              {/if}
            </div>
          {:else}
            <!-- repo view: left tree sidebar + right content (Gitea layout) -->
            <div class="g-repo-view">
              <aside class="g-tree-sidebar hidden md:block">
                <div class="g-tree-sidebar-head"><GitBranch class="size-3.5" /> Files</div>
                {#if tree.length === 0}
                  <p class="p-3 text-xs g-subtle">Empty repository</p>
                {:else}
                  <TreeNode entries={tree} {expandedDirs} selectedPath={null} onToggle={toggleDir} onOpen={(p) => { selectedPath = p; void openFile(p); nav(`#/${encodeURIComponent(route.org!)}/${encodeURIComponent(route.repo!)}/blob/${p.split('/').map(encodeURIComponent).join('/')}`) }} />
                {/if}
              </aside>
              <div class="g-repo-content">
                {#if loading}
                  <div class="space-y-1">{#each Array(6) as _}<Skeleton.Root class="h-8 w-full" />{/each}</div>
                {:else}
                  <div class="g-files">
                    <div class="g-files-row">
                      <div class="g-files-cell g-files-head">
                        <GitBranch class="size-3.5 g-muted" />
                        <span class="g-muted text-xs">{branch || 'main'}</span>
                      </div>
                    </div>
                    {#each tree as entry (entry.path)}
                      {@const isDir = entry.is_dir}
                      <div class="g-files-row">
                        <div class="g-files-cell name">
                          <span class="g-muted">{#if isDir}<Folder class="size-3.5" />{:else}<File class="size-3.5" />{/if}</span>
                          {#if isDir}
                            <button class="g-files-name" onclick={() => toggleDir(entry.path)}>{entry.name}</button>
                          {:else}
                            <a class="g-files-name" href={`#/${encodeURIComponent(route.org!)}/${encodeURIComponent(route.repo!)}/blob/${entry.path.split('/').map(encodeURIComponent).join('/')}`} onclick={() => { selectedPath = entry.path; void openFile(entry.path) }}>{entry.name}</a>
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

                  {#if readmeLabel() && readmeText !== null}
                    <div class="g-card mt-4 p-5">
                      <h3 class="mb-3 border-b border-[var(--gitea-secondary)] pb-2 text-sm font-semibold">
                        <BookOpen class="mr-1.5 inline size-4 g-muted" />README.md
                      </h3>
                      <Markdown content={readmeText} />
                    </div>
                  {/if}
                {/if}
              </div>
            </div>
          {/if}

        {:else if route.tab === 'commits'}
          {#if route.sub === 'commit' && commitDetail}
            <div class="space-y-4">
              <div class="g-breadcrumb">
                <a href={`#/${encodeURIComponent(route.org!)}/${encodeURIComponent(route.repo!)}/commits`}>Commits</a>
                <span class="divider">/</span>
                <span class="active font-mono">{commitDetail.sha.slice(0, 10)}</span>
              </div>
              <div class="g-card p-4">
                <div class="flex flex-wrap items-center gap-2">
                  <span class="font-mono text-sm" style="color:var(--primary)">{commitDetail.sha}</span>
                  <span class="g-subtle font-mono">change {commitDetail.change_id.slice(0, 10)}</span>
                  <span class="ml-auto g-subtle">{commitDetail.author}</span>
                </div>
                <div class="mt-2 whitespace-pre-wrap">{commitDetail.description}</div>
              </div>
              <div class="g-card overflow-hidden">
                <div class="border-b border-[var(--gitea-secondary)] px-3 py-2 text-xs font-semibold g-muted">Changes</div>
                <DiffView diffText={commitDiffText} />
              </div>
            </div>
          {:else}
            <div class="g-card overflow-hidden">
              {#if commits.length === 0}
                <p class="p-4 text-xs g-subtle">No commits</p>
              {:else}
                <div class="overflow-x-auto">
                  <table class="g-commit-table">
                    <thead>
                      <tr><th class="w-40">Author</th><th class="w-24">SHA</th><th>Message</th><th class="w-28 text-right">Date</th></tr>
                    </thead>
                    <tbody>
                      {#each commits as c (c.sha)}
                        <tr>
                          <td class="g-subtle" title={c.author}>{c.author.split(' <')[0]}</td>
                          <td><a class="sha" href={`#/${encodeURIComponent(route.org!)}/${encodeURIComponent(route.repo!)}/commit/${encodeURIComponent(c.sha)}`}>{c.sha.slice(0, 10)}</a></td>
                          <td class="msg">
                            <a href={`#/${encodeURIComponent(route.org!)}/${encodeURIComponent(route.repo!)}/commit/${encodeURIComponent(c.sha)}`} style="color:inherit">
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
                  <span class="g-subtle">{commitTotal} total</span>
                  <div class="flex gap-1">
                    <button class="g-btn tiny" disabled={commitPage <= 1} onclick={() => { void loadCommits(commitPage - 1) }}>Prev</button>
                    <button class="g-btn tiny" disabled={commitPage * pageSize >= commitTotal} onclick={() => { void loadCommits(commitPage + 1) }}>Next</button>
                  </div>
                </div>
              {/if}
            </div>
          {/if}

        {:else if route.tab === 'branches'}
          <div class="mb-3 flex flex-wrap items-center gap-2">
            <Input.Root class="w-40" placeholder="branch name" bind:value={newBranchName} />
            <Input.Root class="w-44" placeholder="from (empty=head)" bind:value={newBranchFrom} />
            <button class="g-btn tiny primary" onclick={doCreateBranch}><Plus class="size-3.5" /> Branch</button>
            <span class="mx-2 g-muted">|</span>
            <Input.Root class="w-36" placeholder="tag name" bind:value={newTagName} />
            <Input.Root class="w-36" placeholder="from" bind:value={newTagFrom} />
            <button class="g-btn tiny" onclick={doCreateTag}><Tag class="size-3.5" /> Tag</button>
          </div>
          <h3 class="mb-2 text-xs font-semibold g-muted">Branches</h3>
          <div class="g-card overflow-hidden">
            {#each branches as b (b.name)}
              <div class="flex w-full items-center gap-3 border-b border-[var(--gitea-secondary)] px-4 py-2.5 last:border-b-0">
                <a href={`#/${encodeURIComponent(route.org!)}/${encodeURIComponent(route.repo!)}`} onclick={() => { branch = b.name; void loadTree() }}
                   class="flex min-w-0 flex-1 items-center gap-3">
                  <GitBranch class="size-4 shrink-0 g-muted" />
                  <span class="truncate font-mono text-xs">{b.name}</span>
                  <span class="shrink-0 font-mono text-[11px]" style="color:var(--primary)">{b.sha.slice(0, 8)}</span>
                </a>
                <button class="shrink-0 font-mono text-[11px] hover:underline" style="color:var(--destructive)" onclick={() => void doDeleteBranch(b.name)}>delete</button>
              </div>
            {/each}
          </div>
          <h3 class="mb-2 mt-6 text-xs font-semibold g-muted">Tags</h3>
          <div class="g-card overflow-hidden">
            {#if tags.length === 0}
              <p class="p-4 text-xs g-subtle">No tags</p>
            {:else}
              {#each tags as t (t.name)}
                <div class="flex w-full items-center gap-3 border-b border-[var(--gitea-secondary)] px-4 py-2.5 last:border-b-0">
                  <a href={`#/${encodeURIComponent(route.org!)}/${encodeURIComponent(route.repo!)}/commit/${encodeURIComponent(t.sha)}`} class="flex min-w-0 flex-1 items-center gap-3">
                    <Tag class="size-4 shrink-0 g-muted" />
                    <span class="truncate font-mono text-xs">{t.name}</span>
                    <span class="shrink-0 font-mono text-[11px]" style="color:var(--primary)">{t.sha.slice(0, 8)}</span>
                  </a>
                  <button class="shrink-0 font-mono text-[11px] hover:underline" style="color:var(--destructive)" onclick={() => void doDeleteTag(t.name)}>delete</button>
                </div>
              {/each}
            {/if}
          </div>

        {:else if route.tab === 'graph'}
          <h2 class="mb-3 text-sm font-semibold">Change graph</h2>
          <div class="g-card overflow-hidden">
            {#if graph.length === 0}
              <p class="p-4 text-xs g-subtle">No commits</p>
            {:else}
              {@const lay = commitGraph}
              <div class="flex flex-col divide-y divide-[var(--gitea-secondary)]">
                {#each lay.rows as row (row.node.commit_id)}
                  {@const color = LANE_COLORS[row.lane % LANE_COLORS.length]}
                  <div class="flex items-center gap-3 px-4 py-2.5">
                    <svg width="20" height="20" class="shrink-0">
                      <circle cx="10" cy="10" r="5" fill={color} stroke={row.node.is_head ? 'var(--gitea-text-dark)' : 'rgba(127,127,127,0.4)'} stroke-width="1.5" />
                    </svg>
                    <a href={`#/${encodeURIComponent(route.org!)}/${encodeURIComponent(route.repo!)}/commit/${encodeURIComponent(row.node.commit_id)}`} class="flex min-w-0 flex-1 items-center gap-2">
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

        {:else if route.tab === 'changes'}
          <h2 class="mb-3 text-sm font-semibold">Change-ids (jj-native)</h2>
          <div class="grid grid-cols-1 gap-4 lg:grid-cols-2">
            <div class="g-card overflow-hidden">
              <div class="border-b border-[var(--gitea-secondary)] px-4 py-2 text-xs font-semibold g-muted">Bookmarks by change-id</div>
              {#if changesList.length === 0}
                <p class="p-4 text-xs g-subtle">No bookmarks</p>
              {:else}
                {#each changesList as b (b.name)}
                  <button class="flex w-full items-center gap-3 border-b border-[var(--gitea-secondary)] px-4 py-2.5 text-left hover:bg-[var(--gitea-hover-opaque)] last:border-b-0" onclick={() => { void loadChangeDetail(b.change_id) }}>
                    <GitBranch class="size-4 shrink-0 g-muted" />
                    <span class="min-w-0 flex-1 truncate font-mono text-xs">{b.name}</span>
                    <span class="shrink-0 font-mono text-[11px]" style="color:var(--primary)">{b.change_id.slice(0, 10)}</span>
                  </button>
                {/each}
              {/if}
            </div>
            <div class="g-card p-4">
              {#if changeDetail}
                <div class="text-sm font-semibold">change {changeDetail.change_id}</div>
                <div class="mt-1 font-mono text-xs g-muted">commit {changeDetail.sha}</div>
                <div class="mt-2 text-xs">{changeDetail.description}</div>
                <div class="mt-2 text-[11px] g-subtle">{changeDetail.author}</div>
              {:else}
                <p class="text-xs g-subtle">Select a bookmark to inspect its change.</p>
              {/if}
            </div>
          </div>

        {:else if route.tab === 'op-log'}
          <h2 class="mb-3 flex items-center gap-2 text-sm font-semibold"><History class="size-4" /> Operation log</h2>
          {#if conflicts.length > 0}
            <div class="mb-4 border p-3" style="border-color:var(--destructive);background:color-mix(in srgb, var(--destructive) 10%, transparent)">
              <div class="flex items-center gap-2 text-sm font-semibold" style="color:var(--destructive)"><AlertTriangle class="size-4" /> {conflicts.length} conflicted path{conflicts.length === 1 ? '' : 's'}</div>
              {#each conflicts as cf (cf.id)}
                <div class="mt-1 font-mono text-xs">{cf.path}</div>
              {/each}
            </div>
          {/if}
          <div class="g-card overflow-hidden">
            {#if ops.length === 0}
              <p class="p-4 text-xs g-subtle">No operations recorded</p>
            {:else}
              {#each ops as op (op.id)}
                <div class="flex w-full items-center gap-3 border-b border-[var(--gitea-secondary)] px-4 py-2.5 last:border-b-0">
                  <span class="shrink-0 font-mono text-[11px]" style="color:var(--primary)">{op.id.slice(0, 10)}</span>
                  <span class="shrink-0 rounded px-1.5 py-0.5 font-mono text-[10px]" style="background:var(--gitea-light-border)">{op.op_type}</span>
                  {#if op.undo_of}<span class="shrink-0 font-mono text-[10px] g-subtle">undoes {op.undo_of.slice(0, 8)}</span>{/if}
                  {#if (op.op_type === 'write' || op.op_type === 'delete') && (() => { try { return JSON.parse(op.payload).change_id } catch { return null } })() as string}
                    <button class="shrink-0 font-mono text-[11px]" style="color:var(--primary)" onclick={() => { void loadChangeDetail((() => { try { return JSON.parse(op.payload).change_id } catch { return '' } })()) }}>{(() => { try { return JSON.parse(op.payload).change_id } catch { return '' } })().slice(0, 10)}</button>
                  {/if}
                  <span class="ml-auto shrink-0">
                    {#if !op.undo_of}
                      <button class="g-btn tiny basic" onclick={() => void doUndo(op.id)}>undo</button>
                    {/if}
                  </span>
                </div>
              {/each}
            {/if}
          </div>

        {:else if route.tab === 'pulls'}
          {#if route.sub !== '' && route.sub !== 'new' && mrDetail}
            <div class="space-y-4">
              <div class="g-breadcrumb">
                <a href={`#/${encodeURIComponent(route.org!)}/${encodeURIComponent(route.repo!)}/pulls`}>Pull requests</a>
                <span class="divider">/</span>
                <span class="active">#{mrDetail.number}</span>
              </div>
              <div class="g-card p-4">
                <div class="flex flex-wrap items-center gap-2">
                  <span class="text-lg font-semibold">#{mrDetail.number} {mrDetail.title}</span>
                  <span class="rounded px-1.5 py-0.5 text-[11px]" style={mrDetail.state === 'open' ? 'background:var(--success);color:#fff' : 'background:var(--gitea-secondary)'}>{mrDetail.state}</span>
                  <span class="rounded px-1.5 py-0.5 text-[11px]" style="background:var(--gitea-light-border)">{mrDetail.review_state}</span>
                </div>
                <div class="mt-1 g-subtle">{mrDetail.author} wants to merge {mrDetail.head_change_id.slice(0, 10)} into {mrDetail.base}</div>
                {#if mrDetail.body}<div class="mt-3 text-sm">{mrDetail.body}</div>{/if}
                {#if mrDetail.state === 'open'}
                  <div class="mt-3">
                    <button class="g-btn tiny red" onclick={() => { void updateMrState(route.org!, route.repo!, mrDetail.number, 'close').then(() => loadMrDetail(mrDetail.number)) }}>Close pull</button>
                  </div>
                {/if}
              </div>
              <div class="g-card overflow-hidden">
                <div class="border-b border-[var(--gitea-secondary)] px-4 py-2 text-xs font-semibold g-muted">Diff</div>
                <DiffView diffText={mrDiffText} />
              </div>
              <div class="grid grid-cols-1 gap-4 lg:grid-cols-2">
                <div class="g-card p-4">
                  <h3 class="mb-2 text-sm font-semibold">Reviews</h3>
                  {#each mrReviews as r (r.reviewer + r.state + r.body)}
                    <div class="border-b border-[var(--gitea-secondary)] py-2">
                      <div class="flex items-center gap-2 text-xs"><span class="font-medium">{r.reviewer}</span><span class="rounded px-1.5 py-0.5 text-[10px]" style="background:var(--gitea-light-border)">{r.state}</span></div>
                      {#if r.body}<p class="mt-1 text-xs">{r.body}</p>{/if}
                    </div>
                  {/each}
                  <div class="mt-3 flex gap-2">
                    <NativeSelect.Root value={reviewState} onchange={(e) => (reviewState = (e.target as HTMLSelectElement).value)} class="h-8 rounded border border-[var(--gitea-input-border)] bg-[var(--gitea-input-bg)] px-2 text-xs">
                      <NativeSelect.Option value="comment">Comment</NativeSelect.Option>
                      <NativeSelect.Option value="approved">Approve</NativeSelect.Option>
                      <NativeSelect.Option value="request_changes">Request changes</NativeSelect.Option>
                    </NativeSelect.Root>
                    <Input.Root class="flex-1" placeholder="review body" bind:value={reviewBody} />
                    <button class="g-btn tiny primary" onclick={doReview}>Submit</button>
                  </div>
                </div>
                <div class="g-card p-4">
                  <h3 class="mb-2 text-sm font-semibold">Comments</h3>
                  {#each mrComments as c (c.author + c.body)}
                    <div class="border-b border-[var(--gitea-secondary)] py-2">
                      <div class="text-xs font-medium">{c.author}</div>
                      <p class="mt-1 text-xs">{c.body}</p>
                    </div>
                  {/each}
                  <div class="mt-3 flex gap-2">
                    <Input.Root class="flex-1" placeholder="comment" bind:value={commentBody} />
                    <button class="g-btn tiny" onclick={doComment}>Add</button>
                  </div>
                </div>
              </div>
            </div>
          {:else}
            <div class="mb-3 flex items-center justify-between">
              <h2 class="text-sm font-semibold">Pull requests</h2>
              <button class="g-btn small primary" onclick={() => nav(`#/${encodeURIComponent(route.org!)}/${encodeURIComponent(route.repo!)}/pulls/new`)}><Plus class="size-3.5" /> New</button>
            </div>
            {#if route.sub === 'new'}
              <div class="g-card mb-4 space-y-2 p-4">
                <Input.Root placeholder="title" bind:value={newMrTitle} />
                <div class="flex gap-2">
                  <Input.Root class="flex-1" placeholder="head branch" bind:value={newMrHead} />
                  <Input.Root class="flex-1" placeholder="base (main)" bind:value={newMrBase} />
                </div>
                <Textarea.Root placeholder="description" bind:value={newMrBody} />
                <button class="g-btn tiny primary" onclick={doCreateMr}>Create pull request</button>
              </div>
            {/if}
            <div class="g-card overflow-hidden">
              {#if mrs.length === 0}
                <p class="p-4 text-xs g-subtle">No pull requests</p>
              {:else}
                {#each mrs as m (m.number)}
                  <a href={`#/${encodeURIComponent(route.org!)}/${encodeURIComponent(route.repo!)}/pulls/${m.number}`} class="flex w-full items-center gap-3 border-b border-[var(--gitea-secondary)] px-4 py-2.5 hover:bg-[var(--gitea-hover-opaque)] last:border-b-0">
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

        {:else if route.tab === 'releases'}
          <h2 class="mb-3 text-sm font-semibold">Releases</h2>
          <div class="g-card mb-4 space-y-2 p-4">
            <div class="flex gap-2">
              <Input.Root class="w-40" placeholder="tag (e.g. v1.0)" bind:value={relTag} />
              <Input.Root class="flex-1" placeholder="title" bind:value={relName} />
            </div>
            <Textarea.Root placeholder="release notes" bind:value={relBody} />
            <div class="flex items-center gap-3">
              <label class="flex items-center gap-2 text-xs"><input type="checkbox" bind:checked={relPre} /> pre-release</label>
              <button class="g-btn tiny primary" onclick={doCreateRelease}>Publish release</button>
            </div>
          </div>
          <div class="space-y-3">
            {#if releases.length === 0}
              <div class="g-card p-8 text-center text-sm g-subtle">No releases</div>
            {:else}
              {#each releases as r (r.id)}
                <div class="g-card p-4">
                  <div class="flex items-center gap-2">
                    <Tag class="size-4" style="color:var(--primary)" />
                    <span class="font-mono text-sm font-semibold">{r.tag_name}</span>
                    {#if r.prerelease}<span class="rounded px-1.5 py-0.5 text-[10px]" style="background:var(--gitea-light-border)">pre-release</span>{/if}
                    <button class="ml-auto font-mono text-[11px] hover:underline" style="color:var(--destructive)" onclick={() => void doDeleteRelease(r.tag_name)}>delete</button>
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

        {:else if route.tab === 'actions'}
          <h2 class="mb-3 text-sm font-semibold">Workflows</h2>
          <div class="g-card overflow-hidden">
            {#if workflows.length === 0}
              <p class="p-4 text-xs g-subtle">No workflows defined</p>
            {:else}
              {#each workflows as w (w.id)}
                <div class="flex w-full items-center gap-3 border-b border-[var(--gitea-secondary)] px-4 py-2.5 last:border-b-0">
                  <Play class="size-4 shrink-0 g-muted" />
                  <span class="min-w-0 flex-1 font-mono text-xs">{w.name}</span>
                  <span class="font-mono text-[10px] g-subtle">{w.path}</span>
                  <button class="g-btn tiny" onclick={() => void doDispatch(w)}>Run</button>
                </div>
              {/each}
            {/if}
          </div>

          <h2 class="mb-3 mt-6 text-sm font-semibold">Runs</h2>
          <div class="grid grid-cols-1 gap-4 lg:grid-cols-2">
            <div class="g-card overflow-hidden">
              {#if runs.length === 0}
                <p class="p-4 text-xs g-subtle">No runs</p>
              {:else}
                {#each runs as r (r.id)}
                  <button class="flex w-full items-center gap-3 border-b border-[var(--gitea-secondary)] px-4 py-2.5 text-left hover:bg-[var(--gitea-hover-opaque)] last:border-b-0" onclick={() => void openRun(r.id)}>
                    <span class="font-mono text-xs" style="color:var(--primary)">#{r.id}</span>
                    <span class="min-w-0 flex-1 truncate text-xs">{r.trigger_ref || 'manual'}</span>
                    <span class="rounded px-1.5 py-0.5 text-[10px]" style={r.status === 'success' ? 'background:var(--success);color:#fff' : r.status === 'failed' ? 'background:var(--destructive);color:#fff' : 'background:var(--gitea-light-border)'}>{r.status}</span>
                  </button>
                {/each}
              {/if}
            </div>
            <div class="space-y-3">
              {#if activeRun !== null}
                <div class="g-card p-4">
                  <h3 class="mb-2 text-sm font-semibold">Run #{activeRun} jobs</h3>
                  {#each runJobs as j (j.id)}
                    <div class="flex items-center gap-2 border-b border-[var(--gitea-secondary)] py-2">
                      <span class="min-w-0 flex-1 font-mono text-xs">{j.name}</span>
                      <span class="rounded px-1.5 py-0.5 text-[10px]" style="background:var(--gitea-light-border)">{j.status}</span>
                      <button class="g-btn tiny basic" onclick={() => void openJobLog(j.id)}>logs</button>
                    </div>
                  {/each}
                </div>
              {/if}
              {#if jobLogText}
                <div class="g-card p-4">
                  <h3 class="mb-2 text-sm font-semibold">Logs</h3>
                  <pre class="max-h-96 overflow-auto whitespace-pre-wrap font-mono text-xs">{jobLogText}</pre>
                </div>
              {/if}
            </div>
          </div>

        {:else if route.tab === 'settings'}
          <h2 class="mb-3 text-sm font-semibold">Repository settings</h2>
          <div class="g-card space-y-4 p-4">
            <div>
              <div class="mb-1 text-xs font-semibold g-muted">Clone URL</div>
              <div class="flex items-center gap-2">
                <code class="min-w-0 flex-1 rounded border border-[var(--gitea-secondary)] bg-[var(--gitea-box-header)] px-2 py-1.5 font-mono text-xs">{cloneUrls(route.org!, route.repo!).http}</code>
                <button class="g-btn tiny" onclick={() => void copyClone(cloneUrls(route.org!, route.repo!).http)}>{cloneCopied ? 'Copied' : 'Copy'}</button>
              </div>
            </div>
            <div>
              <div class="mb-1 text-xs font-semibold g-muted">Download</div>
              <a href={archiveUrl(route.org!, route.repo!, branch || 'main')} class="text-xs hover:underline" style="color:var(--primary)">source (tar.gz)</a>
            </div>
            <div class="border-t border-[var(--gitea-secondary)] pt-4">
              <div class="mb-1 text-xs font-semibold" style="color:var(--destructive)">Danger zone</div>
              <div class="flex items-center gap-2">
                <Input.Root class="max-w-xs" placeholder="confirm repo name" bind:value={renameName} />
                <button class="g-btn tiny red" disabled={renameName !== route.repo} onclick={doDeleteRepo}>Delete repository</button>
              </div>
              <p class="mt-1 text-[11px] g-subtle">Type the repository name to enable deletion.</p>
            </div>
          </div>
        {/if}
      </div>
    {/if}
  </main>

  {#if error}
    <div class="fixed bottom-4 right-4 z-50 flex max-w-sm items-start gap-2 border p-3 text-xs shadow-md" style="border-color:var(--destructive);background:var(--gitea-overlay-backdrop);color:var(--destructive)">
      <span class="flex-1">{error}</span>
      <button class="shrink-0 underline" onclick={() => (error = null)}>Dismiss</button>
    </div>
  {/if}
  {#if notice}
    <div class="fixed bottom-4 left-4 z-50 flex max-w-sm items-start gap-2 border p-3 text-xs shadow-md" style="border-color:var(--success);background:var(--gitea-menu);color:var(--success)">
      <span class="flex-1">{notice}</span>
    </div>
  {/if}

  <!-- create repo dialog -->
  <Dialog.Root open={createOpen} onOpenChange={(o) => (createOpen = o)}>
    <Dialog.Content class="sm:max-w-md">
      <Dialog.Header>
        <Dialog.Title>New repository</Dialog.Title>
        <Dialog.Description>Create an empty repository (initialized with a README).</Dialog.Description>
      </Dialog.Header>
      <div class="space-y-3 p-4">
        <div>
          <label for="cr-org" class="mb-1 block text-xs font-medium">Organization</label>
          <Input.Root id="cr-org" placeholder="org" bind:value={createOrg} />
        </div>
        <div>
          <label for="cr-repo" class="mb-1 block text-xs font-medium">Repository name</label>
          <Input.Root id="cr-repo" placeholder="repo" bind:value={createRepoName} />
        </div>
      </div>
      <Dialog.Footer>
        <button class="g-btn tiny" onclick={() => (createOpen = false)}>Cancel</button>
        <button class="g-btn tiny primary" disabled={working || !createOrg || !createRepoName} onclick={doCreateRepo}>Create</button>
      </Dialog.Footer>
    </Dialog.Content>
  </Dialog.Root>

  <!-- import dialog -->
  <Dialog.Root open={importOpen} onOpenChange={(o) => (importOpen = o)}>
    <Dialog.Content class="sm:max-w-md">
      <Dialog.Header>
        <Dialog.Title>Import repository</Dialog.Title>
        <Dialog.Description>Clone an external Git URL into a new jjlab repository.</Dialog.Description>
      </Dialog.Header>
      <div class="space-y-3 p-4">
        <Input.Root placeholder="org" bind:value={importOrg} />
        <Input.Root placeholder="repo name" bind:value={importRepoName} />
        <Input.Root placeholder="git url" bind:value={importUrl} oninput={(e) => { const v = (e.target as HTMLInputElement).value; if (!importRepoName) importRepoName = deriveName(v) }} />
        <Input.Root placeholder="branch (optional)" bind:value={importBranch} />
      </div>
      <Dialog.Footer>
        <button class="g-btn tiny" onclick={() => (importOpen = false)}>Cancel</button>
        <button class="g-btn tiny primary" disabled={working || !importOrg || !importRepoName || !importUrl} onclick={doImport}>Import</button>
      </Dialog.Footer>
    </Dialog.Content>
  </Dialog.Root>
</div>