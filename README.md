<div align="center">

# DriverForge

**One pure-Rust `.exe` that finds every missing or outdated driver on your Windows PC and installs the latest from the vendor.**

![Platform](https://img.shields.io/badge/platform-Windows%2010%2F11%20x64-0078D6?logo=windows)
![Language](https://img.shields.io/badge/language-Rust-DEA584?logo=rust)
![Runtime deps](https://img.shields.io/badge/runtime%20deps-none-brightgreen)
![Installer](https://img.shields.io/badge/silent%20install-yes-blue)
![Tests](https://img.shields.io/badge/tests-115%20unit%20%2B%205%20e2e-success)

</div>

## Features

- Detects **missing hardware drivers** (any device with a problem code, e.g. code 28) and outdated ones across all device classes (audio, LAN, WiFi, Bluetooth, USB, storage, chipset, firmware, display...).
- Vendor providers: NVIDIA (GPU driver + NVIDIA App), AMD (Adrenalin, chipset), Windows Update and the Microsoft Update Catalog.
- Runtimes: Visual C++ Redistributable, DirectX End-User Runtime, .NET Desktop Runtime (installed-version detection via registry, shared-framework folders and `dotnet --list-runtimes`).
- Robust: one failing provider or network error never aborts the others; re-runs are idempotent and nothing is hardcoded to a device list or version.
- Safe: HTTPS-only allowlist, hash and signature checks, restore point before install, `--dry-run` / `--scan-only` never change the system.

A single pure-Rust Windows executable (`DriverForge.exe`) that scans every device on the PC,
finds missing or outdated drivers, fetches the true latest version from the vendor, verifies
the download, and installs it silently. No PowerShell, Python or curl is used at runtime.

## Usage

```
DriverForge [--scan-only] [--dry-run] [--yes] [--only nvidia|amd|windows|all]
            [--json] [--no-restore-point] [--log <path>]
```

| Flag | Meaning |
|------|---------|
| `--scan-only` | Only list devices and installed driver versions. |
| `--dry-run` | Look up and report updates; download and install nothing. |
| `--yes` | Do not ask for confirmation. |
| `--only <set>` | Restrict providers: `nvidia`, `amd`, `windows`, or `all` (default). |
| `--json` | Machine-readable report. |
| `--no-restore-point` | Skip creating a system restore point before installing. |
| `--log <path>` | Write the log to this file. |

Exit codes: `0` everything current/installed, `1` failures, `2` reboot needed, `3` not administrator.

The exe embeds a manifest requesting administrator rights (UAC prompt on launch), long-path
support and a UTF-8 code page.

## How it works

1. **Scan**: enumerates devices through WMI (hardware IDs, class, driver version/date, problem code).
2. **Providers** each return update candidates:
   - NVIDIA GPU driver (NVIDIA lookup API) and NVIDIA App (product page).
   - AMD Radeon Adrenalin and AMD chipset (amd.com pages).
   - Windows Update (COM driver search) and the Microsoft Update Catalog for everything else
     (audio, LAN, WiFi, Bluetooth, USB, storage, firmware, ...).
3. **Compare**: vendor versions are normalised and compared with the installed version.
4. **Download**: HTTPS only, resume and retries, SHA-256 checked when the vendor publishes one.
5. **Install**: a restore point is created, then the silent installer runs with a timeout;
   reboot requirements are reported.

## Safety

- HTTPS only; downloads are restricted to an allowlist of vendor hosts, including across redirects.
- Hash verification where a hash is available; signature checking via WinVerifyTrust.
- A system restore point is created before installing (unless `--no-restore-point`).
- `--scan-only` and `--dry-run` never change the system.
- During a real Windows Update install, the program automatically accepts each driver update's license (AcceptEula).
- Requires administrator rights; refuses to install otherwise.

## Build

Toolchain `stable-x86_64-pc-windows-gnu` (needs `windres` on PATH for the manifest).

```
cargo build --release
cargo test
```

`build.rs` embeds `app.manifest` through `app.rc` using the `embed-resource` crate.
The release profile is size-optimised with LTO, stripped, `panic = "abort"`.

## Sources

NVIDIA AjaxDriverService / lookupValueSearch, nvidia.com NVIDIA App page, amd.com support
pages (browser User-Agent required), Windows Update Agent COM API, Microsoft Update Catalog.
