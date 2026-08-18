// Browser storage is an optional convenience, never a prerequisite for the
// board. Safari private windows, locked-down enterprise policies, and exhausted
// quotas can all make even getItem() throw. Keep that failure boundary in one
// place so a preference cannot white-screen the developer's control surface.

export function storageGet(key: string): string | null {
  try {
    return globalThis.localStorage?.getItem(key) ?? null;
  } catch {
    return null;
  }
}

export function storageSet(key: string, value: string): boolean {
  try {
    globalThis.localStorage?.setItem(key, value);
    return true;
  } catch {
    return false;
  }
}

export function storageRemove(key: string): boolean {
  try {
    globalThis.localStorage?.removeItem(key);
    return true;
  } catch {
    return false;
  }
}
