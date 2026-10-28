import sys
from ctypes import *

IS_WINDOWS = sys.platform == 'win32'
void = None

if IS_WINDOWS:
    from ctypes.wintypes import *

    user32 = WinDLL('user32', use_last_error=True)
    kernel32 = WinDLL('kernel32', use_last_error=True)

    class _SECURITY_ATTRIBUTES(Structure):
        _fields_ = [('nLength', DWORD),
                    ('lpSecurityDescriptor', LPVOID),
                    ('bInheritHandle', BOOL),]

    ULONG_PTR = c_ulong
    INFINITE = 0xFFFFFFFF 
    PROCESS_ALL_ACCESS = 0x1FFFFF 
    MEM_COMMIT = 0x00001000
    PAGE_EXECUTE_READWRITE = 0x40
    MEM_RELEASE = 0x00008000
    TH32CS_SNAPMODULE = 0x00000008
    TH32CS_SNAPMODULE32 = 0x00000010
    TH32CS_SNAPPROCESS = 0x00000002
    CS_HREDRAW = 2
    CS_VREDRAW = 1
    COLOR_WINDOW = 5
    WS_OVERLAPPEDWINDOW = 13565952
    CW_USEDEFAULT = -2147483648
    SW_HIDE = 0
    WM_COPYDATA = 0x004A

    LPSECURITY_ATTRIBUTES = POINTER(_SECURITY_ATTRIBUTES)
    LPTHREAD_START_ROUTINE = LPVOID
    FARPROC = SIZE_T = c_ulong
    HCURSOR = c_void_p
    LRESULT = c_int64
    WNDPROC = WINFUNCTYPE(LRESULT, HWND, UINT, WPARAM, LPARAM)

    class COPYDATASTRUCT(Structure):
        _fields_ = [
            ('dwData', LPARAM),
            ('cbData', DWORD),
            ('lpData', c_void_p)
        ]

    class MODULEENTRY32(Structure):
        _fields_ = [
            ("dwSize", DWORD),
            ("th32ModuleID", DWORD),
            ("th32ProcessID", DWORD),
            ("GlblcntUsage", DWORD),
            ("ProccntUsage", DWORD),
            ("modBaseAddr", POINTER(BYTE)),
            ("modBaseSize", DWORD),
            ("hModule", HMODULE),
            ("szModule", c_char*256),
            ("szExePath",  c_char*260),
        ]

    class PROCESSENTRY32(Structure):
        _fields_ = [
            ("dwSize", DWORD),
            ("cntUsage", DWORD),
            ("th32ProcessID", DWORD),
            ("th32DefaultHeapID", ULONG_PTR),
            ("th32ModuleID", DWORD),
            ("cntThreads", DWORD),
            ("th32ParentProcessID", DWORD),
            ("pcPriClassBase", LONG),
            ("dwFlags", DWORD),
            ("szExeFile",  c_char*260),
        ]

    class WNDCLASS(Structure):
        _fields_ = [('style', UINT),
                    ('lpfnWndProc', WNDPROC),
                    ('cbClsExtra', c_int),
                    ('cbWndExtra', c_int),
                    ('hInstance', HINSTANCE),
                    ('hIcon', HICON),
                    ('hCursor', HCURSOR),
                    ('hbrBackground', HBRUSH),
                    ('lpszMenuName', LPCWSTR),
                    ('lpszClassName', LPCWSTR)]

    def func_def(name, restype, *argtypes, dll=kernel32):
        cfunc = getattr(dll, name)
        cfunc.argtypes = argtypes
        cfunc.restype = restype
        return cfunc

    OpenProcess = func_def("OpenProcess", HANDLE, *(DWORD, BOOL, DWORD))
    VirtualAllocEx = func_def("VirtualAllocEx", LPVOID, *(HANDLE, LPVOID, SIZE_T, DWORD, DWORD))
    VirtualFreeEx = func_def("VirtualFreeEx", BOOL, *(HANDLE, LPVOID, SIZE_T, DWORD))
    WriteProcessMemory = func_def("WriteProcessMemory", BOOL, *(HANDLE, LPVOID, LPCVOID, SIZE_T, POINTER(SIZE_T)))
    GetModuleHandleA = func_def("GetModuleHandleA", HMODULE, *(LPCSTR,))
    GetModuleHandleW = func_def("GetModuleHandleW", HMODULE, *(LPCWSTR, ))
    GetProcAddress = func_def("GetProcAddress", c_void_p, *(HMODULE, LPCSTR))
    CreateRemoteThread = func_def("CreateRemoteThread", HANDLE, *(HANDLE, LPSECURITY_ATTRIBUTES, DWORD, LPTHREAD_START_ROUTINE, LPVOID, DWORD, LPDWORD))
    CloseHandle = func_def("CloseHandle", BOOL, *(HANDLE,))
    CreateToolhelp32Snapshot = func_def("CreateToolhelp32Snapshot", HANDLE, *(DWORD, DWORD))
    Module32First = func_def("Module32First", BOOL, *(HANDLE, POINTER(MODULEENTRY32)))
    Module32Next = func_def("Module32Next", BOOL, *(HANDLE, POINTER(MODULEENTRY32)))
    Process32First = func_def("Process32First", BOOL, *(HANDLE, POINTER(PROCESSENTRY32)))
    Process32Next = func_def("Process32Next", BOOL, *(HANDLE, POINTER(PROCESSENTRY32)))
    ReadProcessMemory = func_def("ReadProcessMemory", BOOL, *(HANDLE, LPCVOID, LPVOID, c_size_t, POINTER(c_size_t)))
    FindWindowW = func_def("FindWindowW", HWND, *(LPCWSTR, LPCWSTR), dll=user32)
    GetWindowThreadProcessId = func_def("GetWindowThreadProcessId", DWORD, *(HWND, LPDWORD), dll=user32)
    LoadLibraryW = func_def("LoadLibraryW", HMODULE, *(LPCWSTR,))
    FreeLibrary = func_def("FreeLibrary", BOOL, *(HMODULE, ))
    VirtualProtect = func_def("VirtualProtect", BOOL, *(LPVOID, SIZE_T, DWORD, PDWORD))
    VirtualProtectEx = func_def("VirtualProtectEx", BOOL, *(HANDLE, LPVOID, SIZE_T, DWORD, PDWORD))
    DefWindowProcW = func_def("DefWindowProcW", LRESULT, *(HWND, UINT, WPARAM, LPARAM), dll=user32)
    RegisterClassW = func_def("RegisterClassW", ATOM, *(POINTER(WNDCLASS), ), dll=user32)
    CreateWindowExW = func_def("CreateWindowExW", HWND, *(DWORD, LPCWSTR, LPCWSTR, DWORD, c_int, c_int, c_int, c_int, HWND, HMENU, HINSTANCE, LPVOID), dll=user32)
    ShowWindow = func_def('ShowWindow', BOOL, *(HWND, c_int), dll=user32)
    UpdateWindow = func_def("UpdateWindow", BOOL, *(HWND, ), dll=user32)
    GetMessageW = func_def("GetMessageW", BOOL, *(POINTER(MSG), HWND, UINT, UINT), dll=user32)
    TranslateMessage = func_def('TranslateMessage', BOOL, *(POINTER(MSG), ), dll=user32)
    DispatchMessageW = func_def('DispatchMessageW', LRESULT, *(POINTER(MSG),), dll=user32)
    WaitForSingleObject = func_def('WaitForSingleObject', DWORD, *(HANDLE, DWORD))
    GetExitCodeThread = func_def('GetExitCodeThread', BOOL, *(HANDLE, LPDWORD))

    def CloseSomeHandle(*args):
        for arg in args:
            if arg:
                CloseHandle(arg)

    def getPidByName(name=None, cls=None):
        hwnd = FindWindowW(cls, name)
        cpid = c_ulong()
        if GetWindowThreadProcessId(hwnd, byref(cpid)):
            return cpid.value
        return 0

    def getModuleInfo(moduleName, pid):
        hModuleSnap = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE|TH32CS_SNAPMODULE32, pid)
        me32 = MODULEENTRY32()
        me32.dwSize = sizeof(MODULEENTRY32)
        bRet = Module32First(hModuleSnap, pointer(me32))
        while bRet:
            szModule = me32.szModule.decode()
            if szModule.upper() == moduleName.upper():
                addr = cast(me32.modBaseAddr, c_void_p).value
                CloseHandle(hModuleSnap)
                module = {
                    'modBaseSize': me32.modBaseSize,
                    'th32ProcessID': me32.th32ProcessID,
                    'modBaseAddr': addr,
                    "hModule": me32.hModule,
                    'szModule': me32.szModule.decode("ansi", "ignore"),
                    'szExePath': me32.szExePath.decode("ansi", "ignore")
                }
                return module
            bRet = Module32Next(hModuleSnap, pointer(me32))
        CloseHandle(hModuleSnap)

    def getModuleBaseAddress(moduleName, pid):
        module = getModuleInfo(moduleName, pid)
        if module:
            return module["modBaseAddr"]

    def enumProcess(procName):
        hModuleSnap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
        pe32 = PROCESSENTRY32()
        pe32.dwSize = sizeof(PROCESSENTRY32)
        pids = []
        bRet = Process32First(hModuleSnap, pointer(pe32))
        while bRet:
            szExeFile = pe32.szExeFile.decode()
            if szExeFile.upper() == procName.upper():
                pid = pe32.th32ProcessID
                pids.append(pid)
            bRet = Process32Next(hModuleSnap, pointer(pe32))
        CloseHandle(hModuleSnap)
        return pids

else:
    # Linux & macOS / POSIX fallback definitions
    user32 = None
    kernel32 = None

    DWORD = c_uint32
    BOOL = c_bool
    BYTE = c_uint8
    WORD = c_uint16
    LONG = c_int32
    UINT = c_uint32
    ULONG_PTR = c_ulong
    LPVOID = c_void_p
    LPCVOID = c_void_p
    HANDLE = c_void_p
    HMODULE = c_void_p
    HWND = c_void_p
    HINSTANCE = c_void_p
    HICON = c_void_p
    HCURSOR = c_void_p
    HBRUSH = c_void_p
    HMENU = c_void_p
    ATOM = c_uint16
    WPARAM = c_ulong
    LPARAM = c_int64
    LRESULT = c_int64
    LPCSTR = c_char_p
    LPCWSTR = c_wchar_p
    PDWORD = POINTER(DWORD)
    LPDWORD = POINTER(DWORD)
    WINFUNCTYPE = CFUNCTYPE

    def func_def(name, restype, *argtypes, dll=None):
        if dll is None:
            raise RuntimeError(f"Cannot resolve function {name}: no dynamic library provided on {sys.platform}")
        cfunc = getattr(dll, name)
        cfunc.argtypes = argtypes
        cfunc.restype = restype
        return cfunc

    def CloseSomeHandle(*args):
        pass

    def getPidByName(name=None, cls=None):
        return 0

    def getModuleInfo(moduleName, pid):
        return None

    def getModuleBaseAddress(moduleName, pid):
        return None

    def enumProcess(procName):
        return []
