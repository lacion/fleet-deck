// ADR-008 §3: the "Fleet control" switches of the spawn form. A coordinator grant with a switch on
// is a fleet-control grant: it needs the owner's elevation and carries the extra classes.

export type FleetControl = {
  readonly scope: boolean;
  readonly inspect: boolean;
  readonly manage: boolean;
  readonly answer: boolean;
  readonly answerCap: string;
};

export const NO_FLEET: FleetControl = {
  scope: false,
  inspect: false,
  manage: false,
  answer: false,
  answerCap: '500',
};

/** The largest answer limit that the deck accepts. */
export const MAX_ANSWER_CAP = 5000;

/** True when any fleet-control switch is on. */
export function fleetOn(f: FleetControl): boolean {
  return f.scope || f.inspect || f.manage || f.answer;
}

/** The problem of the answer cap text, or null: an integer 0..5000 (only checked when `answer` is on). */
export function answerCapProblem(f: FleetControl): string | null {
  if (!f.answer) return null;
  const text = f.answerCap.trim();
  if (/^\d+$/.test(text) && Number(text) <= MAX_ANSWER_CAP) return null;
  return 'The answer limit is a whole number from 0 to 5000.';
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
  stations?: string[];
} {
  return {
    classes: [
      'read',
      'mail',
      'drive',
      ...(f.inspect ? ['inspect'] : []),
      ...(f.manage ? ['manage'] : []),
      ...(f.answer ? ['answer'] : []),
    ],
    ...(f.scope ? { scope: 'fleet' as const } : {}),
    ...(f.answer ? { answer_cap: Number(f.answerCap.trim()) } : {}),
    ...(targets.length > 0 ? { stations: [...targets] } : {}),
  };
}

/** The fleet fields show for the owner's login, with Coordinator checked, and not for an import. */
export function showFleetFields(owner: boolean, coordinator: boolean, importing: boolean): boolean {
  return owner && coordinator && !importing;
}

/**
 * The elevation of a start: asked once, only for a coordinator start (not an import) with a
 * switch on or with target stations picked (the deck treats a grant with other stations as fleet
 * control). `{ ok: false }` means the owner cancelled the dialog.
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
  if (!(gate.coordinator && !gate.importing && (fleetOn(gate.fleet) || wide))) {
    return { ok: true, elevation: undefined };
  }
  const code = await ask(['Fleet control']);
  return code === null ? { ok: false } : { ok: true, elevation: code };
}
