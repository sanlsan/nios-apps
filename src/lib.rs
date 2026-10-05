use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::Sender;
use std::thread;

use serde_json::{json, Value};

pub const RUNNER: &str = include_str!("../runner/runner.py");

const PYTHON_VERSION: &str = "3.11.9";
const PYTHON_URL: &str = "https://www.python.org/ftp/python/3.11.9/python-3.11.9-embed-amd64.zip";
const GET_PIP_URL: &str = "https://bootstrap.pypa.io/get-pip.py";
const BASE_PACKAGES: [&str; 3] = ["fastapi", "uvicorn", "nios-apps>=0.1.1"];
const KEY_PREFIX: &str = "nios_app_";

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Setup(String),
    Log(String),
    Online(String),
    Error { code: String, message: String, detail: String },
    Stopped,
}

impl Event {
    pub fn to_json(&self) -> Value {
        match self {
            Event::Setup(m) => json!({"t": "setup", "message": m}),
            Event::Log(m) => json!({"t": "log", "message": m}),
            Event::Online(u) => json!({"t": "online", "url": u}),
            Event::Error { code, message, detail } => json!({"t": "error", "code": code, "message": message, "detail": detail}),
            Event::Stopped => json!({"t": "stopped"}),
        }
    }

    pub fn from_line(line: &str) -> Option<Event> {
        let line = line.trim_end();
        if line.is_empty() {
            return None;
        }
        if !line.starts_with('{') {
            return Some(Event::Log(line.to_string()));
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return Some(Event::Log(line.to_string()));
        };
        let text = |name: &str| v.get(name).and_then(Value::as_str).unwrap_or("").to_string();
        match v.get("t").and_then(Value::as_str) {
            Some("online") => Some(Event::Online(text("url"))),
            Some("install") => {
                let list: Vec<&str> = v.get("packages").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
                Some(Event::Setup(format!("Устанавливаем библиотеки: {}", list.join(", "))))
            }
            Some("error") => Some(Event::Error { code: text("code"), message: text("message"), detail: text("detail") }),
            Some("log") => Some(Event::Log(text("message"))),
            Some(_) => None,
            None => Some(Event::Log(line.to_string())),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Settings {
    pub key: String,
    pub code: String,
    pub packages: String,
    pub folder: String,
    pub port: String,
}

impl Settings {
    pub fn load(dir: &Path) -> Settings {
        let Ok(raw) = fs::read_to_string(dir.join("settings.json")) else {
            return Settings::default();
        };
        let v: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
        let text = |name: &str| v.get(name).and_then(Value::as_str).unwrap_or("").to_string();
        Settings { key: text("key"), code: text("code"), packages: text("packages"), folder: text("folder"), port: text("port") }
    }

    pub fn save(&self, dir: &Path) {
        let _ = fs::create_dir_all(dir);
        let body = json!({"key": self.key, "code": self.code, "packages": self.packages, "folder": self.folder, "port": self.port});
        let _ = fs::write(dir.join("settings.json"), body.to_string());
    }
}

pub fn data_dir() -> PathBuf {
    if let Ok(custom) = std::env::var("NIOS_APPS_HOME") {
        return PathBuf::from(custom);
    }
    if cfg!(windows) {
        let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
        return PathBuf::from(base).join("NiosApps");
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".local").join("share").join("NiosApps")
}

pub fn normalize_key(raw: &str) -> Result<String, String> {
    let text = raw.trim();
    if text.is_empty() {
        return Err("Вставьте ключ из кабинета ni-os.ru/appsdev.".into());
    }
    let key = match text.find(KEY_PREFIX) {
        Some(start) => text[start..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .collect::<String>(),
        None => {
            let bare: String = text.trim_matches(|c| c == '"' || c == '\'' || c == '`').trim().to_string();
            format!("{KEY_PREFIX}{bare}")
        }
    };
    let tail = &key[KEY_PREFIX.len().min(key.len())..];
    if tail.len() < 16 || !tail.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err("Ключ выглядит неполным. Скопируйте его целиком, он начинается с nios_app_.".into());
    }
    Ok(key)
}

pub fn normalize_port(raw: &str) -> Result<String, String> {
    let text = raw.trim();
    if text.is_empty() {
        return Ok(String::new());
    }
    match text.parse::<u32>() {
        Ok(port) if (1024..=65535).contains(&port) => Ok(port.to_string()),
        _ => Err("Порт должен быть числом от 1024 до 65535. Или оставьте поле пустым: тогда порт выберется сам.".into()),
    }
}

pub fn check_folder(raw: &str) -> Result<String, String> {
    let text = raw.trim();
    if text.is_empty() {
        return Ok(String::new());
    }
    if Path::new(text).is_dir() {
        Ok(text.to_string())
    } else {
        Err(format!("Папка проекта не найдена: {text}. Выберите её заново в настройках (значок шестерёнки)."))
    }
}

pub fn reset_runtime(dir: &Path) -> Result<(), String> {
    let runtime = runtime_dir(dir);
    if runtime.exists() {
        fs::remove_dir_all(&runtime).map_err(|e| format!("Не удалось удалить {}: {e}", runtime.display()))?;
    }
    Ok(())
}

pub fn parse_packages(raw: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for token in raw.split(|c: char| c.is_whitespace() || c == ',' || c == ';').filter(|t| !t.is_empty()) {
        let valid = token.chars().all(|c| c.is_ascii_alphanumeric() || "._-[]=<>~!".contains(c));
        if token.starts_with('-') || !valid {
            return Err(format!("«{token}» не похоже на название библиотеки. Пишите через пробел, например: requests pydantic."));
        }
        out.push(token.to_string());
    }
    Ok(out)
}

pub fn patch_pth(content: &str) -> String {
    let mut lines: Vec<String> = content.lines().map(|l| l.trim_end().to_string()).collect();
    let mut has_site = false;
    for line in lines.iter_mut() {
        if line.trim_start_matches('#').trim() == "import site" {
            *line = "import site".into();
            has_site = true;
        }
    }
    if !has_site {
        lines.push("import site".into());
    }
    if !lines.iter().any(|l| l.eq_ignore_ascii_case("Lib\\site-packages")) {
        lines.insert(0, "Lib\\site-packages".into());
    }
    let mut text = lines.join("\r\n");
    text.push_str("\r\n");
    text
}

fn hide(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

fn run_checked(cmd: &mut Command, what: &str) -> Result<(), String> {
    let out = hide(cmd).output().map_err(|e| format!("{what}: не удалось запустить ({e})"))?;
    if out.status.success() {
        return Ok(());
    }
    let mut tail = String::from_utf8_lossy(&out.stderr).to_string();
    tail.push_str(&String::from_utf8_lossy(&out.stdout));
    let short: Vec<&str> = tail.lines().rev().take(4).collect();
    let short: Vec<&str> = short.into_iter().rev().collect();
    Err(format!("{what} не получилось. {}", short.join(" ")))
}

pub fn runtime_dir(dir: &Path) -> PathBuf {
    dir.join("runtime")
}

pub fn python_exe(dir: &Path) -> PathBuf {
    if let Ok(custom) = std::env::var("NIOS_PYTHON") {
        return PathBuf::from(custom);
    }
    if cfg!(windows) {
        runtime_dir(dir).join("python.exe")
    } else {
        runtime_dir(dir).join("bin").join("python3")
    }
}

fn marker(dir: &Path) -> PathBuf {
    runtime_dir(dir).join(format!(".ready-{PYTHON_VERSION}"))
}

pub fn runtime_ready(dir: &Path) -> bool {
    std::env::var("NIOS_PYTHON").is_ok() || marker(dir).exists()
}

fn download(url: &str, dest: &Path) -> Result<(), String> {
    let curl = if cfg!(windows) { "curl.exe" } else { "curl" };
    run_checked(
        Command::new(curl).args(["-L", "--fail", "--silent", "--show-error", "--retry", "3", "-o"]).arg(dest).arg(url),
        "Скачивание",
    )
}

pub fn ensure_runtime(dir: &Path, report: &dyn Fn(&str)) -> Result<(), String> {
    if runtime_ready(dir) {
        return Ok(());
    }
    if !cfg!(windows) {
        return Err("Автоматическая подготовка Python работает только в Windows.".into());
    }
    let runtime = runtime_dir(dir);
    let _ = fs::remove_dir_all(&runtime);
    fs::create_dir_all(&runtime).map_err(|e| format!("Нет доступа к папке {}: {e}", runtime.display()))?;

    report("Скачиваем Python (около 10 МБ)");
    let zip = dir.join("python-embed.zip");
    download(PYTHON_URL, &zip)?;

    report("Распаковываем");
    let script = format!(
        "Expand-Archive -LiteralPath '{}' -DestinationPath '{}' -Force",
        zip.display(),
        runtime.display()
    );
    run_checked(
        Command::new("powershell").args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &script]),
        "Распаковка",
    )?;
    let _ = fs::remove_file(&zip);

    let pth = fs::read_dir(&runtime)
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.extension().map(|x| x == "_pth").unwrap_or(false))
        .ok_or("В скачанном Python не нашёлся файл настроек.")?;
    let patched = patch_pth(&fs::read_to_string(&pth).map_err(|e| e.to_string())?);
    fs::write(&pth, patched).map_err(|e| e.to_string())?;
    fs::create_dir_all(runtime.join("Lib").join("site-packages")).map_err(|e| e.to_string())?;

    report("Устанавливаем менеджер пакетов");
    let get_pip = dir.join("get-pip.py");
    download(GET_PIP_URL, &get_pip)?;
    let python = python_exe(dir);
    run_checked(
        Command::new(&python).arg(&get_pip).args(["--no-warn-script-location", "--disable-pip-version-check", "-q"]),
        "Установка pip",
    )?;
    let _ = fs::remove_file(&get_pip);

    report("Устанавливаем FastAPI и Nios Apps (1-2 минуты)");
    let mut cmd = Command::new(&python);
    cmd.args(["-m", "pip", "install", "-q", "--no-warn-script-location", "--disable-pip-version-check"]).args(BASE_PACKAGES);
    run_checked(&mut cmd, "Установка библиотек")?;

    fs::write(marker(dir), PYTHON_VERSION).map_err(|e| e.to_string())?;
    report("Готово");
    Ok(())
}

pub fn install_packages(dir: &Path, packages: &[String], report: &dyn Fn(&str)) -> Result<(), String> {
    if packages.is_empty() {
        return Ok(());
    }
    report(&format!("Устанавливаем: {}", packages.join(", ")));
    let mut cmd = Command::new(python_exe(dir));
    cmd.args(["-m", "pip", "install", "-q", "--no-warn-script-location", "--disable-pip-version-check"]).args(packages);
    run_checked(&mut cmd, "Установка библиотек")
}

pub struct Runner {
    child: Child,
    _stdin: Option<ChildStdin>,
}

fn pump<R: Read + Send + 'static>(stream: R, tx: Sender<Event>, last: bool) {
    thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            if let Some(event) = Event::from_line(&line) {
                if tx.send(event).is_err() {
                    return;
                }
            }
        }
        if last {
            let _ = tx.send(Event::Stopped);
        }
    });
}

impl Runner {
    pub fn start(dir: &Path, key: &str, code: &str, folder: &str, port: &str, tx: Sender<Event>) -> Result<Runner, String> {
        let user = dir.join("user");
        fs::create_dir_all(&user).map_err(|e| format!("Нет доступа к папке {}: {e}", user.display()))?;
        fs::write(user.join("main.py"), code).map_err(|e| e.to_string())?;
        let runner = dir.join("runner.py");
        fs::write(&runner, RUNNER).map_err(|e| e.to_string())?;

        let mut cmd = Command::new(python_exe(dir));
        cmd.arg("-u")
            .arg(&runner)
            .arg(user.join("main.py"))
            .env("NIOS_APP_KEY", key)
            .env("PYTHONUTF8", "1")
            .env("PYTHONIOENCODING", "utf-8")
            .env("NIOS_PROJECT_DIR", folder)
            .env("NIOS_LOCAL_PORT", port)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = hide(&mut cmd).spawn().map_err(|e| format!("Не удалось запустить Python: {e}"))?;
        let stdin = child.stdin.take();
        if let Some(out) = child.stdout.take() {
            pump(out, tx.clone(), true);
        }
        if let Some(err) = child.stderr.take() {
            pump(err, tx, false);
        }
        Ok(Runner { child, _stdin: stdin })
    }

    pub fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_forms() {
        let full = "nios_app_TESTONLYabcdefghijklmnopqrstuvwxyz0123456789AB";
        assert_eq!(normalize_key(full).unwrap(), full);
        assert_eq!(normalize_key(&format!("  \"{full}\"  ")).unwrap(), full);
        assert_eq!(normalize_key(&full[9..]).unwrap(), full);
        assert_eq!(normalize_key(&format!("nios.app(\"{full}\", port=8000)")).unwrap(), full);
        assert_eq!(normalize_key(&format!("python3 -c 'import nios; nios.app(\"{full}\", port=8000)'")).unwrap(), full);
        assert!(normalize_key("").is_err());
        assert!(normalize_key("   ").is_err());
        assert!(normalize_key("nios_app_short").is_err());
        assert!(normalize_key("abc").is_err());
        assert!(normalize_key("nios_app_has space and more words here").is_err() || normalize_key("nios_app_has space").is_err());
    }

    #[test]
    fn packages() {
        assert_eq!(parse_packages("requests, pydantic  numpy").unwrap(), vec!["requests", "pydantic", "numpy"]);
        assert_eq!(parse_packages("httpx>=0.27 sqlalchemy[asyncio]").unwrap().len(), 2);
        assert!(parse_packages("--index-url").is_err());
        assert!(parse_packages("req;uests&calc").is_err());
        assert!(parse_packages("").unwrap().is_empty());
        assert!(parse_packages("bad|name").is_err());
    }

    #[test]
    fn pth() {
        let original = "python311.zip\r\n.\r\n\r\n# Uncomment to run site.main() automatically\r\n#import site\r\n";
        let out = patch_pth(original);
        assert!(out.contains("\r\nimport site\r\n") || out.starts_with("import site"));
        assert!(!out.contains("#import site"));
        assert!(out.lines().any(|l| l == "Lib\\site-packages"));
        assert_eq!(patch_pth(&out), out);
        assert!(patch_pth("python311.zip\r\n.\r\n").contains("import site"));
    }

    #[test]
    fn events() {
        assert_eq!(Event::from_line(r#"{"t":"online","url":"https://ni-os.ru/apps/x/"}"#), Some(Event::Online("https://ni-os.ru/apps/x/".into())));
        assert_eq!(
            Event::from_line(r#"{"t":"error","code":"key","message":"Неверный","detail":""}"#),
            Some(Event::Error { code: "key".into(), message: "Неверный".into(), detail: "".into() })
        );
        assert_eq!(Event::from_line(r#"{"t":"log","message":"hi"}"#), Some(Event::Log("hi".into())));
        assert_eq!(Event::from_line("INFO: started"), Some(Event::Log("INFO: started".into())));
        assert_eq!(Event::from_line("{broken"), Some(Event::Log("{broken".into())));
        assert_eq!(Event::from_line(r#"{"t":"local","port":1}"#), None);
        assert_eq!(Event::from_line(""), None);
        assert_eq!(
            Event::from_line(r#"{"t":"install","packages":["pillow","tomli-w"]}"#),
            Some(Event::Setup("Устанавливаем библиотеки: pillow, tomli-w".into()))
        );
        assert_eq!(Event::from_line(r#"{"t":"install"}"#), Some(Event::Setup("Устанавливаем библиотеки: ".into())));
    }

    #[test]
    fn port_and_folder() {
        assert_eq!(normalize_port("").unwrap(), "");
        assert_eq!(normalize_port("  8080 ").unwrap(), "8080");
        assert!(normalize_port("80").is_err() && normalize_port("70000").is_err() && normalize_port("abc").is_err() && normalize_port("-1").is_err());
        assert_eq!(check_folder("").unwrap(), "");
        let here = std::env::temp_dir();
        assert_eq!(check_folder(&here.to_string_lossy()).unwrap(), here.to_string_lossy());
        assert!(check_folder("/definitely/not/here").unwrap_err().contains("не найдена"));
    }

    #[test]
    fn reset_removes_runtime_only() {
        let dir = std::env::temp_dir().join(format!("niosapps-reset-{}", std::process::id()));
        fs::create_dir_all(runtime_dir(&dir)).unwrap();
        fs::write(runtime_dir(&dir).join("x"), "1").unwrap();
        fs::write(dir.join("settings.json"), "{}").unwrap();
        reset_runtime(&dir).unwrap();
        assert!(!runtime_dir(&dir).exists() && dir.join("settings.json").exists());
        reset_runtime(&dir).unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn settings_roundtrip() {
        let dir = std::env::temp_dir().join(format!("niosapps-test-{}", std::process::id()));
        let s = Settings { key: "k".into(), code: "print('привет')\n".into(), packages: "requests".into(), folder: "C:\\Мои проекты\\api".into(), port: "8123".into() };
        s.save(&dir);
        assert_eq!(Settings::load(&dir), s);
        assert_eq!(Settings::load(&dir.join("missing")), Settings::default());
        let _ = fs::remove_dir_all(&dir);
    }
}
