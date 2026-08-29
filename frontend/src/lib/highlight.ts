import type { ThemedToken } from 'shiki'
import { type BundledLanguage, createHighlighter } from 'shiki'

export type { ThemedToken }

type ShikiHighlighter = Awaited<ReturnType<typeof createHighlighter>>
type Lang = BundledLanguage | 'text'

let singleton: ShikiHighlighter | null = null
let initPromise: Promise<ShikiHighlighter> | null = null

const PRELOAD_LANGS: BundledLanguage[] = [
  'typescript',
  'javascript',
  'python',
  'json',
  'yaml',
  'bash',
  'html',
  'css',
  'markdown',
  'shellscript',
  'diff',
]

const extMap: Record<string, BundledLanguage> = {
  ts: 'typescript',
  tsx: 'typescript',
  js: 'javascript',
  jsx: 'javascript',
  mjs: 'javascript',
  cjs: 'javascript',
  py: 'python',
  pyi: 'python',
  rs: 'rust',
  go: 'go',
  json: 'json',
  jsonc: 'json',
  yaml: 'yaml',
  yml: 'yaml',
  toml: 'toml',
  html: 'html',
  htm: 'html',
  css: 'css',
  scss: 'scss',
  md: 'markdown',
  mdx: 'markdown',
  sql: 'sql',
  sh: 'shellscript',
  bash: 'shellscript',
  zsh: 'shellscript',
  fish: 'shellscript',
  Makefile: 'make',
  makefile: 'make',
  Dockerfile: 'dockerfile',
  xml: 'xml',
  svg: 'xml',
  vue: 'vue',
  svelte: 'svelte',
  c: 'c',
  h: 'c',
  cpp: 'cpp',
  cxx: 'cpp',
  cc: 'cpp',
  hpp: 'cpp',
  java: 'java',
  kt: 'kotlin',
  kts: 'kotlin',
  swift: 'swift',
  rb: 'ruby',
  lua: 'lua',
  ex: 'elixir',
  exs: 'elixir',
  hs: 'haskell',
  zig: 'zig',
  nix: 'nix',
  tf: 'hcl',
  hcl: 'hcl',
  diff: 'diff',
  patch: 'diff',
}

async function getHighlighter(): Promise<ShikiHighlighter> {
  if (singleton) return singleton
  if (initPromise) return initPromise
  initPromise = createHighlighter({
    themes: ['dark-plus', 'github-light'],
    langs: PRELOAD_LANGS,
  })
  singleton = await initPromise
  return singleton
}

const loadedLangs = new Set<string>(PRELOAD_LANGS)

async function ensureLang(lang: Lang): Promise<void> {
  if (lang === 'text' || loadedLangs.has(lang)) return
  const h = await getHighlighter()
  await h.loadLanguage(lang)
  loadedLangs.add(lang)
}

export function preloadHighlighter(): void {
  getHighlighter()
}

function detectLang(filepath: string): Lang {
  const name = filepath.split('/').pop() || ''
  const basename = name.toLowerCase()
  if (basename === 'makefile') return 'make'
  if (basename.startsWith('dockerfile')) return 'dockerfile'
  if (basename === '.gitignore' || basename === '.env') return 'shellscript'
  const parts = basename.includes('.') ? basename.split('.') : []
  const ext = parts.length > 0 ? parts[parts.length - 1] : ''
  return extMap[ext] || 'text'
}

export async function highlightCode(
  code: string,
  filepath: string,
  theme: 'dark-plus' | 'github-light' = 'dark-plus',
): Promise<string> {
  if (!code) return ''
  const lang = detectLang(filepath)
  await ensureLang(lang)
  const h = await getHighlighter()
  try {
    return h.codeToHtml(code, { lang, theme })
  } catch {
    return h.codeToHtml(code, { lang: 'text', theme })
  }
}

export async function highlightTokens(
  code: string,
  filepath: string,
  theme: 'dark-plus' | 'github-light' = 'dark-plus',
): Promise<ThemedToken[][]> {
  if (!code) return []
  const lang = detectLang(filepath)
  await ensureLang(lang)
  const h = await getHighlighter()
  try {
    const result = h.codeToTokens(code, { lang, theme })
    return result.tokens
  } catch {
    const result = h.codeToTokens(code, { lang: 'text', theme })
    return result.tokens
  }
}
