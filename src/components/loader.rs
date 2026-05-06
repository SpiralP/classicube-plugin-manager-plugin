use tracing::warn;

use crate::{
    component::Component,
    config::{Config, Subscription},
    loader::{self, LifecyclePhase},
};

#[derive(Default)]
pub struct Loader;

impl Component for Loader {
    fn name(&self) -> &'static str {
        "Loader"
    }

    fn init(&mut self) {
        // Load managed plugins from the host's own Init pass, BEFORE the
        // CPE handshake (Game.c calls component Init at lines 468-469, then
        // Server.BeginConnect at line 486). This gives managed plugins the
        // same early window any plugins/-loaded plugin gets, so they can
        // mutate Server.AppName and contribute ExtInfo before login.
        //
        // Network I/O / version checks stay deferred to first OnNewMapLoaded
        // (see Manager). This pass only dlopens binaries already on disk
        // from a previous session.
        let cfg = match Config::load() {
            Ok(c) => c,
            Err(e) => {
                warn!("loading config for early managed-load: {e:#}");
                return;
            }
        };
        let subs: Vec<(String, String, Subscription)> = cfg
            .subscriptions
            .into_iter()
            .flat_map(|(owner, repos)| {
                repos
                    .into_iter()
                    .map(move |(repo, sub)| (owner.clone(), repo, sub))
            })
            .collect();
        loader::init_managed(&subs, LifecyclePhase::Startup);
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
