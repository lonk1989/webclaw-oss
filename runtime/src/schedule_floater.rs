//! 定时任务桌面悬浮窗

use crate::settings;
use crate::ui_host::{AppHandle, Manager, Runtime, WebviewUrl, WebviewWindowBuilder};
use std::sync::Mutex;
use tauri::Emitter;

pub const FLOATER_LABEL: &str = "schedule-floater";
pub const FLOATER_CHANGED_EVENT: &str = "schedule-floater-visibility";

const FLOATER_W: f64 = 300.0;
const FLOATER_H: f64 = 360.0;
const FLOATER_MIN_W: f64 = 160.0;
const FLOATER_MIN_H: f64 = 40.0;
const FLOATER_COLLAPSED_W: f64 = 188.0;
const FLOATER_COLLAPSED_H: f64 = 42.0;
/// 最小化态弹出右键菜单时的临时尺寸（透明区留给菜单）
const FLOATER_MENU_W: f64 = 210.0;
const FLOATER_MENU_H: f64 = 236.0;
/// 最小化态退出确认对话框
const FLOATER_DIALOG_W: f64 = 300.0;
const FLOATER_DIALOG_H: f64 = 268.0;

/// 防止设置开关 / 保存任务并发建窗导致同 label 崩溃
static FLOATER_OP_LOCK: Mutex<()> = Mutex::new(());

/// 窗口创建/销毁必须在主线程；IPC / async 线程直接 build 会在 Windows 上崩进程。
/// `run_on_main_thread` 会阻塞调用方直到主线程执行完毕；若从 WebView 回调（主线程）同步
/// invoke 再调用它，会导致事件循环无法推进而白屏卡死。因此经 IPC 触发的建窗/隐藏统一走
/// async spawn，尺寸调整则像 task_progress 一样直接调用。
fn dispatch_window_op<R: Runtime, F: FnOnce() + Send + 'static>(app: AppHandle<R>, f: F) {
    tauri::async_runtime::spawn(async move {
        if let Err(e) = app.run_on_main_thread(f) {
            eprintln!("[schedule-floater] 无法调度到主线程: {e}");
        }
    });
}

pub fn is_visible<R: Runtime>(app: &AppHandle<R>) -> bool {
    app.get_webview_window(FLOATER_LABEL)
        .map(|w| w.is_visible().unwrap_or(false))
        .unwrap_or(false)
}

fn create_or_show_impl<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let _guard = FLOATER_OP_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    if let Some(win) = app.get_webview_window(FLOATER_LABEL) {
        let _ = win.show();
        let _ = win.unminimize();
        emit_visibility(app, true);
        return Ok(());
    }

    let mut builder = WebviewWindowBuilder::new(
        app,
        FLOATER_LABEL,
        WebviewUrl::App("schedule-floater.html".into()),
    )
    .title("定时任务")
    .inner_size(FLOATER_W, FLOATER_H)
    .min_inner_size(FLOATER_MIN_W, FLOATER_MIN_H)
    .resizable(false)
    .maximizable(false)
    .minimizable(false)
    .always_on_top(true)
    .decorations(false)
    .transparent(true)
    .shadow(true)
    .skip_taskbar(true);

    #[cfg(feature = "desktop")]
    if let Ok(icon) = crate::tray::load_hires_icon() {
        builder = builder.icon(icon).map_err(|e| e.to_string())?;
    }
    #[cfg(windows)]
    {
        builder = builder.drag_and_drop(false);
    }

    // 右上角偏内
    if let Ok(Some(m)) = app.primary_monitor() {
        let size = m.size();
        let scale = m.scale_factor();
        let sw = size.width as f64 / scale;
        let x = (sw - FLOATER_W - 24.0).max(12.0);
        let y = 48.0;
        builder = builder.position(x, y);
    } else {
        builder = builder.position(40.0, 48.0);
    }

    // 捕获可能的底层 panic，避免拖垮整个进程
    let build_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| builder.build()));
    match build_result {
        Ok(Ok(_)) => {
            emit_visibility(app, true);
            Ok(())
        }
        Ok(Err(e)) => Err(format!("无法创建定时任务悬浮窗: {e}")),
        Err(_) => Err("创建定时任务悬浮窗时发生内部错误".into()),
    }
}

pub fn create_or_show<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let app = app.clone();
    dispatch_window_op(app.clone(), move || {
        if let Err(e) = create_or_show_impl(&app) {
            eprintln!("[schedule-floater] {e}");
        }
    });
    Ok(())
}

fn hide_impl<R: Runtime>(app: &AppHandle<R>) {
    let _guard = FLOATER_OP_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(win) = app.get_webview_window(FLOATER_LABEL) {
        let _ = win.hide();
    }
    emit_visibility(app, false);
}

pub fn hide<R: Runtime>(app: &AppHandle<R>) {
    let app = app.clone();
    dispatch_window_op(app.clone(), move || {
        hide_impl(&app);
    });
}

pub fn close_window<R: Runtime>(app: &AppHandle<R>) {
    let app = app.clone();
    dispatch_window_op(app.clone(), move || {
        let _guard = FLOATER_OP_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(win) = app.get_webview_window(FLOATER_LABEL) {
            let _ = win.hide();
            let _ = win.close();
        }
        emit_visibility(&app, false);
    });
}

pub fn apply_from_settings<R: Runtime>(app: &AppHandle<R>) {
    let enabled = settings::load_settings()
        .map(|s| s.schedule_floater_enabled)
        .unwrap_or(false);
    if enabled {
        let _ = create_or_show(app);
    } else {
        hide(app);
    }
}

pub fn set_enabled<R: Runtime>(app: &AppHandle<R>, enabled: bool) -> Result<(), String> {
    let mut s = settings::load_settings()?;
    s.schedule_floater_enabled = enabled;
    settings::save_settings(&s)?;
    if enabled {
        create_or_show(app)?;
    } else {
        hide(app);
    }
    Ok(())
}

fn emit_visibility<R: Runtime>(app: &AppHandle<R>, visible: bool) {
    let _ = app.emit(
        FLOATER_CHANGED_EVENT,
        serde_json::json!({ "visible": visible }),
    );
}

pub fn notify_jobs_changed<R: Runtime>(app: &AppHandle<R>) {
    let _ = app.emit(crate::scheduler::SCHEDULED_JOBS_CHANGED_EVENT, ());
}

/// 展开 / 收起悬浮窗尺寸（前端最小化按钮调用）
pub fn set_collapsed<R: Runtime>(app: &AppHandle<R>, collapsed: bool) -> Result<(), String> {
    let (w, h) = if collapsed {
        (FLOATER_COLLAPSED_W, FLOATER_COLLAPSED_H)
    } else {
        (FLOATER_W, FLOATER_H)
    };
    set_inner_size(app, w, h)
}

/// 最小化态临时放大窗口，避免右键菜单 / 确认框被裁切。
/// `mode`: `"menu"` | `"dialog"` | 其它（恢复为收起尺寸）
pub fn set_popup_space<R: Runtime>(app: &AppHandle<R>, mode: &str) -> Result<(), String> {
    let (w, h) = match mode {
        "menu" => (FLOATER_MENU_W, FLOATER_MENU_H),
        "dialog" => (FLOATER_DIALOG_W, FLOATER_DIALOG_H),
        _ => (FLOATER_COLLAPSED_W, FLOATER_COLLAPSED_H),
    };
    set_inner_size(app, w, h)
}

fn set_inner_size<R: Runtime>(app: &AppHandle<R>, w: f64, h: f64) -> Result<(), String> {
    let Some(win) = app.get_webview_window(FLOATER_LABEL) else {
        return Ok(());
    };
    use crate::ui_host::LogicalSize;
    if let Err(e) = win.set_size(LogicalSize::new(w, h)) {
        eprintln!("[schedule-floater] 调整大小失败: {e}");
    }
    Ok(())
}
