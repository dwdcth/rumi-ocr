//! venv 管理器测试（规范 §5）：ensure 幂等。

use plugins_worker::venv;
use std::path::PathBuf;

fn unique_tmp(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    std::env::temp_dir().join(format!("rumi-venvtest-{}-{}-{}", tag, std::process::id(), nanos))
}

/// ensure：不存在则 `python3 -m venv` 创建；再次调用直接返回同一解释器（不重建）。
#[test]
fn venv_ensure_is_idempotent() {
    let dir = unique_tmp("ensure");
    let py1 = venv::ensure(&dir).expect("首次 ensure 应创建 venv");
    assert!(py1.is_file(), "venv 解释器不存在：{}", py1.display());
    assert!(dir.join("pyvenv.cfg").is_file(), "pyvenv.cfg 缺失");

    // 解释器可用
    let out = std::process::Command::new(&py1)
        .arg("--version")
        .output()
        .expect("运行 venv python");
    assert!(out.status.success());

    // 幂等：第二次 ensure 返回同一路径，且 pyvenv.cfg 未被重建（mtime 不变）
    let m1 = std::fs::metadata(dir.join("pyvenv.cfg"))
        .unwrap()
        .modified()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    let py2 = venv::ensure(&dir).expect("二次 ensure");
    assert_eq!(py1, py2);
    let m2 = std::fs::metadata(dir.join("pyvenv.cfg"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(m1, m2, "二次 ensure 不应重建 venv");

    // pip_install 空包列表 = no-op
    venv::pip_install(&dir, &[]).expect("空 pip_install 应为 no-op");

    let _ = std::fs::remove_dir_all(&dir);
}
