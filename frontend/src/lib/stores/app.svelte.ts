import {
  fetchOrgs, fetchBranches, fetchTags, fetchCommits, fetchCommit, fetchChange,
  fetchTree, fetchFile, fetchRawFile, fetchDiff, fetchGraph, fetchFileLog,
  fetchOpLog, fetchConflicts, fetchBookmarks, undoOp,
  createRepo, deleteRepo, writeFile, createFile, deleteFile, createBranch,
  deleteBranch, createTag, deleteTag, cloneRemote, fetchMrs, fetchMr, createMr,
  updateMrState, fetchReviews, addReview, fetchComments, addComment, fetchMrDiff,
  fetchReleases, createRelease, deleteRelease, fetchWorkflows, dispatchWorkflow,
  fetchRuns, fetchJobs, fetchJobLogs, decodeContent,
  type Org, type CommitInfo, type BranchInfo, type GraphNode, type Mr,
  type MrReview, type MrComment, type Release, type Workflow, type Run, type Job,
  type OpLogEntry, type Conflict, type DbBookmark,
} from '$lib/api'
import { layoutGraph } from '$lib/graph-layout'
import { TABS, type Tab, type Route } from '$lib/route'

/**
 * Single reactive application store (Svelte 5 runes). Holds the routing state,
 * error/notice banners, the org list and every per-repository view's state +
 * actions, so the tab views stay presentational and share one source of truth.
 */
class AppStore {
  // ── routing ──
  route: Route = $state({ org: null, repo: null, tab: 'files', sub: '', sub2: '' })

  // ── banners ──
  error: string | null = $state(null)
  notice: string | null = $state(null)

  flash(msg: string): void {
    this.notice = msg
    setTimeout(() => {
      if (this.notice === msg) this.notice = null
    }, 4000)
  }

  applyRoute(): void {
    const h = window.location.hash.replace(/^#\/?/, '')
    const parts = h.split('/').filter(Boolean).map(decodeURIComponent)
    if (parts.length === 0) {
      this.route = { org: null, repo: null, tab: 'files', sub: '', sub2: '' }
      return
    }
    if (parts.length === 1) {
      this.route = { org: parts[0]!, repo: null, tab: 'files', sub: '', sub2: '' }
      return
    }
    const [org, repo] = parts
    if (parts[2] === 'commit' && parts[3]) {
      this.route = { org: org!, repo: repo!, tab: 'commits', sub: 'commit', sub2: parts[3]! }
      return
    }
    if (parts[2] === 'pulls' && parts[3]) {
      this.route = { org: org!, repo: repo!, tab: 'pulls', sub: parts[3]! || '', sub2: '' }
      return
    }
    if (parts[2] === 'blob' && parts[3]) {
      const path = parts.slice(3).join('/')
      this.route = { org: org!, repo: repo!, tab: 'files', sub: 'blob', sub2: path }
      return
    }
    const tab = TABS.some((t) => t.id === parts[2]) ? (parts[2] as Tab) : 'files'
    this.route = { org: org!, repo: repo!, tab, sub: '', sub2: '' }
  }

  nav(hash: string): void {
    if (window.location.hash === hash) void this.applyRoute()
    else window.location.hash = hash
  }

  // ── app-level data ──
  orgs: Org[] = $state([])
  reposLoading = $state(false)

  async loadOrgs(): Promise<void> {
    this.reposLoading = true
    try {
      this.orgs = await fetchOrgs()
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e)
    } finally {
      this.reposLoading = false
    }
  }

  // ── create / import dialogs ──
  createOpen = $state(false)
  createOrg = $state('')
  createRepoName = $state('')
  importOpen = $state(false)
  importOrg = $state('')
  importRepoName = $state('')
  importUrl = $state('')
  importBranch = $state('')
  working = $state(false)

  async doCreateRepo(): Promise<void> {
    if (!this.createOrg || !this.createRepoName) return
    this.working = true
    try {
      await createRepo(this.createOrg, this.createRepoName)
      this.createOpen = false
      const name = this.createRepoName
      const org = this.createOrg
      this.createRepoName = ''
      await this.loadOrgs()
      this.flash(`Created ${org}/${name}`)
      this.nav(`#/${encodeURIComponent(org)}/${encodeURIComponent(name)}`)
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e)
    } finally {
      this.working = false
    }
  }

  async doImport(): Promise<void> {
    if (!this.importOrg || !this.importRepoName || !this.importUrl) return
    this.working = true
    try {
      await cloneRemote(this.importOrg, this.importRepoName, this.importUrl, this.importBranch || undefined)
      this.importOpen = false
      const org = this.importOrg
      const repo = this.importRepoName
      await this.loadOrgs()
      this.flash(`Imported ${org}/${repo}`)
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e)
    } finally {
      this.working = false
    }
  }

  deriveName(url: string): string {
    const m = url.trim().match(/[/:]([^/:]+?)(?:\.git)?(?:\/)?$/)
    return m ? m[1]! : ''
  }

  // ── repository context ──
  branch = $state('')
  branches: BranchInfo[] = $state([])
  tags: BranchInfo[] = $state([])
  tree: { name: string; path: string; is_dir: boolean; size: number }[] = $state([])
  commits: CommitInfo[] = $state([])
  commitTotal = $state(0)
  commitPage = $state(1)
  readonly pageSize = 30
  graph: GraphNode[] = $state([])
  loading = $state(false)

  async refreshRepo(): Promise<void> {
    const { org, repo } = this.route
    if (!org || !repo) return
    this.loading = true
    this.error = null
    try {
      const bs = await fetchBranches(org, repo)
      this.branches = bs
      if (this.branches.length && !this.branch) {
        this.branch = this.branches.find((b) => b.name === 'main')?.name ?? this.branches[0]!.name
      } else if (this.branches.length === 0) {
        this.branch = 'main'
      } else if (!this.branches.some((b) => b.name === this.branch)) {
        this.branch = this.branches[0]!.name
      }
      this.tags = []
      try {
        this.tags = await fetchTags(org, repo)
      } catch {
        this.tags = []
      }
      await this.loadTree()
      await this.loadCommits(1)
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e)
    } finally {
      this.loading = false
    }
  }

  async loadTree(): Promise<void> {
    const { org, repo } = this.route
    if (!org || !repo || !this.branch) return
    try {
      const raw = await fetchTree(org, repo, this.branch || 'main')
      this.tree = raw.map((e) => ({
        name: e.path.split('/').pop() ?? e.path,
        path: e.path,
        is_dir: e.kind === 'tree',
        size: e.size,
      }))
      await this.loadReadme()
    } catch {
      this.tree = []
    }
  }

  async loadCommits(page: number): Promise<void> {
    const { org, repo } = this.route
    if (!org || !repo) return
    try {
      const res = await fetchCommits(org, repo, page || 1, this.pageSize)
      this.commits = res.commits
      this.commitTotal = res.total_count
      this.commitPage = page || 1
    } catch {
      this.commits = []
    }
  }

  async loadGraph(): Promise<void> {
    const { org, repo } = this.route
    if (!org || !repo) return
    try {
      this.graph = await fetchGraph(org, repo)
    } catch {
      this.graph = []
    }
  }

  // ── files tab ──
  expandedDirs: Set<string> = $state(new Set(['']))
  selectedPath: string | null = $state(null)
  fileData: { content: string; encoding: string; size: number } | null = $state(null)
  editing = $state(false)
  editContent = $state('')
  editMessage = $state('')
  editAmend = $state(true)
  fileLog: CommitInfo[] = $state([])
  readmeText: string | null = $state(null)
  readmeFailed = $state(false)

  readmeLabel(): { path: string } | null {
    return this.tree.find((e) => e.path.toLowerCase() === 'readme.md') ?? null
  }

  async loadReadme(): Promise<void> {
    this.readmeText = null
    this.readmeFailed = false
    const rm = this.readmeLabel()
    const { org, repo } = this.route
    if (!rm || !org || !repo) return
    try {
      this.readmeText = await fetchRawFile(org, repo, rm.path)
    } catch {
      this.readmeFailed = true
    }
  }

  async openFile(path: string): Promise<void> {
    const { org, repo } = this.route
    if (!org || !repo) return
    this.selectedPath = path
    this.editing = false
    this.fileData = null
    try {
      const f = await fetchFile(org, repo, this.branch || 'main', path)
      this.fileData = { content: decodeContent(f), encoding: f.encoding, size: f.size }
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e)
    }
  }

  startEdit(): void {
    this.editing = true
    this.editContent = this.fileData?.content ?? ''
    this.editMessage = ''
    this.editAmend = true
  }

  async saveFile(): Promise<void> {
    const { org, repo } = this.route
    if (!org || !repo || !this.selectedPath) return
    try {
      await writeFile(org, repo, this.branch || 'main', this.selectedPath, this.editContent, this.editMessage || `update ${this.selectedPath}`, this.editAmend)
      this.fileData = { content: this.editContent, encoding: 'utf-8', size: this.editContent.length }
      this.editing = false
      await Promise.all([this.loadTree(), this.loadCommits(this.commitPage)])
      this.flash(`Saved ${this.selectedPath}`)
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e)
    }
  }

  async removeFile(): Promise<void> {
    const { org, repo } = this.route
    if (!org || !repo || !this.selectedPath) return
    if (!confirm(`Delete ${this.selectedPath}?`)) return
    try {
      await deleteFile(org, repo, this.branch || 'main', this.selectedPath, `delete ${this.selectedPath}`)
      this.selectedPath = null
      this.fileData = null
      await Promise.all([this.loadTree(), this.loadCommits(this.commitPage)])
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e)
    }
  }

  async loadFileLog(): Promise<void> {
    const { org, repo } = this.route
    if (!org || !repo || !this.selectedPath) return
    try {
      const res = await fetchFileLog(org, repo, this.selectedPath)
      this.fileLog = res.commits
    } catch {
      this.fileLog = []
    }
  }

  toggleDir(path: string): void {
    const next = new Set(this.expandedDirs)
    if (next.has(path)) next.delete(path)
    else next.add(path)
    this.expandedDirs = next
  }

  // ── branches / tags tab ──
  newBranchFrom = $state('')
  newBranchName = $state('')
  newTagFrom = $state('')
  newTagName = $state('')

  async doCreateBranch(): Promise<void> {
    const { org, repo } = this.route
    if (!org || !repo || !this.newBranchName) return
    try {
      await createBranch(org, repo, this.newBranchName, this.newBranchFrom || this.branch || 'main')
      this.newBranchName = ''
      this.newBranchFrom = ''
      await this.refreshRepo()
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e)
    }
  }

  async doCreateTag(): Promise<void> {
    const { org, repo } = this.route
    if (!org || !repo || !this.newTagName) return
    try {
      await createTag(org, repo, this.newTagName, this.newTagFrom || this.branch || 'main')
      this.newTagName = ''
      this.newTagFrom = ''
      this.tags = await fetchTags(org, repo)
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e)
    }
  }

  async doDeleteBranch(name: string): Promise<void> {
    const { org, repo } = this.route
    if (!org || !repo) return
    if (!confirm(`Delete branch ${name}?`)) return
    try {
      await deleteBranch(org, repo, name)
      await this.refreshRepo()
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e)
    }
  }

  async doDeleteTag(name: string): Promise<void> {
    const { org, repo } = this.route
    if (!org || !repo) return
    if (!confirm(`Delete tag ${name}?`)) return
    try {
      await deleteTag(org, repo, name)
      this.tags = await fetchTags(org, repo)
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e)
    }
  }

  // ── commit detail ──
  commitDetail: CommitInfo | null = $state(null)
  commitDiffText = $state('')

  async loadCommitDetail(sha: string): Promise<void> {
    const { org, repo } = this.route
    if (!org || !repo) return
    this.commitDetail = null
    this.commitDiffText = ''
    try {
      const c = await fetchCommit(org, repo, sha)
      this.commitDetail = c
      const parent = c.parents[0]
      this.commitDiffText = await fetchDiff(org, repo, parent ?? 'root', sha)
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e)
    }
  }

  // ── changes tab ──
  changesList: DbBookmark[] = $state([])
  changeDetail: CommitInfo | null = $state(null)

  async loadChanges(): Promise<void> {
    const { org, repo } = this.route
    if (!org || !repo) return
    try {
      this.changesList = await fetchBookmarks(org, repo)
    } catch {
      this.changesList = []
    }
  }

  async loadChangeDetail(changeId: string): Promise<void> {
    const { org, repo } = this.route
    if (!org || !repo) return
    this.changeDetail = null
    try {
      this.changeDetail = await fetchChange(org, repo, changeId)
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e)
    }
  }

  // ── op-log tab ──
  ops: OpLogEntry[] = $state([])
  conflicts: Conflict[] = $state([])

  async loadOps(): Promise<void> {
    const { org, repo } = this.route
    if (!org || !repo) return
    try {
      this.ops = await fetchOpLog(org, repo)
      this.conflicts = await fetchConflicts(org, repo)
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e)
    }
  }

  async doUndo(opId: string): Promise<void> {
    const { org, repo } = this.route
    if (!org || !repo) return
    if (!confirm(`Undo operation ${opId.slice(0, 10)}? This creates a compensating change.`)) return
    try {
      await undoOp(org, repo, opId)
      await this.loadOps()
      this.flash('Undo recorded')
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e)
    }
  }

  // ── pulls tab ──
  mrs: Mr[] = $state([])
  mrDetail: Mr | null = $state(null)
  mrDiffText = $state('')
  mrReviews: MrReview[] = $state([])
  mrComments: MrComment[] = $state([])
  newMrTitle = $state('')
  newMrHead = $state('')
  newMrBase = $state('main')
  newMrBody = $state('')
  reviewState = $state('comment')
  reviewBody = $state('')
  commentBody = $state('')

  async loadMrs(): Promise<void> {
    const { org, repo } = this.route
    if (!org || !repo) return
    try {
      this.mrs = await fetchMrs(org, repo)
    } catch {
      this.mrs = []
    }
  }

  async loadMrDetail(num: number): Promise<void> {
    const { org, repo } = this.route
    if (!org || !repo) return
    try {
      this.mrDetail = await fetchMr(org, repo, num)
      this.mrDiffText = await fetchMrDiff(org, repo, num)
      this.mrReviews = await fetchReviews(org, repo, num)
      this.mrComments = await fetchComments(org, repo, num)
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e)
    }
  }

  async doCreateMr(): Promise<void> {
    const { org, repo } = this.route
    if (!org || !repo || !this.newMrTitle || !this.newMrHead) return
    try {
      await createMr(org, repo, this.newMrTitle, this.newMrBody, this.newMrHead, this.newMrBase)
      this.newMrTitle = ''
      this.newMrHead = ''
      this.newMrBody = ''
      await this.loadMrs()
      this.flash('Pull request created')
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e)
    }
  }

  async doReview(): Promise<void> {
    const { org, repo } = this.route
    if (!org || !repo || !this.mrDetail) return
    try {
      await addReview(org, repo, this.mrDetail.number, this.reviewState, this.reviewBody)
      this.reviewBody = ''
      await this.loadMrDetail(this.mrDetail.number)
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e)
    }
  }

  async doComment(): Promise<void> {
    const { org, repo } = this.route
    if (!org || !repo || !this.mrDetail) return
    try {
      await addComment(org, repo, this.mrDetail.number, this.commentBody)
      this.commentBody = ''
      await this.loadMrDetail(this.mrDetail.number)
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e)
    }
  }

  async closeMr(): Promise<void> {
    const { org, repo } = this.route
    if (!org || !repo || !this.mrDetail) return
    try {
      await updateMrState(org, repo, this.mrDetail.number, 'close')
      await this.loadMrDetail(this.mrDetail.number)
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e)
    }
  }

  // ── releases ──
  releases: Release[] = $state([])
  relTag = $state('')
  relName = $state('')
  relBody = $state('')
  relPre = $state(false)

  async loadReleases(): Promise<void> {
    const { org, repo } = this.route
    if (!org || !repo) return
    try {
      this.releases = await fetchReleases(org, repo)
    } catch {
      this.releases = []
    }
  }

  async doCreateRelease(): Promise<void> {
    const { org, repo } = this.route
    if (!org || !repo || !this.relTag) return
    try {
      await createRelease(org, repo, this.relTag, this.relName, this.relBody, this.relPre)
      this.relTag = ''
      this.relName = ''
      this.relBody = ''
      this.relPre = false
      await this.loadReleases()
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e)
    }
  }

  async doDeleteRelease(tag: string): Promise<void> {
    const { org, repo } = this.route
    if (!org || !repo) return
    if (!confirm(`Delete release ${tag}?`)) return
    try {
      await deleteRelease(org, repo, tag)
      await this.loadReleases()
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e)
    }
  }

  // ── actions ──
  workflows: Workflow[] = $state([])
  runs: Run[] = $state([])
  runJobs: Job[] = $state([])
  activeRun: number | null = $state(null)
  jobLogText = $state('')

  async loadActions(): Promise<void> {
    const { org, repo } = this.route
    if (!org || !repo) return
    try {
      this.workflows = await fetchWorkflows(org, repo)
      this.runs = await fetchRuns(org, repo)
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e)
    }
  }

  async doDispatch(wf: Workflow): Promise<void> {
    const { org, repo } = this.route
    if (!org || !repo) return
    try {
      await dispatchWorkflow(org, repo, wf.id)
      await this.loadActions()
      this.flash(`Dispatched ${wf.name}`)
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e)
    }
  }

  async openRun(runId: number): Promise<void> {
    const { org, repo } = this.route
    if (!org || !repo) return
    this.activeRun = runId
    this.runJobs = await fetchJobs(org, repo, runId)
  }

  async openJobLog(jobId: number): Promise<void> {
    const { org, repo } = this.route
    if (!org || !repo) return
    this.jobLogText = await fetchJobLogs(org, repo, jobId)
  }

  // ── settings tab ──
  renameName = $state('')

  async doDeleteRepo(): Promise<void> {
    const { org, repo } = this.route
    if (!org || !repo) return
    if (!confirm(`Delete ${org}/${repo}? This is irreversible.`)) return
    try {
      await deleteRepo(org, repo)
      this.nav(`#/${encodeURIComponent(org)}`)
      await this.loadOrgs()
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e)
    }
  }

  cloneCopied = $state(false)

  async copyClone(url: string): Promise<void> {
    try {
      await navigator.clipboard.writeText(url)
      this.cloneCopied = true
      setTimeout(() => (this.cloneCopied = false), 1500)
    } catch {
      /* clipboard unavailable */
    }
  }

  // ── graph ──
  get commitGraph() {
    return layoutGraph(this.graph)
  }

  // ── bootstrap ──
  init(): () => void {
    void this.loadOrgs()
    if (!window.location.hash) window.location.hash = '#/'
    const onHash = () => this.applyRoute()
    window.addEventListener('hashchange', onHash)
    this.applyRoute()
    return () => window.removeEventListener('hashchange', onHash)
  }
}

export const app = new AppStore()

// Route-driven lifecycle, kept at module scope via $effect.root so it is fully
// reactive but owned once (the App root persists for the SPA lifetime).
$effect.root(() => {
  $effect(() => {
    const { org, repo } = app.route
    if (!org || !repo) {
      app.branches = []
      app.tags = []
      app.tree = []
      app.commits = []
      app.graph = []
      return
    }
    app.branch = ''
    app.selectedPath = null
    app.fileData = null
    app.editing = false
    app.commitDetail = null
    app.mrDetail = null
    void app.refreshRepo()
  })

  $effect(() => {
    const { org, repo, tab, sub, sub2 } = app.route
    if (!org || !repo) return
    if (tab === 'graph') void app.loadGraph()
    else if (tab === 'changes') void app.loadChanges()
    else if (tab === 'op-log') void app.loadOps()
    else if (tab === 'pulls') {
      if (sub && sub !== 'new') void app.loadMrDetail(Number(sub))
      else void app.loadMrs()
    } else if (tab === 'releases') void app.loadReleases()
    else if (tab === 'actions') void app.loadActions()
    if (sub === 'commit' && sub2) void app.loadCommitDetail(sub2)
  })

  $effect(() => {
    const { org, repo, tab, sub, sub2 } = app.route
    if (!org || !repo || tab !== 'files' || sub !== 'blob' || !sub2) return
    app.selectedPath = sub2
    void app.openFile(sub2)
  })
})