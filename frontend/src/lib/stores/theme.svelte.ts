import { themeData } from './themes-data'
import { computeGiteaTokens, type GiteaTokens } from './gitea-tokens'

const STORAGE_KEY_THEME = 'jjlab-theme-id'
const STORAGE_KEY_APPEARANCE = 'jjlab-appearance'

const STYLE_ID = 'jjlab-theme-dynamic'

export const ALL_THEMES = Object.keys(themeData).sort()

function getStyleEl(): HTMLStyleElement {
  let el = document.getElementById(STYLE_ID) as HTMLStyleElement | null
  if (!el) {
    el = document.createElement('style')
    el.id = STYLE_ID
    document.head.appendChild(el)
  }
  return el
}

// Gitea-flavored surface ramp, fed to shadcn's `--color-*` contract and the
// app's own `--*` surfaces. `--radius` matches Gitea's `--border-radius: 4px`.
function themeToCSS(t: GiteaTokens, radius: string): string {
  const cssVars: Record<string, string> = {
    // Gitea design tokens (gitea-* + rust-* wrappers).
    'gitea-body': t.body,
    'gitea-box-body': t.boxBody,
    'gitea-box-header': t.boxHeader,
    'gitea-menu': t.menu,
    'gitea-nav-bg': t.navBg,
    'gitea-footer': t.footer,
    'gitea-text': t.text,
    'gitea-text-dark': t.textDark,
    'gitea-text-light': t.textLight,
    'gitea-text-light-1': t.textLight1,
    'gitea-text-light-2': t.textLight2,
    'gitea-text-light-3': t.textLight3,
    'gitea-secondary': t.secondary,
    'gitea-secondary-button': t.secondaryButton,
    'gitea-light-border': t.lightBorder,
    'gitea-button': t.button,
    'gitea-input-bg': t.inputBg,
    'gitea-input-border': t.inputBorder,
    'gitea-hover': t.hover,
    'gitea-hover-opaque': t.hoverOpaque,
    'gitea-active': t.active,
    'gitea-card': t.card,
    'gitea-overlay-backdrop': t.overlayBackdrop,

    // shadcn-svelte contract (keeps the 35-theme switcher working).
    background: t.body,
    foreground: t.text,
    card: t.boxBody,
    'card-foreground': t.text,
    popover: t.menu,
    'popover-foreground': t.text,
    primary: t.primary,
    'primary-foreground': t.primaryContrast,
    secondary: t.secondary,
    'secondary-foreground': t.text,
    muted: t.boxHeader,
    'muted-foreground': t.textLight1,
    accent: t.primary,
    'accent-foreground': t.primaryContrast,
    destructive: t.red,
    'destructive-foreground': '#ffffff',
    border: t.secondary,
    input: t.inputBg,
    ring: t.primary,

    // jjlab application surfaces.
    'header-bg': t.navBg,
    'body-bg': t.body,
    'box-bg': t.boxBody,
    'box-header-bg': t.boxHeader,
    'menu-bg': t.menu,

    radius,
    'radius-sm': 'calc(' + radius + ' * 0.75)',
    'radius-md': 'calc(' + radius + ' * 1.25)',
    'radius-lg': 'calc(' + radius + ' * 1.5)',
  }
  const body = Object.entries(cssVars)
    .map(([k, v]) => `--${k}:${v}`)
    .join(';')
  return `:root{${body}}`
}

const darkOnlyCache = new Map<string, boolean>()

export function isDarkOnlyTheme(themeId: string): boolean {
  const cached = darkOnlyCache.get(themeId)
  if (cached !== undefined) return cached
  const entry = themeData[themeId]
  if (!entry) return false
  const is = !!entry.dark && !!entry.light && entry.dark.background === entry.light.background
  darkOnlyCache.set(themeId, is)
  return is
}

export function applyTheme(themeId: string, mode: 'dark' | 'light') {
  const entry = themeData[themeId]
  if (!entry) return

  const effectiveMode = isDarkOnlyTheme(themeId) ? 'dark' : mode
  const t = computeGiteaTokens(effectiveMode === 'dark', themeId)
  const css = themeToCSS(t, '4px')

  const el = getStyleEl()
  el.textContent = css

  document.documentElement.setAttribute('data-theme', effectiveMode)
  localStorage.setItem(STORAGE_KEY_THEME, themeId)
  localStorage.setItem(STORAGE_KEY_APPEARANCE, mode)
}

export function getSavedTheme(): string {
  return localStorage.getItem(STORAGE_KEY_THEME) || 'opencode'
}

export function getSavedMode(): 'dark' | 'light' {
  const saved = localStorage.getItem(STORAGE_KEY_APPEARANCE)
  return saved === 'light' ? 'light' : 'dark'
}

export function initTheme() {
  const theme = getSavedTheme()
  const mode = getSavedMode()
  applyTheme(theme, mode)
}
