<script lang="ts">
import { onMount } from 'svelte'
import { type DiffFile, parseDiff } from '$lib/diff-parser'
import { highlightTokens, type ThemedToken } from '$lib/highlight'

let { diffText }: { diffText: string } = $props()

let appTheme = $state<string>(
  typeof document !== 'undefined'
    ? document.documentElement.getAttribute('data-theme') || 'dark'
    : 'dark',
)

onMount(() => {
  const observer = new MutationObserver(() => {
    appTheme = document.documentElement.getAttribute('data-theme') || 'dark'
  })
  observer.observe(document.documentElement, {
    attributes: true,
    attributeFilter: ['data-theme'],
  })
  return () => observer.disconnect()
})

let theme = $derived(
  appTheme === 'dark' ? ('dark-plus' as const) : ('github-light' as const),
)

let parsed = $derived.by(() => {
  try {
    return parseDiff(diffText)
  } catch {
    return []
  }
})

let fileTokens = $state<
  Record<string, { oldTokens: ThemedToken[][]; newTokens: ThemedToken[][] }>
>({})
let tokenReady = $state(false)

$effect(() => {
  const files = parsed
  if (files.length === 0) {
    fileTokens = {}
    tokenReady = false
    return
  }
  tokenReady = false
  let cancelled = false
  Promise.all(
    files.map(async f => {
      const oldLines = f.hunks.flatMap(h =>
        h.lines.filter(l => l.type !== 'added'),
      )
      const newLines = f.hunks.flatMap(h =>
        h.lines.filter(l => l.type !== 'removed'),
      )
      const [oldTokens, newTokens] = await Promise.all([
        highlightTokens(
          oldLines.map(l => l.content).join('\n'),
          f.filename,
          theme,
        ),
        highlightTokens(
          newLines.map(l => l.content).join('\n'),
          f.filename,
          theme,
        ),
      ])
      return { key: f.filename, oldTokens, newTokens }
    }),
  ).then(results => {
    if (cancelled) return
    const map: Record<
      string,
      { oldTokens: ThemedToken[][]; newTokens: ThemedToken[][] }
    > = {}
    for (const r of results)
      map[r.key] = { oldTokens: r.oldTokens, newTokens: r.newTokens }
    fileTokens = map
    tokenReady = true
  })
  return () => {
    cancelled = true
  }
})

const MONO =
  "'JetBrains Mono','Fira Code','Cascadia Code',ui-monospace,monospace"
const LINE_NUM_STYLE =
  'color:hsl(0 0% 45%);text-align:right;padding-right:0.6rem;user-select:none;white-space:nowrap;font-family:' +
  MONO +
  ';font-size:11px;line-height:1.6'
const LINE_STYLE =
  'font-family:' +
  MONO +
  ';font-size:12px;line-height:1.6;white-space:pre-wrap;word-break:break-word;overflow-wrap:break-word;padding-left:8px;padding-right:4px'
const FILE_HEADER_STYLE =
  'padding:6px 12px;font-size:11px;font-family:' +
  MONO +
  ';border-bottom:1px solid hsl(0 0% 14%)'
const HUNK_STYLE =
  'padding:4px 12px;font-size:11px;font-family:' +
  MONO +
  ';border-bottom:1px solid hsl(0 0% 12%)'
const GRID_SBS =
  'display:grid;grid-template-columns:3.25rem 1fr 3.25rem 1fr;align-items:stretch'
const GRID_LBL =
  'display:grid;grid-template-columns:3.25rem 3.25rem 0.625rem 1fr;align-items:stretch'
const GUTTER_SBS =
  'display:grid;grid-template-columns:3.25rem 1fr 3.25rem 1fr;gap:0;align-items:stretch'
// VS Code-style: a 4px inner gutter bar + code, on each side of the side-by-side grid.
const REMOVED_BG = 'background:hsl(0 44% 15% / 0.5)'
const ADDED_BG = 'background:hsl(143 44% 15% / 0.5)'
const REMOVED_BG_LIGHT = 'background:hsl(0 80% 93%)'
const ADDED_BG_LIGHT = 'background:hsl(143 70% 93%)'
const REMOVED_BAR = 'background:hsl(0 70% 45%)'
const ADDED_BAR = 'background:hsl(143 70% 40%)'
const REMOVED_BAR_LIGHT = 'background:hsl(0 70% 55%)'
const ADDED_BAR_LIGHT = 'background:hsl(143 55% 42%)'
const FILE_BORDER = 'border:1px solid hsl(0 0% 14%);border-radius:4px;overflow:hidden'

function tokenToHtml(tokens: ThemedToken[]): string {
  if (!tokens || tokens.length === 0) return ''
  return tokens
    .map(t => {
      const styles: string[] = [`color:${t.color}`]
      if (t.fontStyle === 1) styles.push('font-style:italic')
      if (t.fontStyle === 2) styles.push('font-weight:bold')
      if (t.fontStyle === (1 | 2))
        styles.push('font-weight:bold;font-style:italic')
      const esc = t.content
        .replace(/&/g, '&amp;')
        .replace(/</g, '&lt;')
        .replace(/>/g, '&gt;')
      return `<span style="${styles.join(';')}">${esc}</span>`
    })
    .join('')
}

function escapeH(s: string): string {
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
}

function statHtml(f: DiffFile): string {
  let added = 0
  let removed = 0
  for (const h of f.hunks)
    for (const l of h.lines) {
      if (l.type === 'added') added++
      else if (l.type === 'removed') removed++
    }
  const bits: string[] = []
  bits.push(
    `<span style="color:hsl(143 55% 45%)">+${added}</span>`,
    `<span style="color:hsl(0 65% 55%)">\u2212${removed}</span>`,
  )
  return bits.join(' ')
}

function fileHeaderHtml(f: DiffFile, isDark: boolean): string {
  const bg = isDark ? 'hsl(0 0% 10%)' : 'hsl(0 0% 96%)'
  const fg = isDark ? 'hsl(0 0% 72%)' : 'hsl(0 0% 35%)'
  return (
    `<div style="${FILE_HEADER_STYLE};color:${fg};background:${bg};display:flex;align-items:center;justify-content:space-between">` +
    `<span style="overflow:hidden;text-overflow:ellipsis">${escapeH(f.filename)}</span>` +
    `<span style="font-size:10px;margin-left:12px;font-family:${MONO}">${statHtml(f)}</span>` +
    `</div>`
  )
}

function renderSideBySide(
  f: DiffFile,
  tokens: { oldTokens: ThemedToken[][]; newTokens: ThemedToken[][] },
  isDark: boolean,
): string {
  const parts: string[] = []
  const removedBg = isDark ? REMOVED_BG : REMOVED_BG_LIGHT
  const addedBg = isDark ? ADDED_BG : ADDED_BG_LIGHT
  const removedBar = isDark ? REMOVED_BAR : REMOVED_BAR_LIGHT
  const addedBar = isDark ? ADDED_BAR : ADDED_BAR_LIGHT

  parts.push(`<div style="margin-bottom:16px;${FILE_BORDER}">`)
  parts.push(fileHeaderHtml(f, isDark))

  // Tokens are flattened across all hunks; the running indices must too.
  let oldIdx = 0
  let newIdx = 0
  for (const hunk of f.hunks) {
    parts.push(`<div style="${HUNK_STYLE}">${escapeH(hunk.header)}</div>`)
    for (const line of hunk.lines) {
      const oldTokens = line.type !== 'added' ? tokens.oldTokens[oldIdx] : null
      const newTokens =
        line.type !== 'removed' ? tokens.newTokens[newIdx] : null
      const leftHtml = oldTokens ? tokenToHtml(oldTokens) : ''
      const rightHtml = newTokens ? tokenToHtml(newTokens) : ''
      const leftBg = line.type === 'removed' ? removedBg : ''
      const rightBg = line.type === 'added' ? addedBg : ''
      const leftBar = line.type === 'removed' ? removedBar : 'transparent'
      const rightBar = line.type === 'added' ? addedBar : 'transparent'

      parts.push(`<div style="${GRID_SBS}">`)
      parts.push(
        `<span style="${LINE_NUM_STYLE};${leftBg}">${line.type !== 'added' && line.oldLine ? line.oldLine : ''}</span>`,
      )
      parts.push(
        `<span style="display:grid;grid-template-columns:4px 1fr;${leftBg}"><span style="background:${leftBar}"></span><span style="${LINE_STYLE}">${leftHtml}</span></span>`,
      )
      parts.push(
        `<span style="${LINE_NUM_STYLE};${rightBg}">${line.type !== 'removed' && line.newLine ? line.newLine : ''}</span>`,
      )
      parts.push(
        `<span style="display:grid;grid-template-columns:4px 1fr;${rightBg}"><span style="background:${rightBar}"></span><span style="${LINE_STYLE}">${rightHtml}</span></span>`,
      )
      parts.push(`</div>`)

      if (line.type !== 'added') oldIdx++
      if (line.type !== 'removed') newIdx++
    }
  }
  parts.push(`</div>`)
  return parts.join('\n')
}

function renderLineByLine(
  f: DiffFile,
  tokens: { oldTokens: ThemedToken[][]; newTokens: ThemedToken[][] },
  isDark: boolean,
): string {
  const parts: string[] = []
  const removedBg = isDark ? REMOVED_BG : REMOVED_BG_LIGHT
  const addedBg = isDark ? ADDED_BG : ADDED_BG_LIGHT
  const removedBar = isDark ? REMOVED_BAR : REMOVED_BAR_LIGHT
  const addedBar = isDark ? ADDED_BAR : ADDED_BAR_LIGHT

  parts.push(`<div style="margin-bottom:16px;${FILE_BORDER}">`)
  parts.push(fileHeaderHtml(f, isDark))

  let oldIdx = 0
  let newIdx = 0
  for (const hunk of f.hunks) {
    parts.push(`<div style="${HUNK_STYLE}">${escapeH(hunk.header)}</div>`)
    for (const line of hunk.lines) {
      let bg = ''
      let prefix = ' '
      let bar = 'transparent'
      let selected: ThemedToken[] | null = null
      let oldNum = ''
      let newNum = ''
      if (line.type === 'removed') {
        bg = removedBg
        prefix = '-'
        bar = removedBar
        selected = tokens.oldTokens[oldIdx]
        oldNum = line.oldLine ? String(line.oldLine) : ''
        oldIdx++
      } else if (line.type === 'added') {
        bg = addedBg
        prefix = '+'
        bar = addedBar
        selected = tokens.newTokens[newIdx]
        newNum = line.newLine ? String(line.newLine) : ''
        newIdx++
      } else {
        selected = tokens.oldTokens[oldIdx] || tokens.newTokens[newIdx]
        oldNum = line.oldLine ? String(line.oldLine) : ''
        newNum = line.newLine ? String(line.newLine) : ''
        oldIdx++
        newIdx++
      }
      const tokenHtml = selected ? tokenToHtml(selected) : '&nbsp;'
      const prefixColor = line.type === 'removed' ? removedBar : line.type === 'added' ? addedBar : 'transparent'
      parts.push(`<div style="${GRID_LBL};${bg}">`)
      parts.push(`<span style="${LINE_NUM_STYLE}">${oldNum}</span>`)
      parts.push(`<span style="${LINE_NUM_STYLE}">${newNum}</span>`)
      parts.push(
        `<span style="${LINE_NUM_STYLE};text-align:center;padding-right:0;color:${prefixColor};font-weight:600">${prefix}</span>`,
      )
      parts.push(
        `<span style="display:grid;grid-template-columns:4px 1fr"><span style="background:${bar}"></span><span style="${LINE_STYLE}">${tokenHtml}</span></span>`,
      )
      parts.push(`</div>`)
    }
  }
  parts.push(`</div>`)
  return parts.join('\n')
}

function renderPlain(diff: string): string {
  return `<pre class="text-xs font-mono p-4 whitespace-pre-wrap text-muted-foreground">${escapeH(diff)}</pre>`
}

let isDark = $derived(appTheme === 'dark')
let tokensForRender = $derived(fileTokens)
let parsedFiles = $derived(parsed)

let sideBySideHtml = $derived.by(() => {
  if (!tokenReady) return ''
  return parsedFiles
    .map(f =>
      renderSideBySide(
        f,
        tokensForRender[f.filename] || { oldTokens: [], newTokens: [] },
        isDark,
      ),
    )
    .join('\n')
})

let lineByLineHtml = $derived.by(() => {
  if (!tokenReady) return ''
  return parsedFiles
    .map(f =>
      renderLineByLine(
        f,
        tokensForRender[f.filename] || { oldTokens: [], newTokens: [] },
        isDark,
      ),
    )
    .join('\n')
})

let plainHtml = $derived.by(() => {
  if (!diffText.trim())
    return '<div class="text-xs p-4 text-muted-foreground">No changes</div>'
  return renderPlain(diffText)
})
</script>

{#if !tokenReady}
    <div class="diff-view w-full overflow-x-auto">{@html plainHtml}</div>
{:else}
    <div class="diff-view w-full overflow-x-auto block lg:hidden">{@html lineByLineHtml}</div>
    <div class="diff-view w-full overflow-x-auto hidden lg:block">{@html sideBySideHtml}</div>
{/if}
