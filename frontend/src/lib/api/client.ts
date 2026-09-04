// ── transport ──

import type {
  BookmarkInfo,
  ChangeSummary,
  CommitInfo,
  Conflict,
  DbBookmark,
  GraphNode,
  Job,
  Mr,
  MrComment,
  MrReview,
  Org,
  Release,
  Run,
  SearchHit,
  TreeEntry,
  Workflow,
} from './types'

export type {
  FileContent,
  OrgRepo,
  ReleaseAsset,
} from './types'

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

export async function fetchBookmarks(org: string, repo: string): Promise<BookmarkInfo[]> {
  const data = await request<{ bookmarks: BookmarkInfo[] }>(
    `/api/v1/repos/${enc(org)}/${enc(repo)}/bookmarks`,
  )
  return data.bookmarks ?? []
}

export async function fetchTags(org: string, repo: string): Promise<BookmarkInfo[]> {
  const data = await request<{ tags: BookmarkInfo[] }>(
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
  return request(`/api/v1/repos/${enc(org)}/${enc(repo)}/commits/${enc(sha)}`)
}

export async function fetchCommitDiff(org: string, repo: string, sha: string): Promise<{ diff: string }> {
  return request(`/api/v1/repos/${enc(org)}/${enc(repo)}/commits/${enc(sha)}/diff`)
}

export async function fetchChanges(org: string, repo: string, rev: string): Promise<ChangeSummary[]> {
  const data = await request<{ changes: ChangeSummary[] }>(
    `/api/v1/repos/${enc(org)}/${enc(repo)}/changes?rev=${enc(rev)}`,
  )
  return data.changes ?? []
}

export async function fetchContents(
  org: string,
  repo: string,
  rev: string,
  dir = '',
): Promise<TreeEntry[]> {
  const path = `${dir ? `/${dir}` : ''}?ref=${enc(rev)}`
  const data = await request<{ entries: Array<Omit<TreeEntry, 'kind' | 'size'> & { type?: string; size?: number }> }>(
    `/api/v1/repos/${enc(org)}/${enc(repo)}/contents${path}`,
  )
  return (data.entries ?? []).map((e) => ({
    path: e.path,
    mode: e.mode,
    kind: e.type ?? (e.kind ?? 'file'),
    size: e.size ?? 0,
  }))
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

export async function fetchRawFile(
  org: string,
  repo: string,
  path: string,
  rev?: string,
): Promise<string> {
  const headers = new Headers()
  if (authToken) headers.set('authorization', `token ${authToken}`)
  const ref = rev ? `&ref=${enc(rev)}` : ''
  const resp = await fetch(
    `/api/v1/repos/${enc(org)}/${enc(repo)}/blob?path=${enc(path)}${ref}`,
    { headers },
  )
  if (!resp.ok) throw new ApiError(resp.status, `blob ${resp.status}`)
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
    `/api/v1/repos/${enc(org)}/${enc(repo)}/graph?limit=${limit}`,
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


export async function searchCode(
  org: string,
  repo: string,
  bookmark: string,
  pattern: string,
): Promise<SearchHit[]> {
  const data = await request<{ matches: string[] }>(
    `/api/v1/repos/${enc(org)}/${enc(repo)}/search?pattern=${enc(pattern)}&ref=${enc(bookmark)}`,
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

export async function fetchConflicts(org: string, repo: string): Promise<Conflict[]> {
  const data = await request<{ conflicts: Conflict[] }>(
    `/api/v1/repos/${enc(org)}/${enc(repo)}/conflicts`,
  )
  return data.conflicts ?? []
}



// ── writes ──

export async function createRepo(org: string, repo: string): Promise<void> {
  await request(`/api/v1/repos/${enc(org)}/${enc(repo)}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ default_bookmark: 'main' }),
  })
}

export async function deleteRepo(org: string, repo: string): Promise<void> {
  await request(`/api/v1/repos/${enc(org)}/${enc(repo)}`, { method: 'DELETE' })
}

export async function writeFile(
  org: string,
  repo: string,
  bookmark: string,
  path: string,
  content: string,
  message: string,
  amend = true,
): Promise<{ sha: string; change_id: string }> {
  return request(`/api/v1/repos/${enc(org)}/${enc(repo)}/contents/${path}`, {
    method: 'PUT',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ content_base64: btoa(unescape(encodeURIComponent(content))), bookmark, message, amend }),
  })
}

export async function createFile(
  org: string,
  repo: string,
  bookmark: string,
  path: string,
  content: string,
  message: string,
  amend = true,
): Promise<{ sha: string; change_id: string }> {
  return request(`/api/v1/repos/${enc(org)}/${enc(repo)}/contents/${path}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ content_base64: btoa(unescape(encodeURIComponent(content))), bookmark, message, amend }),
  })
}

export async function deleteFile(
  org: string,
  repo: string,
  bookmark: string,
  path: string,
  message: string,
  amend = true,
): Promise<{ sha: string; change_id: string }> {
  return request(`/api/v1/repos/${enc(org)}/${enc(repo)}/contents/${path}`, {
    method: 'DELETE',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ bookmark, message, amend }),
  })
}

export async function createBookmark(
  org: string,
  repo: string,
  name: string,
  target: string,
): Promise<{ name: string; sha: string }> {
  return request(`/api/v1/repos/${enc(org)}/${enc(repo)}/bookmarks/${enc(name)}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ target }),
  })
}

export async function deleteBookmark(org: string, repo: string, name: string): Promise<void> {
  await request(`/api/v1/repos/${enc(org)}/${enc(repo)}/bookmarks/${enc(name)}`, {
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
  bookmark?: string,
): Promise<void> {
  await request(`/api/v1/repos/${enc(org)}/${enc(repo)}/sync/clone`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ url, bookmark: bookmark || undefined }),
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

