<script lang="ts">
  import { DropdownMenu } from '$lib/components/ui/dropdown-menu'
  import * as Input from '$lib/components/ui/input'
  import { KeyRound } from '@lucide/svelte'
  import { getToken, setToken } from '$lib/api'
</script>

<DropdownMenu.Root>
  <DropdownMenu.Trigger>
    {#snippet child({ props })}
      <button {...props} class="g-nav-item" title="Access token">
        <KeyRound class="size-4" />
      </button>
    {/snippet}
  </DropdownMenu.Trigger>
  <DropdownMenu.Content align="end">
    <DropdownMenu.Label>Access token</DropdownMenu.Label>
    <div class="px-2 py-1">
      <Input.Root
        id="tok"
        type="password"
        value={getToken() ?? ''}
        oninput={(e) => setToken((e.target as HTMLInputElement).value || null)}
      />
    </div>
    <DropdownMenu.Label class="text-[11px] font-normal text-muted-foreground">
      Stored in localStorage; sent as <code>Authorization: token</code>.
    </DropdownMenu.Label>
  </DropdownMenu.Content>
</DropdownMenu.Root>