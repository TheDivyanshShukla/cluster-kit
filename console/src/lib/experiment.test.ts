import { expect, test } from 'bun:test';
import { analyse, caption, summaryCsv } from './experiment';

const raw = 'config,repeat,n,seconds,hashes_per_s,hits,pcs,policy,chunk,virtual\n' +
	[1, 2, 4].flatMap((p) => [1, 2].map((r) => `${p} PCs,${r},1,1,${1e7 * p * (r === 1 ? 1 : 0.95)},0,${p},adaptive,0,0`)).join('\n');
const runs = '{"config":"1 PCs","commit":"abc","dirty":true,"head":"h:1","kind":"synth","started_ms":10,"finished_ms":20,"pcs":[{"class":"i5"}]}\nnot json\n';

test('analyse builds series, provenance and a caption', () => {
	const a = analyse(raw, runs, [{ policy: 'adaptive', pcs: 1, throughput_per_s: 1e7 }, { policy: 'adaptive', pcs: 4, throughput_per_s: 3.9e7 }]);
	expect(a.measured[0]!.pts.map((p) => p.pcs)).toEqual([1, 2, 4]);
	expect(a.sim[0]!.pts[1]!.S).toBeCloseTo(3.9);
	expect(a.prov).toMatchObject({ runs: 1, commits: ['abc'], dirty: 1, pcClasses: ['i5'], firstMs: 10, lastMs: 20 });
	expect(summaryCsv(a).split('\n')[0]).toContain('karp_flatt');
	expect(caption('demo', 2, a, false)).toContain('1 to 4 PCs');
});
