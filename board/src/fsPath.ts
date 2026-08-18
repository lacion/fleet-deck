export function normalizeBrowseRoot(root: string): string {
  return (root || '~').replace(/\/+$/, '') || '/';
}

// Return a candidate's path relative to the browse root, or null when it falls
// outside. Both the picker and viewer use this exact containment boundary for
// favorite chips; keeping it here prevents '/' and trailing-slash drift.
export function relativeBrowsePath(root: string, candidate: string): string | null {
  const normalizedRoot = normalizeBrowseRoot(root);
  if (candidate === normalizedRoot) return '';
  const prefix = normalizedRoot.endsWith('/') ? normalizedRoot : `${normalizedRoot}/`;
  return candidate.startsWith(prefix) ? candidate.slice(prefix.length) : null;
}

// Join a server-reported browse root to a relative entry for display/copy.
// POSIX '/' is already its own separator; blindly adding another one produced
// '//child' in the global file viewer when browse_root was the filesystem root.
export function joinBrowsePath(root: string, relative: string): string {
  const normalizedRoot = normalizeBrowseRoot(root);
  if (!relative) return normalizedRoot;
  const prefix = normalizedRoot.endsWith('/') ? normalizedRoot : `${normalizedRoot}/`;
  return `${prefix}${relative.replace(/^\/+/, '')}`;
}
