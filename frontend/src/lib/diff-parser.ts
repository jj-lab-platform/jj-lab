export type DiffLineType = 'context' | 'removed' | 'added' | 'header'

export interface DiffLine {
  content: string
  type: DiffLineType
  oldLine?: number
  newLine?: number
}

export interface DiffFile {
  filename: string
  oldFile: string
  newFile: string
  header: string[]
  hunks: DiffHunk[]
}

export interface DiffHunk {
  header: string
  oldStart: number
  oldCount: number
  newStart: number
  newCount: number
  lines: DiffLine[]
}

export function parseDiff(text: string): DiffFile[] {
  if (!text.trim()) return []

  const files: DiffFile[] = []
  const chunks = text.split(/^diff --git /m)
  for (const chunk of chunks) {
    if (!chunk.trim()) continue
    const header = `diff --git ${chunk}`
    const lines = header.split('\n')

    const fileMatch = header.match(/^diff --git a\/(.+?) b\/(.+?)$/m)
    if (!fileMatch) continue

    const file: DiffFile = {
      filename: fileMatch[2],
      oldFile: fileMatch[1],
      newFile: fileMatch[2],
      header: [],
      hunks: [],
    }

    let inHunk = false
    let currentHunk: DiffHunk | null = null

    for (let i = 0; i < lines.length; i++) {
      const line = lines[i]
      if (line.startsWith('diff --git ')) {
        if (i > 0) continue
        file.header.push(line)
      } else if (
        line.startsWith('index ') ||
        line.startsWith('--- ') ||
        line.startsWith('+++ ')
      ) {
        file.header.push(line)
      } else if (line.startsWith('@@')) {
        const m = line.match(/@@ -(\d+),?(\d*) \+(\d+),?(\d*) @@(.*)/)
        if (m) {
          currentHunk = {
            header: line,
            oldStart: parseInt(m[1], 10),
            oldCount: m[2] ? parseInt(m[2], 10) : 1,
            newStart: parseInt(m[3], 10),
            newCount: m[4] ? parseInt(m[4], 10) : 1,
            lines: [],
          }
          file.hunks.push(currentHunk)
          inHunk = true
        }
      } else if (inHunk && currentHunk) {
        if (line.startsWith('-')) {
          currentHunk.lines.push({
            content: line.slice(1),
            type: 'removed',
            oldLine:
              currentHunk.oldStart +
              currentHunk.lines.filter(l => l.type !== 'added').length,
          })
        } else if (line.startsWith('+')) {
          currentHunk.lines.push({
            content: line.slice(1),
            type: 'added',
            newLine:
              currentHunk.newStart +
              currentHunk.lines.filter(l => l.type !== 'removed').length,
          })
        } else if (line.startsWith(' ') || line === '') {
          const content = line.startsWith(' ') ? line.slice(1) : line
          const oldCnt = currentHunk.lines.filter(
            l => l.type !== 'added',
          ).length
          const newCnt = currentHunk.lines.filter(
            l => l.type !== 'removed',
          ).length
          currentHunk.lines.push({
            content,
            type: 'context',
            oldLine: currentHunk.oldStart + oldCnt,
            newLine: currentHunk.newStart + newCnt,
          })
        } else if (line.startsWith('\\ ')) {
          // "\ No newline at end of file" — skip
        }
      }
    }

    if (file.hunks.length > 0) files.push(file)
  }

  return files
}
