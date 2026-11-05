// Client-side subset of the selector language for the canvas filter: space-separated terms of
// key=a,b | key!=v | key>=N | key | !key, a trailing * is a prefix. Keys: host node lab state job pool cores threads online power + labels.
export type Attrs = Record<string, string | string[] | undefined>;
export type Matcher = (a: Attrs) => boolean;

export function parseSelector(q: string): Matcher | string {
	const terms = q.match(/(?:[^\s"]|"[^"]*")+/g) ?? [];
	const fs: Matcher[] = [];
	for (const t of terms) {
		const m = /^(!?)([a-z_][a-z0-9_.-]*)(?:(!=|>=|<=|=|>|<)(.+))?$/i.exec(t);
		if (!m || (m[1] && m[3])) return `bad term ${t}`;
		const [, neg, key, op, raw] = m;
		const vals = (raw ?? '').replace(/^"|"$/g, '').toLowerCase().split(',');
		fs.push((a) => {
			const v = a[key];
			const list = (Array.isArray(v) ? v : v == null ? [] : [v]).map((x) => x.toLowerCase());
			if (!op) {
				const has = list.some((x) => x !== '' && x !== 'false' && x !== '0');
				return neg ? !has : has;
			}
			// "lab=A" also matches a lab called "Lab A"
			const hit = (s: string) => list.some((x) => (s.endsWith('*') ? x.startsWith(s.slice(0, -1)) : x === s || (key === 'lab' && x === `lab ${s}`)));
			if (op === '=') return vals.some(hit);
			if (op === '!=') return !vals.some(hit);
			const x = Number(list[0]), n = Number(vals[0]);
			return op === '>=' ? x >= n : op === '<=' ? x <= n : op === '>' ? x > n : x < n;
		});
	}
	return (a) => fs.every((f) => f(a));
}
