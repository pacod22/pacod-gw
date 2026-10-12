// The application the end-to-end sign-ins return to: any path answers a small page. Started by
// scripts/e2e.sh on 127.0.0.1:27399 (a browser route cannot answer the target of a redirect).
import http from 'node:http';

http
	.createServer((req, res) => {
		res.writeHead(200, { 'content-type': 'text/html; charset=utf-8' });
		res.end('<!doctype html><title>app</title><p id="app">app</p>');
	})
	.listen(27399, '127.0.0.1');
