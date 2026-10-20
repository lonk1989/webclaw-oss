//! 无桌面 UI 时替代 `tauri` crate，避免链接 WebKit/GTK。
//! 仅实现 worker 编译路径用到的类型与方法（全部为空操作）。

#![allow(dead_code)]

use serde::Serialize;
use std::future::Future;
use std::marker::PhantomData;

pub trait Runtime: Send + Sync + 'static {}

#[derive(Clone, Copy, Debug, Default)]
pub struct Wry;

impl Runtime for Wry {}

pub struct AppHandle<R: Runtime = Wry> {
    _r: PhantomData<R>,
}

impl<R: Runtime> Clone for AppHandle<R> {
    fn clone(&self) -> Self {
        Self { _r: PhantomData }
    }
}

impl<R: Runtime> Default for AppHandle<R> {
    fn default() -> Self {
        Self { _r: PhantomData }
    }
}

pub type Window<R = Wry> = AppHandle<R>;
pub type WebviewWindow<R = Wry> = AppHandle<R>;

pub struct State<'a, T>(pub &'a T);

impl<T> State<'_, T> {
    pub fn inner(&self) -> &T {
        self.0
    }
}

impl<T> std::ops::Deref for State<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        self.0
    }
}

pub trait Manager<R: Runtime>: Sized {
    fn get_webview_window(&self, _label: &str) -> Option<WebviewWindow<R>> {
        None
    }
    fn try_state<T: Send + Sync + 'static>(&self) -> Option<State<'_, T>> {
        None
    }
}

impl<R: Runtime> Manager<R> for AppHandle<R> {}

pub trait Emitter<R: Runtime> {
    fn emit<S: Serialize + Clone>(&self, _event: &str, _payload: S) -> Result<(), ()> {
        Ok(())
    }
}

impl<R: Runtime> Emitter<R> for AppHandle<R> {}

#[derive(Clone, Copy)]
pub struct LogicalSize<T> {
    pub width: T,
    pub height: T,
}

impl<T> LogicalSize<T> {
    pub fn new(width: T, height: T) -> Self {
        Self { width, height }
    }
}

pub enum WebviewUrl {
    App(String),
}

pub struct WebviewWindowBuilder<'a, R: Runtime> {
    _app: &'a AppHandle<R>,
}

impl<'a, R: Runtime> WebviewWindowBuilder<'a, R> {
    pub fn new(_app: &'a AppHandle<R>, _label: impl Into<String>, _url: WebviewUrl) -> Self {
        Self { _app }
    }
    pub fn title(self, _t: impl Into<String>) -> Self {
        self
    }
    pub fn inner_size(self, _w: f64, _h: f64) -> Self {
        self
    }
    pub fn resizable(self, _v: bool) -> Self {
        self
    }
    pub fn maximizable(self, _v: bool) -> Self {
        self
    }
    pub fn minimizable(self, _v: bool) -> Self {
        self
    }
    pub fn always_on_top(self, _v: bool) -> Self {
        self
    }
    pub fn decorations(self, _v: bool) -> Self {
        self
    }
    pub fn transparent(self, _v: bool) -> Self {
        self
    }
    pub fn shadow(self, _v: bool) -> Self {
        self
    }
    pub fn drag_and_drop(self, _v: bool) -> Self {
        self
    }
    pub fn position(self, _x: f64, _y: f64) -> Self {
        self
    }
    pub fn center(self) -> Self {
        self
    }
    pub fn icon<T>(self, _icon: T) -> Result<Self, String> {
        Ok(self)
    }
    pub fn build(self) -> Result<WebviewWindow<R>, String> {
        Err("worker 无桌面窗口".into())
    }
}

impl<R: Runtime> AppHandle<R> {
    pub fn exit(&self, _code: i32) {}
    pub fn set_size<T>(&self, _size: T) -> Result<(), ()> {
        Ok(())
    }
    pub fn show(&self) -> Result<(), ()> {
        Ok(())
    }
    pub fn hide(&self) -> Result<(), ()> {
        Ok(())
    }
    pub fn close(&self) -> Result<(), ()> {
        Ok(())
    }
    pub fn unminimize(&self) -> Result<(), ()> {
        Ok(())
    }
    pub fn set_focus(&self) -> Result<(), ()> {
        Ok(())
    }
}

pub mod async_runtime {
    use super::*;
    pub fn spawn<F>(fut: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        tokio::spawn(fut)
    }

    pub fn spawn_blocking<F, T>(f: F) -> tokio::task::JoinHandle<T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        tokio::task::spawn_blocking(f)
    }
}
