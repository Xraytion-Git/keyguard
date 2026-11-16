// keyguard — 轻量级组合键拦截工具（静默后台 + 系统托盘）
//
// 核心：WH_KEYBOARD_LL 低层键盘钩子拦截系统级组合键（如 Ctrl+Win+D）
// 界面：纯 Win32 Shell_NotifyIcon 托盘图标 + 右键菜单（无控制台、无第三方 GUI 库）
// 配置：%APPDATA%\keyguard\config.json，可扩展到任意组合键

use std::fs;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{GetLastError, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::HICON;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NOTIFYICONDATAW, NIM_ADD, NIM_DELETE,
    Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CallNextHookEx, CreateIconFromResourceEx, CreatePopupMenu,
    CreateWindowExW, DefWindowProcW, DeleteMenu, DestroyIcon, DestroyWindow,
    DispatchMessageW, FindWindowW, GetCursorPos, GetDlgItem, GetMessageW,
    GetWindowTextW, HMENU, LoadCursorW, LoadImageW, MessageBoxW, MSG,
    PostMessageW, RegisterClassW, SendMessageW, SetWindowsHookExW,
    SetWindowTextW, ShowWindow, TrackPopupMenu, TranslateMessage,
    UnhookWindowsHookEx, WNDCLASSEXW,
    CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT,
    IMAGE_FLAGS, IMAGE_ICON, LR_LOADFROMFILE,
    MB_OK, MENU_ITEM_FLAGS, MF_BYCOMMAND, MF_BYPOSITION, MF_CHECKED, MF_STRING, MF_UNCHECKED,
    TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, TRACK_POPUP_MENU_FLAGS, WM_APP,
    WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_QUIT, WH_KEYBOARD_LL,
    // 控件相关
    BS_PUSHBUTTON, ES_AUTOHSCROLL, LBS_NOINTEGRALHEIGHT, SW_SHOW,
    WS_BORDER, WS_CHILD, WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

// ============================================================
// 托盘自定义消息
// ============================================================
const WM_TRAY_CALLBACK: u32 = 0x8001;

// ============================================================
// 设置窗口控件 ID
// ============================================================
const IDC_LIST: u16 = 2001;
const IDC_EDIT: u16 = 2002;
const IDC_ADD: u16 = 2003;
const IDC_DELETE: u16 = 2004;
const IDC_SAVE: u16 = 2005;
const IDC_LABEL_LIST: u16 = 2006;
const IDC_LABEL_ADD: u16 = 2007;
const IDC_ENABLED: u16 = 2008;
const IDC_HEADER: u16 = 2009;
const IDC_SEP1: u16 = 2010;
const IDC_SEP2: u16 = 2011;

// 列表框消息
const LB_ADDSTRING: u32 = 0x0180;
const LB_GETCURSEL: u32 = 0x0188;
const LB_DELETESTRING: u32 = 0x0182;
const LB_RESETCONTENT: u32 = 0x0184;
const LB_GETCOUNT: u32 = 0x018B;
const LB_GETTEXTLEN: u32 = 0x018A;
const LB_GETTEXT: u32 = 0x0189;

// 按钮消息
const BM_GETCHECK: u32 = 0x00F0;
const BM_SETCHECK: u32 = 0x00F1;
const BST_CHECKED: usize = 1;
const BST_UNCHECKED: usize = 0;
const BS_AUTOCHECKBOX: u32 = 0x00000003;

// ============================================================
// 配置
// ============================================================
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Config {
    /// 拦截开关：true=拦截生效，false=临时放行
    #[serde(default = "default_true")]
    enabled: bool,
    /// 要拦截的组合键列表，每项形如 "Ctrl+Win+D"
    #[serde(default)]
    shortcuts: Vec<String>,
}

fn default_true() -> bool {
    true
}

impl Config {
    fn with_default() -> Self {
        Self {
            enabled: true,
            shortcuts: vec!["Ctrl+Win+D".into()],
        }
    }

    fn load() -> Self {
        let p = config_path();
        if p.exists() {
            if let Ok(str) = fs::read_to_string(&p) {
                if let Ok(c) = serde_json::from_str(&str) {
                    return c;
                }
            }
        }
        let c = Self::with_default();
        c.save();
        c
    }

    fn save(&self) {
        let p = config_path();
        if let Some(dir) = p.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = fs::write(&p, s);
        }
    }
}

fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from(r"C:\Users"))
        .join("keyguard")
        .join("config.json")
}

// ============================================================
// 组合键解析
// ============================================================
fn parse_shortcut(input: &str) -> Option<(u32, u16)> {
    const MOD_CTRL: u32 = 0x0001;
    const MOD_SHIFT: u32 = 0x0002;
    const MOD_ALT: u32 = 0x0004;
    const MOD_WIN: u32 = 0x0008;

    let parts: Vec<&str> = input.split('+').map(|p| p.trim()).collect();
    if parts.len() < 2 {
        return None;
    }

    let mut mods: u32 = 0;
    let mut main_vk: Option<u16> = None;

    for p in &parts {
        match p.to_uppercase().as_str() {
            "CTRL" => mods |= MOD_CTRL,
            "SHIFT" => mods |= MOD_SHIFT,
            "ALT" => mods |= MOD_ALT,
            "WIN" => mods |= MOD_WIN,
            _ => {
                if p.len() == 1 {
                    let c = p.chars().next()?;
                    if c.is_ascii_alphabetic() || c.is_ascii_digit() {
                        main_vk = Some(c.to_ascii_uppercase() as u16);
                    } else {
                        return None;
                    }
                } else if p.len() > 1
                    && p[1..].chars().all(|c| c.is_ascii_digit())
                    && p.starts_with('F')
                {
                    let n: u16 = p[1..].parse().ok()?;
                    if (1..=24).contains(&n) {
                        main_vk = Some(0x70 + n - 1);
                    }
                } else {
                    return None;
                }
            }
        }
    }

    let vk = main_vk?;
    Some((mods, vk))
}

// ============================================================
// 全局状态
// ============================================================
static BLOCKED: std::sync::Mutex<Vec<(u32, u16)>> = std::sync::Mutex::new(Vec::new());
static TRAY_ICON: std::sync::Mutex<Option<usize>> = std::sync::Mutex::new(None);
/// 拦截开关：true=拦截生效，false=临时放行
static ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

// ============================================================
// 键盘钩子回调
// ============================================================
unsafe extern "system" fn keyboard_proc(
    n_code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if n_code < 0 {
        return CallNextHookEx(None, n_code, wparam, lparam);
    }

    // 只拦"按下"事件（WM_KEYDOWN=0x0100, WM_SYSKEYDOWN=0x0104）
    if wparam.0 != 0x0100 && wparam.0 != 0x0104 {
        return LRESULT(0);
    }

    #[repr(C)]
    struct KbdLlHookStruct {
        vk_code: u32,
        scan_code: u32,
        flags: u32,
        time: u32,
        dw_extra_info: usize,
    }

    let raw = lparam.0 as *const KbdLlHookStruct;
    if raw.is_null() {
        return LRESULT(0);
    }
    let hook_struct = unsafe { &*raw };
    let vk = hook_struct.vk_code;
    let flags = hook_struct.flags;

    // 拦截开关：关闭时直接放行
    if !ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
        return LRESULT(0);
    }

    // LLKHF_INJECTED = 0x10：软件注入的按键直接放行（不影响音量/媒体键等）
    if flags & 0x10 != 0 {
        return LRESULT(0);
    }

    // 当前按下的修饰键
    let mut mods: u32 = 0;
    let pressed = |vk: i32| unsafe { GetKeyState(vk) } as u16 & 0x8000 != 0;
    if pressed(0x11) { mods |= 0x0001; } // Ctrl
    if pressed(0x10) { mods |= 0x0002; } // Shift
    if pressed(0x12) { mods |= 0x0004; } // Alt
    if pressed(0x5B) || pressed(0x5C) {
        mods |= 0x0008; // Win
    }

    // 命中拦截列表则吃掉
    if let Ok(blocked) = BLOCKED.lock() {
        for &(b_mods, b_vk) in blocked.iter() {
            if b_vk as u32 == vk && (b_mods & mods) == b_mods {
                return LRESULT(1);
            }
        }
    }
    LRESULT(0)
}

// ============================================================
// 图标加载（从 exe 同目录的 keyguard.ico 解码成 HICON）
// ============================================================
fn load_ico() -> Result<usize, Box<dyn std::error::Error>> {
    let ico_bytes: &[u8] = include_bytes!("../keyguard.ico");
    let tmp_path = std::env::temp_dir().join("keyguard_icon.ico");
    std::fs::write(&tmp_path, ico_bytes)?;

    let path_w: Vec<u16> = tmp_path.to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
    let hicon = unsafe {
        LoadImageW(
            None,
            windows::core::PCWSTR(path_w.as_ptr()),
            IMAGE_ICON,
            32,
            32,
            LR_LOADFROMFILE,
        )
    };
    match hicon {
        Ok(h) => Ok(h.0 as usize),
        Err(e) => Err(format!("LoadImageW failed: {e}").into()),
    }
}

// ============================================================
// 隐藏窗口（驱动托盘消息 + 右键菜单）
// ============================================================
fn create_hidden_window() -> Result<HWND, windows::core::Error> {
    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if msg == WM_TRAY_CALLBACK {
            handle_tray_message(hwnd, lparam.0 as i32);
            return LRESULT(0);
        }
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    use windows::Win32::UI::WindowsAndMessaging::WNDCLASSW;

    let class_name: Vec<u16> = "KeyGuardHiddenWnd\0".encode_utf16().collect();
    let module = unsafe { GetModuleHandleW(None) }.ok().unwrap_or_default();
    let cur_name: Vec<u16> = "32512\0".encode_utf16().collect();
    let wc = WNDCLASSW {
        style: windows::Win32::UI::WindowsAndMessaging::WNDCLASS_STYLES(0),
        lpfnWndProc: Some(wnd_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: module.into(),
        hIcon: Default::default(),
        hCursor: unsafe { LoadCursorW(None, windows::core::PCWSTR(cur_name.as_ptr())) }
            .ok()
            .unwrap_or_default(),
        hbrBackground: Default::default(),
        lpszMenuName: windows::core::PCWSTR::null(),
        lpszClassName: windows::core::PCWSTR(class_name.as_ptr()),
    };
    let atom = unsafe { RegisterClassW(&wc) };
    if atom == 0 {
        return Err(unsafe { GetLastError() }.into());
    }

    let win_name: Vec<u16> = "keyguard\0".encode_utf16().collect();
    unsafe {
        CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            windows::core::PCWSTR(class_name.as_ptr()),
            windows::core::PCWSTR(win_name.as_ptr()),
            windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(0),
            CW_USEDEFAULT as i32,
            CW_USEDEFAULT as i32,
            CW_USEDEFAULT as i32,
            CW_USEDEFAULT as i32,
            None,
            None,
            Some(module.into()),
            None,
        )
    }
}

// 托盘右键菜单 + 事件处理
fn handle_tray_message(hwnd: HWND, lparam: i32) {
    let msg_id = lparam;
    if msg_id == 0x0205 || msg_id == 0x0203 {
        unsafe {
            let hmenu = match CreatePopupMenu() {
                Ok(h) => h,
                Err(_) => return,
            };
            let mid_reload = 100;
            let mid_settings = 103;
            let mid_autostart = 104;
            let mid_config = 101;
            let mid_quit = 102;

            let t_reload: Vec<u16> = "重新加载配置\0".encode_utf16().collect();
            let t_settings: Vec<u16> = "设置...\0".encode_utf16().collect();
            let t_autostart: Vec<u16> = "开机启动\0".encode_utf16().collect();
            let t_config: Vec<u16> = "打开配置目录\0".encode_utf16().collect();
            let t_quit: Vec<u16> = "退出 keyguard\0".encode_utf16().collect();

            // 检查开机启动状态
            let autostart_on = is_autostart_enabled();
            let autostart_flags = if autostart_on {
                MF_STRING | MF_CHECKED
            } else {
                MF_STRING | MF_UNCHECKED
            };

            let _ = AppendMenuW(
                hmenu,
                MF_STRING | MF_BYCOMMAND,
                mid_reload as _,
                windows::core::PCWSTR(t_reload.as_ptr()),
            );
            let _ = AppendMenuW(
                hmenu,
                MF_STRING | MF_BYCOMMAND,
                mid_settings as _,
                windows::core::PCWSTR(t_settings.as_ptr()),
            );
            let _ = AppendMenuW(
                hmenu,
                autostart_flags | MF_BYCOMMAND,
                mid_autostart as _,
                windows::core::PCWSTR(t_autostart.as_ptr()),
            );
            let _ = AppendMenuW(
                hmenu,
                MF_STRING | MF_BYCOMMAND,
                mid_config as _,
                windows::core::PCWSTR(t_config.as_ptr()),
            );
            let _ = AppendMenuW(
                hmenu,
                MF_STRING | MF_BYCOMMAND,
                mid_quit as _,
                windows::core::PCWSTR(t_quit.as_ptr()),
            );

            let mut pt = POINT { x: 0, y: 0 };
            let _ = GetCursorPos(&mut pt);

            let ret = TrackPopupMenu(
                hmenu,
                TPM_LEFTALIGN | TPM_RIGHTBUTTON | TPM_RETURNCMD,
                pt.x,
                pt.y,
                None,
                hwnd,
                None,
            );
            let cmd = ret.0 as i32;
            if cmd == mid_reload {
                // 热重载配置（不重启）
                let cfg = Config::load();
                ENABLED.store(cfg.enabled, std::sync::atomic::Ordering::Relaxed);
                let mut parsed: Vec<(u32, u16)> = Vec::new();
                for s in &cfg.shortcuts {
                    if let Some((m, vk)) = parse_shortcut(s) {
                        parsed.push((m, vk));
                    }
                }
                *BLOCKED.lock().unwrap() = parsed;
            } else if cmd == mid_settings {
                show_settings_dialog();
            } else if cmd == mid_autostart {
                toggle_autostart();
            } else if cmd == mid_config {
                let p = config_path();
                if let Some(dir) = p.parent() {
                    let _ = std::process::Command::new("explorer").arg(dir).spawn();
                }
            } else if cmd == mid_quit {
                let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                let _ = PostMessageW(Some(hwnd), WM_QUIT, WPARAM(0), LPARAM(0));
                std::process::exit(0);
            }
            let _ = DeleteMenu(hmenu, 0, MF_BYCOMMAND | MF_BYPOSITION);
        }
    }
}

// ============================================================
// 开机启动管理
// ============================================================
fn autostart_shortcut_path() -> PathBuf {
    // 用 Startup 文件夹（最可靠，不需要管理员权限写注册表）
    let startup = std::env::var("APPDATA")
        .map(|p| PathBuf::from(p))
        .unwrap_or_else(|_| PathBuf::from(r"C:\Users"))
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs")
        .join("Startup");
    startup.join("keyguard.lnk")
}

fn is_autostart_enabled() -> bool {
    autostart_shortcut_path().exists()
}

fn toggle_autostart() {
    let lnk = autostart_shortcut_path();
    if lnk.exists() {
        // 删除快捷方式
        let _ = std::fs::remove_file(&lnk);
    } else {
        // 创建快捷方式
        if let Ok(exe) = std::env::current_exe() {
            // 用 PowerShell 创建 .lnk（比 COM 简单可靠）
            let exe_str = exe.to_string_lossy().to_string();
            let lnk_str = lnk.to_string_lossy().to_string();
            let ps = format!(
                "$s = New-Object -ComObject WScript.Shell; \
                 $sc = $s.CreateShortcut('{}'); \
                 $sc.TargetPath = '{}'; \
                 $sc.Description = 'keyguard'; \
                 $sc.Save()",
                lnk_str, exe_str
            );
            let _ = std::process::Command::new("powershell")
                .args(["-NoProfile", "-Command", &ps])
                .creation_flags(0x08000000) // CREATE_NO_WINDOW
                .spawn();
        }
    }
}

// ============================================================
// 设置窗口（图形配置界面）
// ============================================================
fn show_settings_dialog() {
    let class_name: Vec<u16> = "KeyGuardSettings\0".encode_utf16().collect();
    let existing = unsafe {
        FindWindowW(windows::core::PCWSTR(class_name.as_ptr()), None)
    };
    if let Ok(hwnd) = existing {
        if !hwnd.0.is_null() {
            let _ = unsafe { ShowWindow(hwnd, SW_SHOW) };
            return;
        }
    }

    // 注册设置窗口类
    unsafe extern "system" fn settings_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            0x0001 => {
                // WM_CREATE：创建子控件
                create_settings_controls(hwnd);
                // 填充列表
                populate_list(hwnd);
                LRESULT(0)
            }
            0x0111 => {
                // WM_COMMAND：按钮点击
                let id = (wparam.0 & 0xFFFF) as u16;
                match id {
                    x if x == IDC_ADD => {
                        add_shortcut_from_edit(hwnd);
                        LRESULT(0)
                    }
                    x if x == IDC_DELETE => {
                        delete_selected(hwnd);
                        LRESULT(0)
                    }
                    x if x == IDC_SAVE => {
                        save_config(hwnd);
                        LRESULT(0)
                    }
                    _ => LRESULT(0),
                }
            }
            0x0002 | 0x0010 => {
                // WM_DESTROY / WM_CLOSE
                unsafe { DestroyWindow(hwnd) };
                LRESULT(0)
            }
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    let module = unsafe { GetModuleHandleW(None) }.ok().unwrap_or_default();
    let cur_name: Vec<u16> = "32512\0".encode_utf16().collect();
    use windows::Win32::UI::WindowsAndMessaging::WNDCLASSW;
    // 加载自定义图标
    let hicon = load_ico().map(|h| windows::Win32::UI::WindowsAndMessaging::HICON(h as *mut _))
        .unwrap_or_default();
    let wc = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(settings_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: module.into(),
        hIcon: hicon,
        hCursor: unsafe {
            LoadCursorW(None, windows::core::PCWSTR(cur_name.as_ptr()))
        }
        .ok()
        .unwrap_or_default(),
        hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH(6usize as *mut _),
        lpszMenuName: windows::core::PCWSTR::null(),
        lpszClassName: windows::core::PCWSTR(class_name.as_ptr()),
    };
    unsafe {
        RegisterClassW(&wc);
    }

    let title: Vec<u16> = "keyguard 设置\0".encode_utf16().collect();
    let hwnd = unsafe {
        CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            windows::core::PCWSTR(class_name.as_ptr()),
            windows::core::PCWSTR(title.as_ptr()),
            WS_OVERLAPPEDWINDOW,
            300,
            150,
            460,
            500,
            None,
            None,
            Some(module.into()),
            None,
        )
    };
    if let Ok(hwnd) = hwnd {
        let _ = unsafe { ShowWindow(hwnd, SW_SHOW) };
    }
}

fn create_settings_controls(parent: HWND) {
    unsafe {
        let cls_static: Vec<u16> = "STATIC\0".encode_utf16().collect();
        let cls_listbox: Vec<u16> = "LISTBOX\0".encode_utf16().collect();
        let cls_edit: Vec<u16> = "EDIT\0".encode_utf16().collect();
        let cls_button: Vec<u16> = "BUTTON\0".encode_utf16().collect();
        let cls = |s: &Vec<u16>| windows::core::PCWSTR(s.as_ptr());
        let p = |s: &str| {
            let v: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
            windows::core::PCWSTR(v.as_ptr())
        };
        // 闭包捕获 v，需要让 v 活着——用 leak 保证生命周期
        let pl = |s: &str| {
            let v: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
            let leaked: &'static [u16] = Box::leak(v.into_boxed_slice());
            windows::core::PCWSTR(leaked.as_ptr())
        };

        let style_child = WS_CHILD | WS_VISIBLE;

        // ── 标题栏 ──
        CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            cls(&cls_static),
            pl("keyguard  ⚡ 快捷键拦截工具"),
            style_child,
            15, 12, 410, 28,
            Some(parent), Some(HMENU(IDC_HEADER as _)), None, None,
        );

        // ── 分隔线 1 ──
        CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            cls(&cls_static),
            pl(""),
            windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | 0x00000010), // SS_ETCHEDHORZ
            15, 46, 410, 2,
            Some(parent), Some(HMENU(IDC_SEP1 as _)), None, None,
        );

        // ── 拦截开关区 ──
        CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            cls(&cls_static),
            pl("拦截开关"),
            style_child,
            15, 56, 200, 20,
            Some(parent), None, None, None,
        );

        // 复选框：拦截已启用
        CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            cls(&cls_button),
            pl("启用快捷键拦截"),
            windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(
                WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_AUTOCHECKBOX
            ),
            15, 78, 200, 22,
            Some(parent), Some(HMENU(IDC_ENABLED as _)), None, None,
        );

        // 提示文字
        CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            cls(&cls_static),
            pl("取消勾选可临时放行所有快捷键，再次勾选恢复拦截"),
            style_child,
            15, 104, 410, 18,
            Some(parent), None, None, None,
        );

        // ── 分隔线 2 ──
        CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            cls(&cls_static),
            pl(""),
            windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | 0x00000010),
            15, 130, 410, 2,
            Some(parent), Some(HMENU(IDC_SEP2 as _)), None, None,
        );

        // ── 拦截规则区 ──
        CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            cls(&cls_static),
            pl("拦截规则列表"),
            style_child,
            15, 140, 200, 20,
            Some(parent), None, None, None,
        );

        // 列表框
        CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            cls(&cls_listbox),
            windows::core::PCWSTR::null(),
            windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(
                WS_CHILD.0 | WS_VISIBLE.0 | WS_BORDER.0 | WS_VSCROLL.0 | 0x0100
            ),
            15, 162, 410, 140,
            Some(parent), Some(HMENU(IDC_LIST as _)), None, None,
        );

        // ── 添加规则区 ──
        CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            cls(&cls_static),
            pl("添加新规则（格式如 Ctrl+Win+D）"),
            style_child,
            15, 312, 410, 20,
            Some(parent), None, None, None,
        );

        // 输入框
        CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            cls(&cls_edit),
            windows::core::PCWSTR::null(),
            windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(
                WS_CHILD.0 | WS_VISIBLE.0 | WS_BORDER.0 | WS_TABSTOP.0 | 0x0080
            ),
            15, 336, 300, 28,
            Some(parent), Some(HMENU(IDC_EDIT as _)), None, None,
        );

        // "添加" 按钮
        CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            cls(&cls_button),
            pl("＋  添加"),
            windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(
                WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0
            ),
            325, 336, 100, 28,
            Some(parent), Some(HMENU(IDC_ADD as _)), None, None,
        );

        // ── 分隔线 3 ──
        CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            cls(&cls_static),
            pl(""),
            windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | 0x00000010),
            15, 380, 410, 2,
            Some(parent), None, None, None,
        );

        // ── 操作按钮区 ──
        CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            cls(&cls_button),
            pl("删除选中"),
            windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(
                WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0
            ),
            15, 398, 120, 34,
            Some(parent), Some(HMENU(IDC_DELETE as _)), None, None,
        );

        CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            cls(&cls_button),
            pl("保存并生效"),
            windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(
                WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0
            ),
            305, 398, 120, 34,
            Some(parent), Some(HMENU(IDC_SAVE as _)), None, None,
        );
    }
}

fn populate_list(hwnd: HWND) {
    let config = Config::load();
    unsafe {
        // 设置勾选框状态
        let check_hwnd = GetDlgItem(Some(hwnd), IDC_ENABLED as i32).unwrap_or_default();
        let check_state = if config.enabled { BST_CHECKED } else { BST_UNCHECKED };
        SendMessageW(check_hwnd, BM_SETCHECK, Some(WPARAM(check_state)), Some(LPARAM(0)));

        // 填充列表
        let list_hwnd = GetDlgItem(Some(hwnd), IDC_LIST as i32).unwrap_or_default();
        SendMessageW(list_hwnd, LB_RESETCONTENT, Some(WPARAM(0)), Some(LPARAM(0)));
        for s in &config.shortcuts {
            let item: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
            SendMessageW(list_hwnd, LB_ADDSTRING, Some(WPARAM(0)), Some(LPARAM(item.as_ptr() as isize)));
        }
    }
}

fn add_shortcut_from_edit(hwnd: HWND) {
    unsafe {
        let edit_hwnd = GetDlgItem(Some(hwnd), IDC_EDIT as i32).unwrap_or_default();
        let mut buf = [0u16; 256];
        let len = GetWindowTextW(edit_hwnd, &mut buf);
        if len == 0 {
            return;
        }
        let input = String::from_utf16_lossy(&buf[..len as usize]).trim().to_string();
        if input.is_empty() {
            return;
        }

        // 验证格式
        if parse_shortcut(&input).is_none() {
            let msg: Vec<u16> = "格式无效！请用如 Ctrl+Win+D 的格式\0".encode_utf16().collect();
            MessageBoxW(Some(hwnd), windows::core::PCWSTR(msg.as_ptr()),
                windows::core::PCWSTR("提示\0".encode_utf16().collect::<Vec<u16>>().as_ptr()), MB_OK);
            return;
        }

        // 加入列表
        let list_hwnd = GetDlgItem(Some(hwnd), IDC_LIST as i32).unwrap_or_default();
        let item: Vec<u16> = input.encode_utf16().chain(std::iter::once(0)).collect();
        SendMessageW(list_hwnd, LB_ADDSTRING, Some(WPARAM(0)), Some(LPARAM(item.as_ptr() as isize)));

        // 清空输入框
        let _ = SetWindowTextW(edit_hwnd, windows::core::PCWSTR::null());
    }
}

fn delete_selected(hwnd: HWND) {
    unsafe {
        let list_hwnd = GetDlgItem(Some(hwnd), IDC_LIST as i32).unwrap_or_default();
        let sel = SendMessageW(list_hwnd, LB_GETCURSEL, Some(WPARAM(0)), Some(LPARAM(0)));
        if sel.0 >= 0 {
            SendMessageW(list_hwnd, LB_DELETESTRING, Some(WPARAM(sel.0 as usize)), Some(LPARAM(0)));
        }
    }
}

fn save_config(hwnd: HWND) {
    let mut shortcuts: Vec<String> = Vec::new();
    let mut enabled = true;
    unsafe {
        // 读取勾选框状态
        let check_hwnd = GetDlgItem(Some(hwnd), IDC_ENABLED as i32).unwrap_or_default();
        let check = SendMessageW(check_hwnd, BM_GETCHECK, Some(WPARAM(0)), Some(LPARAM(0)));
        enabled = check.0 == BST_CHECKED as isize;

        // 读取列表
        let list_hwnd = GetDlgItem(Some(hwnd), IDC_LIST as i32).unwrap_or_default();
        let count = SendMessageW(list_hwnd, LB_GETCOUNT, Some(WPARAM(0)), Some(LPARAM(0)));
        for i in 0..count.0 {
            let len = SendMessageW(list_hwnd, LB_GETTEXTLEN, Some(WPARAM(i as usize)), Some(LPARAM(0)));
            if len.0 > 0 {
                let mut buf = vec![0u16; len.0 as usize + 1];
                SendMessageW(list_hwnd, LB_GETTEXT, Some(WPARAM(i as usize)), Some(LPARAM(buf.as_mut_ptr() as isize)));
                let s = String::from_utf16_lossy(&buf[..len.0 as usize]).trim().to_string();
                if !s.is_empty() {
                    shortcuts.push(s);
                }
            }
        }
    }

    // 写入配置文件
    let config = Config { enabled, shortcuts: shortcuts.clone() };
    config.save();

    // 热更新运行时状态（不重启！）
    ENABLED.store(enabled, std::sync::atomic::Ordering::Relaxed);
    let mut parsed: Vec<(u32, u16)> = Vec::new();
    for s in &shortcuts {
        if let Some((m, vk)) = parse_shortcut(s) {
            parsed.push((m, vk));
        }
    }
    *BLOCKED.lock().unwrap() = parsed;

    // 关闭设置窗口（静默保存，不弹窗）
    unsafe { DestroyWindow(hwnd) };
}

// ============================================================
// 托盘图标注册
// ============================================================
fn register_tray(hwnd: HWND) -> Result<(), Box<dyn std::error::Error>> {
    let hicon = load_ico()?;

    let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = hwnd;
    data.uID = 1;
    data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    data.uCallbackMessage = WM_TRAY_CALLBACK;
    data.hIcon = HICON(hicon as *mut _);
    let tip: Vec<u16> = "keyguard\0".encode_utf16().collect();
    for i in 0..tip.len() {
        if i < 128 {
            data.szTip[i] = tip[i];
        }
    }

    let ok = unsafe { Shell_NotifyIconW(NIM_ADD, &mut data) };
    if ok.0 == 0 {
        return Err("Shell_NotifyIconW NIM_ADD failed".into());
    }
    *TRAY_ICON.lock().unwrap() = Some(hicon);
    Ok(())
}

fn remove_tray(hwnd: HWND) {
    let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = hwnd;
    data.uID = 1;
    let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &mut data) };
    let hicon = *TRAY_ICON.lock().unwrap();
    if let Some(hi) = hicon {
        unsafe {
            DestroyIcon(HICON(hi as *mut _));
        }
    }
}

// ============================================================
// 主程序
// ============================================================
fn main() {
    // 1. 读配置
    let config = Config::load();

    // 2. 解析成 (mods, vk)
    let mut blocked: Vec<(u32, u16)> = Vec::new();
    for s in &config.shortcuts {
        match parse_shortcut(s) {
            Some((m, vk)) => blocked.push((m, vk)),
            None => {}
        }
    }
    // 初始化拦截规则
    *BLOCKED.lock().unwrap() = blocked;
    // 初始化拦截开关
    ENABLED.store(config.enabled, std::sync::atomic::Ordering::Relaxed);

    // 3. 装钩子
    let hook = match unsafe {
        SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), None, 0)
    } {
        Ok(h) => h,
        Err(_) => {
            std::process::exit(1);
        }
    };

    // 4. 创建隐藏窗口 + 注册托盘
    let hwnd = match create_hidden_window() {
        Ok(h) => h,
        Err(_) => {
            run_message_loop_no_window();
            let _ = unsafe { UnhookWindowsHookEx(hook) };
            return;
        }
    };
    let _ = register_tray(hwnd);

    // 5. 消息循环
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        let r = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if r.0 == 0 { break; }
        if r.0 == -1 { continue; }
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    // 6. 退出清理
    remove_tray(hwnd);
    let _ = unsafe { UnhookWindowsHookEx(hook) };
}

fn run_message_loop_no_window() {
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        let r = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if r.0 == 0 {
            break;
        }
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
