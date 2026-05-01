use classicube_helpers::async_manager;

use crate::component::Component;

#[derive(Default)]
pub struct AsyncManager;

impl Component for AsyncManager {
    fn name(&self) -> &'static str {
        "AsyncManager"
    }

    fn init(&mut self) {
        async_manager::initialize();
    }

    fn free(&mut self) {
        async_manager::shutdown();
    }
}
