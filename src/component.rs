use std::{cell::RefCell, os::raw::c_int, ptr};

use classicube_sys::IGameComponent;
use tracing::debug;

use crate::components::init_components;

pub trait Component {
    fn name(&self) -> &'static str {
        "UnknownComponent"
    }

    fn init(&mut self) {}
    fn free(&mut self) {}
    fn reset(&mut self) {}
    fn on_new_map(&mut self) {}
    fn on_new_map_loaded(&mut self) {}
}

type Inner = RefCell<Box<dyn Component>>;
thread_local!(
    static COMPONENTS: RefCell<Vec<Inner>> = {
        let mut components = init_components();
        let refcell_components = components.drain(..).map(RefCell::new).collect::<Vec<_>>();
        RefCell::new(refcell_components)
    };
);

fn with_components<R, F: FnOnce(&mut Vec<Inner>) -> R>(f: F) -> R {
    COMPONENTS.with_borrow_mut(|components| f(components))
}

extern "C" fn init() {
    with_components(|components| {
        for component in components {
            let mut component = component.borrow_mut();
            debug!("init {}", component.name());
            component.init();
        }
    });
}

extern "C" fn free() {
    with_components(|components| {
        for component in components.iter().rev() {
            let mut component = component.borrow_mut();
            debug!("free {}", component.name());
            component.free();
        }
    });

    with_components(|components| {
        for component in components.drain(..).rev() {
            drop(component);
        }
    });
}

extern "C" fn reset() {
    with_components(|components| {
        for component in components {
            component.borrow_mut().reset();
        }
    });
}

extern "C" fn on_new_map() {
    with_components(|components| {
        for component in components {
            component.borrow_mut().on_new_map();
        }
    });
}

extern "C" fn on_new_map_loaded() {
    with_components(|components| {
        for component in components {
            component.borrow_mut().on_new_map_loaded();
        }
    });
}

#[allow(non_upper_case_globals)]
#[unsafe(no_mangle)]
pub static Plugin_ApiVersion: c_int = 1;

#[allow(non_upper_case_globals)]
#[unsafe(no_mangle)]
pub static mut Plugin_Component: IGameComponent = IGameComponent {
    Init: Some(init),
    Free: Some(free),
    Reset: Some(reset),
    OnNewMap: Some(on_new_map),
    OnNewMapLoaded: Some(on_new_map_loaded),
    next: ptr::null_mut(),
};
