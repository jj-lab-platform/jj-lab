<script lang="ts">
  import { onMount } from 'svelte'
  import { highlightTokens, type ThemedToken } from '$lib/highlight'

  let {
    code,
    filepath,
    blameLines = [] as string[],
    onBlameClick = undefined as ((commit: string | null) => void) | undefined,
  }: {
    code: string
    filepath: string
    blameLines?: string[]
    onBlameClick?: (commit: string | null) => void
  } = $props()

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

  const theme = $derived(
    appTheme === 'dark' ? ('dark-plus' as const) : ('github-light' as const),
  )

  const lines = $derived(code.split('\n'))

  let tokenRows = $state<ThemedToken[][]>([])

  function tokensToHtml(tokens: ThemedToken[]): string {
    return tokens
      .map(t => {
        const s: string[] = [`color:${t.color}`]
        if (t.fontStyle === 1) s.push('font-style:italic')
        if (t.fontStyle === 2) s.push('font-weight:bold')
        const esc = t.content
          .replace(/&/g, '&amp;')
          .replace(/</g, '&lt;')
          .replace(/>/g, '&gt;')
        return `<span style="${s.join(';')}">${esc}</span>`
      })
      .join('')
  }

  $effect(() => {
    const c = code
    const fp = filepath
    if (!c || !fp) {
      tokenRows = []
      return
    }
    let cancelled = false
    highlightTokens(c, fp, theme).then(tokens => {
      if (!cancelled) tokenRows = tokens
    })
    return () => {
      cancelled = true
    }
  })

  // Parse a blame line "commitHash content" into [hash, content].
  function blameParts(line: string): [string, string] {
    const i = line.indexOf(' ')
    if (i === -1) return [line, '']
    return [line.slice(0, i), line.slice(i + 1)]
  }

  const hasBlame = $derived(blameLines.length > 0)
</script>

<div class="h-full overflow-auto">
    <table class="w-full font-mono text-xs leading-relaxed">
        <tbody>
            {#each lines as line, i}
                <tr class="group">
                    <td class="w-12 shrink-0 text-right pr-2 select-none border-r border-border/50 align-top sticky left-0 bg-background text-muted-foreground/60 whitespace-pre-wrap break-words">{i + 1}</td>
                    <td class="min-w-0 whitespace-pre-wrap break-words align-top pl-2">{@html tokensToHtml(tokenRows[i] ?? [])}</td>
                    {#if hasBlame}
                      {@const [hash, _content] = blameParts(blameLines[i] ?? '')}
                      <td class="w-24 shrink-0 pl-3 pr-2 align-top text-right text-muted-foreground/70 opacity-0 group-hover:opacity-100 transition-opacity">
                        {#if hash && onBlameClick}
                          <button class="cursor-pointer hover:text-primary hover:underline font-mono text-[10px]" onclick={() => onBlameClick(hash)}>{hash.slice(0, 8)}</button>
                        {:else if hash}
                          <span class="font-mono text-[10px]">{hash.slice(0, 8)}</span>
                        {/if}
                      </td>
                    {/if}
                </tr>
            {/each}
        </tbody>
    </table>
</div>
