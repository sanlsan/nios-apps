#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(windows)]
mod gui;

use std::fs;
use std::sync::mpsc;

use niosapps::update;
use niosapps::{check_folder, data_dir, ensure_runtime, install_packages, normalize_key, normalize_port, parse_packages, Event, Runner};

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn headless(args: &[String]) -> i32 {
    let say = |text: &str| println!("{text}");
    let Some(key_raw) = arg(args, "--key") else {
        say("usage: NiosApps --headless --key KEY --file main.py [--packages \"a b\"] [--folder DIR] [--port N]");
        return 2;
    };
    let Some(file) = arg(args, "--file") else {
        say("usage: NiosApps --headless --key KEY --file main.py [--packages \"a b\"]");
        return 2;
    };
    let key = match normalize_key(&key_raw) {
        Ok(key) => key,
        Err(message) => return fail(&message),
    };
    let code = match fs::read_to_string(&file) {
        Ok(code) => code,
        Err(error) => return fail(&format!("cannot read {file}: {error}")),
    };
    let packages = match parse_packages(&arg(args, "--packages").unwrap_or_default()) {
        Ok(list) => list,
        Err(message) => return fail(&message),
    };
    let folder = match check_folder(&arg(args, "--folder").unwrap_or_default()) {
        Ok(folder) => folder,
        Err(message) => return fail(&message),
    };
    let port = match normalize_port(&arg(args, "--port").unwrap_or_default()) {
        Ok(port) => port,
        Err(message) => return fail(&message),
    };
    let dir = data_dir();
    let report = |text: &str| println!("{}", Event::Setup(text.into()).to_json());
    if let Err(message) = ensure_runtime(&dir, &report).and_then(|_| install_packages(&dir, &packages, &report)) {
        return fail(&message);
    }
    let (tx, rx) = mpsc::channel();
    let _runner = match Runner::start(&dir, &key, &code, &folder, &port, tx) {
        Ok(runner) => runner,
        Err(message) => return fail(&message),
    };
    for event in rx {
        println!("{}", event.to_json());
        match event {
            Event::Error { .. } => return 1,
            Event::Stopped => return 0,
            _ => {}
        }
    }
    0
}

fn updates(args: &[String]) -> i32 {
    let current = update::current_version();
    let found = match update::check() {
        Ok(found) => found,
        Err(message) => return fail(&message),
    };
    let Some(found) = found else {
        println!("{}", serde_json::json!({"t": "update", "state": "latest", "version": current}));
        return 0;
    };
    if !args.iter().any(|a| a == "--self-update") {
        println!("{}", serde_json::json!({"t": "update", "state": "available", "version": found.tag, "current": current}));
        return 0;
    }
    let dir = data_dir();
    let say = |text: &str| println!("{}", Event::Setup(text.into()).to_json());
    if let Err(message) = update::download_and_stage(&dir, &found, &say) {
        return fail(&message);
    }
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => return fail(&error.to_string()),
    };
    match update::apply_staged(&dir, &exe) {
        Ok(Some(tag)) => {
            println!("{}", serde_json::json!({"t": "updated", "version": tag}));
            0
        }
        Ok(None) => {
            println!("{}", serde_json::json!({"t": "update", "state": "latest", "version": current}));
            0
        }
        Err(message) => fail(&message),
    }
}

fn fail(message: &str) -> i32 {
    println!("{}", Event::Error { code: "cli".into(), message: message.into(), detail: String::new() }.to_json());
    1
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--check-update" || a == "--self-update") {
        std::process::exit(updates(&args));
    }
    if args.iter().any(|a| a == "--headless") {
        std::process::exit(headless(&args));
    }
    #[cfg(windows)]
    gui::run();
    #[cfg(not(windows))]
    {
        eprintln!("The window is available on Windows. Use --headless here.");
        std::process::exit(2);
    }
}
