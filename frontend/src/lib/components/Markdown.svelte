<script lang="ts">
  import { marked } from 'marked'
  import { cn } from '$lib/utils'

  let { content, class: cls }: { content: string; class?: string } = $props()

  let html = $derived.by(() => {
    try {
      return marked.parse(content, { async: false }) as string
    } catch {
      return content
    }
  })
</script>

<div class={cn('prose prose-sm max-w-none dark:prose-invert', cls)}>
  {@html html}
</div>
