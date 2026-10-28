import asyncio
import os
import tempfile
import time
from enum import Enum
from typing import Dict, Callable, Set
from multiprocessing import Queue, Value

from . import ocr_wx4_pb2
from .winapi import *
from .mmmojo_dll import MMMojoInfoMethod
from .xplugin_manager import XPluginManager
from .config import resolve

OCR_MAX_TASK_ID = 32

# ── 协议常量的唯一事实源 (变更须同步 docs/PROTOCOL.md 与 tools/verify_protocol.py) ──
# 逆向验证于 wechat-universal 4.1.13.26 (Linux); 微信大版本更新后请先跑
# tools/verify_protocol.py 复验, 失败项的复推步骤见 docs/PROTOCOL.md。
PROTOCOL_VERIFIED_VERSION = "4.1.13.26"

# qrcode 结果的 type 值 → 名称（实测 wechat-universal 4.1.13；引擎内嵌 zxing 改造版，
# 小程序码为自有扩展 zxing::vqrcode::VQRCodeReader，其余条码格式走 ocrResult 文本行）
QRCODE_TYPE_NAMES = {
    19: "QR_CODE",    # 普通二维码（text 为解码内容）
    22: "VQR_CODE",   # 小程序码/菊花码（text 为原始载荷，跳转信息需微信服务端解析）
}


class RequestIdOCR(Enum):
    """微信 4.x OCR 组件的请求/响应 ID (见 mmmojo 的 RequestIdOCR4)"""
    HAND_SHAKE = 10001
    REQ_OCR = 10010
    RESP_OCR = 10011
    # 零拷贝通路 (逆向 wechat-universal 4.1.13 主程序发送方 + wxocr 处理器):
    # method=kMMShared + request_id=10018 (PARSEOCR_INFO_REQ_SHARED)
    REQ_OCR_SHARED = 10018


def _pb_varint(value: int) -> bytes:
    out = bytearray()
    while value > 0x7F:
        out.append(0x80 | (value & 0x7F))
        value >>= 7
    out.append(value)
    return bytes(out)


def shared_ocr_meta_pb(width: int, height: int) -> bytes:
    """构造 ParseOCRReqSharedMessage (手写 pb, 字段布局逆向自微信主程序序列化器):

        message ParseOCRReqSharedMessage {
            optional string pic_path = 1;   # 可空
            optional uint32 width  = 2;     # 图片宽 (像素)
            optional uint32 height = 3;     # 图片高 (像素)
            optional ReqType rt = 4;        # t1=t2=true 同路径版
        }
    """
    rt = b"\x08\x01\x10\x01"  # {t1: true, t2: true}
    return (b"\x10" + _pb_varint(width) +
            b"\x18" + _pb_varint(height) +
            b"\x22" + bytes([len(rt)]) + rt)


# 兼容旧接口: 导入时尝试用默认规则解析出路径, 失败则为 None (可由配置文件/环境变量补救)
try:
    _default_paths = resolve()
    OCR_DEFAULT_EXE_PATH = _default_paths["ocr_exe_path"]
    OCR_DEFAULT_USER_LIB_DIR = _default_paths["wechat_dir"]
except Exception:
    OCR_DEFAULT_EXE_PATH = None
    OCR_DEFAULT_USER_LIB_DIR = None


def OCRRemoteOnConnect(is_connected:c_bool, user_data:py_object):
    print(f"OCRRemoteOnConnect 回调函数被调用, 参数, is_connected: {is_connected}")
    if user_data:
        manager_obj:OcrManager = cast(user_data, py_object).value
        manager_obj.SetConnectState(True)

def OCRRemoteOnDisConnect(user_data:py_object):
    print(f"OCRRemoteOnDisConnect 回调函数被调用 ")
    if user_data:
        manager_obj:OcrManager = cast(user_data, py_object).value
        manager_obj.SetConnectState(False)

def OCRReadOnPush(request_id:c_uint32, request_info:c_void_p, user_data:py_object):
    print(f"OCRReadOnPush 回调函数被调用 参数, request_id: {request_id}, request_info: {request_info}")
    if user_data:
        manager_obj:OcrManager = cast(user_data, py_object).value
        pb_size = c_uint32()
        pb_data = manager_obj.GetPbSerializedData(request_info, pb_size)
        if pb_size.value > 10:
            print(f"正在解析pb数据，pb数据大小: {pb_size.value}")
            manager_obj.CallUsrCallback(request_id, pb_data, pb_size.value)
            manager_obj.RemoveReadInfo(request_info)


class OcrManager(XPluginManager):
    # 注意: 任务队列/回调等状态必须是实例属性。类属性会跨实例共享
    # (C++ 成员变量直译的坑): 第二个实例 __init__ 时向满队列 put 会死锁,
    # 且实例间任务 id 相互污染。
    def __init__(self, wechat_path=None) -> None:
        super().__init__(wechat_path)
        self.m_task_id = Queue(OCR_MAX_TASK_ID)
        self.m_id_path: Dict[int, str] = {}
        self.m_wechatocr_running: bool = False
        self.m_connect_state: Value = Value('b', False)
        self.m_usr_callback: Callable = None
        self.m_usr_lib_dir = self.m_wechat_dir
        self._tmp_pic_paths: Set[str] = set()
        self._shared_pending: bool = False   # Shared 通路在途标记 (响应 task_id=0)
        for i in range(1, 33):
            self.m_task_id.put(i)

    def __del__(self):
        if self.m_wechatocr_running:
            self.KillWeChatOCR()

    def SetUsrLibDir(self, usr_lib_dir:str):
        self.m_usr_lib_dir = usr_lib_dir
        self.AppendSwitchNativeCmdLine("user-lib-dir", usr_lib_dir)

    def SetOcrResultCallback(self, func:Callable):
        self.m_usr_callback = func

    def StartWeChatOCR(self):
        self.SetCallbackUsrData(self)
        self.InitMMMojoEnv()
        self.m_wechatocr_running = True

    def KillWeChatOCR(self):
        self.m_connect_state.value = False
        self.m_wechatocr_running = False
        self._shared_pending = False   # 引擎停止, 在途任务不再有回调
        self.StopMMMojoEnv()
        self._cleanup_tmp_pics()

    def _wait_connected(self, timeout:float=None):
        """等待 wxocr 子进程握手完成。安静等待 (每 5 秒提示一次), 超时抛 TimeoutError。"""
        if self.m_connect_state.value:
            return
        print("等待Ocr服务连接成功…")
        deadline = None if timeout is None else time.time() + timeout
        last = time.time()
        while not self.m_connect_state.value:
            if deadline is not None and time.time() >= deadline:
                raise TimeoutError(f"等待微信OCR服务连接超时({timeout}秒)")
            time.sleep(0.1)
            if time.time() - last >= 5:
                print("仍在等待Ocr服务连接…")
                last = time.time()

    def DoOCRTask(self, pic_path:str):
        if not self.m_wechatocr_running:
            raise Exception("请先调用StartWeChatOCR启动")
        if not os.path.exists(pic_path):
            raise Exception(f"给定图片路径pic_path不存在: {pic_path}")
        # wx4 中，图片路径必须是绝对路径，否则会失败
        pic_path = os.path.abspath(pic_path)
        self._wait_connected()
        _id = self.GetIdleTaskId()
        if not _id:
            print("当前队列已满，请等待后重试")
            return
        self.SendOCRTask(_id, pic_path)

    def PreparePicData(self, pic_data:bytes, suffix:str=".png") -> str:
        """把编码图字节流落临时文件并登记 (回调后自动清理), 返回路径。
        供需要「先注册回调关联、再提交」的异步门面复刻 DoOCRTaskData 时使用。"""
        fd, pic_path = tempfile.mkstemp(suffix=suffix, prefix="wechat_ocr_")
        with os.fdopen(fd, "wb") as f:
            f.write(pic_data)
        self._tmp_pic_paths.add(pic_path)
        return pic_path

    def DoOCRTaskData(self, pic_data:bytes, suffix:str=".png") -> str:
        """提交图片字节流识别 (非阻塞), 返回实际使用的临时文件路径。

        优先使用 DoOCRTaskShared (零拷贝, 无需 Pillow 时也可用本方法兜底)。
        pic_data 为编码图 (PNG/JPEG 等), 先落临时文件再以绝对路径提交;
        结果回调后临时文件自动清理。
        """
        pic_path = self.PreparePicData(pic_data, suffix)
        try:
            self.DoOCRTask(pic_path)
        except Exception:
            self._remove_tmp_pic(pic_path)
            raise
        return pic_path

    def DoOCRTaskShared(self, rgba:bytes, width:int, height:int) -> None:
        """零拷贝字节流识别 (微信自身截图 OCR 同款通路, 逆向实证于 4.1.13):

        - request 缓冲区 = RGBA8888 原始像素 (长度须为 width*height*4)
        - attach 缓冲区  = ParseOCRReqSharedMessage 元数据 (宽/高/rt)
        - method=kMMShared(4), request_id=10018 (REQ_OCR_SHARED)
        - 响应仍是普通 10011 推送, task_id=0 (本消息无 task 字段)

        注意: 载荷格式错误会打断 mojo 连接 (子进程侧无容错), 请确保
        rgba 长度与 width/height 一致。
        """
        if not self.m_wechatocr_running:
            raise Exception("请先调用StartWeChatOCR启动")
        expected = width * height * 4
        if len(rgba) != expected:
            raise ValueError(f"rgba 长度 {len(rgba)} != width*height*4 = {expected}")
        self._wait_connected()
        self.m_id_path[0] = "<shared>"
        self._shared_pending = True
        self.SendBuffersWithAttach(
            rgba, shared_ocr_meta_pb(width, height),
            MMMojoInfoMethod.kMMShared.value, False, RequestIdOCR.REQ_OCR_SHARED.value,
        )

    # ── 协议级异步 (对应 swigger/wechat-ocr 的 doOCR(nullptr) + wait_done) ──
    # mojo 消息天然异步: 提交即返回, 结果经 10011 推送进 SetOcrResultCallback
    # 设置的回调; 路径版在途上限 32 (任务队列), Shared 版同刻仅 1。

    def PendingCount(self) -> int:
        """当前在途任务数 (路径版 + Shared 版)。"""
        n = OCR_MAX_TASK_ID - self.m_task_id.qsize()
        if self._shared_pending:
            n += 1
        return n

    def WaitDone(self, timeout:float=None) -> bool:
        """阻塞等待所有在途任务完成 (队列回到满员且无 Shared 在途)。
        timeout=None 无限等; 返回是否全部完成。"""
        deadline = None if timeout is None else time.time() + timeout
        while self.PendingCount() > 0:
            if deadline is not None and time.time() >= deadline:
                return self.PendingCount() == 0
            time.sleep(0.05)
        return True

    def _WaitTaskSlot(self, timeout:float=60.0) -> None:
        """等待任务队列出现空位 (批量入队超过 32 张时的背压点)。
        注意: 队列存放的是空闲 id —— 空 = 32 个全在途, 满 = 全空闲。"""
        deadline = time.time() + timeout
        while self.m_task_id.empty():
            if time.time() >= deadline:
                raise TimeoutError("等待任务队列空位超时")
            time.sleep(0.05)

    def DoOCRTasks(self, pic_paths, timeout_per_slot:float=60.0) -> int:
        """批量提交 (发射后不管): 队列满时自动等空位, 返回提交数。
        结果按完成顺序流入回调, 收尾用 WaitDone。"""
        n = 0
        for p in pic_paths:
            self._WaitTaskSlot(timeout_per_slot)
            self.DoOCRTask(p)
            n += 1
        return n

    def _remove_tmp_pic(self, pic_path:str) -> None:
        self._tmp_pic_paths.discard(pic_path)
        try:
            os.remove(pic_path)
        except OSError:
            pass

    def _cleanup_tmp_pics(self) -> None:
        """清理尚未等到回调的临时图片 (引擎停止/异常兜底)。"""
        for pic_path in list(self._tmp_pic_paths):
            self._remove_tmp_pic(pic_path)

    def SetConnectState(self, connect:bool):
        self.m_connect_state.value = connect

    def SendOCRTask(self, task_id:int, pic_path:str):
        self.m_id_path[task_id] = pic_path
        # 微信 4.x 协议: ParseOCRReqMessage
        ocr_request = ocr_wx4_pb2.ParseOCRReqMessage()
        ocr_request.task_id = task_id
        ocr_request.pic_path = pic_path
        # 字段含义未完全确认, 但置位 t1/t2 是识别成功的前提 (与微信4.x行为一致)
        ocr_request.rt.t1 = True
        ocr_request.rt.t2 = True
        ocr_request.rt.t3 = False
        serialized_data = ocr_request.SerializeToString()
        self.SendPbSerializedData(serialized_data, len(serialized_data), MMMojoInfoMethod.kMMPush.value, 0, RequestIdOCR.REQ_OCR.value)

    def CallUsrCallback(self, request_id:c_uint32, serialized_data: c_void_p, data_size: int):
        ocr_response_ubyte = (c_ubyte * data_size).from_address(serialized_data)
        ocr_response = ocr_wx4_pb2.ParseOCRRespMessage()
        ocr_response.ParseFromString(bytearray(ocr_response_ubyte))
        task_id = ocr_response.task_id
        pic_path = self.m_id_path.get(task_id)
        if pic_path is None:
            return
        if self.m_usr_callback:
            self.m_usr_callback(pic_path, self.parse_ocr_response(ocr_response))
        # task_id=0 是 Shared 通路 (无 task 字段), 不入任务队列
        if task_id:
            self.SetTaskIdIdle(task_id)
        elif task_id == 0:
            self._shared_pending = False
        if pic_path in self._tmp_pic_paths:
            self._remove_tmp_pic(pic_path)

    @staticmethod
    def parse_ocr_response(ocr_response) -> dict:
        """把微信 4.x 的 ParseOCRRespMessage 转成结果 dict。

        结构与旧版 (3.x) 保持基本一致, 方便下游代码迁移:
            {"taskId", "errcode", "width", "height",
             "ocrResult": [{"text", "rate", "location": {"left","top","right","bottom"}}],
             "qrcode": [{"text", "type", "corners": {...}}]}   # 图片含二维码时才有
        条形码 (EAN 等) 的数字会作为普通文本行出现在 ocrResult 中。
        """
        results = {
            "taskId": ocr_response.task_id,
            "errcode": ocr_response.err_code,
            "ocrResult": []
        }
        if ocr_response.HasField("res"):
            results["width"] = ocr_response.res.img_width
            results["height"] = ocr_response.res.img_height
            for line in ocr_response.res.lines:
                results["ocrResult"].append({
                    "text": line.text,
                    "rate": line.rate,
                    "location": {
                        "left": line.left,
                        "top": line.top,
                        "right": line.right,
                        "bottom": line.bottom,
                    },
                })
        if ocr_response.HasField("qrcode"):
            qrs = []
            for qr in ocr_response.qrcode.results:
                qrs.append({
                    "text": qr.text,
                    "type": qr.type,      # 码类型: 见 QRCODE_TYPE_NAMES (QR_CODE=19, VQR_CODE/小程序码=22)
                    "type_name": QRCODE_TYPE_NAMES.get(qr.type, "UNKNOWN"),
                    "corners": {
                        "topleft": {"x": qr.corners.topleft.x, "y": qr.corners.topleft.y},
                        "topright": {"x": qr.corners.topright.x, "y": qr.corners.topright.y},
                        "bottomright": {"x": qr.corners.bottomright.x, "y": qr.corners.bottomright.y},
                        "bottomleft": {"x": qr.corners.bottomleft.x, "y": qr.corners.bottomleft.y},
                    },
                })
            results["qrcode"] = qrs
        return results

    def GetIdleTaskId(self):
        # 队列满时 get(timeout=1) 抛 queue.Empty (上游漏接, 会让
        # DoOCRTask 的"队列已满"提示分支永远到不了) —— 接住返回 None
        try:
            return self.m_task_id.get(timeout=1)
        except Exception:
            return None

    def SetTaskIdIdle(self, _id):
        self.m_task_id.put(_id)


    def SetDefaultCallbaks(self):
        super().SetOneCallback("kMMRemoteConnect", OCRRemoteOnConnect)
        super().SetOneCallback("kMMRemoteDisconnect", OCRRemoteOnDisConnect)
        super().SetOneCallback("kMMReadPush", OCRReadOnPush)
        super().SetDefaultCallbaks()


class AsyncOcrManager(OcrManager):
    """OcrManager 的 asyncio 门面：await 即得结果 dict。

    - 原生线程回调经 call_soon_threadsafe 回填 Future（线程安全）
    - 路径版 (ocr_path/ocr_bytes) 可并发，上限即引擎任务队列 32
    - Shared 版 (ocr_rgba) 协议响应 task_id 恒为 0、无关联字段，
      用 asyncio.Lock 串行化——同一时刻仅一个在途
    - SetOcrResultCallback 设置的用户回调仍然全程触发（旁路观察）
    """

    def __init__(self, wechat_path=None) -> None:
        super().__init__(wechat_path)
        self._loop = None
        self._futures = {}           # 关联键(pic_path) -> Future
        self._user_cb = None
        self._shared_lock = None
        super().SetOcrResultCallback(self._dispatch)

    def SetOcrResultCallback(self, func:Callable):
        # 不覆盖内部派发器，用户回调作为旁路挂在 _dispatch 上
        self._user_cb = func

    def _dispatch(self, pic_path:str, r:dict):
        fut = self._futures.pop(pic_path, None)
        if fut is not None and not fut.done():
            self._loop.call_soon_threadsafe(self._resolve, fut, r)
        if self._user_cb is not None:
            self._user_cb(pic_path, r)

    @staticmethod
    def _resolve(fut, r:dict):
        if not fut.done():
            fut.set_result(r)

    async def _submit_keyed(self, key:str, submit:Callable):
        loop = asyncio.get_running_loop()
        self._loop = loop
        fut = loop.create_future()
        self._futures[key] = fut
        try:
            submit()
        except Exception:
            self._futures.pop(key, None)
            raise
        return await fut

    async def ocr_path(self, pic_path:str) -> dict:
        """识别图片文件（可并发，≤32）。同一路径并发提交会相互抢占关联键。"""
        pic_path = os.path.abspath(pic_path)
        return await self._submit_keyed(pic_path, lambda: self.DoOCRTask(pic_path))

    async def ocr_bytes(self, image_bytes:bytes, suffix:str=".png") -> dict:
        """识别编码图字节流（临时文件路径版，可并发）。
        先注册 future 再提交（消除回调先到的竞态），失败/完成均清理临时文件。"""
        path = self.PreparePicData(image_bytes, suffix)
        loop = asyncio.get_running_loop()
        self._loop = loop
        fut = loop.create_future()
        self._futures[path] = fut
        try:
            self.DoOCRTask(path)
        except Exception:
            self._futures.pop(path, None)
            self._remove_tmp_pic(path)
            raise
        try:
            return await fut
        finally:
            self._remove_tmp_pic(path)

    async def ocr_rgba(self, rgba:bytes, width:int, height:int) -> dict:
        """零拷贝识别 RGBA8888 像素（协议限制：串行，同一时刻仅一个在途）。"""
        if self._shared_lock is None:
            self._shared_lock = asyncio.Lock()
        async with self._shared_lock:
            return await self._submit_keyed(
                "<shared>", lambda: self.DoOCRTaskShared(rgba, width, height))

    async def ocr_paths(self, paths, concurrency:int=8):
        """批量识别文件路径，返回与入参同序的结果列表。"""
        sem = asyncio.Semaphore(min(concurrency, OCR_MAX_TASK_ID))

        async def one(p):
            async with sem:
                return await self.ocr_path(p)

        return await asyncio.gather(*(one(p) for p in paths))

