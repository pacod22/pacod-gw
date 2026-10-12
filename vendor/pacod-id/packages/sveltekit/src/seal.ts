import { CompactEncrypt, compactDecrypt } from 'jose';

/**
 * A 256-bit key from a secret of at least 32 characters, for one purpose: what is sealed for one
 * (the pending sign-in) does not open as another (the session).
 */
export async function keyOf(secret: string, purpose: string): Promise<Uint8Array> {
	if (secret.length < 32) throw new Error('the secret needs at least 32 characters');
	const material = new TextEncoder().encode(`pacod-id-sveltekit ${purpose}\u0000${secret}`);
	return new Uint8Array(await crypto.subtle.digest('SHA-256', material));
}

export async function seal(value: unknown, key: Uint8Array): Promise<string> {
	return new CompactEncrypt(new TextEncoder().encode(JSON.stringify(value)))
		.setProtectedHeader({ alg: 'dir', enc: 'A256GCM' })
		.encrypt(key);
}

export async function unseal<T>(sealed: string | undefined, key: Uint8Array): Promise<T | null> {
	if (!sealed) return null;
	try {
		// Only what seal() makes: a forged header naming another algorithm (PBES2 with many
		// iterations, say) costs nothing to refuse.
		const { plaintext } = await compactDecrypt(sealed, key, {
			keyManagementAlgorithms: ['dir'],
			contentEncryptionAlgorithms: ['A256GCM']
		});
		return JSON.parse(new TextDecoder().decode(plaintext)) as T;
	} catch {
		return null;
	}
}
