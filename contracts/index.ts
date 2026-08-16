// The contracts barrel — every wire shape the daemon, the board, and the future
// Bun binary share. Import from here (`import { ... } from '../../contracts/index.ts'`)
// so a consumer never reaches into an individual module's path.
//
// `verbatimModuleSyntax` + `isolatedModules` require types and values to be
// re-exported through separate statements (`export type` vs `export`).

// -- values (runtime) -------------------------------------------------------
export { WIRE_SCHEMA_VERSION } from './version.ts';
export {
  ok,
  fail,
  isRecord,
  isNonEmptyString,
  isBoolean,
  isFiniteNumber,
  isPositiveInt,
} from './validate.ts';
export { HOOK_EVENT_NAMES, isHookEventName, validateHookEvent } from './hooks.ts';
export { PERMISSION_MODES, validateSpawnRequest } from './spawn.ts';

// -- types ------------------------------------------------------------------
export type { ValidationResult } from './validate.ts';
export type {
  SessionSpawn,
  AdoptState,
  SessionEntry,
  RepoEntry,
  RepoCatalogEntry,
  TickerEntry,
  ConflictEntry,
  MailRoute,
  MailMeta,
  QuestionEntry,
  SpawnCapability,
  SpawnOrphan,
  PlanEntry,
  PathChoice,
  Settings,
  Lan,
  LegacyUpgrade,
  Snapshot,
  StateResponse,
  WsSnapshot,
} from './state.ts';
export type { HookEventName, HookEventBody } from './hooks.ts';
export type {
  SpawnKind,
  PermissionMode,
  RepoHost,
  RepoTransport,
  BranchMode,
  SpawnRequest,
} from './spawn.ts';
export type {
  ApiOk,
  ApiErrReason,
  ApiErrErr,
  ApiResult,
  MailTarget,
  MailRequest,
  MailResponse,
  CommandRequest,
  CommandResponse,
  PermissionBehavior,
  PermissionAnswer,
  ElicitationAnswer,
  ChoiceAnswer,
  QuestionAnswerBody,
  QuestionAnswerResponse,
} from './wire.ts';
