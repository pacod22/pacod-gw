// Short names for the types generated from the OpenAPI document.
import type { components } from './schema';

export type Schemas = components['schemas'];

export type Principal = Schemas['PrincipalView'];
export type RoleView = Schemas['RoleView'];
export type RoleName = Schemas['RoleName'];
export type RoleScope = Schemas['RoleScope'];

export type Organisation = Schemas['Item'];
export type Workspace = Schemas['Item2'];
export type ApiKey = Schemas['Item3'];
export type Provider = Schemas['Item4'];
export type Credential = Schemas['Item5'];
export type User = Schemas['Item6'];
export type RoleBinding = Schemas['Item7'];
export type AdminToken = Schemas['Item8'];

export type SystemStatus = Schemas['SystemStatus'];
export type AuditRow = Schemas['AuditRow'];
export type GlobalSettings = Schemas['GlobalSettings'];
