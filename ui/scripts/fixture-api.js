#!/usr/bin/env node
// A stand-in for a core with a working analytics service, for developing the Insight pages.
//
// It passes every request on to a real core and answers only the request log itself, from
// src/lib/fixtures/requests.json, in the shape the analytics service documents for
// /reports/requests (which the core forwards unchanged). A session is still required: the
// fixture is served only to a caller the real core accepts.
//
//   node scripts/fixture-api.js            # listens on 127.0.0.1:28181, core on 127.0.0.1:28081
//   GATEWAY_API_URL=http://127.0.0.1:28181 npm run dev
//
// Development only. It is not part of the built console.
import { readFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { Readable } from 'node:stream';

const core = (process.env.FIXTURE_CORE_URL ?? 'http://127.0.0.1:28081').replace(/\/+$/, '');
const port = Number(process.env.FIXTURE_PORT ?? 28181);
const fixture = JSON.parse(
	readFileSync(new URL('../src/lib/fixtures/requests.json', import.meta.url), 'utf8')
);

function json(response, status, body) {
	response.writeHead(status, { 'content-type': 'application/json' });
	response.end(JSON.stringify(body));
}

async function authorised(request) {
	const check = await fetch(`${core}/api/v1/session`, {
		headers: { authorization: request.headers.authorization ?? '' }
	});
	return check.ok;
}

function page(url) {
	const only = (name, field) => {
		const wanted = url.searchParams.getAll(name);
		return (event) => wanted.length === 0 || wanted.includes(String(event[field]));
	};
	const filters = [
		only('organisation_id', 'organisation_id'),
		only('workspace_id', 'workspace_id'),
		only('status', 'status'),
		only('provider', 'provider'),
		only('model', 'model_served'),
		only('key_id', 'key_id'),
		only('end_user', 'end_user'),
		only('trace_id', 'trace_id'),
		only('session_id', 'session_id')
	];
	const matching = fixture.requests.filter((event) => filters.every((f) => f(event)));
	// Two per page, so that paging can be tried with four events.
	const start = Number(url.searchParams.get('cursor') ?? 0) || 0;
	const requests = matching.slice(start, start + 2);
	const more = start + 2 < matching.length;
	return { ...fixture, requests, next_cursor: more ? String(start + 2) : null };
}

createServer(async (request, response) => {
	const url = new URL(request.url ?? '/', 'http://fixture');
	try {
		if (request.method === 'GET' && url.pathname.startsWith('/api/v1/requests')) {
			if (!(await authorised(request))) {
				return json(response, 401, {
					error: {
						type: 'unauthorized',
						message: 'send a session or admin token as a Bearer token'
					}
				});
			}
			const id = decodeURIComponent(
				url.pathname.slice('/api/v1/requests'.length).replace(/^\//, '')
			);
			if (!id) return json(response, 200, page(url));
			const event = fixture.requests.find((e) => e.id === id);
			return event
				? json(response, 200, event)
				: json(response, 404, { error: { type: 'not_found', message: 'request not found' } });
		}

		const headers = new Headers();
		for (const [name, value] of Object.entries(request.headers)) {
			if (typeof value === 'string' && name !== 'host' && name !== 'connection')
				headers.set(name, value);
		}
		const hasBody = request.method !== 'GET' && request.method !== 'HEAD';
		const upstream = await fetch(core + url.pathname + url.search, {
			method: request.method,
			headers,
			body: hasBody ? Readable.toWeb(request) : undefined,
			duplex: hasBody ? 'half' : undefined
		});
		const out = {};
		upstream.headers.forEach((value, name) => {
			if (
				name !== 'content-encoding' &&
				name !== 'content-length' &&
				name !== 'transfer-encoding'
			) {
				out[name] = value;
			}
		});
		response.writeHead(upstream.status, out);
		// An event stream may stay silent for a while: send the headers now.
		response.flushHeaders();
		if (upstream.body) Readable.fromWeb(upstream.body).pipe(response);
		else response.end();
	} catch (error) {
		json(response, 502, { error: { type: 'unreachable', message: String(error) } });
	}
}).listen(port, '127.0.0.1', () => {
	console.log(`fixture API on http://127.0.0.1:${port}, passing on to ${core}`);
});
