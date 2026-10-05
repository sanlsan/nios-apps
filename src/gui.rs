use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use niosapps::{
    check_folder, data_dir, ensure_runtime, install_packages, normalize_key, normalize_port, parse_packages, reset_runtime,
    runtime_ready, Event, Runner, Settings,
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

fn pick_folder(proxy: EventLoopProxy<UserEvent>) {
    thread::spawn(move || {
        let script = "[Console]::OutputEncoding=[Text.Encoding]::UTF8; Add-Type -AssemblyName System.Windows.Forms; \
            $owner = New-Object System.Windows.Forms.Form -Property @{TopMost=$true}; \
            $d = New-Object System.Windows.Forms.FolderBrowserDialog; $d.Description='Выберите папку проекта'; $d.ShowNewFolderButton=$true; \
            if ($d.ShowDialog($owner) -eq 'OK') { [Console]::Out.Write($d.SelectedPath) }";
        let Ok(out) = hidden(Command::new("powershell").args(["-NoProfile", "-STA", "-Command", script])).output() else { return };
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !path.is_empty() {
            push(&proxy, json!({"t": "folder", "path": path}));
        }
    });
}

fn open_folder(path: &std::path::Path) {
    let _ = Command::new("explorer").arg(path).spawn();
}

fn start(
    dir: std::path::PathBuf,
    proxy: EventLoopProxy<UserEvent>,
    runner: SharedRunner,
    key_raw: String,
    code: String,
    pkgs_raw: String,
    folder_raw: String,
    port_raw: String,
) {
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
        let folder = match check_folder(&folder_raw) {
            Ok(folder) => folder,
            Err(message) => return fail("folder", message),
        };
        let port = match normalize_port(&port_raw) {
            Ok(port) => port,
            Err(message) => return fail("input", message),
        };
        Settings { key: key.clone(), code: code.clone(), packages: pkgs_raw, folder: folder.clone(), port: port.clone() }.save(&dir);

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
        match Runner::start(&dir, &key, &code, &folder, &port, tx) {
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
        Some("save") => Settings { key: text("key"), code: text("code"), packages: text("packages"), folder: text("folder"), port: text("port") }.save(dir),
        Some("start") => start(
            dir.to_path_buf(), proxy.clone(), runner.clone(), text("key"), text("code"), text("packages"), text("folder"), text("port"),
        ),
        Some("pick_folder") => pick_folder(proxy.clone()),
        Some("open_data") => open_folder(dir),
        Some("reset_runtime") => {
            stop_runner(runner);
            let result = reset_runtime(dir);
            push(proxy, json!({"t": "reset_done", "ok": result.is_ok(), "message": result.err().unwrap_or_default()}));
        }
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
        json!({"key": settings.key, "code": settings.code, "packages": settings.packages, "folder": settings.folder, "port": settings.port})
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
