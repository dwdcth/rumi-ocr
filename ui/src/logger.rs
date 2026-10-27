//! logger —— 运行日志：文件落盘 + stderr 双写（设置页可查看 / 复制 / 导出）。
//!
//! - 路径：`RUMI_DATA_DIR` 环境变量优先（测试用），否则
//!   `dirs::data_local_dir()/rumi/logs/rumi.log`（Windows/macOS/Linux 各自正确位置）。
//! - 启动时轮转：主日志 > 1MB 则改名为 `rumi.log.old`（保留一代），重新起一份。
//! - 写入：全局 `RwLock<Option<Mutex<File>>>`（测试需要换目录重初始化，不能用 OnceLock）。
//! - 时间戳：chrono Local `%Y-%m-%d %H:%M:%S`（用户本地时区——日志要对着"刚刚/几点
//!   发生"看，UTC 会差 8 小时；chrono 已在依赖树，声明直用零成本）。
//! - info/err 同时 eprintln（保留 nohup ui_run.out 的既有调试工作流）。

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, RwLock};

/// 轮转阈值：主日志启动时超过 1MB → 改名 `rumi.log.old`（保留一代），重新起一份
pub const ROTATE_BYTES: u64 = 1024 * 1024;

/// 当前日志目录（`init_at` 注入；None = 未初始化，按默认规则现算）
static ACTIVE_DIR: RwLock<Option<PathBuf>> = RwLock::new(None);
/// 写入句柄：None = 未初始化（info/err 只 eprintln，不 panic）
static HANDLE: RwLock<Option<Mutex<std::fs::File>>> = RwLock::new(None);

/// 日志目录（RUMI_DATA_DIR 优先，测试注入用）
pub fn log_dir() -> PathBuf {
    // init_at/init 跑过 → 用注入目录；否则按默认规则（与 init 同源）
    if let Some(dir) = ACTIVE_DIR.read().unwrap_or_else(|p| p.into_inner()).clone() {
        return dir;
    }
    if let Some(d) = std::env::var_os("RUMI_DATA_DIR") {
        return PathBuf::from(d).join("rumi").join("logs");
    }
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("rumi")
        .join("logs")
}

/// 当前日志文件路径
pub fn log_path() -> PathBuf {
    log_dir().join("rumi.log")
}

/// 启动初始化：建目录、轮转、打开句柄、写入启动行（含版本与 OS）
pub fn init() {
    // 与默认路径规则同源：RUMI_DATA_DIR 优先，兜底系统数据目录（再兜底当前目录）
    let base = std::env::var_os("RUMI_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::data_local_dir())
        .unwrap_or_else(|| PathBuf::from("."));
    init_at(&base);
}

/// [`init`] 的测试版：显式指定数据目录（绕过 env，避免测试间竞争）
pub fn init_at(data_dir: &std::path::Path) {
    let dir = data_dir.join("rumi").join("logs");
    let _ = std::fs::create_dir_all(&dir);
    // 先记目录（log_dir/log_path/read_all/export_to 据此定位），再换句柄
    *ACTIVE_DIR.write().unwrap_or_else(|p| p.into_inner()) = Some(dir.clone());
    let path = dir.join("rumi.log");
    // 轮转：> ROTATE_BYTES → 删旧 .old → rename（保留一代；失败静默，日志尽力而为）
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > ROTATE_BYTES {
            let old = dir.join("rumi.log.old");
            let _ = std::fs::remove_file(&old);
            let _ = std::fs::rename(&path, &old);
        }
    }
    // append 模式：未超限的旧日志继续追加（轮转只在超限时发生）
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok();
    *HANDLE.write().unwrap_or_else(|p| p.into_inner()) = file.map(Mutex::new);
    // 启动行：独立醒目分段（不带时间戳前缀），stderr 同步一份
    let line = format!(
        "===== Rumi OCR 启动 v0.1.0 os={} =====",
        std::env::consts::OS
    );
    write_line(&line);
}

/// 追加一行 "{ts} [{module}] {msg}"：写文件（失败忽略）+ eprintln
pub fn info(module: &str, msg: String) {
    write_line(&format!("{} [{module}] {msg}", now_ts()));
}

/// 同 [`info`]（模块名会原样进入行内，err 语义由调用方在 module/msg 体现）
pub fn err(module: &str, msg: String) {
    info(module, msg);
}

/// 单行落盘 + stderr（唯一写入口；句柄 None = 未初始化，只 eprintln 不 panic）
fn write_line(line: &str) {
    let guard = HANDLE.read().unwrap_or_else(|p| p.into_inner());
    if let Some(f) = guard.as_ref() {
        if let Ok(mut f) = f.lock() {
            let _ = writeln!(f, "{line}");
        }
    }
    drop(guard);
    eprintln!("{line}");
}

/// 当前时刻 → "YYYY-MM-DD HH:MM:SS"（本地时区，见模块注释）
fn now_ts() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 读取全部日志（文件缺失返回空串）
pub fn read_all() -> String {
    std::fs::read_to_string(log_path()).unwrap_or_default()
}

/// 读取尾部 max_bytes 字节；发生截断时首行加 "…（仅显示末尾 {n} 字节）"
pub fn tail(max_bytes: usize) -> String {
    let all = read_all();
    if all.len() <= max_bytes {
        return all;
    }
    // 字节窗口起点可能落在多字节字符（中文 3 字节）中间：推进到下一个字符边界
    let mut start = all.len() - max_bytes;
    while start < all.len() && !all.is_char_boundary(start) {
        start += 1;
    }
    format!("…（仅显示末尾 {max_bytes} 字节）\n{}", &all[start..])
}

/// 复制当前日志到目标路径（文件缺失 → Err("暂无日志文件")）
pub fn export_to(dest: &std::path::Path) -> Result<(), String> {
    let src = log_path();
    if !src.exists() {
        return Err("暂无日志文件".into());
    }
    std::fs::copy(&src, dest)
        .map(|_| ())
        .map_err(|e| format!("复制日志到 {}：{e}", dest.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// 日志目录/句柄是进程级全局，两个测试都要 init_at 换目录：必须串行，
    /// 否则甲测试 info 期间乙测试 init_at 会把行写进另一个文件（互相踩）。
    static SERIAL: Mutex<()> = Mutex::new(());

    /// 唯一临时目录（pid + 原子计数，结束清理，并行测试互不冲突）
    fn tmp_dir() -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("rumi-log-{}-{n}", std::process::id()))
    }

    /// 测试 1：init_at + 两行 info → read_all 含两个模块标记；tail(50) 截断提示生效
    #[test]
    fn init_info_read_and_tail_truncates() {
        let _g = SERIAL.lock().unwrap();
        let dir = tmp_dir();
        init_at(&dir);
        info("测试甲", "第一行内容".into());
        info("测试乙", "第二行内容".into());
        let all = read_all();
        assert!(all.contains("[测试甲]"), "read_all 应含模块标记甲：{all}");
        assert!(all.contains("[测试乙]"), "read_all 应含模块标记乙：{all}");
        assert!(
            all.contains("===== Rumi OCR 启动 v0.1.0 os="),
            "启动行缺失：{all}"
        );
        // tail(50)：内容远超 50 字节 → 首行为截断提示，且开头模块行已不可见
        let t = tail(50);
        assert!(
            t.starts_with("…（仅显示末尾 50 字节）\n"),
            "截断提示缺失：{t:?}"
        );
        assert!(!t.contains("[测试甲]"), "尾部 50 字节不应含开头模块行：{t:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 测试 2：预写 1.2MB 主日志再 init_at → 轮转为 rumi.log.old，新文件只有启动行
    #[test]
    fn init_rotates_oversized_log() {
        let _g = SERIAL.lock().unwrap();
        let dir = tmp_dir();
        let log = dir.join("rumi").join("logs").join("rumi.log");
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        // 1.2MB（> ROTATE_BYTES）：ROTATE_BYTES + 0.2MB（整数运算，不引浮点）
        let blob = vec![b'x'; ROTATE_BYTES as usize + 200 * 1024];
        std::fs::write(&log, &blob).unwrap();

        init_at(&dir);

        let old = dir.join("rumi").join("logs").join("rumi.log.old");
        assert!(old.exists(), "超限主日志应轮转为 rumi.log.old");
        assert_eq!(std::fs::metadata(&old).unwrap().len() as usize, blob.len());
        let fresh = std::fs::read_to_string(&log).unwrap();
        assert_eq!(
            fresh.lines().count(),
            1,
            "轮转后新文件应只有启动行：{fresh:?}"
        );
        assert!(
            fresh.contains(&format!("os={}", std::env::consts::OS)),
            "启动行应含 OS：{fresh:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
