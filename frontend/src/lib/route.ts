// The single route type and tab list drive the top-level App shell. Extracted
// so views and the shell share one routing vocabulary without importing UI.
export type Tab =
  | 'files'
  | 'commits'
  | 'bookmarks'
  | 'graph'
  | 'changes'
  | 'pulls'
  | 'releases'
  | 'actions'
  | 'settings'

export interface Route {
  org: string | null
  repo: string | null
  tab: Tab
  sub: string
  sub2: string
}

export const TABS: { id: Tab; label: string }[] = [
  { id: 'files', label: 'Code' },
  { id: 'commits', label: 'Commits' },
  { id: 'bookmarks', label: 'Bookmarks' },
  { id: 'graph', label: 'Graph' },
  { id: 'changes', label: 'Changes' },
  { id: 'pulls', label: 'Pulls' },
  { id: 'releases', label: 'Releases' },
  { id: 'actions', label: 'Actions' },
  { id: 'settings', label: 'Settings' },
]