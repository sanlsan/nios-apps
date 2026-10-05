use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use niosapps::{
    data_dir, ensure_runtime, install_packages, normalize_key, parse_packages, runtime_ready, Event, Runner, Settings,
};
use serde_json::{json, Value};
use tao::dpi::LogicalSize;
use tao::event::{Event as TaoEvent, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::window::WindowBuilder;
use wry::WebViewBuilder;

const HTML: &str = include_str!("../ui/index.html");
const FONTS: &str = include_str!("../ui/fonts.css");
const WEBVIEW2_URL: &str = "https://go.microsoft.com/fwlink/p/?LinkId=2124703";

static SESSION: AtomicU64 = AtomicU64::new(0);

enum UserEvent {
    Js(String),
}

type SharedRunner = Arc<Mutex<Option<Runner>>>;

fn push(proxy: &EventLoopProxy<UserEvent>, value: Value) {
    let text = value.to_string().replace('\u{2028}', "\\u2028").replace('\u{2029}', "\\u2029");
    let _ = proxy.send_event(UserEvent::Js(format!("window.onNative({text});")));
}

fn hidden(cmd: &mut Command) -> &mut Command {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0800_0000)
}

fn open_url(url: &str) {
    if !url.starts_with("https://") {
        return;
    }
    let _ = hidden(Command::new("rundll32").arg("url.dll,FileProtocolHandler").arg(url)).spawn();
}

fn copy_text(text: &str) {
    if let Ok(mut child) = hidden(Command::new("clip").stdin(Stdio::piped())).spawn() {
        if let Some(stdin) = child.stdin.as_mut() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
    }
}

fn stop_runner(runner: &SharedRunner) {
    SESSION.fetch_add(1, Ordering::SeqCst);
    if let Some(mut running) = runner.lock().unwrap().take() {
        running.stop();
    }
}

fn start(dir: std::path::PathBuf, proxy: EventLoopProxy<UserEvent>, runner: SharedRunner, key_raw: String, code: String, pkgs_raw: String) {
    stop_runner(&runner);
    let session = SESSION.fetch_add(1, Ordering::SeqCst) + 1;
    thread::spawn(move || {
        let current = || SESSION.load(Ordering::SeqCst) == session;
        let fail = |code: &str, message: String| {
            if current() {
                push(&proxy, Event::Error { code: code.into(), message, detail: String::new() }.to_json());
            }
        };
        let key = match normalize_key(&key_raw) {
            Ok(key) => key,
            Err(message) => return fail("input", message),
        };
        if code.trim().is_empty() {
            return fail("input", "Вставьте код сервера на FastAPI.".into());
        }
        let packages = match parse_packages(&pkgs_raw) {
            Ok(list) => list,
            Err(message) => return fail("input", message),
        };
        Settings { key: key.clone(), code: code.clone(), packages: pkgs_raw }.save(&dir);

        if !runtime_ready(&dir) {
            push(&proxy, json!({"t": "setup_begin"}));
        }
        let report = |text: &str| {
            if current() {
                push(&proxy, Event::Setup(text.into()).to_json());
            }
        };
        if let Err(message) = ensure_runtime(&dir, &report) {
            return fail("setup", format!("Не удалось подготовить Python. Проверьте интернет и запустите снова. {message}"));
        }
        if let Err(message) = install_packages(&dir, &packages, &report) {
            return fail("packages", message);
        }
        if !current() {
            return;
        }
        report("Запускаем сервер и подключаем к Nios Apps");
        let (tx, rx) = mpsc::channel();
        match Runner::start(&dir, &key, &code, tx) {
            Ok(started) => *runner.lock().unwrap() = Some(started),
            Err(message) => return fail("start", message),
        }
        for event in rx {
            if !current() {
                return;
            }
            let finished = matches!(event, Event::Stopped | Event::Error { .. });
            push(&proxy, event.to_json());
            if finished {
                break;
            }
        }
        if current() {
            if let Some(mut running) = runner.lock().unwrap().take() {
                running.stop();
            }
        }
    });
}

fn handle(body: &str, dir: &std::path::Path, proxy: &EventLoopProxy<UserEvent>, runner: &SharedRunner) {
    let Ok(msg) = serde_json::from_str::<Value>(body) else { return };
    let text = |name: &str| msg.get(name).and_then(Value::as_str).unwrap_or("").to_string();
    match msg.get("cmd").and_then(Value::as_str) {
        Some("save") => Settings { key: text("key"), code: text("code"), packages: text("packages") }.save(dir),
        Some("start") => start(dir.to_path_buf(), proxy.clone(), runner.clone(), text("key"), text("code"), text("packages")),
        Some("stop") => stop_runner(runner),
        Some("open") => open_url(&text("url")),
        Some("copy") => copy_text(&text("text")),
        _ => {}
    }
}

fn missing_webview(error: &dyn std::fmt::Display) {
    let script = format!(
        "Add-Type -AssemblyName PresentationFramework; \
         $r = [System.Windows.MessageBox]::Show('Не удалось открыть окно: нужен компонент Microsoft Edge WebView2.`n`nНажмите ОК, чтобы открыть страницу загрузки, установите его и запустите Nios Apps снова.`n`n{}', 'Nios Apps', 'OKCancel'); \
         if ($r -eq 'OK') {{ Start-Process '{}' }}",
        error.to_string().replace('\'', " "),
        WEBVIEW2_URL
    );
    let _ = hidden(Command::new("powershell").args(["-NoProfile", "-Command", &script])).status();
}

pub fn run() {
    let dir = data_dir();
    let settings = Settings::load(&dir);
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let window = WindowBuilder::new()
        .with_title("Nios Apps")
        .with_inner_size(LogicalSize::new(940.0, 800.0))
        .with_min_inner_size(LogicalSize::new(640.0, 600.0))
        .build(&event_loop)
        .expect("window");

    let runner: SharedRunner = Arc::new(Mutex::new(None));
    let init = format!(
        "window.__init = {};",
        json!({"key": settings.key, "code": settings.code, "packages": settings.packages})
            .to_string()
            .replace('\u{2028}', "\\u2028")
            .replace('\u{2029}', "\\u2029")
    );
    let ipc_dir = dir.clone();
    let ipc_proxy = proxy.clone();
    let ipc_runner = runner.clone();
    let built = WebViewBuilder::new()
        .with_html(HTML.replace("/*FONTS*/", FONTS))
        .with_initialization_script(init)
        .with_ipc_handler(move |request| handle(request.body(), &ipc_dir, &ipc_proxy, &ipc_runner))
        .build(&window);
    let webview = match built {
        Ok(view) => view,
        Err(error) => return missing_webview(&error),
    };

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            TaoEvent::UserEvent(UserEvent::Js(script)) => {
                let _ = webview.evaluate_script(&script);
            }
            TaoEvent::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                stop_runner(&runner);
                *control_flow = ControlFlow::Exit;
            }
            _ => {}
        }
    });
}
