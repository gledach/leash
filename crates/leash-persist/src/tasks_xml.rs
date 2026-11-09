//! Best-effort parser for `schtasks /query /xml` output.
//!
//! The command emits a stream of XML documents (one `<Task>` each), sometimes
//! with an XML declaration between them.

use leash_snapshot::ScheduledTask;

pub fn parse_schtasks_xml(xml: &str) -> Vec<ScheduledTask> {
    let chunks = split_task_documents(xml);
    let mut tasks = Vec::new();
    for chunk in chunks {
        if let Some(task) = parse_one(&chunk) {
            tasks.push(task);
        }
    }
    tasks
}

fn split_task_documents(xml: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let lower = xml.to_ascii_lowercase();
    let mut starts = Vec::new();
    let mut idx = 0;
    while let Some(rel) = lower[idx..].find("<task") {
        let at = idx + rel;
        // avoid matching </Task>
        if !lower[at..].starts_with("</") {
            starts.push(at);
        }
        idx = at + 5;
    }
    if starts.is_empty() {
        return chunks;
    }
    starts.push(xml.len());
    for w in starts.windows(2) {
        chunks.push(xml[w[0]..w[1]].to_string());
    }
    chunks
}

fn parse_one(chunk: &str) -> Option<ScheduledTask> {
    let uri = first_tag(chunk, "URI").or_else(|| first_tag(chunk, "uri"));
    let command = first_tag(chunk, "Command").or_else(|| first_tag(chunk, "command"));
    let arguments = first_tag(chunk, "Arguments").or_else(|| first_tag(chunk, "arguments"));
    let user = first_tag(chunk, "UserId")
        .or_else(|| first_tag(chunk, "userid"))
        .or_else(|| first_tag(chunk, "LogonType").and(None));
    let enabled = first_tag(chunk, "Enabled")
        .or_else(|| first_tag(chunk, "enabled"))
        .and_then(|v| match v.to_ascii_lowercase().as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        });
    let path = uri.unwrap_or_default();
    if path.is_empty() && command.is_none() {
        return None;
    }
    let name = path
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(path.as_str())
        .to_string();
    Some(ScheduledTask {
        path: if path.is_empty() { name.clone() } else { path },
        name,
        enabled,
        command,
        arguments,
        user,
    })
}

fn first_tag(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let lower = xml.to_ascii_lowercase();
    let open_l = open.to_ascii_lowercase();
    let close_l = close.to_ascii_lowercase();
    let start = lower.find(&open_l)?;
    let after_open = start + xml[start..].find('>')? + 1;
    let end_rel = lower[after_open..].find(&close_l)?;
    let body = xml[after_open..after_open + end_rel].trim();
    let decoded = body
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'");
    if decoded.is_empty() {
        None
    } else {
        Some(decoded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_two_concatenated_tasks() {
        let xml = r#"<?xml version="1.0"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><URI>\Odd\Agent</URI></RegistrationInfo>
  <Principals><Principal><UserId>Alek</UserId></Principal></Principals>
  <Settings><Enabled>true</Enabled></Settings>
  <Actions><Exec><Command>C:\tools\agent.exe</Command><Arguments>--quiet</Arguments></Exec></Actions>
</Task>
<?xml version="1.0"?>
<Task>
  <RegistrationInfo><URI>\Other\Thing</URI></RegistrationInfo>
  <Settings><Enabled>false</Enabled></Settings>
  <Actions><Exec><Command>C:\tools\other.exe</Command></Exec></Actions>
</Task>
"#;
        let tasks = parse_schtasks_xml(xml);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].path, r"\Odd\Agent");
        assert_eq!(tasks[0].name, "Agent");
        assert_eq!(tasks[0].command.as_deref(), Some(r"C:\tools\agent.exe"));
        assert_eq!(tasks[0].arguments.as_deref(), Some("--quiet"));
        assert_eq!(tasks[0].user.as_deref(), Some("Alek"));
        assert_eq!(tasks[0].enabled, Some(true));
        assert_eq!(tasks[1].enabled, Some(false));
        assert_eq!(tasks[1].name, "Thing");
    }
}
