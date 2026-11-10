# REVIEW.md (Rev) — correctness review. Status: OPEN / FIXED / WONTFIX

Build at review time (own target dir .targets/rev): FAILS — see R1. Tests not yet runnable.

## BLOCKING
| # | Owner | Where | Expected vs actual | Status |
|---|-------|-------|--------------------|--------|
| R1 | Build (Cargo.toml) / Inst | installer.rs:277 | `windows::Win32::System::Restore` unresolved -> add feature `Win32_System_Restore` (also Win32_Security_WinTrust, Win32_Security_Cryptography if missing). Also installer.rs:316 `st.nStatus` is WIN32_ERROR, no Display: use `st.nStatus.0`. Tree does not compile. | OPEN |
| R2 | Inst + Wu + Core | installer.rs:184, main.rs:131 | WindowsUpdate candidates reach `installer::install`, which returns `Skipped("handled by Windows Update sweep")`; nothing in main calls `wupdater::install_updates`. WU drivers are never installed. Expected: main (or install) collects WU candidates' update ids (url field) and calls `wupdater::install_updates` once, mapping reboot_required -> InstalledRebootNeeded. | OPEN |
| R3 | Wu | wupdater.rs:203-205 | `check` ignores `_devices`; every WU entry has `installed: None`, so main treats all as newer. Expected: match entry.hardware_id to a scanned device (hardware_ids / compat_ids) and set `installed` from device.driver_version; skip when `version::is_newer(latest, installed)` is false (avoids reinstalling the same driver). WU Search can offer already-equal drivers. | OPEN |
| R4 | Cat + Inst | catalog.rs:189, installer.rs:120 | Catalog URL ends in `.cab` but kind ZipThenSetup extracts with the `zip` crate; a CAB is not a zip, so `extract_zip` returns "bad zip" and every catalog install fails. Expected: a CAB extractor (e.g. `cab` crate via Build, or Win32 SetupAPI/`expand.exe`) then pnputil. Also catalog passes pnputil args in silent_args that installer ignores for the INF path (fine) but would pass `/add-driver *.inf` to a setup.exe if one exists in the CAB. | OPEN |
| R5 | Net | http.rs:111-113 | `join_url` for protocol-relative redirect `//host/x`: `&base[..scheme_end-2]` yields `https:` and then `trim_start_matches('/')` -> `https:host/x` (missing `//`). Expected `https://host/x`. | OPEN |

## SUGGESTIONS
| # | Owner | Where | Note |
|---|-------|-------|------|
| S1 | NvDrv | nvidia.rs:28-30 | `&s[i+1..i+3]` can panic (non char boundary) when `%` is followed by an ASCII hex digit then a multibyte char; use byte slice parse. |
| S2 | NvDrv | nvidia.rs:135-155 | Duplicates `version::nvidia_from_windows` and `newer`; own copy turns "32.0.16." into 600.00 (version.rs returns None). Use `crate::version`. |
| S3 | NvDrv | nvidia.rs:211 | `-clean` wipes user NVIDIA settings on every update; consider omitting (default `-s -noreboot -noeula`). |
| S4 | NvDrv/AmdGpu | nvidia.rs:174, amd_gpu.rs:81 | Two GPUs with same vendor/name -> duplicate candidate and installer runs twice. Dedupe by url. |
| S5 | NvDrv | nvidia.rs:158 | Requires class "Display"; a GPU with missing driver (class Unknown/empty, code 28) is skipped, so a missing NVIDIA driver is never reported. Accept empty class with VEN_10DE. |
| S6 | AmdGpu | amd_gpu.rs:12 | Hardcoded 9800X3D product page; falls back to generic page that may contain no exe links. OK on this PC. |
| S7 | Scan | scan.rs:190 | problem_devices includes code 22 (user-disabled) and 24; these are not missing drivers. Core report counts them as problems. |
| S8 | Inst | installer.rs:24-27 | `signer_allowed` uses substring contains: "Microsoft" matches any publisher with that word. Use exact CN match ("NVIDIA Corporation", "Advanced Micro Devices, Inc.", "Microsoft Corporation"). |
| S9 | Inst | installer.rs:178 | Installer file lives in %TEMP% (user-writable) between signature check and execute (TOCTOU). Use ProgramData with admin-only ACL, or hold an open handle. |
| S10 | Inst | installer.rs:86 | `read_dir(&d).ok()?` aborts whole find_setup on one unreadable subdir; use `continue`. extract_zip has no size/entry cap (zip bomb). |
| S11 | Net | http.rs:214 | 416 on resume treated as complete without validating the .part belongs to this file; with sha256=None (NVIDIA/AMD) a stale .part gives a corrupt installer. Verify Content-Range `*/total` equals part size, else restart from 0. |
| S12 | Core | main.rs:60 | Admin check also blocks `--dry-run`; only real installs need admin. |
| S13 | Cat | catalog.rs:156-165 | One HTTP request per device + 1.5s sleep, unbounded (100+ devices = minutes). Cap, or skip devices already satisfied by WU. |
| S14 | AmdChip | amd_chipset.rs | Charter lists NPU/other AMD; not present. |
| S15 | NvApp | nvapp.rs:104-105 | `installed_version()` (registry walk) runs twice. |

## Verified OK
version.rs (numeric compare, leading zeros, pre-release, never panics, tests cover 621.14 conversion); scan.rs mapping & WMI property names; amd_gpu/nvapp/amd_chipset regex+compare logic; http https-only on every redirect hop; installer exit-code map, zip-slip protection.
