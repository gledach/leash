use crate::{PersistCollection, PersistOptions};

pub fn collect(opts: &PersistOptions) -> PersistCollection {
    let mut col = PersistCollection::default();
    col.warnings.push(
        "registry, services, Task Scheduler, and IP Helper are Windows-only; collected hashed user files and process-less env from this host only."
            .into(),
    );
    if let Some(home) = opts.home.as_deref() {
        let files = crate::collect_user_files(home);
        col.profiles = files.profiles;
        col.git_configs = files.git_configs;
        col.ssh = files.ssh;
    }
    // Capture the current process environment as a weak stand-in so Linux tests
    // and a mis-built host binary still produce a snapshot document.
    for (k, v) in std::env::vars() {
        if k.eq_ignore_ascii_case("PATH") || k.starts_with("LEASH_") {
            col.env_user.insert(k, v);
        }
    }
    col
}
