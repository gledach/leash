use crate::{PersistCollection, PersistOptions};
use leash_snapshot::{ListeningPort, ProcessEntry, RunKeyEntry, ServiceEntry};
use std::collections::{BTreeMap, HashMap};
use std::mem;
use std::sync::Mutex;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use windows::core::{BOOL, HRESULT, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_INSUFFICIENT_BUFFER, ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS,
    ERROR_SUCCESS, FILETIME, HANDLE, HWND, LPARAM, WIN32_ERROR,
};
use windows::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, GetExtendedUdpTable, TCP_TABLE_OWNER_PID_ALL, UDP_TABLE_OWNER_PID,
};
use windows::Win32::Networking::WinSock::{AF_INET, AF_INET6};
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE,
    KEY_READ, REG_DWORD, REG_EXPAND_SZ, REG_SZ, REG_VALUE_TYPE,
};
use windows::Win32::System::Services::{
    CloseServiceHandle, EnumServicesStatusExW, OpenSCManagerW, OpenServiceW, QueryServiceConfigW,
    ENUM_SERVICE_STATUS_PROCESSW, QUERY_SERVICE_CONFIGW, SC_ENUM_PROCESS_INFO, SC_HANDLE,
    SC_MANAGER_CONNECT, SC_MANAGER_ENUMERATE_SERVICE, SERVICE_AUTO_START, SERVICE_BOOT_START,
    SERVICE_CONTINUE_PENDING, SERVICE_DEMAND_START, SERVICE_DISABLED, SERVICE_PAUSED,
    SERVICE_PAUSE_PENDING, SERVICE_QUERY_CONFIG, SERVICE_RUNNING, SERVICE_START_PENDING,
    SERVICE_STATE_ALL, SERVICE_STOPPED, SERVICE_STOP_PENDING, SERVICE_SYSTEM_START, SERVICE_WIN32,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetProcessTimes, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW,
    PROCESS_NAME_WIN32, PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_VM_READ,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const PROCESS_COMMAND_LINE_INFORMATION: u32 = 60;

static PROCESS_INFO_FAILURES: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub fn take_process_info_failures() -> Vec<String> {
    PROCESS_INFO_FAILURES
        .lock()
        .map(|mut v| std::mem::take(&mut *v))
        .unwrap_or_default()
}

fn clear_process_info_failures() {
    if let Ok(mut v) = PROCESS_INFO_FAILURES.lock() {
        v.clear();
    }
}

fn note_process_info_failure(msg: String) {
    if let Ok(mut v) = PROCESS_INFO_FAILURES.lock() {
        if v.len() < 80 {
            v.push(msg);
        }
    }
}

pub fn is_elevated() -> bool {
    unsafe {
        let mut handle = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut handle).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut returned = 0u32;
        let ok = GetTokenInformation(
            handle,
            TokenElevation,
            Some((&mut elevation as *mut TOKEN_ELEVATION).cast()),
            mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        );
        let _ = CloseHandle(handle);
        ok.is_ok() && elevation.TokenIsElevated != 0
    }
}

pub fn collect(opts: &PersistOptions) -> PersistCollection {
    let mut col = PersistCollection::default();
    match collect_run_keys() {
        Ok(keys) => col.run_keys = keys,
        Err(err) => col.warnings.push(format!("registry Run keys: {err}")),
    }
    match collect_tasks() {
        Ok((tasks, notes)) => {
            col.scheduled_tasks = tasks;
            col.warnings.extend(notes);
        }
        Err(err) => col.warnings.push(format!("scheduled tasks: {err}")),
    }
    match collect_services() {
        Ok(svcs) => col.services = svcs,
        Err(err) => col.warnings.push(format!("services: {err}")),
    }
    match collect_env() {
        Ok((user, system)) => {
            col.env_user = user;
            col.env_system = system;
        }
        Err(err) => col.warnings.push(format!("environment: {err}")),
    }
    match collect_processes() {
        Ok(procs) => col.processes = procs,
        Err(err) => col.warnings.push(format!("processes: {err}")),
    }
    match collect_ports() {
        Ok(mut ports) => {
            fill_port_names(&mut ports, &col.processes);
            col.ports = ports;
        }
        Err(err) => col.warnings.push(format!("listening ports: {err}")),
    }
    let home = opts.home.clone().or_else(default_home);
    if let Some(home) = home {
        let files = crate::collect_user_files(&home);
        col.profiles = files.profiles;
        col.git_configs = files.git_configs;
        col.ssh = files.ssh;
    } else {
        col.warnings
            .push("could not determine the user profile path".into());
    }
    col
}

pub fn doctor_lines() -> Vec<String> {
    let mut lines = vec![format!(
        "Administrator: {}",
        if is_elevated() { "yes" } else { "no" }
    )];
    match collect_run_keys() {
        Ok(keys) => lines.push(format!("Run/RunOnce values visible: {}", keys.len())),
        Err(err) => lines.push(format!("Run keys: {err}")),
    }
    match collect_services() {
        Ok(svcs) => lines.push(format!("services enumerated: {}", svcs.len())),
        Err(err) => lines.push(format!("services: {err}")),
    }
    match collect_ports() {
        Ok(ports) => lines.push(format!("listening TCP/UDP sockets: {}", ports.len())),
        Err(err) => lines.push(format!("ports: {err}")),
    }
    lines
}

fn default_home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").map(PathBuf::from)
}

fn collect_run_keys() -> anyhow::Result<Vec<RunKeyEntry>> {
    let mut out = Vec::new();
    const KEYS: &[(&str, HKEY, &str)] = &[
        (
            "HKCU",
            HKEY_CURRENT_USER,
            r"Software\Microsoft\Windows\CurrentVersion\Run",
        ),
        (
            "HKCU",
            HKEY_CURRENT_USER,
            r"Software\Microsoft\Windows\CurrentVersion\RunOnce",
        ),
        (
            "HKLM",
            HKEY_LOCAL_MACHINE,
            r"Software\Microsoft\Windows\CurrentVersion\Run",
        ),
        (
            "HKLM",
            HKEY_LOCAL_MACHINE,
            r"Software\Microsoft\Windows\CurrentVersion\RunOnce",
        ),
        (
            "HKLM",
            HKEY_LOCAL_MACHINE,
            r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Run",
        ),
    ];
    for (hive, handle, subkey) in KEYS {
        let short = subkey.rsplit('\\').next().unwrap_or(subkey);
        match enum_reg_values(*handle, subkey) {
            Ok(vals) => {
                for (name, value) in vals {
                    out.push(RunKeyEntry {
                        hive: (*hive).to_string(),
                        key: short.to_string(),
                        name,
                        value,
                    });
                }
            }
            Err(err) => {
                // WOW6432Node or a missing RunOnce is not fatal.
                if *hive == "HKLM" && subkey.contains("WOW6432Node") {
                    continue;
                }
                let _ = err;
            }
        }
    }
    Ok(out)
}

fn enum_reg_values(hive: HKEY, subkey: &str) -> anyhow::Result<Vec<(String, String)>> {
    unsafe {
        let mut key = HKEY::default();
        let wide = wide(subkey);
        let status = RegOpenKeyExW(hive, PCWSTR(wide.as_ptr()), Some(0), KEY_READ, &mut key);
        if status != ERROR_SUCCESS {
            anyhow::bail!("RegOpenKeyExW({subkey}) = {status:?}");
        }
        let mut values = Vec::new();
        for index in 0u32.. {
            let mut name = [0u16; 512];
            let mut name_len = name.len() as u32;
            let mut typ = 0u32;
            let mut data = [0u8; 8192];
            let mut data_len = data.len() as u32;
            let status = RegEnumValueW(
                key,
                index,
                Some(PWSTR(name.as_mut_ptr())),
                &mut name_len,
                None,
                Some(&mut typ as *mut u32),
                Some(data.as_mut_ptr()),
                Some(&mut data_len),
            );
            if status == ERROR_NO_MORE_ITEMS {
                break;
            }
            if status == ERROR_MORE_DATA {
                continue;
            }
            if status != ERROR_SUCCESS {
                break;
            }
            let name = utf16_to_string(&name[..name_len as usize]);
            let value = decode_reg_value(REG_VALUE_TYPE(typ), &data[..data_len as usize]);
            values.push((name, value));
        }
        let _ = RegCloseKey(key);
        Ok(values)
    }
}

fn decode_reg_value(typ: REG_VALUE_TYPE, data: &[u8]) -> String {
    if typ == REG_SZ || typ == REG_EXPAND_SZ {
        if data.len() < 2 {
            return String::new();
        }
        let u16s: Vec<u16> = data
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .take_while(|c| *c != 0)
            .collect();
        String::from_utf16_lossy(&u16s)
    } else if typ == REG_DWORD && data.len() >= 4 {
        u32::from_le_bytes(data[..4].try_into().unwrap_or([0; 4])).to_string()
    } else {
        format!("(type {:?} {} bytes)", typ.0, data.len())
    }
}

fn collect_env() -> anyhow::Result<(BTreeMap<String, String>, BTreeMap<String, String>)> {
    let user = enum_reg_values(HKEY_CURRENT_USER, r"Environment")?
        .into_iter()
        .collect();
    let system = enum_reg_values(
        HKEY_LOCAL_MACHINE,
        r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
    )?
    .into_iter()
    .collect();
    Ok((user, system))
}

fn collect_tasks() -> anyhow::Result<(Vec<leash_snapshot::ScheduledTask>, Vec<String>)> {
    let mut notes = Vec::new();
    match run_schtasks_xml() {
        Ok(xml) => Ok((crate::parse_schtasks_xml(&xml), notes)),
        Err(err) => {
            notes.push(format!("schtasks /query /xml failed: {err}"));
            Ok((Vec::new(), notes))
        }
    }
}

fn run_schtasks_xml() -> anyhow::Result<String> {
    let output = Command::new("schtasks")
        .args(["/query", "/xml"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()?;
    if !output.status.success() && output.stdout.is_empty() {
        anyhow::bail!(
            "schtasks exited {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn collect_services() -> anyhow::Result<Vec<ServiceEntry>> {
    unsafe {
        let scm = OpenSCManagerW(
            None,
            None,
            SC_MANAGER_CONNECT | SC_MANAGER_ENUMERATE_SERVICE,
        )?;
        let mut bytes_needed = 0u32;
        let mut resume = 0u32;
        let mut count = 0u32;
        let first = EnumServicesStatusExW(
            scm,
            SC_ENUM_PROCESS_INFO,
            SERVICE_WIN32,
            SERVICE_STATE_ALL,
            None,
            &mut bytes_needed,
            &mut count,
            Some(&mut resume),
            None,
        );
        // We expect ERROR_MORE_DATA / INSUFFICIENT_BUFFER.
        let _ = first;
        if bytes_needed == 0 {
            let _ = CloseServiceHandle(scm);
            return Ok(Vec::new());
        }
        let mut buf = vec![0u8; bytes_needed as usize];
        resume = 0;
        EnumServicesStatusExW(
            scm,
            SC_ENUM_PROCESS_INFO,
            SERVICE_WIN32,
            SERVICE_STATE_ALL,
            Some(&mut buf),
            &mut bytes_needed,
            &mut count,
            Some(&mut resume),
            None,
        )?;
        let mut services = Vec::new();
        if count > 0 {
            let slice = std::slice::from_raw_parts(
                buf.as_ptr() as *const ENUM_SERVICE_STATUS_PROCESSW,
                count as usize,
            );
            for item in slice {
                let name = pcwstr_to_string(item.lpServiceName);
                let display = pcwstr_to_string(item.lpDisplayName);
                let state = service_state(item.ServiceStatusProcess.dwCurrentState.0);
                let mut entry = ServiceEntry {
                    name: name.clone(),
                    display_name: if display.is_empty() {
                        None
                    } else {
                        Some(display)
                    },
                    start_type: None,
                    state: Some(state),
                    binary_path: None,
                    account: None,
                };
                if let Ok((start, path, account)) = query_service_config(scm, &name) {
                    entry.start_type = start;
                    entry.binary_path = path;
                    entry.account = account;
                }
                services.push(entry);
            }
        }
        let _ = CloseServiceHandle(scm);
        Ok(services)
    }
}

fn query_service_config(
    scm: SC_HANDLE,
    name: &str,
) -> anyhow::Result<(Option<String>, Option<String>, Option<String>)> {
    unsafe {
        let wide = wide(name);
        let svc = OpenServiceW(scm, PCWSTR(wide.as_ptr()), SERVICE_QUERY_CONFIG)?;
        let mut needed = 0u32;
        let _ = QueryServiceConfigW(svc, None, 0, &mut needed);
        if needed == 0 {
            let _ = CloseServiceHandle(svc);
            anyhow::bail!("QueryServiceConfigW size failed");
        }
        let mut buf = vec![0u8; needed as usize];
        QueryServiceConfigW(
            svc,
            Some(buf.as_mut_ptr() as *mut QUERY_SERVICE_CONFIGW),
            needed,
            &mut needed,
        )?;
        let cfg = &*(buf.as_ptr() as *const QUERY_SERVICE_CONFIGW);
        let start = match cfg.dwStartType {
            SERVICE_BOOT_START => Some("boot".into()),
            SERVICE_SYSTEM_START => Some("system".into()),
            SERVICE_AUTO_START => Some("auto".into()),
            SERVICE_DEMAND_START => Some("manual".into()),
            SERVICE_DISABLED => Some("disabled".into()),
            _ => Some(format!("{}", cfg.dwStartType.0)),
        };
        let path = pcwstr_opt(cfg.lpBinaryPathName);
        let account = pcwstr_opt(cfg.lpServiceStartName);
        let _ = CloseServiceHandle(svc);
        Ok((start, path, account))
    }
}

fn service_state(state: u32) -> String {
    match state {
        x if x == SERVICE_STOPPED.0 => "stopped".into(),
        x if x == SERVICE_START_PENDING.0 => "start pending".into(),
        x if x == SERVICE_STOP_PENDING.0 => "stop pending".into(),
        x if x == SERVICE_RUNNING.0 => "running".into(),
        x if x == SERVICE_CONTINUE_PENDING.0 => "continue pending".into(),
        x if x == SERVICE_PAUSE_PENDING.0 => "pause pending".into(),
        x if x == SERVICE_PAUSED.0 => "paused".into(),
        other => other.to_string(),
    }
}

fn collect_ports() -> anyhow::Result<Vec<ListeningPort>> {
    let mut ports = Vec::new();
    ports.extend(tcp_table(false)?);
    ports.extend(tcp_table(true)?);
    ports.extend(udp_table(false)?);
    ports.extend(udp_table(true)?);
    ports.sort();
    ports.dedup();
    Ok(ports)
}

fn tcp_table(v6: bool) -> anyhow::Result<Vec<ListeningPort>> {
    let af = if v6 { AF_INET6 } else { AF_INET };
    let mut size = 0u32;
    unsafe {
        let _ = GetExtendedTcpTable(
            None,
            &mut size,
            true,
            af.0 as u32,
            TCP_TABLE_OWNER_PID_ALL,
            0,
        );
        if size == 0 {
            return Ok(Vec::new());
        }
        let mut buf = vec![0u8; size as usize];
        let code = GetExtendedTcpTable(
            Some(buf.as_mut_ptr().cast()),
            &mut size,
            true,
            af.0 as u32,
            TCP_TABLE_OWNER_PID_ALL,
            0,
        );
        if WIN32_ERROR(code) != ERROR_SUCCESS && code != 0 {
            if WIN32_ERROR(code) == ERROR_INSUFFICIENT_BUFFER {
                buf.resize(size as usize, 0);
                let code = GetExtendedTcpTable(
                    Some(buf.as_mut_ptr().cast()),
                    &mut size,
                    true,
                    af.0 as u32,
                    TCP_TABLE_OWNER_PID_ALL,
                    0,
                );
                if code != 0 {
                    anyhow::bail!("GetExtendedTcpTable {}", code);
                }
            } else {
                anyhow::bail!("GetExtendedTcpTable {}", code);
            }
        }
        Ok(parse_tcp_table(&buf, v6))
    }
}

fn udp_table(v6: bool) -> anyhow::Result<Vec<ListeningPort>> {
    let af = if v6 { AF_INET6 } else { AF_INET };
    let mut size = 0u32;
    unsafe {
        let _ = GetExtendedUdpTable(None, &mut size, true, af.0 as u32, UDP_TABLE_OWNER_PID, 0);
        if size == 0 {
            return Ok(Vec::new());
        }
        let mut buf = vec![0u8; size as usize];
        let code = GetExtendedUdpTable(
            Some(buf.as_mut_ptr().cast()),
            &mut size,
            true,
            af.0 as u32,
            UDP_TABLE_OWNER_PID,
            0,
        );
        if code != 0 {
            anyhow::bail!("GetExtendedUdpTable {code}");
        }
        Ok(parse_udp_table(&buf, v6))
    }
}

const MIB_TCP_STATE_LISTEN: u32 = 2;

fn parse_tcp_table(buf: &[u8], v6: bool) -> Vec<ListeningPort> {
    if buf.len() < 4 {
        return Vec::new();
    }
    let count = u32::from_le_bytes(buf[0..4].try_into().unwrap()) as usize;
    let mut out = Vec::new();
    if !v6 {
        // MIB_TCPROW_OWNER_PID: 6 * u32 = 24, table starts at offset 4
        let row = 24usize;
        for i in 0..count {
            let off = 4 + i * row;
            if off + row > buf.len() {
                break;
            }
            let state = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap());
            if state != MIB_TCP_STATE_LISTEN {
                continue;
            }
            let addr = u32::from_le_bytes(buf[off + 4..off + 8].try_into().unwrap());
            let port = u32::from_le_bytes(buf[off + 8..off + 12].try_into().unwrap());
            let pid = u32::from_le_bytes(buf[off + 20..off + 24].try_into().unwrap());
            out.push(ListeningPort {
                protocol: "tcp".into(),
                local_addr: ipv4(addr),
                local_port: nport(port),
                pid,
                process_name: None,
                state: Some("LISTEN".into()),
            });
        }
    } else {
        // MIB_TCP6ROW_OWNER_PID: 16 addr + 4 scope + 4 port + 16 rem + 4 rscope + 4 rport + 4 state + 4 pid = 56
        let row = 56usize;
        for i in 0..count {
            let off = 4 + i * row;
            if off + row > buf.len() {
                break;
            }
            let addr = &buf[off..off + 16];
            let port = u32::from_le_bytes(buf[off + 20..off + 24].try_into().unwrap());
            let state = u32::from_le_bytes(buf[off + 48..off + 52].try_into().unwrap());
            let pid = u32::from_le_bytes(buf[off + 52..off + 56].try_into().unwrap());
            if state != MIB_TCP_STATE_LISTEN {
                continue;
            }
            out.push(ListeningPort {
                protocol: "tcp".into(),
                local_addr: ipv6(addr),
                local_port: nport(port),
                pid,
                process_name: None,
                state: Some("LISTEN".into()),
            });
        }
    }
    out
}

fn parse_udp_table(buf: &[u8], v6: bool) -> Vec<ListeningPort> {
    if buf.len() < 4 {
        return Vec::new();
    }
    let count = u32::from_le_bytes(buf[0..4].try_into().unwrap()) as usize;
    let mut out = Vec::new();
    if !v6 {
        // MIB_UDPROW_OWNER_PID: addr + port + pid = 12
        let row = 12usize;
        for i in 0..count {
            let off = 4 + i * row;
            if off + row > buf.len() {
                break;
            }
            let addr = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap());
            let port = u32::from_le_bytes(buf[off + 4..off + 8].try_into().unwrap());
            let pid = u32::from_le_bytes(buf[off + 8..off + 12].try_into().unwrap());
            out.push(ListeningPort {
                protocol: "udp".into(),
                local_addr: ipv4(addr),
                local_port: nport(port),
                pid,
                process_name: None,
                state: None,
            });
        }
    } else {
        // MIB_UDP6ROW_OWNER_PID: 16 addr + 4 scope + 4 port + 4 pid = 28
        let row = 28usize;
        for i in 0..count {
            let off = 4 + i * row;
            if off + row > buf.len() {
                break;
            }
            let addr = &buf[off..off + 16];
            let port = u32::from_le_bytes(buf[off + 20..off + 24].try_into().unwrap());
            let pid = u32::from_le_bytes(buf[off + 24..off + 28].try_into().unwrap());
            out.push(ListeningPort {
                protocol: "udp".into(),
                local_addr: ipv6(addr),
                local_port: nport(port),
                pid,
                process_name: None,
                state: None,
            });
        }
    }
    out
}

fn ipv4(addr: u32) -> String {
    let b = addr.to_le_bytes();
    format!("{}.{}.{}.{}", b[0], b[1], b[2], b[3])
}

fn ipv6(bytes: &[u8]) -> String {
    if bytes.iter().all(|b| *b == 0) {
        return "::".into();
    }
    let mut parts = Vec::new();
    for c in bytes.as_chunks::<2>().0 {
        parts.push(format!("{:x}", u16::from_be_bytes([c[0], c[1]])));
    }
    format!("[{}]", parts.join(":"))
}

fn nport(p: u32) -> u16 {
    ((p) as u16).to_be()
}

fn fill_port_names(ports: &mut [ListeningPort], procs: &[ProcessEntry]) {
    let map: BTreeMap<u32, String> = procs.iter().map(|p| (p.pid, p.name.clone())).collect();
    for port in ports {
        if port.process_name.is_none() {
            port.process_name = map.get(&port.pid).cloned();
        }
    }
}

pub fn collect_processes() -> anyhow::Result<Vec<ProcessEntry>> {
    clear_process_info_failures();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)?;
        let mut entry = PROCESSENTRY32W {
            dwSize: mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut out = Vec::new();
        if Process32FirstW(snap, &mut entry).is_ok() {
            loop {
                let pid = entry.th32ProcessID;
                let ppid = entry.th32ParentProcessID;
                let name = utf16_to_string(&entry.szExeFile);
                let (exe, cmd, created, cwd) = process_details(pid, &name);
                out.push(ProcessEntry {
                    pid,
                    ppid: Some(ppid),
                    name,
                    exe,
                    command_line: cmd,
                    create_time: created,
                    cwd,
                });
                if Process32NextW(snap, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
        Ok(out)
    }
}

fn process_details(pid: u32, name: &str) -> (Option<String>, Option<String>, Option<i64>, Option<String>) {
    if pid == 0 {
        return (None, None, None, None);
    }
    unsafe {
        let access =
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_QUERY_INFORMATION | PROCESS_VM_READ;
        let handle = match OpenProcess(access, false, pid) {
            Ok(h) => h,
            Err(_) => match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                Ok(h) => h,
                Err(_) => {
                    let err = GetLastError();
                    if pid > 4 {
                        note_process_info_failure(format!(
                            "pid {pid} ({name}): OpenProcess failed (status {}); often access denied unless elevated",
                            err.0
                        ));
                    }
                    return (None, None, None, None);
                }
            },
        };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let exe = if QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .is_ok()
        {
            Some(utf16_to_string(&buf[..len as usize]))
        } else {
            None
        };
        let created = process_create_time(handle);
        let cmd = process_command_line(handle);
        if cmd.is_none() && pid > 4 {
            note_process_info_failure(format!(
                "pid {pid} ({name}): command line unavailable (NtQueryInformationProcess); often access denied unless elevated"
            ));
        }
        let cwd = process_cwd(handle);
        if cwd.is_none() && pid > 4 {
            // Not every process exposes a cwd; only log when we already had a handle.
            // Keep this quieter than command-line failures: many system processes have no usable PEB.
        }
        let _ = CloseHandle(handle);
        (exe, cmd, created, cwd)
    }
}

fn process_create_time(handle: HANDLE) -> Option<i64> {
    unsafe {
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user).ok()?;
        let ticks = ((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64;
        // FILETIME is 100-ns intervals since 1601-01-01.
        let unix = (ticks as i64 / 10_000_000) - 11_644_473_600;
        Some(unix)
    }
}


fn process_cwd(handle: HANDLE) -> Option<String> {
    // x64 PEB / RTL_USER_PROCESS_PARAMETERS offsets. leash ships only as x86_64.
    const PROCESS_BASIC_INFORMATION: u32 = 0;
    const PEB_PROCESS_PARAMETERS_OFFSET: usize = 0x20;
    const PARAMS_CURRENT_DIRECTORY_OFFSET: usize = 0x38;
    type NtQueryInformationProcess =
        unsafe extern "system" fn(HANDLE, u32, *mut core::ffi::c_void, u32, *mut u32) -> i32;
    type NtReadVirtualMemory = unsafe extern "system" fn(
        HANDLE,
        *const core::ffi::c_void,
        *mut core::ffi::c_void,
        usize,
        *mut usize,
    ) -> i32;
    #[repr(C)]
    struct ProcessBasicInformation {
        _reserved1: *mut core::ffi::c_void,
        peb_base: *mut core::ffi::c_void,
        _reserved2: [*mut core::ffi::c_void; 2],
        _pid: usize,
        _reserved3: *mut core::ffi::c_void,
    }
    unsafe {
        let module =
            windows::Win32::System::LibraryLoader::GetModuleHandleW(windows::core::w!("ntdll.dll"))
                .ok()?;
        let q = windows::Win32::System::LibraryLoader::GetProcAddress(
            module,
            windows::core::s!("NtQueryInformationProcess"),
        )?;
        let r = windows::Win32::System::LibraryLoader::GetProcAddress(
            module,
            windows::core::s!("NtReadVirtualMemory"),
        )?;
        let ntqip: NtQueryInformationProcess = mem::transmute(q);
        let ntrvm: NtReadVirtualMemory = mem::transmute(r);
        let mut pbi = ProcessBasicInformation {
            _reserved1: core::ptr::null_mut(),
            peb_base: core::ptr::null_mut(),
            _reserved2: [core::ptr::null_mut(); 2],
            _pid: 0,
            _reserved3: core::ptr::null_mut(),
        };
        let mut ret = 0u32;
        let status = ntqip(
            handle,
            PROCESS_BASIC_INFORMATION,
            (&mut pbi as *mut ProcessBasicInformation).cast(),
            mem::size_of::<ProcessBasicInformation>() as u32,
            &mut ret,
        );
        if status < 0 || pbi.peb_base.is_null() {
            return None;
        }
        let read = |addr: *const core::ffi::c_void, buf: &mut [u8]| -> bool {
            let mut n = 0usize;
            ntrvm(handle, addr, buf.as_mut_ptr().cast(), buf.len(), &mut n) >= 0 && n == buf.len()
        };
        let mut params_ptr = [0u8; 8];
        if !read(
            (pbi.peb_base as usize + PEB_PROCESS_PARAMETERS_OFFSET) as *const _,
            &mut params_ptr,
        ) {
            return None;
        }
        let params = usize::from_le_bytes(params_ptr);
        if params == 0 {
            return None;
        }
        let mut us_bytes = [0u8; 16];
        if !read(
            (params + PARAMS_CURRENT_DIRECTORY_OFFSET) as *const _,
            &mut us_bytes,
        ) {
            return None;
        }
        let length = u16::from_le_bytes(us_bytes[0..2].try_into().ok()?) as usize;
        let buffer = usize::from_le_bytes(us_bytes[8..16].try_into().ok()?);
        if length == 0 || buffer == 0 || length > 4096 {
            return None;
        }
        let mut wide = vec![0u16; length / 2];
        let byte_len = wide.len() * 2;
        let mut n = 0usize;
        if ntrvm(
            handle,
            buffer as *const _,
            wide.as_mut_ptr().cast(),
            byte_len,
            &mut n,
        ) < 0
            || n < 2
        {
            return None;
        }
        let used = (n / 2).min(wide.len());
        let s = String::from_utf16_lossy(&wide[..used]).trim_end_matches('\0').trim().to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
}

fn process_command_line(handle: HANDLE) -> Option<String> {
    type NtQueryInformationProcess =
        unsafe extern "system" fn(HANDLE, u32, *mut core::ffi::c_void, u32, *mut u32) -> i32;
    unsafe {
        let module =
            windows::Win32::System::LibraryLoader::GetModuleHandleW(windows::core::w!("ntdll.dll"))
                .ok()?;
        let proc = windows::Win32::System::LibraryLoader::GetProcAddress(
            module,
            windows::core::s!("NtQueryInformationProcess"),
        )?;
        let ntqip: NtQueryInformationProcess = mem::transmute(proc);
        let mut buf = vec![0u8; 32 * 1024];
        let mut ret = 0u32;
        let status = ntqip(
            handle,
            PROCESS_COMMAND_LINE_INFORMATION,
            buf.as_mut_ptr().cast(),
            buf.len() as u32,
            &mut ret,
        );
        if status < 0 || ret < 16 {
            return None;
        }
        let length = u16::from_le_bytes(buf[0..2].try_into().ok()?) as usize;
        if length == 0 || length > ret as usize {
            return None;
        }
        let ptr = usize::from_le_bytes(buf[8..16].try_into().ok()?);
        let base = buf.as_ptr() as usize;
        if ptr < base || ptr + length > base + buf.len() {
            return None;
        }
        let off = ptr - base;
        let wide = std::slice::from_raw_parts(buf[off..].as_ptr() as *const u16, length / 2);
        Some(String::from_utf16_lossy(wide))
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn utf16_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

fn pcwstr_to_string(p: impl AsRawWide) -> String {
    let ptr = p.as_raw_wide();
    if ptr.is_null() {
        String::new()
    } else {
        unsafe { PCWSTR(ptr).to_string().unwrap_or_default() }
    }
}

fn pcwstr_opt(p: impl AsRawWide) -> Option<String> {
    let s = pcwstr_to_string(p);
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

trait AsRawWide {
    fn as_raw_wide(&self) -> *const u16;
}

impl AsRawWide for PCWSTR {
    fn as_raw_wide(&self) -> *const u16 {
        self.0
    }
}

impl AsRawWide for PWSTR {
    fn as_raw_wide(&self) -> *const u16 {
        self.0
    }
}

// Types that may need name fixes at compile time are referenced here so we
// can drop unused imports after the first Windows build.
#[allow(dead_code)]
fn _keep(h: HRESULT) -> HRESULT {
    h
}


pub fn list_window_titles() -> HashMap<u32, String> {
    let mut map = HashMap::new();
    unsafe {
        let _ = EnumWindows(
            Some(enum_windows_proc),
            LPARAM(&mut map as *mut HashMap<u32, String> as isize),
        );
    }
    map
}

unsafe extern "system" fn enum_windows_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let map = unsafe { &mut *(lparam.0 as *mut HashMap<u32, String>) };
    if !IsWindowVisible(hwnd).as_bool() {
        return BOOL(1);
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == 0 {
        return BOOL(1);
    }
    let mut buf = [0u16; 512];
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    if n <= 0 {
        return BOOL(1);
    }
    let title = utf16_to_string(&buf[..n as usize]);
    if title.is_empty() {
        return BOOL(1);
    }
    match map.get(&pid) {
        Some(old) if old.len() >= title.len() => {}
        _ => {
            map.insert(pid, title);
        }
    }
    BOOL(1)
}

pub fn list_processes() -> Vec<leash_snapshot::ProcessEntry> {
    collect_processes().unwrap_or_default()
}
