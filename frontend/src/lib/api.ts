// Typed client for the jjlab REST surface (mirrors `build_router` in
// `crates/server/src/lib.rs`). All writes go through the same static-token
// auth as git smart-http.

export interface CommitInfo {
  sha: string
  change_id: string
  description: string
  author: string
  committer: string
  parents: string[]
}

export interface BranchInfo {
  name: string
  sha: string
}

export interface TreeEntry {
  path: string
  mode: string
  kind: string
  size: number
}

export interface OrgRepo {
  repo: string
  default_bookmark: string
}

export interface Org {
  org: string
  repos: OrgRepo[]
}

export interface GraphNode {
  commit_id: string
  change_id: string
  message: string
  author: string
  parents: string[]
  edge_types: string[]
  is_head: boolean
}

export interface OpLogEntry {
  id: string
  repo_id: string
  op_type: string
  payload: string
  undo_of: string | null
}

export interface Conflict {
  id: string
  repo_id: string
  change_id: string
  path: string
  adds: unknown
  removes: unknown
}

export interface DbBookmark {
  name: string
  change_id: string
  is_remote: boolean
}

export interface Mr {
  number: number
  title: string
  body: string
  author: string
  state: string
  head_change_id: string
  head_sha: string | null
  base: string
  review_state: string
}

export interface MrReview {
  reviewer: string
  state: string
  body: string
  commit_sha: string | null
}

export interface MrComment {
  author: string
  body: string
  path: string | null
  commit_sha: string | null
}

export interface ReleaseAsset {
  name: string
  size: number
  digest: string
  content_type: string
  browser_download_url: string
}

export interface Release {
  id: number
  tag_name: string
  name: string
  body: string
  draft: boolean
  prerelease: boolean
  assets: ReleaseAsset[]
}

export interface Workflow {
  id: number
  name: string
  path: string
  trigger: string
  enabled: boolean
}

export interface Run {
  id: number
  workflow_id: number
  trigger_ref: string
  status: string
}

export interface Job {
  id: number
  run_id: number
  name: string
  status: string
  exit_code: number | null
}

// ── transport ──

let authToken: string | null =
  typeof localStorage !== 'undefined' ? localStorage.getItem('jjlab_token') : null

export function setToken(token: string | null): void {
  authToken = token
  if (typeof localStorage !== 'undefined') {
    if (token) localStorage.setItem('jjlab_token', token)
    else localStorage.removeItem('jjlab_token')
  }
}

export function getToken(): string | null {
  return authToken
}

export class ApiError extends Error {
  status: number
  constructor(status: number, message: string) {
    super(message)
    this.status = status
  }
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const headers = new Headers(init?.headers)
  if (authToken) headers.set('authorization', `token ${authToken}`)
  const resp = await fetch(path, { ...init, headers })
  if (!resp.ok) {
    let message = `HTTP ${resp.status}`
    try {
      const body = await resp.json()
      if (body && typeof body.message === 'string') message = body.message
    } catch {
      /* ignore non-JSON error bodies */
    }
    throw new ApiError(resp.status, message)
  }
  return (await resp.json()) as T
}

function enc(s: string): string {
  return encodeURIComponent(s)
}

// ── explore / orgs ──

export async function fetchOrgs(): Promise<Org[]> {
  const data = await request<{ orgs: Org[] }>('/api/v1/repos')
  return data.orgs ?? []
}

// ── repo reads ──

export async function fetchBranches(org: string, repo: string): Promise<BranchInfo[]> {
  const data = await request<{ branches: BranchInfo[] }>(
    `/api/v1/repos/${enc(org)}/${enc(repo)}/branches`,
  )
  return data.branches ?? []
}

export async function fetchTags(org: string, repo: string): Promise<BranchInfo[]> {
  const data = await request<{ tags: BranchInfo[] }>(
    `/api/v1/repos/${enc(org)}/${enc(repo)}/tags`,
  )
  return data.tags ?? []
}

export async function fetchCommits(
  org: string,
  repo: string,
  page = 1,
  limit = 30,
): Promise<{ commits: CommitInfo[]; total_count: number }> {
  return request(
    `/api/v1/repos/${enc(org)}/${enc(repo)}/commits?page=${page}&limit=${limit}`,
  )
}

export async function fetchCommit(org: string, repo: string, sha: string): Promise<CommitInfo> {
  return request(`/api/v1/repos/${enc(org)}/${enc(repo)}/git/commits/${enc(sha)}`)
}

export async function fetchChange(org: string, repo: string, changeId: string): Promise<CommitInfo> {
  return request(`/api/v1/repos/${enc(org)}/${enc(repo)}/changes/${enc(changeId)}`)
}

export async function fetchTree(org: string, repo: string, rev: string): Promise<TreeEntry[]> {
  const data = await request<{ tree: TreeEntry[] }>(
    `/api/v1/repos/${enc(org)}/${enc(repo)}/tree/${enc(rev)}`,
  )
  return data.tree ?? []
}

export interface FileContent {
  name: string
  path: string
  sha: string
  type: string
  size: number
  encoding: 'base64' | string
  content: string
}

export async function fetchFile(
  org: string,
  repo: string,
  rev: string,
  path: string,
): Promise<FileContent> {
  return request(
    `/api/v1/repos/${enc(org)}/${enc(repo)}/contents/${path}?ref=${enc(rev)}`,
  )
}

export async function fetchRawFile(org: string, repo: string, path: string): Promise<string> {
  const headers = new Headers()
  if (authToken) headers.set('authorization', `token ${authToken}`)
  const resp = await fetch(`/api/v1/repos/${enc(org)}/${enc(repo)}/raw/${path}`, { headers })
  if (!resp.ok) throw new ApiError(resp.status, `raw ${resp.status}`)
  return resp.text()
}

export async function fetchDiff(
  org: string,
  repo: string,
  base: string,
  head: string,
): Promise<string> {
  const data = await request<{ diff: string }>(
    `/api/v1/repos/${enc(org)}/${enc(repo)}/compare?base=${enc(base)}&head=${enc(head)}`,
  )
  return data.diff ?? ''
}

export async function fetchGraph(org: string, repo: string, limit = 100): Promise<GraphNode[]> {
  const data = await request<{ graph: GraphNode[] }>(
    `/api/v1/graph/${enc(org)}/${enc(repo)}?limit=${limit}`,
  )
  return data.graph ?? []
}

export async function fetchFileLog(
  org: string,
  repo: string,
  path: string,
  limit = 50,
): Promise<{ commits: CommitInfo[]; total_count: number }> {
  return request(
    `/api/v1/repos/${enc(org)}/${enc(repo)}/file-log?path=${enc(path)}&limit=${limit}`,
  )
}

export interface SearchHit {
  path: string
  line: number
  text: string
}

export async function searchCode(
  org: string,
  repo: string,
  branch: string,
  pattern: string,
): Promise<SearchHit[]> {
  const data = await request<{ matches: string[] }>(
    `/api/v1/repos/${enc(org)}/${enc(repo)}/${enc(branch)}/search?pattern=${enc(pattern)}`,
  )
  return (data.matches ?? []).map((m) => {
    const i = m.indexOf(':')
    const j = m.indexOf(':', i + 1)
    const path = m.slice(0, i)
    const line = Number(m.slice(i + 1, j))
    const text = m.slice(j + 1)
    return { path, line, text }
  })
}

// ── jj-native metadata ──

export async function fetchOpLog(org: string, repo: string): Promise<OpLogEntry[]> {
  const data = await request<{ ops: OpLogEntry[] }>(
    `/api/v1/repos/${enc(org)}/${enc(repo)}/op-log`,
  )
  return data.ops ?? []
}

export async function fetchConflicts(org: string, repo: string): Promise<Conflict[]> {
  const data = await request<{ conflicts: Conflict[] }>(
    `/api/v1/repos/${enc(org)}/${enc(repo)}/conflicts`,
  )
  return data.conflicts ?? []
}

export async function fetchBookmarks(org: string, repo: string): Promise<DbBookmark[]> {
  const data = await request<{ bookmarks: DbBookmark[] }>(
    `/api/v1/repos/${enc(org)}/${enc(repo)}/bookmarks`,
  )
  return data.bookmarks ?? []
}

export async function undoOp(
  org: string,
  repo: string,
  opId: string,
): Promise<{ ok: boolean; undo_op_id: string; undo_of: string | null }> {
  return request(`/api/v1/repos/${enc(org)}/${enc(repo)}/op-log/${enc(opId)}/undo`, {
    method: 'POST',
  })
}

// ── writes ──

export async function createRepo(org: string, repo: string): Promise<void> {
  await request(`/api/v1/repos/${enc(org)}/${enc(repo)}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ default_branch: 'main' }),
  })
}

export async function deleteRepo(org: string, repo: string): Promise<void> {
  await request(`/api/v1/repos/${enc(org)}/${enc(repo)}`, { method: 'DELETE' })
}

export async function writeFile(
  org: string,
  repo: string,
  branch: string,
  path: string,
  content: string,
  message: string,
): Promise<{ sha: string; change_id: string }> {
  return request(`/api/v1/repos/${enc(org)}/${enc(repo)}/contents/${path}`, {
    method: 'PUT',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ content_base64: btoa(unescape(encodeURIComponent(content))), branch, message }),
  })
}

export async function createFile(
  org: string,
  repo: string,
  branch: string,
  path: string,
  content: string,
  message: string,
): Promise<{ sha: string; change_id: string }> {
  return request(`/api/v1/repos/${enc(org)}/${enc(repo)}/contents/${path}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ content_base64: btoa(unescape(encodeURIComponent(content))), branch, message }),
  })
}

export async function deleteFile(
  org: string,
  repo: string,
  branch: string,
  path: string,
  message: string,
): Promise<{ sha: string; change_id: string }> {
  return request(`/api/v1/repos/${enc(org)}/${enc(repo)}/contents/${path}`, {
    method: 'DELETE',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ branch, message }),
  })
}

export async function createBranch(
  org: string,
  repo: string,
  name: string,
  target: string,
): Promise<{ name: string; sha: string }> {
  return request(`/api/v1/repos/${enc(org)}/${enc(repo)}/branches/${enc(name)}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ target }),
  })
}

export async function deleteBranch(org: string, repo: string, name: string): Promise<void> {
  await request(`/api/v1/repos/${enc(org)}/${enc(repo)}/branches/${enc(name)}`, {
    method: 'DELETE',
  })
}

export async function createTag(
  org: string,
  repo: string,
  name: string,
  target: string,
): Promise<{ name: string; sha: string }> {
  return request(`/api/v1/repos/${enc(org)}/${enc(repo)}/tags/${enc(name)}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ target }),
  })
}

export async function deleteTag(org: string, repo: string, name: string): Promise<void> {
  await request(`/api/v1/repos/${enc(org)}/${enc(repo)}/tags/${enc(name)}`, {
    method: 'DELETE',
  })
}

export async function cloneRemote(
  org: string,
  repo: string,
  url: string,
  branch?: string,
): Promise<void> {
  await request(`/api/v1/repos/${enc(org)}/${enc(repo)}/sync/clone`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ url, branch: branch || undefined }),
  })
}

// ── merge requests ──

export async function fetchMrs(org: string, repo: string): Promise<Mr[]> {
  const data = await request<{ pull_requests: Mr[] }>(
    `/api/v1/repos/${enc(org)}/${enc(repo)}/pulls`,
  )
  return data.pull_requests ?? []
}

export async function fetchMr(org: string, repo: string, number: number): Promise<Mr> {
  return request(`/api/v1/repos/${enc(org)}/${enc(repo)}/pulls/${number}`)
}

export async function createMr(
  org: string,
  repo: string,
  title: string,
  body: string,
  head: string,
  base: string,
): Promise<Mr> {
  return request(`/api/v1/repos/${enc(org)}/${enc(repo)}/pulls`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ title, body, head, base }),
  })
}

export async function updateMrState(
  org: string,
  repo: string,
  number: number,
  state: 'open' | 'close',
): Promise<Mr> {
  return request(`/api/v1/repos/${enc(org)}/${enc(repo)}/pulls/${number}`, {
    method: 'PATCH',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ state: state === 'open' ? 'open' : 'close' }),
  })
}

export async function fetchReviews(org: string, repo: string, number: number): Promise<MrReview[]> {
  const data = await request<{ reviews: MrReview[] }>(
    `/api/v1/repos/${enc(org)}/${enc(repo)}/pulls/${number}/reviews`,
  )
  return data.reviews ?? []
}

export async function addReview(
  org: string,
  repo: string,
  number: number,
  state: string,
  body: string,
): Promise<void> {
  await request(`/api/v1/repos/${enc(org)}/${enc(repo)}/pulls/${number}/reviews`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ state, body }),
  })
}

export async function fetchComments(
  org: string,
  repo: string,
  number: number,
): Promise<MrComment[]> {
  const data = await request<{ comments: MrComment[] }>(
    `/api/v1/repos/${enc(org)}/${enc(repo)}/pulls/${number}/comments`,
  )
  return data.comments ?? []
}

export async function addComment(
  org: string,
  repo: string,
  number: number,
  body: string,
  path?: string,
): Promise<void> {
  await request(`/api/v1/repos/${enc(org)}/${enc(repo)}/pulls/${number}/comments`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ body, path }),
  })
}

export async function fetchMrDiff(org: string, repo: string, number: number): Promise<string> {
  const data = await request<{ diff: string }>(
    `/api/v1/repos/${enc(org)}/${enc(repo)}/pulls/${number}/diff`,
  )
  return data.diff ?? ''
}

// ── releases ──

export async function fetchReleases(org: string, repo: string): Promise<Release[]> {
  const data = await request<{ releases: Release[] }>(
    `/api/v1/repos/${enc(org)}/${enc(repo)}/releases`,
  )
  return data.releases ?? []
}

export async function createRelease(
  org: string,
  repo: string,
  tag: string,
  name: string,
  body: string,
  prerelease: boolean,
): Promise<Release> {
  return request(`/api/v1/repos/${enc(org)}/${enc(repo)}/releases`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ tag_name: tag, name, body, prerelease }),
  })
}

export async function deleteRelease(org: string, repo: string, tag: string): Promise<void> {
  await request(`/api/v1/repos/${enc(org)}/${enc(repo)}/releases/${enc(tag)}`, {
    method: 'DELETE',
  })
}

// ── actions ──

export async function fetchWorkflows(org: string, repo: string): Promise<Workflow[]> {
  const data = await request<{ workflows: Workflow[] }>(
    `/api/v1/repos/${enc(org)}/${enc(repo)}/actions/workflows`,
  )
  return data.workflows ?? []
}

export async function dispatchWorkflow(
  org: string,
  repo: string,
  workflowId: number,
): Promise<{ run_ids: number[] }> {
  return request(
    `/api/v1/repos/${enc(org)}/${enc(repo)}/actions/workflows/${workflowId}/dispatch`,
    { method: 'POST' },
  )
}

export async function fetchRuns(org: string, repo: string): Promise<Run[]> {
  const data = await request<{ runs: Run[] }>(
    `/api/v1/repos/${enc(org)}/${enc(repo)}/actions/runs`,
  )
  return data.runs ?? []
}

export async function fetchJobs(org: string, repo: string, runId: number): Promise<Job[]> {
  const data = await request<{ jobs: Job[] }>(
    `/api/v1/repos/${enc(org)}/${enc(repo)}/actions/runs/${runId}/jobs`,
  )
  return data.jobs ?? []
}

export async function fetchJobLogs(org: string, repo: string, jobId: number): Promise<string> {
  const headers = new Headers()
  if (authToken) headers.set('authorization', `token ${authToken}`)
  const resp = await fetch(
    `/api/v1/repos/${enc(org)}/${enc(repo)}/actions/jobs/${jobId}/logs`,
    { headers },
  )
  if (!resp.ok) throw new ApiError(resp.status, `logs ${resp.status}`)
  return resp.text()
}

export function archiveUrl(org: string, repo: string, sha: string): string {
  return `/api/v1/repos/${enc(org)}/${enc(repo)}/archive/tarball/${enc(sha)}`
}

export function cloneUrls(org: string, repo: string): { http: string } {
  const host = window.location.host
  return { http: `http://${host}/${org}/${repo}.git` }
}

export function decodeContent(f: { content: string; encoding: string }): string {
  if (f.encoding === 'base64') {
    try {
      return decodeURIComponent(escape(atob(f.content)))
    } catch {
      return atob(f.content)
    }
  }
  return f.content
}