// Keyboard focus over the canvas PCs: arrows jump to the nearest PC in that direction.
export type Dir = 'left' | 'right' | 'up' | 'down';
export type P = { id: string; x: number; y: number };

/** The nearest PC in a direction (within a 45 degree cone first, then anywhere ahead); the current one if none. */
export function moveFocus(pcs: P[], cur: string | null, dir: Dir): string | null {
	if (!pcs.length) return null;
	const c = pcs.find((p) => p.id === cur);
	if (!c) return pcs[0]!.id;
	const horiz = dir === 'left' || dir === 'right';
	const sign = dir === 'right' || dir === 'down' ? 1 : -1;
	let best: P | null = null, bestScore = Infinity;
	for (const p of pcs) {
		if (p === c) continue;
		const along = ((horiz ? p.x - c.x : p.y - c.y)) * sign;
		const across = Math.abs(horiz ? p.y - c.y : p.x - c.x);
		if (along <= 0) continue;
		const score = Math.hypot(along, across * 2); // sideways costs double: stay in the row/column
		if (score < bestScore) (best = p), (bestScore = score);
	}
	return best?.id ?? c.id;
}
