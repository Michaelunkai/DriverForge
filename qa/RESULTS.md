# QA RESULTS

Run: `cargo test` (target dir .targets/qa). Fixtures: tests/fixtures (NVIDIA lookup JSON, series XML, AMD AM5 page), qa/fixtures (Catalog Realtek pages).

## Real exe: `DriverForge --scan-only --dry-run --json --no-restore-point`
Exit 0, no panic, ~27s, 246 devices, 0 problem devices. AMD Radeon installed 32.0.21045.20006 shown; NVIDIA reported UpToDate.

## Bugs
1. [P2] report.rs: up-to-date rows print installed "-" / "(nothing found)", so NVIDIA 621.14 / NVApp 11.0.9.585 are not visible in output (charter expects them). Owner: Core.
2. [P2] Duplicated vendor in component names: "AMD AMD Radeon...", "Microsoft Corporation Microsoft Corporation ..." (amd_gpu / wupdater / catalog). Owners: AmdGpu, Wu, Cat.
3. [P2] wupdater: row with installed 1.0.4.7057 vs pending 1.0.4.55794 has latest "" and is UpToDate (WU update dropped/blank latest). Owner: Wu.
4. [FIXED by Inst, re-verified] installer.rs unit tests failed: names_safe (left "_.._evil" vs right "__.._evil", line 458) and timeout_kills_and_exit_code_runs (exit 1 vs 3010, line 531). Owner: Inst.
5. [P3] catalog: "lookup cap reached" warning on 246 devices (design cap 12).
6. [P3] A full scan takes ~27s; AMD GPU shows update 32.0.32015.12012 available (dry run only; nothing installed).

## Latest run
cargo test: 100 unit passed, 5 ignored (live), 0 failed; tests/e2e.rs 4 passed (real exe dry-run exit 0, fixtures).

## Not covered
Mock HTTP server integration tests: the crate is bin-only (no lib), so integration tests cannot call http::download; Net added DRIVERFORGE_ALLOW_LOOPBACK=1 and mock tests live inside http.rs unit tests.
