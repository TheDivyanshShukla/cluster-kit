// lucide icon geometry (24x24, stroke 2) as Path2D data, so the canvas draws the same Crown / Eye / Moon / Sunrise as the rest of the console.
export const GLYPHS = {
	crown: ['M11.562 3.266a.5.5 0 0 1 .876 0L15.39 8.87a1 1 0 0 0 1.516.294L21.183 5.5a.5.5 0 0 1 .798.519l-2.834 10.246a1 1 0 0 1-.956.734H5.81a1 1 0 0 1-.957-.734L2.02 6.02a.5.5 0 0 1 .798-.519l4.276 3.664a1 1 0 0 0 1.516-.294z', 'M5 21h14'],
	eye: ['M2.062 12.348a1 1 0 0 1 0-.696 10.75 10.75 0 0 1 19.876 0 1 1 0 0 1 0 .696 10.75 10.75 0 0 1-19.876 0', 'M15 12a3 3 0 1 1-6 0 3 3 0 0 1 6 0'],
	moon: ['M20.985 12.486a9 9 0 1 1-9.473-9.472c.405-.022.617.46.402.803a6 6 0 0 0 8.268 8.268c.344-.215.825-.004.803.401'],
	sunrise: ['M12 2v8', 'm4.93 10.93 1.41 1.41', 'M2 18h2', 'M20 18h2', 'm19.07 10.93-1.41 1.41', 'M22 22H2', 'm8 6 4-4 4 4', 'M16 18a4 4 0 0 0-8 0'],
	userPlus: ['M2 21a8 8 0 0 1 13.292-6', 'M19 16v6', 'M22 19h-6', 'M10 13a5 5 0 1 0 0-10 5 5 0 0 0 0 10']
} as const;
export type GlyphName = keyof typeof GLYPHS;

const cache = new Map<GlyphName, Path2D[]>();
/** Stroke a glyph centred at (x, y), `size` world units wide. */
export function drawGlyph(ctx: CanvasRenderingContext2D, name: GlyphName, x: number, y: number, size: number) {
	let paths = cache.get(name);
	if (!paths) cache.set(name, (paths = GLYPHS[name].map((d) => new Path2D(d))));
	const k = size / 24;
	ctx.save();
	ctx.translate(x - size / 2, y - size / 2);
	ctx.scale(k, k);
	ctx.lineWidth = 2;
	ctx.lineCap = ctx.lineJoin = 'round';
	for (const p of paths) ctx.stroke(p);
	ctx.restore();
}
