use crate::launch;
use crate::paths;
use crate::task_progress;
use crate::AppState;
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    App, AppHandle, Manager, Runtime,
};

pub const TRAY_ID: &str = "main-tray";

/// 高 DPI 用 256 图标（避免 default_window_icon 偏糊）
pub fn load_hires_icon() -> Result<tauri::image::Image<'static>, String> {
    tauri::image::Image::from_bytes(include_bytes!("../icons/128x128@2x.png"))
        .map(|img| img.to_owned())
        .map_err(|e| format!("加载应用图标失败: {e}"))
}

pub fn apply_main_window_icon<R: Runtime>(app: &AppHandle<R>) {
    let Ok(icon) = load_hires_icon() else {
        return;
    };
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.set_icon(icon);
    }
}

pub fn setup(app: &mut App) -> Result<(), Box<dyn std::error::Error>> {
    let show_i = MenuItem::with_id(app, "tray_show", "打开主界面", true, None::<&str>)?;
    let hide_i = MenuItem::with_id(app, "tray_hide", "隐藏主界面", true, None::<&str>)?;
    let floater_show_i =
        MenuItem::with_id(app, "tray_floater_show", "显示定时任务悬浮窗", true, None::<&str>)?;
    let floater_hide_i =
        MenuItem::with_id(app, "tray_floater_hide", "隐藏定时任务悬浮窗", true, None::<&str>)?;
    let stop_i = MenuItem::with_id(app, "tray_stop_all", "停止全部任务", true, None::<&str>)?;
    let data_i = MenuItem::with_id(app, "tray_data_dir", "打开数据目录", true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let sep3 = PredefinedMenuItem::separator(app)?;
    let quit_i = MenuItem::with_id(app, "tray_quit", "退出 webclaw", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[
            &show_i,
            &hide_i,
            &sep1,
            &floater_show_i,
            &floater_hide_i,
            &sep2,
            &stop_i,
            &data_i,
            &sep3,
            &quit_i,
        ],
    )?;

    let icon = load_hires_icon()?;
    apply_main_window_icon(&app.handle());

    let _tray = TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .tooltip("webclaw 运行中")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(on_menu_event)
        .on_tray_icon_event(on_tray_icon_event)
        .build(app)?;

    Ok(())
}

fn on_menu_event<R: Runtime>(app: &AppHandle<R>, event: tauri::menu::MenuEvent) {
    match event.id().as_ref() {
        "tray_show" => launch::show_main_ui(app),
        "tray_hide" => hide_main_window(app),
        "tray_floater_show" => {
            let _ = crate::schedule_floater::set_enabled(app, true);
        }
        "tray_floater_hide" => {
            let _ = crate::schedule_floater::set_enabled(app, false);
        }
        "tray_stop_all" => stop_all_tasks(app),
        "tray_data_dir" => open_app_data_dir(),
        "tray_quit" => quit_app(app),
        _ => {}
    }
}

fn on_tray_icon_event(tray: &tauri::tray::TrayIcon, event: TrayIconEvent) {
    match event {
        TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        }
        | TrayIconEvent::DoubleClick {
            button: MouseButton::Left,
            ..
        } => {
            let app = tray.app_handle();
            launch::show_main_ui(&app);
        }
        _ => {}
    }
}

pub fn refresh_tooltip<R: Runtime>(app: &AppHandle<R>) {
    let tip = if let Some(state) = app.try_state::<AppState>() {
        let n = state.task_registry.active_count();
        if n > 0 {
            format!("webclaw 运行中 · {n} 个任务进行中")
        } else {
            "webclaw 运行中（托盘图标表示程序未退出）".into()
        }
    } else {
        "webclaw 运行中".into()
    };
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_tooltip(Some(tip));
    }
}

fn hide_main_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.hide();
    }
}

fn stop_all_tasks<R: Runtime>(app: &AppHandle<R>) {
    if let Some(state) = app.try_state::<AppState>() {
        let n = state.task_registry.active_count();
        if n == 0 {
            state.push_log("托盘：当前没有进行中的任务");
            crate::emit_logs_public(app, &state);
            return;
        }
        state.task_registry.stop_all();
        state.cancel_flag.store(true, std::sync::atomic::Ordering::SeqCst);
        crate::cdp::abort_all_pending("已取消");
        state.task_registry.for_each_active(|task_id, ctrl| {
            task_progress::emit_task_control_feedback(app, task_id, ctrl);
        });
        state.push_log(format!("托盘：已请求停止 {n} 个任务（浏览器保持运行）"));
        crate::emit_logs_public(app, &state);
        refresh_tooltip(app);
    }
}

pub fn quit_app<R: Runtime>(app: &AppHandle<R>) {
    if let Some(state) = app.try_state::<AppState>() {
        state.task_registry.stop_all();
        state.cancel_flag.store(true, std::sync::atomic::Ordering::SeqCst);
        crate::chrome::shutdown_debug_chrome(&state.chrome_child);
    } else {
        crate::chrome::kill_listeners_on_port(crate::chrome::DEBUG_PORT);
    }
    app.exit(0);
}

fn open_app_data_dir() {
    let path = paths::app_data_dir();
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer").arg(&path).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(&path).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(&path).spawn();
    }
}
