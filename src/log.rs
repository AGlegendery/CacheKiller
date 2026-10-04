//! delete.log / failed.log — ~/.local/state/cachekiller on Linux (owned by the real
//! user under sudo), %LOCALAPPDATA%\CacheKiller on Windows.

use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use crate::platform::{self, Ctx};

pub struct Logger {
    dir: Option<PathBuf>,
    owner: Option<(u32, u32)>,
    pub ok: VecDeque<String>,
    pub failed: VecDeque<String>,
}

const KEEP: usize = 5;

impl Logger {
    pub fn new(ctx: &Ctx) -> Logger {
        let dir = ctx.log_dir.clone();
        // Create missing ancestors one by one so each can be handed back to the user.
        let mut missing = Vec::new();
        let mut cur = dir.as_path();
        while !cur.exists() && cur.starts_with(&ctx.home) && cur != ctx.home {
            missing.push(cur.to_path_buf());
            match cur.parent() {
                Some(p) => cur = p,
                None => break,
            }
        }
        let ok = fs::create_dir_all(&dir).is_ok();
        for p in &missing {
            platform::chown(p, ctx.log_owner);
        }
        Logger { dir: ok.then_some(dir), owner: ctx.log_owner, ok: VecDeque::new(), failed: VecDeque::new() }
    }

    pub fn add(&mut self, msg: &str, failed: bool) {
        let entry = format!("[{}] {msg}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
        if let Some(dir) = &self.dir {
            let file = dir.join(if failed { "failed.log" } else { "delete.log" });
            let fresh = !file.exists();
            if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&file) {
                let _ = writeln!(f, "{entry}");
            }
            if fresh {
                platform::chown(&file, self.owner);
            }
        }
        let q = if failed { &mut self.failed } else { &mut self.ok };
        q.push_front(entry);
        q.truncate(KEEP);
    }
}
