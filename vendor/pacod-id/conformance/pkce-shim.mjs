// Between Caddy and pacod-id in the conformance setup: adds PKCE where the suite leaves it out.
//
// pacod-id requires PKCE (S256) of every client; the suite's OpenID
// Connect plans never send it. So an authorization request without code_challenge gets a fixed
// one, and a code grant without code_verifier gets the matching verifier. Requests that carry
// their own (the suite's PKCE test) pass untouched; so does everything else. pacod-id's own
// tests cover refusing a request without PKCE (crates/pacod-id/tests).

import { createHash } from 'node:crypto';
import http from 'node:http';

const UPSTREAM = {
	host: process.env.UPSTREAM_HOST ?? 'pacod-id',
	port: Number(process.env.UPSTREAM_PORT ?? 8070)
};
const VERIFIER = 'conformance-shim-verifier-0123456789-abcdefghijklmnopqrstuvwxyz';
const CHALLENGE = createHash('sha256').update(VERIFIER).digest('base64url');

function withPkce(params, grant) {
	if (grant) {
		if (params.get('grant_type') === 'authorization_code' && !params.has('code_verifier')) {
			params.set('code_verifier', VERIFIER);
			return true;
		}
		return false;
	}
	if (!params.has('code_challenge')) {
		params.set('code_challenge', CHALLENGE);
		params.set('code_challenge_method', 'S256');
		return true;
	}
	return false;
}

function forward(req, res, path, body) {
	const headers = { ...req.headers };
	if (body !== undefined) headers['content-length'] = Buffer.byteLength(body);
	const up = http.request({ ...UPSTREAM, method: req.method, path, headers }, (r) => {
		res.writeHead(r.statusCode, r.rawHeaders);
		r.pipe(res);
	});
	up.on('error', () => {
		res.writeHead(502).end();
	});
	if (body !== undefined) up.end(body);
	else req.pipe(up);
}

http
	.createServer((req, res) => {
		const url = new URL(req.url, 'http://shim');
		const authorize = url.pathname === '/oidc/authorize';
		const token = url.pathname === '/oidc/token';
		if (authorize && req.method === 'GET') {
			if (withPkce(url.searchParams, false)) console.log('authorize: added a challenge');
			return forward(req, res, url.pathname + url.search);
		}
		const form = (req.headers['content-type'] ?? '').startsWith(
			'application/x-www-form-urlencoded'
		);
		if ((authorize || token) && req.method === 'POST' && form) {
			const chunks = [];
			req.on('data', (c) => chunks.push(c));
			req.on('end', () => {
				const params = new URLSearchParams(Buffer.concat(chunks).toString());
				if (withPkce(params, token)) console.log(`${token ? 'token' : 'authorize'}: added PKCE`);
				forward(req, res, req.url, params.toString());
			});
			return;
		}
		forward(req, res, req.url);
	})
	.listen(8080, () => console.log('pkce-shim on 8080'));
