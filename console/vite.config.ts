import adapter from '@sveltejs/adapter-static';
import { sveltekit } from '@sveltejs/kit/vite';
import tailwindcss from '@tailwindcss/vite';
import { defineConfig } from 'vite';

// The console is a static SPA embedded in the ck binary (docs/CONSOLE_REQUIREMENTS.md D8).
// In dev, API calls go to a running `ck head` (CK_DASH = its dashboard port, default 7702).
const head = `http://127.0.0.1:${process.env.CK_DASH ?? 7702}`;

export default defineConfig({
	plugins: [
		tailwindcss(),
		sveltekit({
			compilerOptions: {
				runes: ({ filename }) => (filename.split(/[/\\]/).includes('node_modules') ? undefined : true)
			},
			adapter: adapter({ pages: 'build', assets: 'build', fallback: 'index.html', strict: true })
		})
	],
	server: { proxy: { '/api': head, '/stats': head } }
});
