// Who is signed in. GET /api/v1/me decides what the layout shows: bootstrap, login, or the app.
import { api, ApiError, setUnauthorizedHandler } from './api/client';
import type { Me, Role } from './api/schema';

export const session = $state({ me: null as Me | null, loading: true, error: null as string | null });

/** The signed-in role. Open mode (CK_AUTH=off) makes everyone an admin. */
export const roleNow = (): Role | null => session.me?.user?.role ?? (session.me?.mode === 'open' ? 'admin' : null);
export const userNow = (): string | null => session.me?.user?.name ?? null;
export const authed = (): boolean => roleNow() !== null;

export async function loadMe() {
	try {
		session.me = await api.me();
		session.error = null;
	} catch (e) {
		session.error = e instanceof ApiError ? e.message : 'Something went wrong reading the head.';
	}
	session.loading = false;
}

// Any 401 from a data call means the cookie expired: ask again, which shows the login form.
setUnauthorizedHandler(() => void loadMe());

export async function signIn(name: string, password: string, bootstrap: boolean) {
	await (bootstrap ? api.bootstrap(name, password) : api.login(name, password));
	await loadMe();
}

export async function signOut() {
	await api.logout().catch(() => {});
	await loadMe();
}
