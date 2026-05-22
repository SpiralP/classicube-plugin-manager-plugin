use tracing::{info, warn};

use crate::{
    component::Component,
    config::{self, Config, Subscription},
    loader::{self, LifecyclePhase},
};

#[derive(Default)]
pub struct Loader;

impl Component for Loader {
    fn name(&self) -> &'static str {
        "Loader"
    }

    fn init(&mut self) {
        // Sweep stale per-process breadcrumb files (BREADCRUMB_DIR) from
        // previous-session crashes BEFORE any of the early-return paths
        // below. A user with zero subscriptions, an all-disabled config,
        // or only the self sub (each of which short-circuits
        // classify_carryover_at) would otherwise never trigger the lazy
        // scan and dead-PID breadcrumb files would accumulate forever.
        loader::prime_carryover_scan();

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
        // Honor /disable on the manager's own subscription: skip loading any
        // managed plugins this session. The deferred initial pass in
        // Manager::on_new_map_loaded checks the same flag and bails out, so
        // no Catchup load runs either.
        if config::is_self_disabled(&cfg) {
            info!("manager subscription is disabled; skipping startup managed-load");
            return;
        }
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
