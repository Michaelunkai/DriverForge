#[derive(Debug, Clone, Default)]
pub struct Device {
    pub name: String,
    pub hardware_ids: Vec<String>,
    pub compat_ids: Vec<String>,
    pub class: String,
    pub manufacturer: String,
    pub driver_version: Option<String>,
    pub driver_date: Option<String>,
    pub inf_name: Option<String>,
    pub problem_code: u32,
    pub pnp_id: String,
}

#[derive(Debug, Clone)]
pub enum InstallKind {
    ExeSilent,
    Msi,
    ZipThenSetup,
    WindowsUpdate,
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub vendor: String,
    pub component: String,
    pub installed: Option<String>,
    pub latest: String,
    pub url: String,
    pub sha256: Option<String>,
    pub size_bytes: Option<u64>,
    pub kind: InstallKind,
    pub silent_args: Vec<String>,
    pub needs_reboot_hint: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    UpToDate,
    Installed,
    InstalledRebootNeeded,
    Skipped(String),
    Failed(String),
    DryRun,
}
