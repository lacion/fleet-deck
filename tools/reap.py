#!/usr/bin/env python3
"""A child subreaper: runs a command and reaps every orphaned descendant.

tools/check.sh runs the Rust tests under it on Linux. Process-group tests
(media::recorder) check that an ended child is gone; an orphan that the
init of a WSL machine or an old station shim does not reap stays a zombie
and fails them. A subreaper reaps those orphans at once.

    python3 tools/reap.py cargo nextest run --workspace
"""
import ctypes, os, signal, sys

PR_SET_CHILD_SUBREAPER = 36

def main():
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) != 0:
        sys.exit("prctl PR_SET_CHILD_SUBREAPER failed")
    pid = os.fork()
    if pid == 0:
        # Python ignores these; an ignored signal stays ignored across exec.
        signal.signal(signal.SIGPIPE, signal.SIG_DFL)
        signal.signal(signal.SIGXFSZ, signal.SIG_DFL)
        os.execvp(sys.argv[1], sys.argv[1:])
    status = 0
    while True:
        try:
            done, st = os.wait()
        except ChildProcessError:
            break
        if done == pid:
            status = os.waitstatus_to_exitcode(st)
    sys.exit(status if status >= 0 else 128 - status)

if __name__ == "__main__":
    main()
