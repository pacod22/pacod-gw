#!/usr/bin/env node
// Latency of the token endpoint and of the authorization feed, against the development stack
// (make compose-up). Prints the percentiles; exits 1 when a target is missed:
//
//   token endpoint   p99 under 50 ms, password hashing excluded (refresh and client
//                    credentials grants; a sign-in's Argon2 is not part of them)
//   feed             a scope registered by a product reaches a waiting follower, p99 under 1 s
//
//   node scripts/measure.mjs [--rounds 500] [--workers 8]
//
// Secrets come from deploy/compose/.env and are never printed.

import { createHash, randomBytes } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { parseArgs } from 'node:util';

const { values: args } = parseArgs({
	options: {
		rounds: { type: 'string', default: '500' },
		workers: { type: 'string', default: '8' },
		issuer: { type: 'string', default: 'http://127.0.0.1:26070' },
		internal: { type: 'string', default: 'http://127.0.0.1:26071' },
		env: { type: 'string', default: new URL('../deploy/compose/.env', import.meta.url).pathname }
	}
});
const ROUNDS = Number(args.rounds);
const WORKERS = Number(args.workers);
const ISSUER = args.issuer;
const INTERNAL = args.internal;

const env = Object.fromEntries(
	readFileSync(args.env, 'utf8')
		.split('\n')
		.filter((l) => /^[A-Z_]+=/.test(l))
		.map((l) => [l.slice(0, l.indexOf('=')), l.slice(l.indexOf('=') + 1)])
);
const APP = {
	id: 'dev-app',
	secret: env.PID_DEV_APP_SECRET,
	redirect: 'http://127.0.0.1:26099/callback'
};
const SERVICE = { id: 'dev-service', secret: env.PID_DEV_SERVICE_SECRET };
const ADMIN = { email: 'admin@pacod-id.dev.test', password: env.PID_BOOTSTRAP_ADMIN_PASSWORD };

const basic = (c) => 'Basic ' + Buffer.from(`${c.id}:${c.secret}`).toString('base64');
const b64url = (b) => b.toString('base64url');

function percentiles(ms) {
	const s = [...ms].sort((a, b) => a - b);
	const at = (p) => s[Math.min(s.length - 1, Math.ceil((p / 100) * s.length) - 1)];
	return { n: s.length, p50: at(50), p95: at(95), p99: at(99), max: s[s.length - 1] };
}

function show(name, p) {
	const f = (v) => v.toFixed(1).padStart(7);
	console.log(
		`${name.padEnd(44)} n=${String(p.n).padStart(5)}  p50${f(p.p50)}  p95${f(p.p95)}  p99${f(p.p99)}  max${f(p.max)} ms`
	);
}

async function timed(fn) {
	const t = performance.now();
	const r = await fn();
	return [performance.now() - t, r];
}

async function ok(r, what) {
	if (!r.ok) throw new Error(`${what}: ${r.status} ${await r.text()}`);
	return r;
}

/** A sign-in through the API the hosted pages use: authorize, the password step, the code. */
async function signIn() {
	const verifier = b64url(randomBytes(32));
	const challenge = b64url(createHash('sha256').update(verifier).digest());
	const u = new URL('/oidc/authorize', ISSUER);
	u.search = new URLSearchParams({
		response_type: 'code',
		client_id: APP.id,
		redirect_uri: APP.redirect,
		scope: 'openid email',
		state: b64url(randomBytes(8)),
		code_challenge: challenge,
		code_challenge_method: 'S256'
	}).toString();
	const a = await fetch(u, { redirect: 'manual' });
	const cookie = a.headers
		.getSetCookie()
		.map((c) => c.split(';')[0])
		.join('; ');
	const flow = new URL(a.headers.get('location'), ISSUER).searchParams.get('flow');
	if (!flow) throw new Error(`authorize: no flow (${a.status} ${a.headers.get('location')})`);
	const p = await ok(
		await fetch(new URL(`/ui-api/v1/flows/${flow}/password`, ISSUER), {
			method: 'POST',
			headers: { 'content-type': 'application/json', origin: ISSUER, cookie },
			body: JSON.stringify({ email: ADMIN.email, password: ADMIN.password })
		}),
		'password step'
	);
	const step = await p.json();
	if (step.next !== 'redirect') throw new Error(`password step: ${JSON.stringify(step)}`);
	const code = new URL(step.to).searchParams.get('code');
	const t = await ok(
		await fetch(new URL('/oidc/token', ISSUER), {
			method: 'POST',
			headers: { authorization: basic(APP), 'content-type': 'application/x-www-form-urlencoded' },
			body: new URLSearchParams({
				grant_type: 'authorization_code',
				code,
				redirect_uri: APP.redirect,
				code_verifier: verifier
			})
		}),
		'code grant'
	);
	return t.json();
}

/** One refresh chain: each round redeems the newest refresh token. */
async function refreshChain(rounds) {
	let { refresh_token: rt } = await signIn();
	const times = [];
	for (let i = 0; i < rounds; i++) {
		const [ms, r] = await timed(() =>
			fetch(new URL('/oidc/token', ISSUER), {
				method: 'POST',
				headers: { authorization: basic(APP), 'content-type': 'application/x-www-form-urlencoded' },
				body: new URLSearchParams({ grant_type: 'refresh_token', refresh_token: rt })
			})
		);
		const body = await (await ok(r, 'refresh grant')).json();
		rt = body.refresh_token;
		times.push(ms);
	}
	return times;
}

async function serviceToken() {
	const r = await ok(
		await fetch(new URL('/oidc/token', INTERNAL), {
			method: 'POST',
			headers: {
				authorization: basic(SERVICE),
				'content-type': 'application/x-www-form-urlencoded'
			},
			body: new URLSearchParams({ grant_type: 'client_credentials' })
		}),
		'client credentials'
	);
	return r;
}

async function clientCredentials(rounds) {
	const times = [];
	for (let i = 0; i < rounds; i++) {
		const [ms, r] = await timed(serviceToken);
		await r.json();
		times.push(ms);
	}
	return times;
}

async function parallel(n, fn) {
	return (await Promise.all(Array.from({ length: n }, () => fn()))).flat();
}

/** A scope put by the product, seen by a follower already waiting on the feed. */
async function feed(rounds) {
	const token = (await (await serviceToken()).json()).access_token;
	const call = (method, path, body) =>
		fetch(new URL(path, INTERNAL), {
			method,
			headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' },
			body: body && JSON.stringify(body)
		});
	await ok(
		await call('PUT', '/internal/v1/catalog/dev', {
			manifest: {
				product: 'dev',
				scope_types: { thing: { parent: 'platform' } },
				roles: { viewer: { at: ['platform', 'dev.thing'], permissions: ['things.read'] } },
				grant: { default: 'things.read' }
			}
		}),
		'catalogue push'
	);
	const head = async () =>
		(
			await (
				await ok(await call('GET', '/internal/v1/authz/snapshot?product=dev'), 'snapshot')
			).json()
		).seq;
	let after = await head();
	const run = Date.now().toString(36);
	const times = [];
	for (let i = 0; i < rounds; i++) {
		const waiting = call('GET', `/internal/v1/authz/changes?product=dev&after=${after}&wait=10`);
		// The follower's request is on its way before the change; give it a moment to park.
		await new Promise((r) => setTimeout(r, 5));
		const t = performance.now();
		await ok(
			await call('PUT', `/internal/v1/scopes/dev.thing/m-${run}-${i}`, { parent: 'platform' }),
			'scope put'
		);
		const seen = await (await ok(await waiting, 'changes')).json();
		const ms = performance.now() - t;
		if (!seen.events.length) throw new Error(`feed: no event within the wait (after ${after})`);
		after = seen.seq;
		times.push(ms);
	}
	return times;
}

const WARMUP = 50;
const results = [];

await clientCredentials(WARMUP);
const cc = percentiles(await clientCredentials(ROUNDS));
show('client_credentials, one caller', cc);
results.push(['client_credentials, one caller', cc, 50]);
const ccp = percentiles(
	await parallel(WORKERS, () => clientCredentials(Math.ceil(ROUNDS / WORKERS)))
);
show(`client_credentials, ${WORKERS} callers`, ccp);

const rt = percentiles((await refreshChain(ROUNDS + WARMUP)).slice(WARMUP));
show('refresh_token, one caller', rt);
results.push(['refresh_token, one caller', rt, 50]);
const rtp = percentiles(await parallel(WORKERS, () => refreshChain(Math.ceil(ROUNDS / WORKERS))));
show(`refresh_token, ${WORKERS} callers`, rtp);

const fd = percentiles(await feed(Math.min(ROUNDS, 200)));
show('feed: scope put to a waiting follower', fd);
results.push(['feed', fd, 1000]);

const missed = results.filter(([, p, limit]) => p.p99 >= limit);
for (const [name, p, limit] of missed)
	console.error(`MISSED: ${name} p99 ${p.p99.toFixed(1)} ms >= ${limit} ms`);
process.exit(missed.length ? 1 : 0);
