//! 托管 venv 管理（规范 §5）。
//!
//! 数据目录：`$XDG_DATA_HOME/rumi/venv`（默认 `~/.local/share/rumi/venv`）。
//! 首次启用插件时：无 venv → `python3 -m venv <dir>`；然后按规范 §6 依赖映射
//! [`pip_install`]。探测顺序：venv 存在用 venv python，否则系统 python3。

use crate::{Error, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// venv 内解释器路径（跨平台：Linux/macOS `bin/python3`，Windows `Scripts\python.exe`）。
pub fn python_path(venv_dir: &Path) -> PathBuf {
    if cfg!(windows) {
        venv_dir.join("Scripts").join("python.exe")
    } else {
        venv_dir.join("bin").join("python3")
    }
}

/// 确保 venv 存在，返回其解释器路径。
/// 解释器已存在则直接返回（幂等，不重建）；否则 `python3 -m venv <dir>`。
pub fn ensure(venv_dir: &Path) -> Result<PathBuf> {
    let py = python_path(venv_dir);
    if py.is_file() {
        return Ok(py);
    }
    if let Some(parent) = venv_dir.parent() {
        std::fs::create_dir_all(parent).map_err(Error::Io)?;
    }
    let out = Command::new("python3")
        .arg("-m")
        .arg("venv")
        .arg(venv_dir)
        .output()
        .map_err(|e| Error::Spawn(format!("无法运行 python3：{e}")))?;
    if !out.status.success() {
        return Err(Error::Spawn(format!(
            "python3 -m venv {} 失败：{}",
            venv_dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    if !py.is_file() {
        return Err(Error::Spawn(format!(
            "venv 创建后找不到解释器：{}",
            py.display()
        )));
    }
    Ok(py)
}

/// 在 venv 里 `pip install` 若干包。
/// `venv` 传 venv 目录（内部解析解释器）或直接传 ensure 返回的解释器路径。
pub fn pip_install(venv: &Path, pkgs: &[&str]) -> Result<()> {
    if pkgs.is_empty() {
        return Ok(());
    }
    let py = if venv.is_file() {
        venv.to_path_buf()
    } else {
        python_path(venv)
    };
    if !py.is_file() {
        return Err(Error::Spawn(format!(
            "pip_install：venv 解释器不存在 {}",
            py.display()
        )));
    }
    let out = Command::new(&py)
        .arg("-m")
        .arg("pip")
        .arg("install")
        .args(pkgs)
        .output()
        .map_err(Error::Io)?;
    if !out.status.success() {
        return Err(Error::Spawn(format!(
            "pip install {} 失败：{}",
            pkgs.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}
