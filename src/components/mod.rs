pub mod logger;

use crate::{
    component::Component,
    components::logger::Logger,
};

pub fn init_components() -> Vec<Box<dyn Component>> {
    vec![Box::new(Logger)]
}
