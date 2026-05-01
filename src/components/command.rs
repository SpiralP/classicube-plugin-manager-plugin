use std::{cell::RefCell, os::raw::c_int, slice};

use classicube_helpers::{chat, color};
use classicube_sys::{OwnedChatCommand, cc_string};
use tracing::error;

use crate::{
    component::Component,
    config::{Config, Subscription},
};

thread_local!(
    static COMMAND: RefCell<Option<OwnedChatCommand>> = const { RefCell::new(None) };
);

fn parse_owner_repo(s: &str) -> Option<(String, String)> {
    let (owner, repo) = s.split_once('/')?;
    if owner.is_empty()
        || repo.is_empty()
        || owner.contains(char::is_whitespace)
        || repo.contains(char::is_whitespace)
        || repo.contains('/')
    {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

const USAGE_LINES: &[&str] = &[
    "&a/client Updater subscribe <owner>/<repo>",
    "&a/client Updater unsubscribe <owner>/<repo>",
    "&a/client Updater list",
];

fn print_usage() {
    chat::print(format!("{}Usage:", color::YELLOW));
    for line in USAGE_LINES {
        chat::print(*line);
    }
}

fn print_load_error(e: &anyhow::Error) {
    error!("loading config: {e:#}");
    chat::print(format!(
        "{}Refusing to modify config (load failed — fix plugins/plugin-updater.toml first): {}{e}",
        color::RED,
        color::WHITE,
    ));
}

fn print_save_error(e: &anyhow::Error) {
    error!("saving config: {e:#}");
    chat::print(format!(
        "{}Failed to save config: {}{e}",
        color::RED,
        color::WHITE,
    ));
}

fn handle_subscribe(spec: &str) {
    let Some((owner, repo)) = parse_owner_repo(spec) else {
        chat::print(format!("{}Expected owner/repo, got: {spec}", color::RED));
        return;
    };

    let mut config = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            print_load_error(&e);
            return;
        }
    };

    let already = config
        .subscriptions
        .iter()
        .any(|s| s.owner.eq_ignore_ascii_case(&owner) && s.repo.eq_ignore_ascii_case(&repo));
    if already {
        chat::print(format!(
            "{}Already subscribed to {}{}/{}",
            color::YELLOW,
            color::LIME,
            owner,
            repo,
        ));
        return;
    }

    config.subscriptions.push(Subscription {
        owner: owner.clone(),
        repo: repo.clone(),
        installed_version: None,
    });
    if let Err(e) = config.save() {
        print_save_error(&e);
        return;
    }
    chat::print(format!(
        "{}Subscribed to {}{}/{}",
        color::PINK,
        color::LIME,
        owner,
        repo,
    ));
}

fn handle_unsubscribe(spec: &str) {
    let Some((owner, repo)) = parse_owner_repo(spec) else {
        chat::print(format!("{}Expected owner/repo, got: {spec}", color::RED));
        return;
    };

    let mut config = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            print_load_error(&e);
            return;
        }
    };

    let before = config.subscriptions.len();
    config
        .subscriptions
        .retain(|s| !(s.owner.eq_ignore_ascii_case(&owner) && s.repo.eq_ignore_ascii_case(&repo)));
    if config.subscriptions.len() == before {
        chat::print(format!(
            "{}Not subscribed to {}{}/{}",
            color::YELLOW,
            color::LIME,
            owner,
            repo,
        ));
        return;
    }

    if let Err(e) = config.save() {
        print_save_error(&e);
        return;
    }
    chat::print(format!(
        "{}Unsubscribed from {}{}/{}",
        color::PINK,
        color::LIME,
        owner,
        repo,
    ));
}

fn handle_list() {
    let config = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            error!("loading config: {e:#}");
            chat::print(format!(
                "{}Failed to load config: {}{e}",
                color::RED,
                color::WHITE,
            ));
            return;
        }
    };

    if config.subscriptions.is_empty() {
        chat::print(format!("{}No subscriptions", color::YELLOW));
        return;
    }
    chat::print(format!(
        "{}Subscriptions ({}):",
        color::PINK,
        config.subscriptions.len()
    ));
    for sub in &config.subscriptions {
        match &sub.installed_version {
            Some(v) => chat::print(format!(
                "  {}{}/{} {}(installed: {}{}{})",
                color::LIME,
                sub.owner,
                sub.repo,
                color::PINK,
                color::YELLOW,
                v,
                color::PINK,
            )),
            None => chat::print(format!("  {}{}/{}", color::LIME, sub.owner, sub.repo)),
        }
    }
}

extern "C" fn c_callback(args: *const cc_string, args_count: c_int) {
    let args = unsafe { slice::from_raw_parts(args, args_count as usize) };
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let args: Vec<&str> = args.iter().map(AsRef::as_ref).collect();

    match args.as_slice() {
        ["subscribe", spec] => handle_subscribe(spec),
        ["unsubscribe", spec] => handle_unsubscribe(spec),
        ["list"] => handle_list(),
        _ => print_usage(),
    }
}

#[derive(Default)]
pub struct Command;

impl Component for Command {
    fn name(&self) -> &'static str {
        "Command"
    }

    fn init(&mut self) {
        COMMAND.with(|cell| {
            let mut cmd = OwnedChatCommand::new("Updater", c_callback, false, USAGE_LINES.to_vec());
            cmd.register();
            *cell.borrow_mut() = Some(cmd);
        });
    }

    fn free(&mut self) {
        COMMAND.with(|cell| {
            cell.borrow_mut().take();
        });
    }
}
