import { error } from '@sveltejs/kit';
import { requirePlatformAdmin, testerStream } from '#lib/server/tester.ts';
import { RUN_ID } from '#lib/testing.ts';
import type { RequestHandler } from './$types';

/**
 * Passes the tester's progress events of a run on to the browser. The session token is added
 * here, so the browser listens to the console and never to the tester.
 */
export const GET: RequestHandler = async ({ locals, params, request }) => {
	requirePlatformAdmin(locals);
	if (!RUN_ID.test(params.id)) error(404, 'There is no such run.');

	// When the browser leaves, the connection to the tester closes too.
	const upstream = await testerStream(locals, `/runs/${params.id}/events`, request.signal);
	if (!upstream) return new Response('The tester does not answer.', { status: 502 });
	if (!upstream.ok || !upstream.body) {
		await upstream.body?.cancel();
		return new Response('The tester did not open the event stream.', {
			status: upstream.status === 404 ? 404 : 502
		});
	}

	const reader = upstream.body.getReader();
	const stream = new ReadableStream<Uint8Array>({
		async pull(controller) {
			try {
				const { done, value } = await reader.read();
				if (done) controller.close();
				else controller.enqueue(value);
			} catch {
				// The browser left or the tester closed the stream: end it without an error.
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
