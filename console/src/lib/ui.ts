// Shared class strings (tokens only) so buttons and fields look the same everywhere.
const btn =
	'inline-flex items-center justify-center gap-2 whitespace-nowrap rounded-lg px-3.5 py-2 text-[13px] font-medium transition-colors disabled:pointer-events-none disabled:opacity-50';
export const btnPrimary = `${btn} bg-primary text-primary-foreground hover:opacity-90`;
export const btnGhost = `${btn} border border-line bg-surface text-foreground hover:bg-raised`;
export const btnDanger = `${btn} bg-bad text-primary-foreground hover:opacity-90`;
// 16px on touch screens so iOS does not zoom the page on focus.
export const field =
	'bg-surface border-line text-foreground placeholder:text-muted-foreground w-full rounded-lg border px-3 py-2 text-[16px] md:text-[13px] focus-visible:border-primary';
export const card = 'bg-surface border-line rounded-lg border';
