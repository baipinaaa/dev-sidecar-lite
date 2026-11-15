//! 活跃连接注册表：支持列出、单独关闭、全部关闭。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use serde::Serialize;
use tokio::sync::oneshot;

/// 连接类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConnKind {
    Tunnel,
    Mitm,
    Http,
}

impl ConnKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tunnel => "tunnel",
            Self::Mitm => "mitm",
            Self::Http => "http",
        }
    }
}

/// 对外展示的连接信息。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnInfo {
    pub id: u64,
    pub host: String,
    pub port: u16,
    pub kind: ConnKind,
    /// 建立至今的毫秒数
    pub age_ms: u64,
    pub up_bytes: u64,
    pub down_bytes: u64,
    pub closed: bool,
}

#[derive(Debug)]
struct Entry {
    host: String,
    port: u16,
    kind: ConnKind,
    started: Instant,
    up_bytes: u64,
    down_bytes: u64,
    closed: bool,
    cancel: Option<oneshot::Sender<()>>,
}

/// 线程安全的连接注册表。
#[derive(Debug, Default)]
pub struct Registry {
    next_id: AtomicU64,
    entries: Mutex<HashMap<u64, Entry>>,
}

impl Registry {
    pub fn new() -> Self {
        Self {
            next_id: AtomicU64::new(1),
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// 注册一条连接，返回 id 与取消句柄。
    pub fn register(&self, host: impl Into<String>, port: u16, kind: ConnKind) -> ConnHandle<'_> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();

        if let Ok(mut entries) = self.entries.lock() {
            entries.insert(
                id,
                Entry {
                    host: host.into(),
                    port,
                    kind,
                    started: Instant::now(),
                    up_bytes: 0,
                    down_bytes: 0,
                    closed: false,
                    cancel: Some(tx),
                },
            );
        }

        ConnHandle {
            registry: self,
            id,
            cancel: Some(rx),
            finished: false,
        }
    }

    /// 快照（按 id 升序）。
    pub fn list(&self) -> Vec<ConnInfo> {
        let Ok(entries) = self.entries.lock() else {
            return Vec::new();
        };

        let mut list: Vec<ConnInfo> = entries
            .iter()
            .map(|(id, entry)| ConnInfo {
                id: *id,
                host: entry.host.clone(),
                port: entry.port,
                kind: entry.kind,
                age_ms: entry.started.elapsed().as_millis() as u64,
                up_bytes: entry.up_bytes,
                down_bytes: entry.down_bytes,
                closed: entry.closed,
            })
            .collect();

        list.sort_by_key(|info| info.id);
        list
    }

    /// 关闭单条连接；返回是否存在。
    pub fn close(&self, id: u64) -> bool {
        let Ok(mut entries) = self.entries.lock() else {
            return false;
        };
        let Some(entry) = entries.get_mut(&id) else {
            return false;
        };
        if let Some(tx) = entry.cancel.take() {
            let _ = tx.send(());
        }
        entry.closed = true;
        true
    }

    /// 关闭全部连接，返回关闭数量。
    pub fn close_all(&self) -> usize {
        let Ok(mut entries) = self.entries.lock() else {
            return 0;
        };
        let mut closed = 0;
        for entry in entries.values_mut() {
            if let Some(tx) = entry.cancel.take() {
                let _ = tx.send(());
                closed += 1;
            }
            entry.closed = true;
        }
        closed
    }

    /// 统计（连接数、累计上下行）。
    pub fn stats(&self) -> (usize, u64, u64) {
        let Ok(entries) = self.entries.lock() else {
            return (0, 0, 0);
        };
        let active = entries.values().filter(|entry| !entry.closed).count();
        let up: u64 = entries.values().map(|entry| entry.up_bytes).sum();
        let down: u64 = entries.values().map(|entry| entry.down_bytes).sum();
        (active, up, down)
    }

    fn finish(&self, id: u64, up: u64, down: u64) {
        if let Ok(mut entries) = self.entries.lock() {
            if let Some(entry) = entries.get_mut(&id) {
                entry.up_bytes = up;
                entry.down_bytes = down;
                entry.closed = true;
                entry.cancel = None;
            }
        }
    }

    /// 清理已关闭且结束时长的条目，避免长期运行累积。
    pub fn prune_closed(&self) -> usize {
        let Ok(mut entries) = self.entries.lock() else {
            return 0;
        };
        let before = entries.len();
        entries.retain(|_, entry| !entry.closed);
        before - entries.len()
    }
}

/// 单条连接的句柄：负责取消与字节数回填。
pub struct ConnHandle<'a> {
    registry: &'a Registry,
    id: u64,
    cancel: Option<oneshot::Receiver<()>>,
    finished: bool,
}

impl ConnHandle<'_> {
    pub fn id(&self) -> u64 {
        self.id
    }

    /// 取出取消信号（交给 `select!` 使用，避免与 `&mut self` 借用冲突）。
    pub fn take_cancel(&mut self) -> Option<oneshot::Receiver<()>> {
        self.cancel.take()
    }

    /// 等待取消信号（不消费句柄，`select!` 之后仍可 `finish`）。
    pub async fn cancelled(&mut self) {
        match self.cancel.take() {
            Some(rx) => {
                let _ = rx.await;
            }
            None => std::future::pending::<()>().await,
        }
    }

    /// 连接结束，回填字节数并标记关闭（不消费句柄，便于 `select!` 后调用）。
    pub fn finish(&mut self, up: u64, down: u64) {
        self.finished = true;
        self.registry.finish(self.id, up, down);
    }
}

impl Drop for ConnHandle<'_> {
    /// 兜底：出错提前返回时也要把连接标记为已关闭。
    fn drop(&mut self) {
        if !self.finished {
            self.registry.finish(self.id, 0, 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_list_and_finish() {
        let registry = Registry::new();
        let handle = registry.register("example.com", 443, ConnKind::Tunnel);
        let id = handle.id();

        let list = registry.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].host, "example.com");
        assert_eq!(list[0].kind, ConnKind::Tunnel);
        assert!(!list[0].closed);

        handle.finish(10, 20);
        let list = registry.list();
        assert!(list[0].closed);
        assert_eq!(list[0].up_bytes, 10);
        assert_eq!(list[0].down_bytes, 20);

        assert_eq!(registry.stats().0, 0, "已关闭的连接不计入活跃数");
        assert_eq!(registry.prune_closed(), 1);
        assert!(registry.list().is_empty());
        let _ = id;
    }

    #[tokio::test]
    async fn close_signals_cancel() {
        let registry = Registry::new();
        let mut handle = registry.register("a.test", 443, ConnKind::Mitm);
        let id = handle.id();

        assert!(registry.close(id), "关闭已注册连接应成功");
        handle.cancelled().await;

        assert!(!registry.close(9999), "关闭不存在的连接应返回 false");
        assert_eq!(registry.close_all(), 0, "已关闭的连接不再重复计数");
    }
}
