/**
 * The icon in front of each navigation item, keyed by group and item label. Kept apart from
 * nav.ts so that the navigation model stays plain data.
 */
import type { Component } from 'svelte';
import Activity from '@lucide/svelte/icons/activity';
import BadgeCheck from '@lucide/svelte/icons/badge-check';
import Boxes from '@lucide/svelte/icons/boxes';
import Building from '@lucide/svelte/icons/building';
import ChartColumn from '@lucide/svelte/icons/chart-column';
import ChartLine from '@lucide/svelte/icons/chart-line';
import ChartPie from '@lucide/svelte/icons/chart-pie';
import ChartSpline from '@lucide/svelte/icons/chart-spline';
import Circle from '@lucide/svelte/icons/circle';
import FileCode from '@lucide/svelte/icons/file-code';
import FileSearch from '@lucide/svelte/icons/file-search';
import FlaskConical from '@lucide/svelte/icons/flask-conical';
import FolderKanban from '@lucide/svelte/icons/folder-kanban';
import Gauge from '@lucide/svelte/icons/gauge';
import House from '@lucide/svelte/icons/house';
import KeyRound from '@lucide/svelte/icons/key-round';
import LayoutDashboard from '@lucide/svelte/icons/layout-dashboard';
import MonitorCog from '@lucide/svelte/icons/monitor-cog';
import Route from '@lucide/svelte/icons/route';
import ScrollText from '@lucide/svelte/icons/scroll-text';
import Server from '@lucide/svelte/icons/server';
import Settings from '@lucide/svelte/icons/settings';
import SlidersHorizontal from '@lucide/svelte/icons/sliders-horizontal';
import Tag from '@lucide/svelte/icons/tag';
import TrendingUp from '@lucide/svelte/icons/trending-up';
import TestTubeDiagonal from '@lucide/svelte/icons/test-tube-diagonal';
import TriangleAlert from '@lucide/svelte/icons/triangle-alert';
import UserRound from '@lucide/svelte/icons/user-round';
import Users from '@lucide/svelte/icons/users';
import UsersRound from '@lucide/svelte/icons/users-round';

const ICONS: Record<string, Component> = {
	'Home/Dashboard': LayoutDashboard,
	'Platform/Providers': Server,
	'Platform/Models': Boxes,
	'Platform/Prices': Tag,
	'Platform/Price tracker': TrendingUp,
	'Platform/All organisations': Building,
	'Platform/Access': Users,
	'Platform/Testing': TestTubeDiagonal,
	'Platform/System': MonitorCog,
	'Platform/Settings': Settings,
	'Organisation/Overview': House,
	'Organisation/Workspaces': FolderKanban,
	'Organisation/Members': UsersRound,
	'Organisation/Model grants': BadgeCheck,
	'Organisation/Limits': Gauge,
	'Organisation/Routing schemes': Route,
	'Organisation/Settings': SlidersHorizontal,
	'Workspace/Overview': House,
	'Workspace/Members': UsersRound,
	'Workspace/Model grants': BadgeCheck,
	'Workspace/Templates': FileCode,
	'Workspace/API keys': KeyRound,
	'Workspace/Limits': Gauge,
	'Workspace/Playground': FlaskConical,
	'Workspace/Settings': SlidersHorizontal,
	'Insight/Analytics': ChartLine,
	'Insight/Usage and costs': ChartColumn,
	'Insight/Breakdown': ChartPie,
	'Insight/End users': UserRound,
	'Insight/Errors': TriangleAlert,
	'Insight/Requests': ScrollText,
	'Insight/Audit records': FileSearch,
	'Insight/Monitoring': Activity,
	'Insight/Metrics': ChartSpline
};

export function navIcon(group: string, item: string): Component {
	return ICONS[`${group}/${item}`] ?? Circle;
}
