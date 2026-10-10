import { loadObjects } from '#lib/server/objects.ts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = (event) => loadObjects(event, 'batches');
