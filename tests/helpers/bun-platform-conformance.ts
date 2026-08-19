// Generic contract runner for Bun-platform adapters. Keep Fleet Deck domain
// policy out of this helper so the same suite shape can follow an app-local
// adapter into an extracted package or an upstream contribution.

export interface BunPlatformConformanceCase<Adapter> {
  readonly name: string;
  readonly run: (adapter: Adapter) => unknown | PromiseLike<unknown>;
}

export interface BunPlatformConformanceSpec<Adapter> {
  readonly adapter: string;
  readonly acquire: () => Adapter | PromiseLike<Adapter>;
  readonly release: (adapter: Adapter) => unknown | PromiseLike<unknown>;
  readonly cases: readonly BunPlatformConformanceCase<Adapter>[];
}

export interface BunPlatformConformanceReport {
  readonly adapter: string;
  readonly completedCases: readonly string[];
  readonly released: true;
}

export function defineBunPlatformConformance<Adapter>(
  spec: BunPlatformConformanceSpec<Adapter>,
): BunPlatformConformanceSpec<Adapter> {
  return spec;
}

export async function runBunPlatformConformance<Adapter>(
  spec: BunPlatformConformanceSpec<Adapter>,
): Promise<BunPlatformConformanceReport> {
  const adapter = await spec.acquire();
  const completedCases: string[] = [];
  try {
    for (const testCase of spec.cases) {
      await testCase.run(adapter);
      completedCases.push(testCase.name);
    }
  } finally {
    await spec.release(adapter);
  }
  return { adapter: spec.adapter, completedCases, released: true };
}

// This structural canary intentionally names only stable Bun primitives needed
// by Fleet Deck's adapter candidates. The floor and latest-types CI lanes both
// compile this file, detecting incompatible declaration changes without
// modifying the canonical lockfile.
export interface BunPlatformTypeSurface {
  readonly file: typeof Bun.file;
  readonly serve: typeof Bun.serve;
  readonly spawn: typeof Bun.spawn;
  readonly write: typeof Bun.write;
}

export const bunPlatformTypeSurface = {
  file: Bun.file,
  serve: Bun.serve,
  spawn: Bun.spawn,
  write: Bun.write,
} satisfies BunPlatformTypeSurface;
