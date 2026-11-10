# DriverForge TEAM CHARTER (read fully; also read F:/backup/multiagents/team-brief.md and obey it)

GOAL: pure-Rust Windows exe `DriverForge.exe`. On run: scan every device/driver on this PC, find what is
missing or outdated, fetch the TRUE latest from the vendor online, download, verify, silently install.
Covers NVIDIA GPU driver + NVIDIA App, AMD Radeon/Adrenalin, AMD chipset (Ryzen), AMD NPU/other AMD,
and EVERY other driver class (audio, LAN, WiFi, BT, USB, storage, chipset, firmware, monitors...) via
Windows Update COM sweep + Microsoft Update Catalog. No scripts: no PowerShell/python/curl inside the exe.
Project: F:/study/projects/DriverForge (crate `driverforge`, bin `DriverForge`, toolchain stable-x86_64-pc-windows-gnu).

## THIS MACHINE (facts, verified)
Win11 26200 x64. GPUs: NVIDIA RTX 5080 (PCI VEN_10DE DEV_2C02, installed 32.0.16.2114 = NVIDIA 621.14, which is current)
and AMD Radeon iGPU (VEN_1002 DEV_13C0, installed 32.0.21045.20006). CPU Ryzen 7 9800X3D (AM5).
NVIDIA API works: https://gfwsl.geforce.com/services_toolkit/services/com/nvidia/services/AjaxDriverService.php?func=DriverManualLookup&psid=<series>&pfid=<product>&osID=135&languageCode=1033&beta=0&isWHQL=0&dltype=-1&dch=1&upCRD=0&qnf=0&ctk=0&sort1=1&numberOfResults=1
(returns JSON, fields Version, DownloadURL, IsBeta, ReleaseDateTime). Series/product ids come from
https://www.nvidia.com/Download/API/lookupValueSearch.aspx?TypeID=2&ParentID=1 (series, XML) and TypeID=3&ParentID=<series id> (products).
Windows driver version -> NVIDIA version: last digit of 3rd part + 4th part: 32.0.16.2114 -> 621.14.
AMD pages answer HTTP 200 ONLY with a browser User-Agent: https://www.amd.com/en/support/downloads/drivers.html/chipsets/am5/x670.html
contains `https://drivers.amd.com/drivers/amd_chipset_software_8.09.28.1124.exe`; the exe link regex is `https://drivers\.amd\.com[^"]*\.exe`.
Default browser UA: `Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36`.
Existing code (reuse, owner may fix): src/comdisp.rs (COM IDispatch wrapper), src/wupdater.rs (WUA driver sweep).

## SAFETY LAW (binding for all 15)
Real driver installs on this PC are FORBIDDEN for agents. Test only with `--scan-only`, `--dry-run`, unit tests,
and mock servers/fixtures. Only the orchestrator may run a real install. Downloads in tests: small files only.
Never edit a file you do not own. Never print secrets. Forward slashes in bash paths.
BUILD LAW: 15 agents share one tree. ALWAYS build with your own target dir:
`CARGO_TARGET_DIR=F:/study/projects/DriverForge/.targets/<yourname> cargo check` (or build/test). Never `cargo clean` others' dirs.
If the tree does not compile because of ANOTHER agent's file, message that owner, do not fix it. Stub modules may be created by Core on minute 0.
Dependencies: only the Build Engineer edits Cargo.toml; ask by message (crate, version, features). Already present: wmi, serde, serde_json, ureq(tls), regex, winreg, chrono, windows 0.61.

## ROSTER (address by name via send_message)
Core(main.rs, cli.rs, report.rs, types.rs, provider.rs) orchestration + shared types + CLI flags + final report
Scan(scan.rs) device inventory + problem devices
Net(http.rs) HTTP get/download/resume/UA/retry/proxy
NvDrv(nvidia.rs) NVIDIA GPU driver provider
NvApp(nvapp.rs) NVIDIA App provider
AmdGpu(amd_gpu.rs) AMD Radeon Adrenalin provider
AmdChip(amd_chipset.rs) AMD chipset + Ryzen platform + NPU/other AMD providers
Wu(comdisp.rs, wupdater.rs) Windows Update driver sweep provider
Cat(catalog.rs) Microsoft Update Catalog provider (Intel/Realtek/etc. gaps WU misses)
Inst(installer.rs) silent install runner, exit codes, reboot, restore point, timeouts
Ver(version.rs, log.rs) version compare/parse/convert + logging
Build(Cargo.toml, build.rs, app.manifest, README.md, .gitignore) build, requireAdministrator manifest, release profile, README
Rev(read-only reviewer, writes only REVIEW.md) correctness review of everything
Qa(tests/**, qa/**) adversarial tests, fixtures, mock HTTP server, dry-run end-to-end
Sec(read-only, writes only SECURITY.md) HTTPS-only, domain allowlist, WinVerifyTrust signature check design, hash checks, command injection, path traversal

## CONTRACT (exact names; Core creates src/types.rs and src/provider.rs in the first 3 minutes; all others code against this)
```rust
// types.rs
#[derive(Debug, Clone, Default)] pub struct Device { pub name:String, pub hardware_ids:Vec<String>, pub compat_ids:Vec<String>,
  pub class:String, pub manufacturer:String, pub driver_version:Option<String>, pub driver_date:Option<String>,
  pub inf_name:Option<String>, pub problem_code:u32, pub pnp_id:String }
#[derive(Debug, Clone)] pub enum InstallKind { ExeSilent, Msi, ZipThenSetup, WindowsUpdate }
#[derive(Debug, Clone)] pub struct Candidate { pub vendor:String, pub component:String, pub installed:Option<String>,
  pub latest:String, pub url:String, pub sha256:Option<String>, pub size_bytes:Option<u64>, pub kind:InstallKind,
  pub silent_args:Vec<String>, pub needs_reboot_hint:bool }
#[derive(Debug, Clone, PartialEq)] pub enum Outcome { UpToDate, Installed, InstalledRebootNeeded, Skipped(String), Failed(String), DryRun }
// provider.rs
pub trait Provider { fn name(&self)->&'static str; fn check(&self, devices:&[Device])->Result<Vec<Candidate>,String>; }
// each provider module exposes:  pub fn provider() -> Box<dyn Provider>
// http.rs   pub fn get_text(url:&str)->Result<String,String>;  pub fn get_json(url:&str)->Result<serde_json::Value,String>;
//           pub fn download(url:&str, dest:&std::path::Path, expect_sha256:Option<&str>)->Result<u64,String>;   (https only, UA baked in, 3 retries, resume)
// scan.rs   pub fn scan_devices()->Result<Vec<Device>,String>;   pub fn problem_devices(d:&[Device])->Vec<&Device>;
// version.rs pub fn cmp(a:&str,b:&str)->std::cmp::Ordering; pub fn is_newer(latest:&str, installed:&str)->bool;
//           pub fn nvidia_from_windows(v:&str)->Option<String>;  (32.0.16.2114 -> 621.14)
// log.rs    pub fn init(path:&std::path::Path); pub fn info(m:&str); pub fn warn(m:&str); pub fn err(m:&str);  (also prints to console)
// installer.rs pub fn install(c:&Candidate, dry_run:bool)->Outcome;  pub fn create_restore_point()->Result<(),String>;  pub fn is_admin()->bool;
// CLI (Core): DriverForge [--scan-only] [--dry-run] [--yes] [--only nvidia|amd|windows|all] [--json] [--no-restore-point] [--log <path>]
// Exit codes: 0 all current/installed, 1 failures, 2 reboot needed, 3 not admin.
```
Contract changes: announce to ALL affected + store_knowledge. query_knowledge before deciding shared things.

## HANDOFF MAP
Core->all: types.rs/provider.rs stubs ready (minute 3). Net->providers+Inst: http.rs API compiles with real impl (target minute 8).
Ver->providers: version.rs ready (minute 5). Scan->Core: scan_devices real impl (minute 10).
Providers (NvDrv,NvApp,AmdGpu,AmdChip,Wu,Cat)->Qa: each tells Qa the fixture/mocking approach and how to run check() offline.
Inst->Core: install() ready. Build->all: manifest/release profile ready; tells Core if a new crate is added.
Rev/Sec start reading as soon as a module exists, report [BLOCKING]/[SUGGESTION] with file:line to the OWNER (cc orchestrator).
Qa: build fixtures from REAL captured responses (curl them), run `DriverForge --scan-only --dry-run --json` end to end on this PC.

## DONE (everyone)
`CARGO_TARGET_DIR=... cargo build --release` and `cargo test` green for your area; your module has unit tests with
fixtures; `DriverForge --scan-only --dry-run` runs on this PC without panic and prints correct installed versions
(NVIDIA 621.14, AMD Radeon 32.0.21045.20006). Escalation: stuck >3 min -> "BLOCKED:" to orchestrator.
