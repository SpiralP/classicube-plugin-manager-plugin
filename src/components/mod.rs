pub mod async_manager;
pub mod command;
pub mod loader;
pub mod logger;
pub mod updater;

use crate::{
    component::Component,
    components::{
        async_manager::AsyncManager, command::Command, loader::Loader, logger::Logger,
        updater::Updater,
    },
};

pub fn init_components() -> Vec<Box<dyn Component>> {
    vec![
        Box::new(Logger),
        Box::new(AsyncManager),
        Box::new(Updater),
        Box::new(Loader),
        Box::new(Command),
    ]
}
