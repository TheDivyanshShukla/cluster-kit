// After `vite build`: write a .gz beside every text asset so the head (which embeds console/build) can send it compressed.
// Bun's built-in zlib, no extra package. Only kept where it saves bytes.
import { readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

const TEXT = /\.(js|css|html|svg|json|txt)$/;
let raw = 0,
	gz = 0;
function walk(d: string) {
	for (const n of readdirSync(d)) {
		const p = join(d, n);
		if (statSync(p).isDirectory()) walk(p);
		else if (TEXT.test(n) && statSync(p).size >= 1024) {
			const b = readFileSync(p);
			const z = Bun.gzipSync(b, { level: 9 });
			if (z.length < b.length * 0.9) {
				writeFileSync(p + '.gz', z);
				raw += b.length;
				gz += z.length;
			}
		}
	}
}
walk('build');
console.log(`gzip: ${(raw / 1024).toFixed(0)} KB -> ${(gz / 1024).toFixed(0)} KB`);
