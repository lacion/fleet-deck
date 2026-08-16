// tests/helpers/stub-dying-daemon.ts
//
// A fleetd-CLI-shaped stub that exits immediately with a nonzero code,
// without ever binding FLEETDECK_PORT. Used by the BUG-164 regression test
// to prove startDaemon races readiness against the child's exit instead of
// polling /health until the timeout.

process.exit(3);
