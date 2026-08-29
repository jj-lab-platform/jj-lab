import { themeData } from './themes-data'

// Gitea's design tokens are a fixed, carefully-separated neutral grey ramp
// (see web_src/css/themes/theme-gitea-dark.css / -light.css). The exact ramp
// is what makes a skin read as "gitea", so we reproduce those neutral values
// verbatim and only inject the 35-palette `primary`/`accent` (plus red/green
// status colors) so the theme switcher still changes the accent identity.
//
// This deliberately ignores each theme's background/foreground for surfaces:
// the whole point is a Gitea-look that stays stable across all 35 themes.

export interface GiteaTokens {
  body: string
  boxBody: string
  boxHeader: string
  menu: string
  navBg: string
  footer: string
  text: string
  textDark: string
  textLight: string
  textLight1: string
  textLight2: string
  textLight3: string
  primary: string
  primaryContrast: string
  primaryHover: string
  primaryActive: string
  secondary: string
  secondaryButton: string
  lightBorder: string
  button: string
  inputBg: string
  inputBorder: string
  hover: string
  hoverOpaque: string
  active: string
  overlayBackdrop: string
  red: string
  green: string
  card: string
  rising: string
}

function hex(s: string): [number, number, number] {
  let h = s.trim()
  if (h.startsWith('#')) h = h.slice(1)
  if (h.length === 3) h = h.split('').map(x => x + x).join('')
  const n = parseInt(h.slice(0, 6), 16)
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255]
}

export function mix(a: string, b: string, t: number): string {
  const pa = hex(a)
  const pb = hex(b)
  const c: [number, number, number] = [
    Math.round(pa[0] + (pb[0] - pa[0]) * t),
    Math.round(pa[1] + (pb[1] - pa[1]) * t),
    Math.round(pa[2] + (pb[2] - pa[2]) * t),
  ]
  return `rgb(${c[0]} ${c[1]} ${c[2]})`
}

function luma(s: string): number {
  const [r, g, b] = hex(s)
  return 0.2126 * r + 0.7152 * g + 0.0722 * b
}

export function contrastText(bg: string): string {
  return luma(bg) > 145 ? '#01050a' : '#ffffff'
}

// Gitea dark neutrals (verbatim from theme-gitea-dark.css).
const DARK: Omit<GiteaTokens, 'primary' | 'primaryContrast' | 'primaryHover' | 'primaryActive'> = {
  body: '#1e1f20',
  boxBody: '#161718',
  boxHeader: '#1b1c1e',
  menu: '#191a1c',
  navBg: '#18191b',
  footer: '#18191b',
  text: '#d2d4d8',
  textDark: '#f8f8f8',
  textLight: '#c0c2c7',
  textLight1: '#aaadb4',
  textLight2: '#969aa1',
  textLight3: '#80858f',
  secondary: '#3f4248',
  secondaryButton: '#5e626a',
  lightBorder: '#f3f3f428',
  button: '#191a1c',
  inputBg: '#191a1c',
  inputBorder: '#3f4248',
  hover: '#f3f3f419',
  hoverOpaque: '#232528',
  active: '#f3f3f424',
  overlayBackdrop: '#080808c0',
  red: '#cc4848',
  green: '#87ab63',
  card: '#191a1c',
  rising: '#232528',
}

// Gitea light neutrals (verbatim from theme-gitea-light.css).
const LIGHT: Omit<GiteaTokens, 'primary' | 'primaryContrast' | 'primaryHover' | 'primaryActive'> = {
  body: '#ffffff',
  boxBody: '#ffffff',
  boxHeader: '#f1f3f5',
  menu: '#f8f9fb',
  navBg: '#f6f7fa',
  footer: '#f6f7fa',
  text: '#181c21',
  textDark: '#01050a',
  textLight: '#4f5861',
  textLight1: '#59636e',
  textLight2: '#6e7781',
  textLight3: '#848d97',
  secondary: '#d0d7de',
  secondaryButton: '#e7ebef',
  lightBorder: '#0000171d',
  button: '#f8f9fb',
  inputBg: '#ffffff',
  inputBorder: '#d0d7de',
  hover: '#00001708',
  hoverOpaque: '#f1f3f5',
  active: '#00001714',
  overlayBackdrop: '#00000080',
  red: '#d1242f',
  green: '#1a7f37',
  card: '#f8f9fb',
  rising: '#eaeef2',
}

export function computeGiteaTokens(isDark: boolean, themeId: string): GiteaTokens {
  const entry = themeData[themeId]
  const base = isDark ? DARK : LIGHT

  let primary = '#4183c4'
  if (entry) {
    const palette = isDark ? entry.dark : entry.light
    if (palette.primary) primary = palette.primary
    else if (palette.accent) primary = palette.accent
  }

  return {
    ...base,
    primary,
    primaryContrast: contrastText(primary),
    primaryHover: mix(primary, '#000000', 0.12),
    primaryActive: mix(primary, '#000000', 0.22),
  }
}