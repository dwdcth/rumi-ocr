"""跨平台路径解析与配置文件支持。

微信路径解析顺序（从高到低）:
  1. 代码显式传参 (OcrManager(wechat_path) / SetExePath)
  2. 环境变量: WECHAT_OCR_DIR / WECHAT_OCR_EXE / WECHAT_OCR_MMMOJO
  3. 配置文件 (JSON):
       - Linux/macOS: ~/.config/wechat_ocr/config.json (或 $XDG_CONFIG_HOME/wechat_ocr/config.json)
       - Windows:     %APPDATA%\\wechat_ocr\\config.json
     也可用环境变量 WECHAT_OCR_CONFIG 直接指定配置文件完整路径。
  4. 常见安装路径自动探测

配置文件示例 (config.json):
    {
        "wechat_dir": "/opt/wechat-universal",
        "ocr_exe_path": "/opt/wechat-universal/wxocr",
        "mmmojo_path": "/opt/wechat-universal/libmmmojo.so"
    }
只填 wechat_dir 即可, ocr_exe_path / mmmojo_path 缺省时会在该目录下递归查找。
"""
import glob
import json
import os
import re
import struct
import sys

APP_NAME = "wechat_ocr"


def _lib_names():
    """mmmojo 动态库的候选文件名。"""
    if sys.platform == "win32":
        return ["mmmojo_64.dll", "mmmojo.dll"]
    if sys.platform == "darwin":
        return ["libmmmojo.dylib", "mmmojo.dylib"]
    return ["libmmmojo.so"]


def _exe_names():
    """WeChatOCR 子进程可执行文件的候选文件名。"""
    if sys.platform == "win32":
        return ["WeChatOCR.exe"]
    return ["wxocr"]


def _config_dirs():
    if sys.platform == "win32":
        base = os.environ.get("APPDATA") or os.path.expanduser("~/AppData/Roaming")
        return [os.path.join(base, APP_NAME)]
    dirs = []
    xdg = os.environ.get("XDG_CONFIG_HOME")
    if xdg:
        dirs.append(os.path.join(xdg, APP_NAME))
    dirs.append(os.path.expanduser(os.path.join("~", ".config", APP_NAME)))
    return dirs


def config_file_candidates():
    env = os.environ.get("WECHAT_OCR_CONFIG")
    if env:
        return [env]
    return [os.path.join(d, "config.json") for d in _config_dirs()]


def load_config():
    """返回 (配置dict, 配置文件路径或None)。文件不存在返回 ({}, None)。"""
    for p in config_file_candidates():
        if p and os.path.isfile(p):
            try:
                with open(p, "r", encoding="utf-8") as f:
                    return json.load(f), p
            except Exception as e:
                raise RuntimeError(f"配置文件 {p} 解析失败: {e}")
    return {}, None


def _iter_files(root, names, max_depth=6):
    root = os.path.abspath(root)
    base_depth = root.rstrip(os.sep).count(os.sep)
    for dirpath, dirnames, filenames in os.walk(root):
        depth = dirpath.rstrip(os.sep).count(os.sep) - base_depth
        if depth > max_depth:
            dirnames[:] = []
            continue
        for fn in filenames:
            if fn in names:
                yield os.path.join(dirpath, fn)


def _find_first(root, names):
    """在 root 下查找 names 中任意文件, 先查根目录再递归(限深)。"""
    if not root or not os.path.isdir(root):
        return None
    for n in names:
        p = os.path.join(root, n)
        if os.path.isfile(p):
            return p
    for p in _iter_files(root, set(names)):
        return p
    return None


def _candidate_roots():
    """各平台常见微信安装根目录。"""
    if sys.platform == "win32":
        cands = []
        for drive in ("C:", "D:", "E:", "F:"):
            for prog in ("Program Files", "Program Files (x86)"):
                cands.append(os.path.join(drive + os.sep, prog, "Tencent", "WeChat"))
                cands.extend(glob.glob(os.path.join(drive + os.sep, prog, "Tencent", "WeChat*")))
        # 去重且保持顺序
        seen, out = set(), []
        for c in cands:
            if c.lower() not in seen:
                seen.add(c.lower())
                out.append(c)
        return out
    if sys.platform == "darwin":
        return ["/Applications/WeChat.app",
                os.path.expanduser("~/Applications/WeChat.app")]
    cands = ["/opt/wechat-universal", "/opt/wechat", "/usr/lib/wechat",
             os.path.expanduser("~/.local/share/WeChat")]
    cands += glob.glob("/opt/wechat*")
    seen, out = set(), []
    for c in cands:
        if c not in seen:
            seen.add(c)
            out.append(c)
    return out


def resolve(wechat_dir=None, ocr_exe_path=None, mmmojo_path=None):
    """解析微信目录 / OCR 可执行文件 / mmmojo 动态库的绝对路径。

    返回 dict: {"wechat_dir", "ocr_exe_path", "mmmojo_path", "source"}
    找不到时抛 FileNotFoundError, 消息中包含可用的配置方式提示。
    """
    cfg, cfg_path = load_config()
    wechat_dir = wechat_dir or os.environ.get("WECHAT_OCR_DIR") or cfg.get("wechat_dir")
    ocr_exe_path = ocr_exe_path or os.environ.get("WECHAT_OCR_EXE") or cfg.get("ocr_exe_path")
    mmmojo_path = mmmojo_path or os.environ.get("WECHAT_OCR_MMMOJO") or cfg.get("mmmojo_path")

    # 由 wechat_dir 推导缺失项
    if wechat_dir:
        ocr_exe_path = ocr_exe_path or _find_first(wechat_dir, _exe_names())
        mmmojo_path = mmmojo_path or _find_first(wechat_dir, _lib_names())
    else:
        # 自动探测: 找到第一个包含 mmmojo 库的根目录
        for root in _candidate_roots():
            lib = _find_first(root, _lib_names())
            if lib:
                wechat_dir = root
                mmmojo_path = mmmojo_path or lib
                ocr_exe_path = ocr_exe_path or _find_first(root, _exe_names())
                break

    missing = [name for name, val in
               (("wechat_dir(微信安装目录)", wechat_dir),
                ("ocr_exe_path(OCR可执行文件)", ocr_exe_path),
                ("mmmojo_path(mmmojo动态库)", mmmojo_path))
               if not val]
    if missing:
        raise FileNotFoundError(
            "无法自动定位: " + ", ".join(missing) + "。"
            "请任选其一配置: ① OcrManager(wechat_path) 显式传参; "
            "② 环境变量 WECHAT_OCR_DIR / WECHAT_OCR_EXE / WECHAT_OCR_MMMOJO; "
            "③ 配置文件 (示例见模块文档)。配置文件候选位置: "
            + ", ".join(config_file_candidates())
        )

    if cfg_path and any(cfg.get(k) for k in ("wechat_dir", "ocr_exe_path", "mmmojo_path")):
        source = cfg_path
    elif any(os.environ.get(k) for k in ("WECHAT_OCR_DIR", "WECHAT_OCR_EXE", "WECHAT_OCR_MMMOJO")):
        source = "environment"
    else:
        source = "autodetect"

    return {
        "wechat_dir": os.path.abspath(wechat_dir),
        "ocr_exe_path": os.path.abspath(ocr_exe_path),
        "mmmojo_path": os.path.abspath(mmmojo_path),
        "source": source,
    }


def detect_wechat_version(wechat_dir=None) -> "str | None":
    """尽力探测已安装微信的版本号 (如 '4.1.13.26'), 失败返回 None。

    - Windows: 读 wechat.dll / WeChatWin.dll 的文件版本资源
    - macOS:   读 WeChat.app/Contents/Info.plist
    - Linux:   扫描 wechat 主程序内嵌的 x.y.z.w 明文版本串 (取出现最多的 4.x)

    仅用于诊断/协议兼容提示 (配合 tools/verify_protocol.py), 不参与路径解析。
    """
    try:
        if sys.platform == "win32":
            import ctypes
            root = wechat_dir or resolve().get("wechat_dir") or ""
            for dll in ("wechat.dll", "WeChatWin.dll"):
                p = os.path.join(root, dll)
                if not os.path.isfile(p):
                    continue
                size = ctypes.windll.version.GetFileVersionInfoSizeW(p, None)
                if not size:
                    continue
                data = ctypes.create_string_buffer(size)
                if not ctypes.windll.version.GetFileVersionInfoW(p, 0, size, data):
                    continue
                val = ctypes.c_void_p()
                ln = ctypes.c_uint()
                if ctypes.windll.version.VerQueryValueW(
                        data, "\\VarFileInfo\\Translation", ctypes.byref(val), ctypes.byref(ln)) and ln.value:
                    lang, cp = struct.unpack("<HH", ctypes.string_at(val.value, 4))
                    sub = rf"\StringFileInfo\{lang:04X}{cp:04X}\ProductVersion"
                    if ctypes.windll.version.VerQueryValueW(
                            data, sub, ctypes.byref(val), ctypes.byref(ln)) and ln.value:
                        return ctypes.wstring_at(val.value, ln.value - 1).strip()
            return None
        if sys.platform == "darwin":
            import plistlib
            app = wechat_dir or "/Applications/WeChat.app"
            with open(os.path.join(app, "Contents", "Info.plist"), "rb") as f:
                return plistlib.load(f).get("CFBundleShortVersionString")
        # Linux: 主程序内嵌明文版本串, 取出现次数最多的 4.x.y.z
        root = wechat_dir or resolve().get("wechat_dir") or ""
        exe = os.path.join(root, "wechat") if os.path.isdir(root) else root
        if not os.path.isfile(exe):
            return None
        from collections import Counter
        pat = re.compile(rb"\b4\.\d{1,2}\.\d{1,3}\.\d{1,4}\b")
        counter = Counter()
        with open(exe, "rb") as f:
            for chunk in iter(lambda: f.read(8 << 20), b""):
                counter.update(pat.findall(chunk))
        return counter.most_common(1)[0][0].decode() if counter else None
    except Exception:
        return None
