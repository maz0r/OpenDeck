<script lang="ts">
	export let icon: string;
	export let name: string;
	export let subtitle: string;
	export let hidden: boolean = false;
	export let action: () => void;
</script>

<div
	class="flex flex-row items-center m-2 p-2 bg-neutral-200 dark:bg-neutral-700 rounded-md"
	class:hidden
>
	<img src={icon} class="w-24 h-24 rounded-md" alt={name} loading="lazy" />
	<div class="ml-4 mr-2 dark:text-neutral-300 [overflow-wrap:anywhere]">
		<p class="font-semibold">{name}</p> {subtitle}
	</div>

	<button
		class="ml-auto mr-4"
		on:click={action}
	>
		<slot />
	</button>
</div>
