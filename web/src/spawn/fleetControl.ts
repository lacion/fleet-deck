// ADR-008 §3: the "Fleet control" switches of the spawn form. A coordinator grant with a switch on
// is a fleet-control grant: it needs the owner's elevation and carries the extra classes.

import { NO_LIMIT } from '../fleet/limitText';

export type FleetControl = {
  readonly scope: boolean;
  readonly inspect: boolean;
  readonly manage: boolean;
  readonly answer: boolean;
  readonly answerCap: string;
  readonly desktop: boolean;
  readonly capture: boolean;
  readonly desktopCap: string;
  readonly captureCap: string;
  /** The image read budget in MiB. */
  readonly imageMib: string;
  /** Allow monitor recordings (only meaningful while `capture` is on). */
  readonly recording: boolean;
  /** The recording count limit. */
  readonly recordingCap: string;
  /** The recording time limit in seconds. */
  readonly recordingSeconds: string;
  /** Allow keyboard input and pointer movement (only meaningful while `desktop` and `capture` are on). */
  readonly input: boolean;
  /** The input action limit. */
  readonly inputCap: string;
  /** The typed character limit. */
  readonly inputChars: string;
  /** Allow pointer clicks (P7b; only meaningful while the input opt-in is on). */
  readonly click: boolean;
  /** The scope is "other application windows" (P7b; only meaningful while the input opt-in is on). */
  readonly anyWindow: boolean;
  /** The owner checked "Allow input in other application windows" (P7b). */
  readonly anyAck: boolean;
  /** Allow the agent to start sessions (the `spawn` class; not fleet control). */
  readonly spawn: boolean;
  /** The session limit (spawn cap). */
  readonly spawnCap: string;
  /** "No limit" is checked for the session limit (B27); the text is kept for when it is cleared. */
  readonly spawnNone: boolean;
  /** "No limit" is checked for the answer limit (B27). */
  readonly answerNone: boolean;
  /** The idle limit of the grant in minutes (B27). */
  readonly idleMinutes: string;
  /** "No limit" is checked for the idle limit (B27): the grant has no idle limit. */
  readonly idleNone: boolean;
  /** The highest Claude mode that its sessions may get. */
  readonly claudeCeiling: string;
  /** The highest Codex `policy/sandbox` mode that its sessions may get. */
  readonly codexCeiling: string;
};

export const NO_FLEET: FleetControl = {
  scope: false,
  inspect: false,
  manage: false,
  answer: false,
  answerCap: '500',
  desktop: false,
  capture: false,
  desktopCap: '300',
  captureCap: '20',
  imageMib: '64',
  recording: false,
  recordingCap: '5',
  recordingSeconds: '600',
  input: false,
  inputCap: '100',
  inputChars: '20000',
  click: false,
  anyWindow: false,
  anyAck: false,
  spawn: false,
  spawnCap: '3',
  spawnNone: false,
  answerNone: false,
  idleMinutes: '30',
  idleNone: false,
  claudeCeiling: 'acceptEdits',
  codexCeiling: 'on-request/workspace-write',
};

/** The largest desktop, screenshot, recording and input limit that the deck accepts. */
export const MAX_ANSWER_CAP = 5000;

/** True when any fleet-control switch is on. The `spawn` switch is not fleet control. */
export function fleetOn(f: FleetControl): boolean {
  return f.scope || f.inspect || f.manage || f.answer || f.desktop || f.capture;
}

/** The problem of the answer cap text, or null: a whole number (only checked when `answer` is on and "No limit" is off). The deck checks the upper bound. */
export function answerCapProblem(f: FleetControl): string | null {
  if (!f.answer || f.answerNone) return null;
  if (/^\d+$/.test(f.answerCap.trim())) return null;
  return 'The answer limit is a whole number.';
}

const MAX_IDLE_MINUTES = 10080;

/** The problem of the idle limit text, or null: whole minutes from 1 to 10080 (not checked while "No limit" is on). */
export function idleLimitProblem(f: FleetControl): string | null {
  if (f.idleNone) return null;
  const text = f.idleMinutes.trim();
  if (/^\d+$/.test(text) && Number(text) >= 1 && Number(text) <= MAX_IDLE_MINUTES) return null;
  return 'The idle limit is a whole number of minutes from 1 to 10080.';
}

const MAX_DESKTOP_CAP = 5000;
const MAX_IMAGE_MIB = 1024;
const MAX_RECORDING_SECONDS = 600_000;
const MAX_INPUT_CHARS = 1_000_000;
const MIB = 1_048_576;

const wholeUpTo = (text: string, max: number): boolean => {
  const t = text.trim();
  return /^\d+$/.test(t) && Number(t) <= max;
};

/** The problem of the desktop action limit, or null (checked when `desktop` or `capture` is on). */
export function desktopCapProblem(f: FleetControl): string | null {
  if (!(f.desktop || f.capture)) return null;
  if (wholeUpTo(f.desktopCap, MAX_DESKTOP_CAP)) return null;
  return 'The desktop action limit is a whole number from 0 to 5000.';
}

/** The problem of the screenshot limit, or null (checked when `capture` is on). */
export function captureCapProblem(f: FleetControl): string | null {
  if (!f.capture) return null;
  if (wholeUpTo(f.captureCap, MAX_DESKTOP_CAP)) return null;
  return 'The screenshot limit is a whole number from 0 to 5000.';
}

/** The problem of the image read budget, or null (checked when `capture` is on). */
export function imageBudgetProblem(f: FleetControl): string | null {
  if (!f.capture) return null;
  if (wholeUpTo(f.imageMib, MAX_IMAGE_MIB)) return null;
  return 'The image read budget is a whole number of MiB from 0 to 1024.';
}

/** The problem of the recording limit, or null (checked when `capture` and `recording` are on). */
export function recordingCapProblem(f: FleetControl): string | null {
  if (!(f.capture && f.recording)) return null;
  if (wholeUpTo(f.recordingCap, MAX_DESKTOP_CAP)) return null;
  return 'The recording limit is a whole number from 0 to 5000.';
}

/** The problem of the recording time limit, or null (checked when `capture` and `recording` are on). */
export function recordingSecondsProblem(f: FleetControl): string | null {
  if (!(f.capture && f.recording)) return null;
  if (wholeUpTo(f.recordingSeconds, MAX_RECORDING_SECONDS)) return null;
  return 'The recording time limit is a whole number of seconds from 0 to 600000.';
}

/** True when the input opt-in is effective: the switch, with `desktop` and `capture` on. */
export function inputOn(f: FleetControl): boolean {
  return f.input && f.desktop && f.capture;
}

/** The problem of the input action limit, or null (checked when the input opt-in is on). */
export function inputCapProblem(f: FleetControl): string | null {
  if (!inputOn(f)) return null;
  if (wholeUpTo(f.inputCap, MAX_DESKTOP_CAP)) return null;
  return 'The input action limit is a whole number from 0 to 5000.';
}

/** The problem of the typed character limit, or null (checked when the input opt-in is on). */
export function inputCharsProblem(f: FleetControl): string | null {
  if (!inputOn(f)) return null;
  if (wholeUpTo(f.inputChars, MAX_INPUT_CHARS)) return null;
  return 'The typed character limit is a whole number from 0 to 1000000.';
}

/** P7b §14: the problem of the missing acknowledgment of the broader scope, or null (checked when the input opt-in is on). */
export function anyAckProblem(f: FleetControl): string | null {
  if (!inputOn(f)) return null;
  if (!f.anyWindow || f.anyAck) return null;
  return 'Check "Allow input in other application windows" to give this agent the broader scope.';
}

/** The problem of the session limit text, or null: a whole number (only checked when `spawn` is on and "No limit" is off). The deck checks the upper bound. */
export function spawnCapProblem(f: FleetControl): string | null {
  if (!f.spawn || f.spawnNone) return null;
  if (/^\d+$/.test(f.spawnCap.trim())) return null;
  return 'The session limit is a whole number.';
}

/** The first problem of the limit inputs (session, answer, idle, desktop, screenshot, image budget, recording, input) and the acknowledgment, or null. */
export function fleetProblem(f: FleetControl): string | null {
  return (
    spawnCapProblem(f) ??
    answerCapProblem(f) ??
    idleLimitProblem(f) ??
    desktopCapProblem(f) ??
    captureCapProblem(f) ??
    imageBudgetProblem(f) ??
    recordingCapProblem(f) ??
    recordingSecondsProblem(f) ??
    inputCapProblem(f) ??
    inputCharsProblem(f) ??
    anyAckProblem(f)
  );
}

/** True when the shown grant fields of a coordinator start have a problem, so Start stays disabled. */
export function fleetBlocked(coordinator: boolean, shown: boolean, f: FleetControl): boolean {
  return coordinator && shown && fleetProblem(f) !== null;
}

/**
 * The grant part of the coordinator spec: classes, scope/answer_cap only for a fleet-control grant,
 * and the target stations (besides the session's own) only when the owner picked some.
 */
export function grantFields(
  f: FleetControl,
  targets: readonly string[] = [],
): {
  classes: string[];
  scope?: 'fleet';
  answer_cap?: number;
  idle_ttl_ms: number;
  desktop_cap?: number;
  capture_cap?: number;
  image_byte_cap?: number;
  allow_recording?: boolean;
  recording_cap?: number;
  recording_second_cap?: number;
  allow_input?: boolean;
  input_scope?: 'own_windows' | 'any_window';
  allow_click?: boolean;
  input_cap?: number;
  input_character_cap?: number;
  stations?: string[];
  spawn_cap?: number;
  max_modes?: { [key in string]: string };
} {
  return {
    classes: [
      'read',
      'mail',
      'drive',
      ...(f.spawn ? ['spawn'] : []),
      ...(f.inspect ? ['inspect'] : []),
      ...(f.manage ? ['manage'] : []),
      ...(f.answer ? ['answer'] : []),
      ...(f.desktop ? ['desktop'] : []),
      ...(f.capture ? ['capture'] : []),
    ],
    idle_ttl_ms: f.idleNone ? NO_LIMIT : Number(f.idleMinutes.trim()) * 60_000,
    ...(f.spawn ? { spawn_cap: f.spawnNone ? NO_LIMIT : Number(f.spawnCap.trim()) } : {}),
    ...(f.spawn || f.manage
      ? { max_modes: { claude: f.claudeCeiling, codex: f.codexCeiling } }
      : {}),
    ...(f.scope ? { scope: 'fleet' as const } : {}),
    ...(f.answer ? { answer_cap: f.answerNone ? NO_LIMIT : Number(f.answerCap.trim()) } : {}),
    ...(f.desktop || f.capture ? { desktop_cap: Number(f.desktopCap.trim()) } : {}),
    ...(f.capture
      ? {
          capture_cap: Number(f.captureCap.trim()),
          image_byte_cap: Number(f.imageMib.trim()) * MIB,
        }
      : {}),
    ...(f.capture && f.recording
      ? {
          allow_recording: true,
          recording_cap: Number(f.recordingCap.trim()),
          recording_second_cap: Number(f.recordingSeconds.trim()),
        }
      : {}),
    ...(inputOn(f)
      ? {
          allow_input: true,
          input_scope: f.anyWindow ? ('any_window' as const) : ('own_windows' as const),
          ...(f.click ? { allow_click: true } : {}),
          input_cap: Number(f.inputCap.trim()),
          input_character_cap: Number(f.inputChars.trim()),
        }
      : {}),
    ...(targets.length > 0 ? { stations: [...targets] } : {}),
  };
}

/** The fleet fields show for the owner's login, with Coordinator checked, and not for an import. */
export function showFleetFields(owner: boolean, coordinator: boolean, importing: boolean): boolean {
  return owner && coordinator && !importing;
}

/**
 * The elevation of a start: asked once, only for a coordinator start (not an import) with a
 * switch on, with the spawn switch on, or with target stations picked (the deck treats a grant
 * with other stations as fleet control). `{ ok: false }` means the owner cancelled the dialog.
 */
export async function fleetElevation(
  gate: {
    readonly coordinator: boolean;
    readonly importing: boolean;
    readonly fleet: FleetControl;
    readonly targets?: readonly string[];
  },
  ask: (labels?: readonly string[]) => Promise<string | null>,
): Promise<{ readonly ok: true; readonly elevation: string | undefined } | { readonly ok: false }> {
  const wide = (gate.targets?.length ?? 0) > 0;
  if (!(gate.coordinator && !gate.importing && (fleetOn(gate.fleet) || wide || gate.fleet.spawn))) {
    return { ok: true, elevation: undefined };
  }
  const labels = [
    gate.fleet.spawn && !fleetOn(gate.fleet) && !wide ? 'Starting sessions' : 'Fleet control',
    ...(inputOn(gate.fleet) && gate.fleet.click ? ['Agent pointer clicks'] : []),
    ...(inputOn(gate.fleet) && gate.fleet.anyWindow
      ? ['Agent input in other application windows']
      : []),
  ];
  const code = await ask(labels);
  return code === null ? { ok: false } : { ok: true, elevation: code };
}
