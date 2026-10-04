use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::delete::{self, DelProgress, DelResult, DelTask};
use crate::locations::Category;
use crate::log::Logger;
use crate::scan::{self, Item, Progress, ScanResult};
use crate::platform::{self, Ctx};
use crate::util::{fmt_size, tilde};

pub const LARGE_THRESHOLD: u64 = 200 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Scanning,
    Browse,
    Confirm,
    Deleting,
    Help,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    Size,
    Age,
    Category,
    Path,
}

impl Sort {
    pub fn name(self) -> &'static str {
        match self {
            Sort::Size => "size",
            Sort::Age => "age",
            Sort::Category => "category",
            Sort::Path => "path",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SizeView {
    Reclaimable,
    Disk,
    Apparent,
}

impl SizeView {
    pub fn name(self) -> &'static str {
        match self {
            SizeView::Reclaimable => "reclaimable",
            SizeView::Disk => "disk usage",
            SizeView::Apparent => "apparent",
        }
    }

    pub fn of(self, it: &Item) -> u64 {
        match self {
            SizeView::Reclaimable => it.stats.reclaimable(),
            SizeView::Disk => it.stats.disk,
            SizeView::Apparent => it.stats.apparent,
        }
    }
}

pub struct ScanInfo {
    pub elapsed: Duration,
    pub files: u64,
    pub hidden_tmp: usize,
}

pub struct App {
    pub ctx: Arc<Ctx>,
    pub items: Vec<Item>,
    /// Indices into `items`, filtered and sorted.
    pub view: Vec<usize>,
    pub cursor: usize,
    pub offset: usize,
    pub page: usize,
    pub sort: Sort,
    pub size_view: SizeView,
    pub filter: Option<Category>,
    pub mode: Mode,
    pub scan_job: Option<(JoinHandle<ScanResult>, Arc<Progress>, Instant)>,
    pub del_job: Option<(JoinHandle<Vec<DelResult>>, Arc<DelProgress>, Instant)>,
    pub last_scan: Option<ScanInfo>,
    pub freed_session: u64,
    pub logger: Logger,
    pub flash: Option<(String, Instant)>,
    pub tick: usize,
    pub quit: bool,
    /// Quit and re-launch through sudo.
    pub elevate: bool,
}

impl App {
    pub fn new(ctx: Ctx) -> App {
        let logger = Logger::new(&ctx);
        let mut app = App {
            ctx: Arc::new(ctx),
            items: Vec::new(),
            view: Vec::new(),
            cursor: 0,
            offset: 0,
            page: 10,
            sort: Sort::Size,
            size_view: SizeView::Reclaimable,
            filter: None,
            mode: Mode::Scanning,
            scan_job: None,
            del_job: None,
            last_scan: None,
            freed_session: 0,
            logger,
            flash: None,
            tick: 0,
            quit: false,
            elevate: false,
        };
        app.start_scan();
        app
    }

    pub fn busy(&self) -> bool {
        self.scan_job.is_some() || self.del_job.is_some()
    }

    pub fn start_scan(&mut self) {
        let prog = Arc::new(Progress::default());
        let (ctx, p) = (self.ctx.clone(), prog.clone());
        let handle = thread::spawn(move || scan::scan(&ctx, &p));
        self.scan_job = Some((handle, prog, Instant::now()));
        self.mode = Mode::Scanning;
    }

    pub fn label(&self, it: &Item) -> String {
        let p = tilde(&it.path, &self.ctx.home);
        match &it.note {
            Some(n) => format!("{p}  ({n})"),
            None => p,
        }
    }

    fn start_delete(&mut self) {
        let tasks: Vec<DelTask> = self
            .items
            .iter()
            .filter(|i| i.selected)
            .map(|i| DelTask {
                label: self.label(i),
                targets: i.targets.clone(),
                roots: i.roots.clone(),
                special: i.special,
                expected: i.stats.reclaimable(),
            })
            .collect();
        if tasks.is_empty() {
            self.mode = Mode::Browse;
            return;
        }
        let prog = Arc::new(DelProgress::default());
        let (ctx, p) = (self.ctx.clone(), prog.clone());
        let handle = thread::spawn(move || delete::run(tasks, &ctx, &p));
        self.del_job = Some((handle, prog, Instant::now()));
        self.mode = Mode::Deleting;
    }

    /// Collect finished background jobs.
    pub fn poll_jobs(&mut self) {
        if self.scan_job.as_ref().is_some_and(|j| j.0.is_finished()) {
            let (h, _, _) = self.scan_job.take().unwrap();
            if let Ok(res) = h.join() {
                self.items = res.items;
                self.last_scan = Some(ScanInfo {
                    elapsed: res.elapsed,
                    files: res.files,
                    hidden_tmp: res.hidden_recent_tmp,
                });
            }
            self.cursor = 0;
            self.offset = 0;
            if self.filter.is_some_and(|c| !self.items.iter().any(|i| i.category == c)) {
                self.filter = None;
            }
            self.rebuild_view();
            self.mode = Mode::Browse;
        }
        if self.del_job.as_ref().is_some_and(|j| j.0.is_finished()) {
            let (h, _, started) = self.del_job.take().unwrap();
            let results = h.join().unwrap_or_default();
            let mut freed = 0;
            for r in &results {
                freed += r.freed;
                if r.removed > 0 {
                    self.logger.add(
                        &format!("Deleted {} ({}, {} entries)", r.label, fmt_size(r.freed), r.removed),
                        false,
                    );
                }
                if r.error_count > 0 {
                    let first = r.errors.first().cloned().unwrap_or_default();
                    let more = if r.error_count > 1 { format!(" (+{} more)", r.error_count - 1) } else { String::new() };
                    self.logger.add(&format!("Failed: {} - {first}{more}", r.label), true);
                }
            }
            self.freed_session += freed;
            self.flash = Some((
                format!("Freed {} in {:.1}s — rescanning…", fmt_size(freed), started.elapsed().as_secs_f64()),
                Instant::now(),
            ));
            self.start_scan();
        }
    }

    pub fn rebuild_view(&mut self) {
        let keep = self.view.get(self.cursor).copied();
        let mut v: Vec<usize> = (0..self.items.len())
            .filter(|&i| self.filter.is_none_or(|c| self.items[i].category == c))
            .collect();
        let items = &self.items;
        let sv = self.size_view;
        match self.sort {
            Sort::Size => v.sort_by_key(|&i| std::cmp::Reverse(sv.of(&items[i]))),
            Sort::Age => v.sort_by_key(|&i| items[i].stats.newest),
            Sort::Category => v.sort_by(|&a, &b| {
                items[a].category.cmp(&items[b].category).then(sv.of(&items[b]).cmp(&sv.of(&items[a])))
            }),
            Sort::Path => v.sort_by(|&a, &b| items[a].path.cmp(&items[b].path)),
        }
        self.view = v;
        self.cursor = keep.and_then(|k| self.view.iter().position(|&i| i == k)).unwrap_or(0);
        self.clamp();
    }

    fn clamp(&mut self) {
        let n = self.view.len();
        self.cursor = if n == 0 { 0 } else { self.cursor.min(n - 1) };
        let page = self.page.max(1);
        if self.cursor < self.offset {
            self.offset = self.cursor;
        } else if self.cursor >= self.offset + page {
            self.offset = self.cursor + 1 - page;
        }
        self.offset = self.offset.min(n.saturating_sub(page));
    }

    fn move_by(&mut self, d: isize) {
        let n = self.view.len() as isize;
        if n > 0 {
            self.cursor = (self.cursor as isize + d).clamp(0, n - 1) as usize;
        }
        self.clamp();
    }

    pub fn current(&self) -> Option<&Item> {
        self.view.get(self.cursor).map(|&i| &self.items[i])
    }

    pub fn selected(&self) -> (usize, u64) {
        self.items
            .iter()
            .filter(|i| i.selected)
            .fold((0, 0), |(n, s), i| (n + 1, s + i.stats.reclaimable()))
    }

    fn select_where(&mut self, f: impl Fn(&Item) -> bool) -> usize {
        let mut n = 0;
        for &i in &self.view {
            let it = &mut self.items[i];
            if it.selectable() && f(it) {
                it.selected = true;
                n += 1;
            }
        }
        n
    }

    fn cycle_filter(&mut self) {
        let present: Vec<Category> =
            Category::ALL.into_iter().filter(|c| self.items.iter().any(|i| i.category == *c)).collect();
        self.filter = match self.filter {
            None => present.first().copied(),
            Some(cur) => present.iter().skip_while(|c| **c != cur).nth(1).copied(),
        };
        self.offset = 0;
        self.rebuild_view();
    }

    pub fn on_key(&mut self, k: KeyEvent) {
        if k.modifiers.contains(KeyModifiers::CONTROL) && matches!(k.code, KeyCode::Char('c') | KeyCode::Char('q')) {
            self.quit = true;
            return;
        }
        match self.mode {
            Mode::Scanning | Mode::Deleting => {
                if matches!(k.code, KeyCode::Char('q')) {
                    self.quit = true;
                }
            }
            Mode::Help => self.mode = Mode::Browse,
            Mode::Confirm => match k.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => self.start_delete(),
                _ => self.mode = Mode::Browse,
            },
            Mode::Browse => self.browse_key(k),
        }
    }

    fn browse_key(&mut self, k: KeyEvent) {
        let page = self.page.max(1) as isize;
        match k.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
            KeyCode::PageUp => self.move_by(-page),
            KeyCode::PageDown => self.move_by(page),
            KeyCode::Home | KeyCode::Char('g') => self.move_by(isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => self.move_by(isize::MAX / 2),
            KeyCode::Char(' ') => {
                if let Some(&i) = self.view.get(self.cursor) {
                    let it = &mut self.items[i];
                    if it.selectable() || it.selected {
                        it.selected = !it.selected;
                    } else {
                        self.flash = Some((
                            format!("Locked: no permission to delete — press E to restart as {}", platform::ADMIN),
                            Instant::now(),
                        ));
                    }
                }
                self.move_by(1);
            }
            KeyCode::Char('a') | KeyCode::Char('A') => {
                self.select_where(|_| true);
            }
            KeyCode::Char('u') | KeyCode::Char('U') => self.items.iter_mut().for_each(|i| i.selected = false),
            KeyCode::Char('l') | KeyCode::Char('L') => {
                let n = self.select_where(|i| i.stats.reclaimable() > LARGE_THRESHOLD);
                self.flash = Some((format!("Selected {n} items larger than {}", fmt_size(LARGE_THRESHOLD)), Instant::now()));
            }
            KeyCode::Char('c') | KeyCode::Char('C') => {
                if let Some(cat) = self.current().map(|i| i.category) {
                    let n = self.select_where(|i| i.category == cat);
                    self.flash = Some((format!("Selected {n} {} items", cat.name()), Instant::now()));
                }
            }
            KeyCode::Tab => self.cycle_filter(),
            KeyCode::Char('s') | KeyCode::Char('S') => {
                self.sort = match self.sort {
                    Sort::Size => Sort::Age,
                    Sort::Age => Sort::Category,
                    Sort::Category => Sort::Path,
                    Sort::Path => Sort::Size,
                };
                self.rebuild_view();
            }
            KeyCode::Char('b') | KeyCode::Char('B') => {
                self.size_view = match self.size_view {
                    SizeView::Reclaimable => SizeView::Disk,
                    SizeView::Disk => SizeView::Apparent,
                    SizeView::Apparent => SizeView::Reclaimable,
                };
                self.rebuild_view();
            }
            KeyCode::Char('r') | KeyCode::Char('R') => self.start_scan(),
            KeyCode::Char('e') | KeyCode::Char('E') => {
                if self.ctx.is_admin {
                    self.flash = Some((format!("Already running as {}", platform::ROOT_LABEL), Instant::now()));
                } else {
                    self.elevate = true;
                    self.quit = true;
                }
            }
            KeyCode::Char('?') | KeyCode::Char('h') | KeyCode::F(1) => self.mode = Mode::Help,
            KeyCode::Enter | KeyCode::Delete => {
                if self.selected().0 > 0 {
                    self.mode = Mode::Confirm;
                } else {
                    self.flash = Some(("Nothing selected — Space to select, A for all".into(), Instant::now()));
                }
            }
            _ => {}
        }
    }
}
