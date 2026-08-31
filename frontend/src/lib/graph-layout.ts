// Pure lane-layout for the change graph. Nodes list a commit's parents; each
// commit is placed on a lane, the first parent inheriting its child's lane and
// later parents claiming free lanes, so history lines are stable and readable.
import type { GraphNode } from '$lib/api'

export interface GraphRow {
  node: GraphNode
  lane: number
}

export interface GraphLayout {
  rows: GraphRow[]
  lanes: number
}

export const LANE_COLORS = [
  '#54aeff',
  '#f778ba',
  '#3fb950',
  '#d29922',
  '#ab7df8',
  '#39c5cf',
  '#ff7b72',
  '#7ee787',
]

/** @deprecated kept for name continuity — prefer `layoutGraph`. */
export const layoutGraph = (nodes: GraphNode[]): GraphLayout => computeLayout(nodes)

export function computeLayout(nodes: GraphNode[]): GraphLayout {
  const active: Array<string | null> = []
  const rows: GraphRow[] = []
  nodes.forEach((node) => {
    let lane = active.indexOf(node.commit_id)
    if (lane === -1) {
      lane = active.indexOf(null)
      if (lane === -1) {
        active.push(node.commit_id)
        lane = active.length - 1
      } else {
        active[lane] = node.commit_id
      }
    } else {
      active[lane] = null
    }
    // first parent inherits lane; others claim a free lane
    node.parents.forEach((pid, idx) => {
      if (idx === 0) {
        const existing = active.indexOf(pid)
        if (existing === -1) active[lane] = pid
        else active[lane] = null
      } else {
        const existing = active.indexOf(pid)
        if (existing === -1) {
          const free = active.indexOf(null)
          if (free === -1) active.push(pid)
          else active[free] = pid
        }
      }
    })
    rows.push({ node, lane })
  })
  return { rows, lanes: Math.max(1, ...rows.map((r) => r.lane + 1)) }
}