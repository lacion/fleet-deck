//! Fake Claude CLI for tests (see `fleetdeck_testkit::fake`).

fn main() -> std::process::ExitCode {
    fleetdeck_testkit::fake::fake_main(fleetdeck_testkit::FakeAgent::Claude)
}
