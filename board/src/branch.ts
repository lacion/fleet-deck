// Client-side mirror of the daemon's branch input gate, for instant feedback.
// The daemon's `git check-ref-format --branch` remains authoritative; refusing
// its lexical rules here prevents an avoidable POST.
export function branchProblem(branch: string): string | null {
  if (!branch) return 'enter a branch name';
  if (branch.length > 200) return 'too long for a branch name';
  if (branch.startsWith('-')) return 'a branch cannot start with “-”';
  let hasControl = false;
  for (let i = 0; i < branch.length; i++) {
    if (branch.charCodeAt(i) <= 0x1f) {
      hasControl = true;
      break;
    }
  }
  if (/[\s~^:?*[\\]/.test(branch) || hasControl)
    return 'no spaces or git-special characters (~ ^ : ? * [ \\)';
  if (branch.includes('..') || branch.includes('@{')) return 'no “..” or “@{”';
  const components = branch.split('/');
  if (
    components.some(
      (part) => !part || part.startsWith('.') || part.endsWith('.') || part.endsWith('.lock'),
    )
  )
    return 'not a valid ref name';
  return null;
}
