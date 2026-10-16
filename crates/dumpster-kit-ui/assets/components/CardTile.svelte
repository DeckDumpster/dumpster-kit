<!--
	CardTile — the one browse tile every dumpster site renders a card with.

	  art      the card image in a fixed 5:7 box, outlined in its rarity tier's
	           colour, with the price layered over its bottom corner and — for a
	           foil printing — the spectrum wash and the travelling streak
	  footer   ONE line, fixed positions: collector number at the left, one pip
	           per printing at the right. Never wraps; when the tile is narrow
	           the number truncates and the pips shrink together, so 2 columns
	           and 8 columns lay out the same way.

	There are no view modes. The only divergence a site gets is an explicit
	prop: `rotate` turns a landscape image (bases, leaders) upright inside the
	portrait box; `missing` greys a card the owner has none of.

	A pip is `{ id, label, color, owned }`. `color` is per-variant DATA (the
	`variants` table) and arrives as `--c`; everything else is a semantic token.
	The art button and the pips are siblings, never nested.
-->
<script lang="ts">
	type Pip = { id: string; label: string; color?: string; owned: number };

	type Props = {
		name: string;
		image?: string | null;
		number?: string | number | null;
		rarity?: 'common' | 'uncommon' | 'rare' | 'promo' | 'holo' | 'ultra' | 'special' | 'secret' | 'unranked';
		foil?: boolean;
		price?: string | null;
		pips?: Pip[];
		rotate?: boolean;
		missing?: boolean;
		class?: string;
		onselect?: () => void;
		onpip?: (pip: Pip) => void;
	};

	let {
		name,
		image = null,
		number = null,
		rarity = 'unranked',
		foil = false,
		price = null,
		pips = [],
		rotate = false,
		missing = false,
		class: extra = '',
		onselect,
		onpip
	}: Props = $props();

	const classes = $derived(
		['card-tile', `r-${rarity}`, foil && 'foil', rotate && 'rotate', missing && 'missing', extra]
			.filter(Boolean)
			.join(' ')
	);
</script>

<div class={classes}>
	<button type="button" class="art" aria-label={name} onclick={onselect}>
		{#if image}
			<img src={image} alt={name} loading="lazy" />
		{:else}
			<span class="noart">{name}</span>
		{/if}
		{#if price}<span class="price">{price}</span>{/if}
	</button>
	<div class="footer">
		<span class="num">{number != null && number !== '' ? `#${number}` : ''}</span>
		<span class="pips">
			{#each pips as pip (pip.id)}
				<button
					type="button"
					class="pip"
					class:owned={pip.owned > 0}
					style:--c={pip.color}
					title="{pip.label}{pip.owned > 0 ? ` ×${pip.owned}` : ''} — click to add"
					aria-label="Add one {pip.label}{pip.owned > 0 ? ` (own ${pip.owned})` : ''}"
					onclick={() => onpip?.(pip)}
				>
					{#if pip.owned > 1}<span class="count">{pip.owned}</span>{/if}
				</button>
			{/each}
		</span>
	</div>
</div>

<style>
	.card-tile {
		--tile-rarity: var(--color-rarity-unranked);
		container-type: inline-size;
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
		min-width: 0;
	}
	.r-common { --tile-rarity: var(--color-rarity-common); }
	.r-uncommon { --tile-rarity: var(--color-rarity-uncommon); }
	.r-rare { --tile-rarity: var(--color-rarity-rare); }
	.r-promo { --tile-rarity: var(--color-rarity-promo); }
	.r-holo { --tile-rarity: var(--color-rarity-holo); }
	.r-ultra { --tile-rarity: var(--color-rarity-ultra); }
	.r-special { --tile-rarity: var(--color-rarity-special); }
	.r-secret { --tile-rarity: var(--color-rarity-secret); }
	.r-unranked { --tile-rarity: var(--color-rarity-unranked); }

	/* --- art -------------------------------------------------------------- */
	.art {
		position: relative;
		display: block;
		width: 100%;
		aspect-ratio: 5 / 7;
		padding: var(--space-0);
		overflow: hidden;
		background: var(--color-surface-well);
		border: 2px solid var(--tile-rarity);
		border-radius: var(--radius-lg);
		color: var(--color-text-subtle);
		cursor: pointer;
	}
	.art img {
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
		object-fit: cover;
		display: block;
	}
	.noart {
		position: absolute;
		inset: 0;
		display: flex;
		align-items: center;
		justify-content: center;
		padding: var(--space-2);
		font-size: var(--text-sm);
		text-align: center;
	}

	/* A landscape image (base, leader) is turned a quarter turn so it fills the
	   same portrait box: its pre-turn width is the box's height (7/5 of the
	   width), centred, then rotated. */
	.rotate .art img {
		inset: auto;
		top: 50%;
		left: 50%;
		width: 140%;
		height: auto;
		max-width: none;
		transform: translate(-50%, -50%) rotate(90deg);
	}

	.missing .art {
		opacity: 0.82;
	}
	.missing .art img,
	.missing .noart {
		filter: grayscale(0.9) brightness(0.62);
	}

	/* --- price, layered over the art -------------------------------------- */
	.price {
		position: absolute;
		right: var(--space-1);
		bottom: var(--space-1);
		z-index: 3;
		max-width: calc(100% - var(--space-2));
		padding: var(--space-px) var(--space-1);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		border-radius: var(--radius-xs);
		background: var(--color-scrim);
		color: var(--color-text-strong);
		font-size: var(--text-xs);
		font-weight: var(--weight-semibold);
		font-variant-numeric: tabular-nums;
		line-height: var(--leading-tight);
	}

	/* --- foil: the spectrum wash and the travelling streak ---------------- */
	.foil .art::before,
	.foil .art::after {
		content: '';
		position: absolute;
		inset: 0;
		pointer-events: none;
	}
	.foil .art::before {
		z-index: 1;
		background: var(--gradient-foil-spectrum);
		mix-blend-mode: color;
	}
	.foil .art::after {
		z-index: 2;
		background: var(--gradient-foil-streak) 100% 100% / 240% 240%;
		animation: foil-streak 3s ease-in-out infinite;
	}
	@keyframes foil-streak {
		0% {
			background-position: 100% 100%;
		}
		15%,
		100% {
			background-position: 0 0;
		}
	}
	@media (prefers-reduced-motion: reduce) {
		.foil .art::after {
			animation: none;
		}
	}

	/* --- footer: one line, fixed positions -------------------------------- */
	.footer {
		display: flex;
		flex-wrap: nowrap;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-1);
		min-width: 0;
		overflow: hidden;
		font-size: var(--text-sm);
		line-height: var(--leading-tight);
		color: var(--color-text-subtle);
	}
	.num {
		min-width: 0;
		flex: 1 1 auto;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-variant-numeric: tabular-nums;
	}
	.missing .num {
		color: var(--color-text-subtle);
	}
	.pips {
		display: flex;
		flex-wrap: nowrap;
		align-items: center;
		justify-content: flex-end;
		gap: var(--space-0-5);
		flex: 0 1 auto;
		min-width: 0;
	}
	/* Round and equal on every tile; shrink together (never wrap) when the
	   tile is too narrow to hold them at full size. */
	.pip {
		flex: 0 1 1.25rem;
		min-width: 0;
		width: 1.25rem;
		aspect-ratio: 1;
		box-sizing: border-box;
		border-radius: var(--radius-round);
		border: 2px solid var(--c, var(--color-chart-unknown));
		background: transparent;
		color: var(--color-text-inverse);
		padding: var(--space-0);
		cursor: pointer;
		font: inherit;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		overflow: hidden;
		transition: transform var(--dur-fast) var(--ease-standard);
	}
	.pip.owned {
		background: var(--c, var(--color-chart-unknown));
	}
	.pip:hover {
		transform: scale(1.15);
	}
	.count {
		font-size: var(--text-xs);
		font-weight: var(--weight-bold);
		line-height: var(--leading-none);
		text-shadow: var(--shadow-text-halo);
	}
</style>
