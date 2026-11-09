//! Path and registry-key normalisation used to join ETW events to findings.

pub fn normalize_path(raw: &str) -> String {
    let s = raw.trim().trim_matches('"').trim();
    if s.is_empty() {
        return String::new();
    }
    let s = strip_nt_prefix(s);
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'/' || c == b'\\' {
            if !out.ends_with('\\') {
                out.push('\\');
            }
            i += 1;
            continue;
        }
        // Decode percent-free; just lowercase ASCII and copy the rest as chars.
        let rest = &s[i..];
        let ch = rest.chars().next().unwrap();
        i += ch.len_utf8();
        for c in ch.to_lowercase() {
            out.push(c);
        }
    }
    out.trim_end_matches('\\').to_string()
}

fn strip_nt_prefix(s: &str) -> &str {
    const PREFIXES: &[&str] = &[
        r"\\?\",
        r"//?/",
        r"\??\",
        r"\\.\",
    ];
    for p in PREFIXES {
        if let Some(rest) = s.strip_prefix(p) {
            if let Some(rest) = rest.strip_prefix("UNC\\").or_else(|| rest.strip_prefix("unc\\")) {
                return rest;
            }
            return rest;
        }
    }
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return rest;
    }
    s
}

/// True when `event_path` and `finding_path` refer to the same file, or when
/// one is a suffix of the other after a drive or device prefix (so a
/// `\Device\HarddiskVolume3\Users\...` event can still meet `C:\Users\...`).
pub fn paths_match(event_path: &str, finding_path: &str) -> bool {
    let a = normalize_path(event_path);
    let b = normalize_path(finding_path);
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a == b {
        return true;
    }
    suffix_equal(&a, &b)
}

fn suffix_equal(a: &str, b: &str) -> bool {
    let a_tail = drop_prefix(a);
    let b_tail = drop_prefix(b);
    if a_tail.is_empty() || b_tail.is_empty() {
        return false;
    }
    a_tail == b_tail || a.ends_with(&format!("\\{b_tail}")) || b.ends_with(&format!("\\{a_tail}"))
}

fn drop_prefix(p: &str) -> &str {
    // C:\foo, \device\harddiskvolume3\foo, \??\c:\foo already stripped
    let p = p.strip_prefix("\\device\\").unwrap_or(p);
    if let Some(rest) = p.strip_prefix("harddiskvolume") {
        if let Some(idx) = rest.find('\\') {
            return &rest[idx + 1..];
        }
    }
    if p.len() >= 2 && p.as_bytes()[1] == b':' {
        return p[2..].trim_start_matches('\\');
    }
    p.trim_start_matches('\\')
}

pub fn normalize_registry(raw: &str) -> String {
    let s = raw.trim().trim_matches('"');
    let mut s = s.replace('/', "\\");
    // Collapse slashes and lowercase.
    s = normalize_path(&s).replace('/', "\\");
    // Already lowercased by normalize_path.
    const MAP: &[(&str, &str)] = &[
        (r"\registry\machine\system\currentcontrolset", r"hklm\system\currentcontrolset"),
        (r"\registry\machine", "hklm"),
        (r"\registry\user\.default", r"hku\.default"),
        (r"hkey_local_machine", "hklm"),
        (r"hkey_current_user", "hkcu"),
        (r"hkey_users", "hku"),
        (r"hkey_classes_root", "hkcr"),
    ];
    let mut out = s;
    for (from, to) in MAP {
        if let Some(rest) = out.strip_prefix(from) {
            out = format!("{to}{rest}");
            break;
        }
    }
    // \registry\user\<sid>\...  → keep as hku\<sid>\... and also offer a
    // suffix without the SID for HKCU findings.
    if let Some(rest) = out.strip_prefix(r"\registry\user\") {
        out = format!(r"hku\{rest}");
    }
    out
}

pub fn registry_match(event_key: &str, finding_key: &str) -> bool {
    let a = normalize_registry(event_key);
    let b = normalize_registry(finding_key);
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a == b {
        return true;
    }
    // HKCU vs HKU\<sid>
    let a_suf = strip_user_hive(&a);
    let b_suf = strip_user_hive(&b);
    if !a_suf.is_empty() && a_suf == b_suf {
        return true;
    }
    a.ends_with(&format!("\\{b}"))
        || b.ends_with(&format!("\\{a}"))
        || a.ends_with(&b)
        || b.ends_with(&a)
}

fn strip_user_hive(key: &str) -> &str {
    if let Some(rest) = key.strip_prefix("hkcu\\") {
        return rest;
    }
    if let Some(rest) = key.strip_prefix("hku\\") {
        if let Some(idx) = rest.find('\\') {
            return &rest[idx + 1..];
        }
    }
    key
}

pub fn image_basename(path_or_name: &str) -> String {
    let n = normalize_path(path_or_name);
    n.rsplit('\\')
        .next()
        .unwrap_or(&n)
        .trim()
        .to_string()
}

pub fn names_equal(a: &str, b: &str) -> bool {
    fn canon(s: &str) -> String {
        let mut n = image_basename(s);
        if n.ends_with(".exe") {
            n.truncate(n.len() - 4);
        }
        n
    }
    let a = canon(a);
    let b = canon(b);
    !a.is_empty() && a == b
}
