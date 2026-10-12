import { describe, expect, it } from 'vitest';
import { createLocalJWKSet, exportJWK, generateKeyPair, SignJWT } from 'jose';
import { SingleFlight } from './lock.js';
import { safeNext } from './client.js';
import { TokenError, verifyLogoutToken } from './tokens.js';

describe('SingleFlight', () => {
	it('runs once for parallel callers and hands later ones the same result', async () => {
		const flight = new SingleFlight<number>(1000);
		let calls = 0;
		const slow = async () => {
			calls++;
			await new Promise((r) => setTimeout(r, 20));
			return calls;
		};
		const results = await Promise.all(Array.from({ length: 10 }, () => flight.run('k', slow)));
		expect(results).toEqual(Array(10).fill(1));
		expect(await flight.run('k', slow)).toBe(1);
		expect(await flight.run('other', slow)).toBe(2);
	});
});

describe('safeNext', () => {
	it('keeps paths of the application only', () => {
		expect(safeNext('/orgs/1?x=2')).toBe('/orgs/1?x=2');
		for (const bad of [
			'https://evil.example',
			'//evil.example',
			'/\\evil',
			'/\t/evil.example',
			'/\n/evil.example',
			'/ /evil.example',
			null,
			undefined,
			''
		])
			expect(safeNext(bad)).toBe('/');
	});
});

describe('logout tokens', async () => {
	const { publicKey, privateKey } = await generateKeyPair('RS256');
	const jwk = { ...(await exportJWK(publicKey)), kid: 'k1', alg: 'RS256' };
	const keys = createLocalJWKSet({ keys: [jwk] });
	const expect_ = { issuer: 'https://id.example', clientId: 'console' };
	const sign = (claims: Record<string, unknown>, typ = 'logout+jwt') =>
		new SignJWT({
			events: { 'http://schemas.openid.net/event/backchannel-logout': {} },
			sid: 'sid_1',
			sub: 'usr_1',
			...claims
		})
			.setProtectedHeader({ alg: 'RS256', kid: 'k1', typ })
			.setIssuer('https://id.example')
			.setAudience('console')
			.setIssuedAt()
			.setExpirationTime('2m')
			.setJti(crypto.randomUUID())
			.sign(privateKey);

	it('takes a good one once', async () => {
		const token = await sign({});
		expect(await verifyLogoutToken(token, keys, expect_)).toEqual({ sid: 'sid_1', sub: 'usr_1' });
		await expect(verifyLogoutToken(token, keys, expect_)).rejects.toThrow(/used already/);
	});

	it('takes one from a clock a little ahead', async () => {
		const ahead = await new SignJWT({
			events: { 'http://schemas.openid.net/event/backchannel-logout': {} },
			sid: 'sid_2',
			sub: 'usr_1'
		})
			.setProtectedHeader({ alg: 'RS256', kid: 'k1', typ: 'logout+jwt' })
			.setIssuer('https://id.example')
			.setAudience('console')
			.setIssuedAt(Math.floor(Date.now() / 1000) + 5)
			.setExpirationTime('2m')
			.setJti(crypto.randomUUID())
			.sign(privateKey);
		expect(await verifyLogoutToken(ahead, keys, expect_)).toEqual({ sid: 'sid_2', sub: 'usr_1' });
	});

	it('refuses the wrong kind, client, or content', async () => {
		await expect(verifyLogoutToken(await sign({}, 'JWT'), keys, expect_)).rejects.toThrow(
			TokenError
		);
		await expect(
			verifyLogoutToken(await sign({}), keys, { ...expect_, clientId: 'other' })
		).rejects.toThrow(TokenError);
		await expect(verifyLogoutToken(await sign({ events: {} }), keys, expect_)).rejects.toThrow(
			/event/
		);
		await expect(verifyLogoutToken(await sign({ nonce: 'n' }), keys, expect_)).rejects.toThrow(
			/nonce/
		);
		await expect(verifyLogoutToken(await sign({ sid: undefined }), keys, expect_)).rejects.toThrow(
			/session/
		);
	});
});
