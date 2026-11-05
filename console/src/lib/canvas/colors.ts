// Theme colours for the canvas, resolved from the console's CSS tokens (the canvas cannot use var()).
export type Tokens = { bg: string; surface: string; raised: string; line: string; fg: string; muted: string; primary: string; ok: string; warn: string; bad: string; idle: string; dark: boolean };

const NAMES: [keyof Omit<Tokens, 'dark'>, string][] = [
	['bg', '--background'], ['surface', '--surface'], ['raised', '--raised'], ['line', '--line'], ['fg', '--foreground'], ['muted', '--muted-foreground'],
	['primary', '--primary'], ['ok', '--ok'], ['warn', '--warn'], ['bad', '--bad'], ['idle', '--idle']
];

/** Custom properties read back unresolved (light-dark(...)), so each is resolved through a probe element's computed colour. */
export function readTokens(host: HTMLElement): Tokens {
	const probe = document.createElement('span');
	probe.style.cssText = 'position:absolute;visibility:hidden;pointer-events:none';
	host.appendChild(probe);
	const t = {} as Tokens;
	for (const [k, v] of NAMES) {
		probe.style.color = `var(${v})`;
		(t as Record<string, string | boolean>)[k] = getComputedStyle(probe).color;
	}
	probe.remove();
	t.dark = getComputedStyle(host).colorScheme.includes('dark') || document.documentElement.dataset.theme === 'dark' || (!document.documentElement.dataset.theme && matchMedia('(prefers-color-scheme: dark)').matches);
	return t;
}

/** Okabe-Ito order (colour-blind safe): orange, sky blue, bluish green, yellow, blue, vermillion, reddish purple. Slot 0 is the console's primary. */
export const OKABE = ['#E69F00', '#56B4E9', '#009E73', '#F0E442', '#0072B2', '#D55E00', '#CC79A7'] as const;
/** Slot 0 is the primary colour, 1..7 the Okabe-Ito hues, -1 (more than 8 jobs) is grey. */
export function jobColor(t: Pick<Tokens, 'primary' | 'idle' | 'dark'>, slot: number): string {
	if (slot < 0) return t.idle;
	if (slot === 0) return t.primary;
	return OKABE[(slot - 1) % OKABE.length];
}
