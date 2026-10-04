import { defineEnvVars } from '@sveltejs/kit/env';

export const variables = defineEnvVars({
	PUBLIC_GATEWAY_ADMIN_URL: {
		public: true,
		description: 'Base URL of the pacod-gw-core admin API',
		schema: (value) => value ?? 'http://localhost:8081'
	},
	PUBLIC_ANALYTICS_URL: {
		public: true,
		description: 'Base URL of the pacod-gw-analytics reports API',
		schema: (value) => value ?? 'http://localhost:8090'
	}
});
