//! Image names we will not treat as "the agent". If the only process that
//! touched a finding is one of these, the finding is unattributed and labelled
//! "possibly via a service".

use crate::paths::image_basename;

/// Well-known Windows service / daemon images. Compared case-insensitively
/// against the executable basename, with or without `.exe`.
const SERVICE_IMAGES: &[&str] = &[
    "system",
    "registry",
    "memcompression",
    "smss.exe",
    "csrss.exe",
    "wininit.exe",
    "winlogon.exe",
    "services.exe",
    "lsass.exe",
    "svchost.exe",
    "taskhostw.exe",
    "taskeng.exe",
    "msiexec.exe",
    "trustedinstaller.exe",
    "tiworker.exe",
    "wmiprvse.exe",
    "dllhost.exe",
    "svchost",
    "dockerd.exe",
    "docker.exe",
    "com.docker.service",
    "containerd.exe",
    "vmcompute.exe",
    "msmpeng.exe",
    "nissrv.exe",
    "securityhealthservice.exe",
    "searchindexer.exe",
    "usocoreworker.exe",
    "mousocoreworker.exe",
    "usoclient.exe",
    "dwm.exe",
    "spoolsv.exe",
    "fontdrvhost.exe",
    "lsm.exe",
    "dashost.exe",
];

pub fn is_service_image(name_or_path: &str) -> bool {
    let base = image_basename(name_or_path);
    if base.is_empty() {
        return false;
    }
    SERVICE_IMAGES.iter().any(|s| {
        let s = s.to_ascii_lowercase();
        base == s || base.trim_end_matches(".exe") == s.trim_end_matches(".exe")
    })
}

pub fn is_kernel_pid(pid: u32) -> bool {
    pid == 0 || pid == 4
}
