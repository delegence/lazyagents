use std::sync::Mutex;

pub static TEST_LOCK: Mutex<()> = Mutex::new(());

pub struct EnvGuard {
    previous_home: Option<String>,
    previous_xdg: Option<String>,
}

impl EnvGuard {
    pub fn new(path: &std::path::Path) -> Self {
        let previous_home = std::env::var("HOME").ok();
        let previous_xdg = std::env::var("XDG_CONFIG_HOME").ok();
        std::env::set_var("HOME", path);
        std::env::set_var("XDG_CONFIG_HOME", path);
        Self {
            previous_home,
            previous_xdg,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(value) = &self.previous_home {
            std::env::set_var("HOME", value);
        } else {
            std::env::remove_var("HOME");
        }

        if let Some(value) = &self.previous_xdg {
            std::env::set_var("XDG_CONFIG_HOME", value);
        } else {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
    }
}
