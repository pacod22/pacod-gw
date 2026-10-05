import { GATEWAY_API_URL } from '$app/env/private';
import { API_BASE } from '#lib/server/api.ts';
import type { RequestHandler } from './$types';

/**
 * Passes the core's stream of configuration changes on to the browser. The session token is
 * added here, so the browser listens to the console and never to the core.
 */
export const GET: RequestHandler = async ({ locals, request }) => {
	if (!locals.token) return new Response('Sign in first.', { status: 401 });

	let upstream: Response;
	try {
		upstream = await fetch(`${GATEWAY_API_URL}${API_BASE}/events`, {
			headers: { authorization: `Bearer ${locals.token}`, accept: 'text/event-stream' },
			// When the browser leaves, the connection to the core closes too.
			signal: request.signal
		});
	} catch {
		return new Response('The gateway does not answer.', { status: 502 });
	}
	if (!upstream.ok || !upstream.body) {
		await upstream.body?.cancel();
		return new Response('The gateway did not open the event stream.', {
			status: upstream.status === 401 ? 401 : 502
		});
	}

	// The response head is only sent with the first chunk, and a quiet installation has no
	// event for a long time: open the stream with a comment so that the browser sees the
	// connection at once.
	const reader = upstream.body.getReader();
	const stream = new ReadableStream<Uint8Array>({
		start(controller) {
			controller.enqueue(new TextEncoder().encode(': connected\n\n'));
		},
		async pull(controller) {
			try {
				const { done, value } = await reader.read();
				if (done) controller.close();
				else controller.enqueue(value);
			} catch {
				// The browser left or the core closed the stream: end it without an error.
				controller.close();
			}
		},
		cancel(reason) {
			return reader.cancel(reason);
		}
	});

	return new Response(stream, {
		headers: {
			'content-type': 'text/event-stream',
			'cache-control': 'no-cache',
			// Tells a reverse proxy in front of the console not to buffer the stream.
			'x-accel-buffering': 'no'
		}
	});
};
