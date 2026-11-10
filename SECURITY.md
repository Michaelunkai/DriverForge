# DriverForge Security Review (owner: Sec)

## Threat model
Assets: the PC (DriverForge runs elevated, installs kernel drivers = SYSTEM-level code execution).
Attackers: (A1) network MITM / compromised or hijacked CDN or metadata response; (A2) malicious redirect or
URL injected via scraped vendor pages/JSON; (A3) same-user, non-elevated local process (can write %TEMP%);
(A4) malicious archive contents; (A5) planted binaries next to the exe.

Controls required (status = review of http.rs / installer.rs):
| Control | Status |
|---|---|
| HTTPS only, every redirect hop re-checked, max 8 hops | OK (http.rs:13, 80-104). Test-only loopback http is `#[cfg(test)]` |
| TLS cert validation (ureq `tls` = rustls, webpki roots), no pinning | OK |
| Host allowlist (exact host / `.suffix`, parse authority, reject `user@host`) for URLs AND each redirect hop | MISSING (S-1) |
| Authenticode: WinVerifyTrust + exact signer allowlist | WEAK (S-2, S-3) |
| Verify-then-execute without TOCTOU (admin-only work dir) | MISSING (S-4) |
| Archive integrity (zip-slip, bombs, stale files, sibling DLLs) | PARTIAL (S-5) |
| Command injection | OK: `Command::args`, no shell, names sanitized by `safe_component` |
| Absolute paths for msiexec/pnputil | MISSING (S-6) |
| No secrets in logs | OK (no tokens/creds logged; URLs only). Proxy URL is never logged |
| SHA-256 | Optional by design; vendor hash from same server gives no MITM protection; signature is the real control |

## Findings
### BLOCKING
- **S-1 http.rs:80-104, installer.rs:187 (Net/Inst)** No host allowlist. Any https host (and any redirect target) is
  downloaded and executed if signed by an allowlisted name. Add in http.rs `allowed_host(url)` applied in `scheme_ok`
  for every hop: parse host after `://` up to `/ ? # :`, reject if authority contains `@`, lowercase, match exact or
  `.`+suffix against: drivers.amd.com, www.amd.com, amd.com, gfwsl.geforce.com, www.nvidia.com, us.download.nvidia.com,
  international.download.nvidia.com, download.nvidia.com, download.windowsupdate.com, catalog.update.microsoft.com,
  *.delivery.mp.microsoft.com, *.dl.delivery.mp.microsoft.com, *.microsoft.com. Add tests: `https://drivers.amd.com@evil.com`,
  `https://evil.com/drivers.amd.com`, `https://drivers.amd.com.evil.com`.
- **S-2 installer.rs:24-27 (Inst)** `signer_allowed` uses substring `contains`: a cert with CN "Microsoft Evil Ltd" or
  "NVIDIA-fake" passes. Use exact equality on the full simple display name ("NVIDIA Corporation",
  "Advanced Micro Devices, Inc.", "Microsoft Corporation", "Microsoft Windows Hardware Compatibility Publisher" only if needed).
  Add negative tests ("Microsoft Evil", "NVIDIA Fake Inc").
- **S-3 installer.rs:331-410 (Inst)** Signer name is read by a second `CryptQueryObject` open of the file, not from the chain
  WinVerifyTrust validated (second read of file, and signer[0] may differ from the validated signature). Take the signer from
  the trust state: keep `WTD_STATEACTION_VERIFY` open, `WTHelperProvDataFromStateData` -> `WTHelperGetProvSignerFromChain`
  -> `WTHelperGetProvCertFromChain`, then close. Also pass `WTD_REVOKE_WHOLECHAIN` + `WTD_REVOCATION_CHECK_CHAIN` (fall back to
  none only if offline is explicitly accepted).
- **S-4 installer.rs:178,190-194,203-239 (Inst)** Work dir is `%TEMP%\DriverForge\<vendor>`: predictable, writable by any
  same-user medium-integrity process, while DriverForge is elevated. Attacker pre-plants the dir/`.part`/`extracted\setup.exe`,
  or swaps the exe between signature check (line 216/230) and spawn (220/238): local privilege escalation. Fix: use
  `%ProgramData%\DriverForge\work\<random>` (or `%SystemRoot%\Temp`) created fresh with an admin/SYSTEM-only DACL
  (`CreateDirectory` with SDDL `D:P(A;OICI;GA;;;SY)(A;OICI;GA;;;BA)`), fail if it pre-exists, delete after. Re-verify the signature
  immediately before spawn and keep a read-share-deny handle open on the file during verify+launch.
- **S-5 installer.rs:203-226 (Inst)** ZipThenSetup verifies only `setup.exe`; the archive's other files (DLLs sideloaded by the
  signed setup.exe, drivers) are unverified, and with `sha256 == None` nothing authenticates the zip. Also `pnputil /add-driver *.inf`
  installs ANY validly signed third-party driver (BYOVD) with no signer allowlist. Require `c.sha256.is_some()` for ZipThenSetup/INF
  paths (else Failed), or verify every .exe/.dll/.sys/.cat in the extraction against the allowlist. Also: `extract_zip` must start
  from an empty directory (stale files, finding S-4), and cap entry count / total uncompressed size (zip bomb).

### SUGGESTION
- **S-6 installer.rs:65,223** `msiexec.exe`/`pnputil.exe` resolved by name; Rust searches the exe directory first -> planted
  binary. Use `%SystemRoot%\System32\...` absolute paths.
- **S-7 http.rs:226-272 (Net)** No max download size; add cap (e.g. 4 GB or `size_bytes`+slack). `416` (line 92/232) is treated
  as "complete" even if the stale `.part` is larger than the real file; with no sha it is accepted. On 416 delete `.part`
  and refetch from 0. Key `.part` by URL hash so a stale part from another version is not resumed/appended.
- **S-8 http.rs:106-123 (Net)** `join_url` for `//host/path`: `trim_start_matches('/')` yields `https:host/path` (broken, fails closed).
  Fix to `format!("{}//{}", scheme_with_colon, rest)`. Also reject redirects containing `\\`, control chars.
- **S-9 http.rs:25-62 (Net)** Proxy taken from env/registry; TLS still validated so acceptable, but never log the proxy string
  (may hold credentials) and ignore `http://` proxies carrying `user:pass@`.
- **S-10 installer.rs:147-170 (Inst)** On timeout only the parent is killed; use a Job object (KILL_ON_JOB_CLOSE) so installer
  child processes die too.
- **S-11 installer.rs:119-144 (Inst)** Reject entries with `:` (NTFS ADS) and reserved device names; `enclosed_name` does not cover them.

### Build / supply chain (Build)
- app.manifest: requireAdministrator, uiAccess=false, no auto-elevation tricks. OK.
- Cargo.toml: minimal deps, ureq uses rustls, zip has no unneeded codecs. OK.
- SUGGESTION: commit Cargo.lock and build releases with `cargo build --release --locked`; run `cargo audit` before release; sign DriverForge.exe (Authenticode) when distributing.

## Status
Reviewed: http.rs, installer.rs, catalog/nvidia/nvapp/amd_chipset/wupdater/scan/Cargo/manifest (provider files by pattern search for spawn/network/query building).
BLOCKING closed: S-1 (host allowlist every hop; loopback test switch compiled out of release), S-2 (exact full-name signer allowlist),
S-4 (fresh per-run ProgramData folder, protected DACL SYSTEM+Administrators, create fails if exists, no temp fallback, deleted afterwards),
S-5 (setup.exe archives require sha256; .cat files signature-checked before pnputil; CAB via System32 expand.exe; contained-path check; 20k file cap).
Residual (accepted, SUGGESTION): log dir ProgramData\DriverForge\logs refuses junctions/symlinks but has no protected DACL (small check-then-use race); S-3 signer read from a second open of the file (mitigated by the protected folder; use WTHelper chain later);
revocation checking off; no Job object on timeout; amd_gpu.rs only pattern-checked; Authenticode-sign the release exe.
