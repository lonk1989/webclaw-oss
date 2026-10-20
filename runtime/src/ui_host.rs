//! 桌面走真实 tauri；worker 走 stub，避免链接 WebKit。

#![allow(unused_imports)]

#[cfg(feature = "desktop")]
pub use tauri::{
    AppHandle, Emitter, LogicalSize, Manager, Runtime, State, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder, Window, Wry,
};

#[cfg(not(feature = "desktop"))]
pub use crate::tauri_stub::{
    AppHandle, Emitter, LogicalSize, Manager, Runtime, State, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder, Window, Wry,
};
