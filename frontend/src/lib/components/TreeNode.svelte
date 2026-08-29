<script lang="ts">
  import { ChevronDown, ChevronRight, File, Folder, FileText } from '@lucide/svelte'
  import TreeNode from './TreeNode.svelte'

  export interface TreeEntry {
    name: string
    path: string
    is_dir: boolean
    size: number
  }

  let {
    entries = [] as TreeEntry[],
    path = '',
    depth = 0,
    ancestorsLast = [] as boolean[],
    expanded = new Set<string>(),
    selectedPath = null as string | null,
    onToggle = (_path: string) => {},
    onOpen = (_path: string) => {},
  }: {
    entries?: TreeEntry[]
    path?: string
    depth?: number
    ancestorsLast?: boolean[]
    expanded?: Set<string>
    selectedPath?: string | null
    onToggle?: (path: string) => void
    onOpen?: (path: string) => void
  } = $props()

  function formatSize(bytes: number): string {
    if (bytes < 1024) return `${bytes}B`
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)}KB`
    return `${(bytes / (1024 * 1024)).toFixed(1)}MB`
  }

  const local = $derived(
    path === ''
      ? entries.filter((e) => !e.path.includes('/'))
      : entries.filter((e) => {
          const idx = e.path.lastIndexOf('/')
          const parent = idx === -1 ? '' : e.path.slice(0, idx)
          return parent === path
        }),
  )

  function isText(p: string): boolean {
    const ext = p.split('.').pop()?.toLowerCase() ?? ''
    const text = ['md','txt','ts','tsx','js','jsx','json','yaml','yml','toml','rs','go','py','java','c','h','cpp','hpp','css','html','sh','bash','svelte','sql','xml','gradle','mjs','cjs','env','gitignore','lock']
    return text.includes(ext) || p.includes('Dockerfile') || p.includes('Makefile')
  }
</script>

{#each local as entry, i (entry.path)}
  {@const isLast = i === local.length - 1}
  {#if entry.is_dir}
    <button
      class="ruco-row flex w-full cursor-pointer items-center gap-1 py-1.5 text-xs hover:bg-[#161b22] transition-colors select-none font-mono text-left"
      style="padding-left: {12 + depth * 16}px; padding-right: 12px;"
      onclick={() => onToggle(entry.path)}
    >
      {#if expanded.has(entry.path)}
        <ChevronDown class="size-3.5 shrink-0 text-muted-foreground" />
      {:else}
        <ChevronRight class="size-3.5 shrink-0 text-muted-foreground" />
      {/if}
      <Folder class="size-4 shrink-0 text-[#54aeff]" />
      <span class="truncate text-foreground">{entry.name}</span>
    </button>
    {#if expanded.has(entry.path)}
      <TreeNode
        {entries}
        path={entry.path}
        depth={depth + 1}
        ancestorsLast={[...ancestorsLast, isLast]}
        {expanded}
        {selectedPath}
        {onToggle}
        {onOpen}
      />
    {/if}
  {:else}
    <button
      class="ruco-row flex w-full cursor-pointer items-center gap-1 py-1.5 text-xs hover:bg-[#161b22] transition-colors select-none font-mono text-left {selectedPath === entry.path ? 'bg-[#1f6feb]/20 text-foreground' : 'text-foreground'}"
      style="padding-left: {12 + depth * 16}px; padding-right: 12px;"
      onclick={() => onOpen(entry.path)}
    >
      <span class="w-3.5 shrink-0"></span>
      {#if isText(entry.path)}
        <FileText class="size-4 shrink-0 text-muted-foreground" />
      {:else}
        <File class="size-4 shrink-0 text-muted-foreground" />
      {/if}
      <span class="truncate flex-1">{entry.name}</span>
      {#if entry.size > 0}
        <span class="shrink-0 text-[10px] text-muted-foreground">{formatSize(entry.size)}</span>
      {/if}
    </button>
  {/if}
{/each}
