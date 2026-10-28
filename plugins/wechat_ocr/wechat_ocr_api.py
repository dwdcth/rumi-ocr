# -*- coding: utf-8 -*-
"""微信本地OCR —— Umi-OCR 插件协议适配层。

引擎为微信 4.x 自带本地 OCR（mmmojo + wxocr/WeChatOCR.exe 子进程），
库代码 vendored 于本插件目录 `wechat_ocr/`（上游 dwdcth/wechat_ocr，MIT）。

适配要点：
- runBytes 优先零拷贝 Shared 通路（request=RGBA 像素 + attach=元数据 pb +
  method=kMMShared + request_id=10018，逆向自微信主程序发送方，4.1.13 实测）；
  无 Pillow 时回退 tmpfs 临时文件路径版
- 引擎结果为异步回调（mmmojo 原生线程触发），此处用 Event 同步等待；
  宿主协议是一问一答（规范 §3），同一时刻仅一个识别在途
- 每次 start 新建 OcrManager（同实例 stop 后重建 mmmojo 环境会因 ctypes
  签名缓存冲突失败）；库的任务队列已是实例属性，重建无副作用
"""

import base64
import threading
import time


class Api:
    def __init__(self, globalArgd):
        # 目录留空 = 让库自动探测（环境变量 / 配置文件 / 常见安装路径）
        self._wechat_dir = str(globalArgd.get("wechat_dir") or "").strip() or None
        self._timeout = 30.0
        self._qr_mode = "no_text"  # no_text / always / never
        self._mgr = None  # OcrManager 单例，跨 start/stop 复用
        self._lock = threading.Lock()
        self._busy = False
        self._event = threading.Event()
        self._result = None

    # ── 生命周期 ──────────────────────────────────────────────────────

    def start(self, argd):
        try:
            timeout = float(argd.get("timeout") or 30)
        except (TypeError, ValueError):
            timeout = 30.0
        self._timeout = max(1.0, timeout)
        self._qr_mode = argd.get("qr_mode") or "no_text"
        if self._qr_mode not in ("no_text", "always", "never"):
            self._qr_mode = "no_text"

        # 懒 import：缺 protobuf / 无微信时 info 仍可用，错误在 start 呈现
        try:
            from .wechat_ocr.ocr_manager import OcrManager
        except Exception as e:
            return f"[Error] 导入微信OCR库失败（缺 protobuf？pip install protobuf）：{e}"

        # 每次启动用全新实例：mmmojo 环境在同实例上 stop 后重建会因 ctypes
        # 函数签名缓存冲突报错（实测）；库已把任务队列等改为实例属性，重建安全
        old = self._mgr
        if old is not None:
            self._mgr = None
            self._kill_in_background(old, join=5.0)
        try:
            mgr = OcrManager(self._wechat_dir)
            mgr.SetOcrResultCallback(self._on_result)
            mgr.StartWeChatOCR()
        except Exception as e:
            return f"[Error] 启动微信OCR失败：{e}"

        # 等待 wxocr 子进程握手（通常 1~3 秒）；起不来就报错并回收
        deadline = time.time() + self._timeout
        while not mgr.m_connect_state.value:
            if time.time() > deadline:
                self._kill_in_background(mgr)
                return f"[Error] 等待微信OCR服务连接超时（{self._timeout:g}秒），请检查微信 4.x 安装"
            time.sleep(0.2)

        self._mgr = mgr
        return ""

    def stop(self):
        mgr = self._mgr
        if mgr is None:
            return
        # KillWeChatOCR 偶发阻塞（wxocr 子进程异常时 StopMMMojoEnvironment 卡死），
        # 放守护线程限时执行，避免拖死 worker 协议；实例保留供下次 start 复用
        self._kill_in_background(mgr, join=5.0)

    @staticmethod
    def _kill_in_background(mgr, join=None):
        def _kill():
            try:
                mgr.KillWeChatOCR()
            except Exception:
                import traceback
                traceback.print_exc()

        t = threading.Thread(target=_kill, daemon=True)
        t.start()
        if join:
            t.join(join)

    # ── 识别 ──────────────────────────────────────────────────────────

    def runPath(self, imgPath: str):
        return self._submit(lambda mgr: mgr.DoOCRTask(imgPath))

    def runBytes(self, imageBytes):
        # 优先零拷贝 Shared 通路（微信自身截图 OCR 同款，需 Pillow 解码 RGBA）；
        # 无 Pillow / 解码失败时回退 tmpfs 临时文件路径
        try:
            import io
            from PIL import Image
            img = Image.open(io.BytesIO(imageBytes))
            w, h = img.size
            rgba = img.convert("RGBA").tobytes()
        except Exception:
            return self._submit(lambda mgr: mgr.DoOCRTaskData(imageBytes, ".png"))
        return self._submit(lambda mgr: mgr.DoOCRTaskShared(rgba, w, h))

    def runBase64(self, imageBase64: str):
        return self.runBytes(base64.b64decode(imageBase64))

    def _submit(self, submit):
        mgr = self._mgr
        if mgr is None or not mgr.m_wechatocr_running:
            return {"code": 102, "data": "[Error] 微信OCR引擎未启动，请先在插件页启动"}

        with self._lock:
            if self._busy:
                return {"code": 102, "data": "[Error] 上一个识别任务尚未返回"}
            self._busy = True
        try:
            self._event.clear()
            self._result = None
            submit(mgr)
            if not self._event.wait(self._timeout):
                return {"code": 102, "data": f"[Error] 识别超时（{self._timeout:g}秒）"}
            return self._to_umi(self._result or {})
        finally:
            with self._lock:
                self._busy = False

    def _on_result(self, img_path, r):
        with self._lock:
            self._result = r
        self._event.set()

    def _to_umi(self, r):
        """微信 4.x 结果 dict → Umi 插件契约 {"code":100,"data":[{text,box,score}]}。"""
        err = r.get("errcode", 0)
        if err != 0:
            return {"code": 102, "data": f"微信OCR返回错误 errcode={err}"}

        lines = []
        for it in r.get("ocrResult", []):
            loc = it.get("location") or {}
            left = loc.get("left", 0)
            top = loc.get("top", 0)
            right = loc.get("right", 0)
            bottom = loc.get("bottom", 0)
            lines.append({
                "text": it.get("text", ""),
                "box": [[left, top], [right, top], [right, bottom], [left, bottom]],
                "score": it.get("rate", 1.0),
            })

        # 二维码/小程序码（qr_mode 决定何时附加；条形码数字已作为文字行返回）
        qrs = r.get("qrcode", [])
        if qrs and self._qr_mode != "never" and (
            self._qr_mode == "always" or not lines
        ):
            lines.extend(self._qr_to_lines(qrs))

        return {"code": 100, "data": lines}

    @staticmethod
    def _qr_to_lines(qrs):
        """qrcode 结果 → Umi 行。小程序码载荷离线不可解析，加类型前缀标注。"""
        out = []
        for qr in qrs:
            c = qr.get("corners", {})
            pts = (c.get("topleft"), c.get("topright"),
                   c.get("bottomright"), c.get("bottomleft"))
            box = [[p.get("x", 0), p.get("y", 0)] if p else [0, 0] for p in pts]
            t = qr.get("type")
            text = qr.get("text", "")
            if t == 22:  # VQR_CODE 小程序码：载荷非明文，标注来源
                text = f"[小程序码] {text}"
            elif t != 19:  # 非 QR_CODE 的未知码型同样标注
                text = f"[码 type={t}] {text}"
            out.append({"text": text, "box": box, "score": 1.0})
        return out
