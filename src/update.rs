use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

use crate::{curl_bin, hide, run_checked};

const RELEASES_API: &str = "https://api.github.com/repos/sanlsan/nios-apps/releases?per_page=15";
const DOWNLOAD_PREFIX: &str = "https://github.com/sanlsan/nios-apps/releases/download/";
const ASSET: &str = "NiosApps.exe";
const SUM_ASSET: &str = "NiosApps.exe.sha256";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    core: [u64; 3],
    pre: Option<(String, u64)>,
}

impl Version {
    pub fn parse(raw: &str) -> Option<Version> {
        let text = raw.trim().trim_start_matches('v');
        let (core_text, pre_text) = match text.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (text, None),
        };
        let mut core = [0u64; 3];
        let parts: Vec<&str> = core_text.split('.').collect();
        if parts.is_empty() || parts.len() > 3 {
            return None;
        }
        for (slot, part) in core.iter_mut().zip(parts.iter()) {
            *slot = part.parse().ok()?;
        }
        let pre = pre_text.map(|p| {
            let digits: String = p.chars().rev().take_while(|c| c.is_ascii_digit()).collect::<Vec<_>>().into_iter().rev().collect();
            let label = p[..p.len() - digits.len()].trim_end_matches(['.', '-']).to_ascii_lowercase();
            (label, digits.parse().unwrap_or(0))
        });
        Some(Version { core, pre })
    }

    pub fn is_prerelease(&self) -> bool {
        self.pre.is_some()
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.core.cmp(&other.core).then_with(|| match (&self.pre, &other.pre) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(a), Some(b)) => a.cmp(b),
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Update {
    pub tag: String,
    pub url: String,
    pub sha256: Option<String>,
    pub sum_url: Option<String>,
    pub size: u64,
}

pub fn current_version() -> String {
    #[cfg(debug_assertions)]
    if let Ok(fake) = std::env::var("NIOS_APPS_FAKE_VERSION") {
        return fake;
    }
    option_env!("NIOS_APPS_VERSION").unwrap_or(env!("CARGO_PKG_VERSION")).trim_start_matches('v').to_string()
}

fn api_url() -> String {
    #[cfg(debug_assertions)]
    if let Ok(custom) = std::env::var("NIOS_APPS_RELEASES_API") {
        return custom;
    }
    RELEASES_API.to_string()
}

fn allowed_url(url: &str) -> bool {
    if url.starts_with(DOWNLOAD_PREFIX) {
        return true;
    }
    cfg!(debug_assertions) && url.starts_with("http://127.0.0.1:")
}

pub fn pick_release(releases: &Value, current: &Version) -> Option<Update> {
    let include_pre = current.is_prerelease();
    let mut best: Option<(Version, Update)> = None;
    for release in releases.as_array()? {
        if release.get("draft").and_then(Value::as_bool).unwrap_or(false) {
            continue;
        }
        if release.get("prerelease").and_then(Value::as_bool).unwrap_or(false) && !include_pre {
            continue;
        }
        let tag = release.get("tag_name").and_then(Value::as_str).unwrap_or("");
        let Some(version) = Version::parse(tag) else { continue };
        let assets = release.get("assets").and_then(Value::as_array).cloned().unwrap_or_default();
        let find = |name: &str| assets.iter().find(|a| a.get("name").and_then(Value::as_str) == Some(name)).cloned();
        let Some(exe) = find(ASSET) else { continue };
        let url = exe.get("browser_download_url").and_then(Value::as_str).unwrap_or("").to_string();
        if !allowed_url(&url) {
            continue;
        }
        let digest = exe.get("digest").and_then(Value::as_str).and_then(|d| d.strip_prefix("sha256:")).map(|d| d.to_ascii_lowercase());
        let sum_url = find(SUM_ASSET).and_then(|a| a.get("browser_download_url").and_then(Value::as_str).map(str::to_string)).filter(|u| allowed_url(u));
        let update = Update { tag: tag.to_string(), url, sha256: digest, sum_url, size: exe.get("size").and_then(Value::as_u64).unwrap_or(0) };
        if best.as_ref().map(|(v, _)| version > *v).unwrap_or(true) {
            best = Some((version, update));
        }
    }
    best.filter(|(version, _)| version > current).map(|(_, update)| update)
}

fn curl_text(url: &str) -> Result<String, String> {
    let out = hide(Command::new(curl_bin()).args(["-L", "--fail", "--silent", "--show-error", "--max-time", "20", "-H", "Accept: application/vnd.github+json", "-H", "User-Agent: nios-apps-desktop"]).arg(url))
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("Не удалось связаться с GitHub: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

pub fn check() -> Result<Option<Update>, String> {
    let current = Version::parse(&current_version()).ok_or("Не удалось определить версию программы.")?;
    let body = curl_text(&api_url())?;
    let releases: Value = serde_json::from_str(&body).map_err(|_| "GitHub вернул неожиданный ответ.".to_string())?;
    Ok(pick_release(&releases, &current))
}

fn updates_dir(dir: &Path) -> PathBuf {
    dir.join("updates")
}

fn staged_path(dir: &Path) -> PathBuf {
    updates_dir(dir).join("NiosApps.exe.new")
}

fn marker_path(dir: &Path) -> PathBuf {
    updates_dir(dir).join("staged.json")
}

pub fn sha256_of(path: &Path) -> Result<String, String> {
    let (program, args): (&str, Vec<String>) = if cfg!(windows) {
        ("certutil", vec!["-hashfile".into(), path.display().to_string(), "SHA256".into()])
    } else {
        ("sha256sum", vec![path.display().to_string()])
    };
    let out = hide(Command::new(program).args(&args)).output().map_err(|e| format!("Не удалось посчитать контрольную сумму: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let candidates: Vec<String> = text.lines().map(|l| l.split_whitespace().collect::<String>()).collect();
    candidates
        .into_iter()
        .map(|c| c.chars().take(64).collect::<String>())
        .find(|c| c.len() == 64 && c.chars().all(|ch| ch.is_ascii_hexdigit()))
        .map(|c| c.to_ascii_lowercase())
        .ok_or_else(|| "Не удалось прочитать контрольную сумму.".to_string())
}

pub fn download_and_stage(dir: &Path, update: &Update, report: &dyn Fn(&str)) -> Result<(), String> {
    let folder = updates_dir(dir);
    fs::create_dir_all(&folder).map_err(|e| format!("Нет доступа к {}: {e}", folder.display()))?;
    let dest = staged_path(dir);
    let _ = fs::remove_file(&dest);
    let _ = fs::remove_file(marker_path(dir));
    report(&format!("Скачиваем обновление {}", update.tag));
    run_checked(
        Command::new(curl_bin()).args(["-L", "--fail", "--silent", "--show-error", "--max-time", "600", "-o"]).arg(&dest).arg(&update.url),
        "Скачивание обновления",
    )?;
    let size = fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
    let mut head = [0u8; 2];
    let ok_head = fs::File::open(&dest).and_then(|mut f| std::io::Read::read_exact(&mut f, &mut head)).is_ok();
    if size < 1024 || (cfg!(windows) && (!ok_head || &head != b"MZ")) {
        let _ = fs::remove_file(&dest);
        return Err("Скачанный файл повреждён или это не программа.".into());
    }
    let wanted = match (&update.sha256, &update.sum_url) {
        (Some(sum), _) => sum.clone(),
        (None, Some(url)) => curl_text(url)?.split_whitespace().next().unwrap_or("").to_ascii_lowercase(),
        _ => {
            let _ = fs::remove_file(&dest);
            return Err("У релиза нет контрольной суммы, обновление отменено.".into());
        }
    };
    report("Проверяем контрольную сумму");
    let actual = sha256_of(&dest)?;
    if actual != wanted {
        let _ = fs::remove_file(&dest);
        return Err("Контрольная сумма не совпала, обновление отменено.".into());
    }
    fs::write(marker_path(dir), json!({"tag": update.tag}).to_string()).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn staged_version(dir: &Path) -> Option<String> {
    if !staged_path(dir).exists() {
        return None;
    }
    let raw = fs::read_to_string(marker_path(dir)).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    value.get("tag").and_then(Value::as_str).map(str::to_string)
}

fn old_path(exe: &Path) -> PathBuf {
    let mut name = exe.as_os_str().to_owned();
    name.push(".old");
    PathBuf::from(name)
}

pub fn cleanup_old(exe: &Path) {
    let _ = fs::remove_file(old_path(exe));
}

pub fn apply_staged(dir: &Path, exe: &Path) -> Result<Option<String>, String> {
    let Some(tag) = staged_version(dir) else { return Ok(None) };
    let staged_ver = Version::parse(&tag);
    let current = Version::parse(&current_version());
    if let (Some(staged), Some(now)) = (staged_ver, current) {
        if staged <= now {
            let _ = fs::remove_file(staged_path(dir));
            let _ = fs::remove_file(marker_path(dir));
            return Ok(None);
        }
    }
    let old = old_path(exe);
    let _ = fs::remove_file(&old);
    fs::rename(exe, &old).map_err(|e| format!("Нет прав заменить файл программы ({e}). Скачайте новую версию вручную со страницы ni-os.ru/appsdev."))?;
    if let Err(error) = fs::copy(staged_path(dir), exe) {
        let _ = fs::remove_file(exe);
        let _ = fs::rename(&old, exe);
        return Err(format!("Не удалось записать новую версию ({error}), осталась прежняя."));
    }
    let _ = fs::remove_file(staged_path(dir));
    let _ = fs::remove_file(marker_path(dir));
    Ok(Some(tag))
}

pub fn relaunch(exe: &Path) {
    let _ = Command::new(exe).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn ordering() {
        assert!(v("0.1.0-rc1") < v("0.1.0-rc2") && v("0.1.0-rc2") < v("0.1.0-rc10"));
        assert!(v("0.1.0-rc9") < v("0.1.0") && v("0.1.0") < v("0.1.1-rc1") && v("v0.2.0") > v("0.1.9"));
        assert!(v("1.0") == v("1.0.0") && v("0.1.0-beta1") < v("0.1.0-rc1"));
        assert!(Version::parse("abc").is_none() && Version::parse("1.2.3.4").is_none() && Version::parse("").is_none());
        assert!(v("0.1.0-rc2").is_prerelease() && !v("0.1.0").is_prerelease());
    }

    fn release(tag: &str, pre: bool, name: &str, url: &str, digest: Option<&str>) -> Value {
        let mut asset = json!({"name": name, "browser_download_url": url, "size": 5000});
        if let Some(d) = digest {
            asset["digest"] = json!(format!("sha256:{d}"));
        }
        json!({"tag_name": tag, "prerelease": pre, "draft": false, "assets": [asset, {"name": SUM_ASSET, "browser_download_url": format!("{url}.sha256")}]})
    }

    fn url(tag: &str) -> String {
        format!("{DOWNLOAD_PREFIX}{tag}/NiosApps.exe")
    }

    #[test]
    fn channels() {
        let list = json!([
            release("v0.3.0-rc1", true, ASSET, &url("v0.3.0-rc1"), Some("aa")),
            release("v0.2.0", false, ASSET, &url("v0.2.0"), Some("bb")),
            release("v0.1.0", false, ASSET, &url("v0.1.0"), Some("cc")),
        ]);
        let regular = pick_release(&list, &v("0.1.0")).unwrap();
        assert_eq!((regular.tag.as_str(), regular.sha256.as_deref()), ("v0.2.0", Some("bb")));
        assert!(pick_release(&list, &v("0.2.0")).is_none(), "already latest regular");
        let beta = pick_release(&list, &v("0.2.0-rc1")).unwrap();
        assert_eq!(beta.tag, "v0.3.0-rc1");
        assert_eq!(pick_release(&list, &v("0.1.0-rc2")).unwrap().tag, "v0.3.0-rc1", "pre-release users follow the newest build");
        assert!(pick_release(&list, &v("0.3.0-rc1")).is_none());
        assert!(pick_release(&list, &v("0.3.0")).is_none());
        assert!(pick_release(&json!({"message": "rate limited"}), &v("0.1.0")).is_none());
    }

    #[test]
    fn rejects_bad_releases() {
        let evil = json!([release("v9.9.9", false, ASSET, "https://evil.example/NiosApps.exe", Some("aa"))]);
        assert!(pick_release(&evil, &v("0.1.0")).is_none(), "downloads only from this repository's releases");
        let other = json!([release("v9.9.9", false, "Other.exe", &url("v9.9.9"), Some("aa"))]);
        assert!(pick_release(&other, &v("0.1.0")).is_none());
        let mut draft = release("v9.9.9", false, ASSET, &url("v9.9.9"), Some("aa"));
        draft["draft"] = json!(true);
        assert!(pick_release(&json!([draft]), &v("0.1.0")).is_none());
        let junk_tag = json!([release("latest", false, ASSET, &url("latest"), Some("aa"))]);
        assert!(pick_release(&junk_tag, &v("0.1.0")).is_none());
        let sneaky = json!([release("v9.9.9", false, ASSET, "http://127.0.0.1.evil.example/x", Some("aa"))]);
        assert!(pick_release(&sneaky, &v("0.1.0")).is_none());
    }

    #[test]
    fn apply_swaps_and_keeps_backup() {
        let dir = std::env::temp_dir().join(format!("niosapps-upd-{}", std::process::id()));
        fs::create_dir_all(updates_dir(&dir)).unwrap();
        let exe = dir.join("NiosApps.exe");
        fs::write(&exe, "old build").unwrap();
        assert_eq!(apply_staged(&dir, &exe).unwrap(), None, "nothing staged");
        fs::write(staged_path(&dir), "new build").unwrap();
        fs::write(marker_path(&dir), r#"{"tag":"v99.0.0"}"#).unwrap();
        assert_eq!(staged_version(&dir).as_deref(), Some("v99.0.0"));
        assert_eq!(apply_staged(&dir, &exe).unwrap().as_deref(), Some("v99.0.0"));
        assert_eq!(fs::read_to_string(&exe).unwrap(), "new build");
        assert_eq!(fs::read_to_string(old_path(&exe)).unwrap(), "old build");
        assert!(staged_version(&dir).is_none() && !staged_path(&dir).exists());
        cleanup_old(&exe);
        assert!(!old_path(&exe).exists() && exe.exists());
        fs::write(staged_path(&dir), "stale").unwrap();
        fs::write(marker_path(&dir), r#"{"tag":"v0.0.1"}"#).unwrap();
        assert_eq!(apply_staged(&dir, &exe).unwrap(), None, "an older staged build is discarded");
        assert_eq!(fs::read_to_string(&exe).unwrap(), "new build");
        assert!(!staged_path(&dir).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_failure_keeps_old_build() {
        let dir = std::env::temp_dir().join(format!("niosapps-upd2-{}", std::process::id()));
        fs::create_dir_all(updates_dir(&dir)).unwrap();
        fs::write(marker_path(&dir), r#"{"tag":"v99.0.0"}"#).unwrap();
        fs::write(staged_path(&dir), "new").unwrap();
        let missing = dir.join("nope").join("NiosApps.exe");
        assert!(apply_staged(&dir, &missing).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
