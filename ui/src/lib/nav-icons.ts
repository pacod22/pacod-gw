/**
 * The icon in front of each navigation item, by the meaning nav.ts gives it. Kept apart from
 * nav.ts so that the navigation model stays plain data.
 */
import type { Component } from 'svelte';
import Activity from '@lucide/svelte/icons/activity';
import BadgeCheck from '@lucide/svelte/icons/badge-check';
import Boxes from '@lucide/svelte/icons/boxes';
import Building from '@lucide/svelte/icons/building';
import Building2 from '@lucide/svelte/icons/building-2';
import DatabaseZap from '@lucide/svelte/icons/database-zap';
import ChartColumn from '@lucide/svelte/icons/chart-column';
import FileCode from '@lucide/svelte/icons/file-code';
import FileStack from '@lucide/svelte/icons/file-stack';
import FlaskConical from '@lucide/svelte/icons/flask-conical';
import FolderKanban from '@lucide/svelte/icons/folder-kanban';
import Gauge from '@lucide/svelte/icons/gauge';
import House from '@lucide/svelte/icons/house';
import KeyRound from '@lucide/svelte/icons/key-round';
import LayoutDashboard from '@lucide/svelte/icons/layout-dashboard';
import MonitorCog from '@lucide/svelte/icons/monitor-cog';
import Puzzle from '@lucide/svelte/icons/puzzle';
import Route from '@lucide/svelte/icons/route';
import ScrollText from '@lucide/svelte/icons/scroll-text';
import Server from '@lucide/svelte/icons/server';
import Settings from '@lucide/svelte/icons/settings';
import SlidersHorizontal from '@lucide/svelte/icons/sliders-horizontal';
import Tag from '@lucide/svelte/icons/tag';
import TestTubeDiagonal from '@lucide/svelte/icons/test-tube-diagonal';
import Users from '@lucide/svelte/icons/users';
import UsersRound from '@lucide/svelte/icons/users-round';
import type { NavIcon } from './nav';

const ICONS: Record<NavIcon, Component> = {
	dashboard: LayoutDashboard,
	providers: Server,
	models: Boxes,
	prices: Tag,
	organisations: Building,
	organisation: Building2,
	access: Users,
	system: MonitorCog,
	testing: TestTubeDiagonal,
	'platform-settings': Settings,
	overview: House,
	workspaces: FolderKanban,
	members: UsersRound,
	grants: BadgeCheck,
	limits: Gauge,
	routing: Route,
	settings: SlidersHorizontal,
	keys: KeyRound,
	templates: FileCode,
	playground: FlaskConical,
	objects: FileStack,
	usage: ChartColumn,
	requests: ScrollText,
	monitoring: Activity,
	plugins: Puzzle,
	cache: DatabaseZap
};

export function navIcon(icon: NavIcon): Component {
	return ICONS[icon];
}
