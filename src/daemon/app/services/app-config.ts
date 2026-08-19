import * as Context from 'effect/Context';

export interface AppConfigService {
  readonly home: string;
  readonly port: number;
  readonly version: string;
}

/** Immutable inputs selected by the daemon host before the application Layer is built. */
export class AppConfig extends Context.Service<AppConfig, AppConfigService>()(
  'fleetdeck/daemon/app/AppConfig',
) {}
