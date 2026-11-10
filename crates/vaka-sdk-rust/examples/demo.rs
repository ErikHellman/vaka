//! Run: VAKA_KEY=vk_... VAKA_ENDPOINT=http://localhost:8080 cargo run -p vaka-sdk --example demo
use std::time::Duration;

#[derive(Debug)]
struct ConfigError(String);
impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid config: {}", self.0)
    }
}
impl std::error::Error for ConfigError {}

fn load_config() -> Result<u32, ConfigError> {
    "not-a-number".parse::<u32>().map_err(|e| ConfigError(e.to_string()))
}

fn process(items: &[u32]) -> u32 {
    items[10] // index out of bounds -> panic
}

fn main() {
    let _vaka = vaka_sdk::init(vaka_sdk::Config { release: Some("demo@1.0.0".into()), debug: true, ..Default::default() });
    vaka_sdk::add_breadcrumb("startup", "loading config", vaka_sdk::Level::Info);
    if let Err(e) = load_config() {
        vaka_sdk::capture_error(&e);
    }
    vaka_sdk::track("demo_started", &[("mode", "cli")]);
    vaka_sdk::flush(Duration::from_secs(2));
    if std::env::args().any(|a| a == "--panic") {
        process(&[1, 2, 3]);
    }
}
