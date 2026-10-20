use super::{
    allocate_task_debug_port, create_page_target_on, is_debug_port_ready_on, kill_listeners_on_port,
    launch_debug_chrome_on, list_page_targets_on, wait_for_debug_port_on, yield_focus_to_webclaw,
};
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::Arc;
use parking_lot::Mutex;

/// 后台任务专用 Chrome：独立调试端口 + 独立 Profile，支持多任务并行。
pub struct ChromeInstance {
    pub port: u16,
    pub profile_dir: PathBuf,
    child: Mutex<Option<Child>>,
}

impl ChromeInstance {
    pub fn profile_for_task(task_id: &str) -> PathBuf {
        let safe: String = task_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        crate::paths::chrome_profiles_dir().join(safe)
    }

    pub async fn spawn_for_task(
        task_id: &str,
        download_dir: &Path,
        headless: bool,
    ) -> Result<Arc<Self>, String> {
        let port = allocate_task_debug_port().await?;
        // 主调试 Chrome 未运行时可直接复用其 Profile（同账号 Cookie）；
        // 主 Chrome 在 9222 运行时则使用任务 Profile，并由 executor 从主端口同步 Cookie。
        let profile_dir = if is_debug_port_ready_on(super::DEBUG_PORT).await {
            Self::profile_for_task(task_id)
        } else {
            super::debug_user_data_dir()
        };
        std::fs::create_dir_all(&profile_dir).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(download_dir).map_err(|e| e.to_string())?;

        let child = launch_debug_chrome_on(&profile_dir, port, headless)?;
        let inst = Arc::new(Self {
            port,
            profile_dir,
            child: Mutex::new(Some(child)),
        });
        wait_for_debug_port_on(port, 25).await?;
        let pages = list_page_targets_on(port).await.unwrap_or_default();
        if pages.is_empty() {
            create_page_target_on(port, Some("about:blank")).await?;
        }
        if !headless {
            tokio::time::sleep(std::time::Duration::from_millis(350)).await;
            yield_focus_to_webclaw();
        }
        Ok(inst)
    }

    pub async fn ensure_ready(&self, headless: bool) -> Result<String, String> {
        if !is_debug_port_ready_on(self.port).await {
            self.restart(headless).await?;
            return Ok(format!("已启动任务专用浏览器 | 端口 {}", self.port));
        }

        let pages = list_page_targets_on(self.port).await.unwrap_or_default();
        if pages.is_empty() {
            create_page_target_on(self.port, Some("about:blank")).await?;
        }
        let n = list_page_targets_on(self.port).await.map(|p| p.len()).unwrap_or(1);
        Ok(format!(
            "任务浏览器已就绪 | 端口 {} | {n} 个标签页",
            self.port
        ))
    }

    async fn restart(&self, headless: bool) -> Result<(), String> {
        self.force_kill();
        let child = launch_debug_chrome_on(&self.profile_dir, self.port, headless)?;
        *self.child.lock() = Some(child);
        wait_for_debug_port_on(self.port, 25).await?;
        if !headless {
            tokio::time::sleep(std::time::Duration::from_millis(350)).await;
            yield_focus_to_webclaw();
        }
        Ok(())
    }

    /// 结束本任务 Chrome（含子进程树），供任务结束或用户强制停止。
    pub fn shutdown(&self) {
        self.force_kill();
    }

    /// 立刻杀掉本端口上的 Chrome（不等 CDP），用于 OOM/卡死时快速停止。
    pub fn force_kill(&self) {
        crate::browser_network::drop_port_capture(self.port);
        if let Some(mut child) = self.child.lock().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        kill_listeners_on_port(self.port);
    }

    /// 任务中途崩溃后重启并等待调试端口就绪。
    pub async fn restart_after_crash(&self, headless: bool) -> Result<(), String> {
        self.restart(headless).await
    }
}
