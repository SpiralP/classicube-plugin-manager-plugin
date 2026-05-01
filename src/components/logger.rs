use tracing::info;
use tracing_subscriber::EnvFilter;

use crate::component::Component;

#[derive(Default)]
pub struct Logger;

impl Component for Logger {
    fn name(&self) -> &'static str {
        "Logger"
    }

    fn init(&mut self) {
        let debug = cfg!(debug_assertions);
        let other_crates = false;

        let level = if debug { "debug" } else { "info" };
        let my_crate_name = env!("CARGO_PKG_NAME").replace('-', "_");

        let mut filter = EnvFilter::from_default_env();

        if other_crates {
            filter = filter.add_directive(level.parse().unwrap());
        } else {
            filter = filter.add_directive(format!("{}={}", my_crate_name, level).parse().unwrap());
        }

        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .with_thread_ids(false)
            .with_thread_names(false)
            .with_ansi(true)
            .without_time()
            .init();

        info!(
            "{} v{} init",
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION")
        );
    }
}
