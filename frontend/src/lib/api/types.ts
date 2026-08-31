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

export interface ChangeSummary {
  change_id: string
  commit_id: string
  description: string
  author: string
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

export interface FileContent {
  name: string
  path: string
  sha: string
  type: string
  size: number
  encoding: 'base64' | string
  content: string
}

export interface SearchHit {
  path: string
  line: number
  text: string
}
