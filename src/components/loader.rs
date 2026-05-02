use crate::{component::Component, loader};

#[derive(Default)]
pub struct Loader;

impl Component for Loader {
    fn name(&self) -> &'static str {
        "Loader"
    }

    fn free(&mut self) {
        loader::free();
    }

    fn reset(&mut self) {
        loader::reset();
    }

    fn on_new_map(&mut self) {
        loader::on_new_map();
    }

    fn on_new_map_loaded(&mut self) {
        loader::on_new_map_loaded();
    }
}
