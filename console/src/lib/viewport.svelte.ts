// Viewport width as state, so a page can drop columns on a phone without CSS tricks.
export const vp = $state({ w: typeof innerWidth === 'number' ? innerWidth : 1280 });
if (typeof window !== 'undefined') addEventListener('resize', () => (vp.w = innerWidth), { passive: true });
export const isPhone = () => vp.w < 640;
