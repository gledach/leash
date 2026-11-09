//! Optional logger so attribution can write through `%LOCALAPPDATA%\leash\leash.log`.

use std::sync::Mutex;

static LOGGER: Mutex<Option<fn(&str)>> = Mutex::new(None);

pub fn set_logger(f: fn(&str)) {
    if let Ok(mut guard) = LOGGER.lock() {
        *guard = Some(f);
    }
}

pub fn log(message: impl AsRef<str>) {
    if let Ok(guard) = LOGGER.lock() {
        if let Some(f) = *guard {
            f(message.as_ref());
            return;
        }
    }
    eprintln!("leash: {}", message.as_ref());
}
