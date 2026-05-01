pub mod async_manager;
pub mod logger;
pub mod updater;

use crate::{
    component::Component,
    components::{async_manager::AsyncManager, logger::Logger, updater::Updater},
};

pub fn init_components() -> Vec<Box<dyn Component>> {
    vec![Box::new(Logger), Box::new(AsyncManager), Box::new(Updater)]
}
