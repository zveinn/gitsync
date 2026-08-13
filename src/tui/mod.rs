mod ui;

use std::io::IsTerminal;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;
use tokio::sync::mpsc::UnboundedSender;

use crate::config::Config;
use crate::db::Db;
use crate::model::{ItemDetail, ItemQuery, ItemRow, StateFilter, TimeRange, View};
use crate::sync::{SyncCmd, SyncEvent, SyncKind};

pub fn run(
    db: Db,
    cfg: Config,
    ev_rx: Receiver<SyncEvent>,
    cmd_tx: UnboundedSender<SyncCmd>,
) -> Result<()> {
    if !std::io::stdout().is_terminal() {
        bail!("gitsync needs a real terminal (stdout is not a TTY)");
    }
    let mut terminal = ratatui::try_init()?;
    let mut app = App::new(db, cfg, ev_rx, cmd_tx);
    app.reload();
    let result = app.loop_until_quit(&mut terminal);
    ratatui::restore();
    result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal,
    Filter,
    Help,
    ConfirmBackfill,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    List,
    Detail,
}

struct Status {
    message: String,
    kind: StatusKind,
    set_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusKind {
    Info,
    Ok,
    Warn,
    Err,
}

pub(crate) struct App {
    db: Db,
    cfg: Config,
    ev_rx: Receiver<SyncEvent>,
    cmd_tx: UnboundedSender<SyncCmd>,
    query: ItemQuery,
    items: Vec<ItemRow>,
    counts: Vec<(View, usize)>,
    selected: usize,
    table_state: ratatui::widgets::TableState,
    detail: Option<ItemDetail>,
    detail_scroll: u16,
    preview_open: bool,
    focus: Focus,
    mode: Mode,
    filter_buf: String,
    status: Status,
    syncing: bool,
    sync_label: String,
    last_sync: Option<Instant>,
    last_sync_msg: String,
    gql_remaining: Option<u32>,
    gql_limit: Option<u32>,
    spinner: usize,
    tick: Instant,
    should_quit: bool,
}

impl App {
    fn new(
        db: Db,
        cfg: Config,
        ev_rx: Receiver<SyncEvent>,
        cmd_tx: UnboundedSender<SyncCmd>,
    ) -> Self {
        let query = ItemQuery {
            allowed_repos: cfg.allowed_repos.clone(),
            time: TimeRange::Days(30),
            state: StateFilter::Open,
            view: View::Inbox,
            search: String::new(),
        };
        Self {
            db,
            cfg,
            ev_rx,
            cmd_tx,
            query,
            items: Vec::new(),
            counts: Vec::new(),
            selected: 0,
            table_state: ratatui::widgets::TableState::default(),
            detail: None,
            detail_scroll: 0,
            preview_open: false,
            focus: Focus::List,
            mode: Mode::Normal,
            filter_buf: String::new(),
            status: Status {
                message: "local cache · syncing in background".into(),
                kind: StatusKind::Info,
                set_at: Instant::now(),
            },
            syncing: false,
            sync_label: String::new(),
            last_sync: None,
            last_sync_msg: String::new(),
            gql_remaining: None,
            gql_limit: None,
            spinner: 0,
            tick: Instant::now(),
            should_quit: false,
        }
    }

    fn loop_until_quit(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.should_quit {
            terminal.draw(|f| ui::draw(f, self))?;
            self.drain_sync();
            if event::poll(Duration::from_millis(120))? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => self.on_key(key),
                    Event::Resize(_, _) => {}
                    _ => {}
                }
            }
            if self.tick.elapsed() >= Duration::from_millis(250) {
                self.spinner = self.spinner.wrapping_add(1);
                self.tick = Instant::now();
            }
        }
        let _ = self.cmd_tx.send(SyncCmd::Shutdown);
        Ok(())
    }

    fn drain_sync(&mut self) {
        loop {
            match self.ev_rx.try_recv() {
                Ok(ev) => self.on_sync(ev),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !self.cfg.offline {
                        self.set_status(StatusKind::Warn, "sync worker stopped");
                    }
                    break;
                }
            }
        }
    }

    fn on_sync(&mut self, ev: SyncEvent) {
        match ev {
            SyncEvent::Started { kind, message } => {
                self.syncing = true;
                self.sync_label = message.clone();
                self.set_status(StatusKind::Info, format!("{}…", kind_label(kind)));
            }
            SyncEvent::Progress { message } => {
                self.sync_label = message.clone();
            }
            SyncEvent::Finished {
                kind,
                message,
                upserted,
                unread,
                ..
            } => {
                self.syncing = false;
                self.last_sync = Some(Instant::now());
                self.last_sync_msg = message.clone();
                self.reload();
                let extra = if unread > 0 {
                    format!(" · {unread} unread")
                } else if upserted > 0 {
                    format!(" · {upserted} updated")
                } else {
                    String::new()
                };
                self.set_status(StatusKind::Ok, format!("{}{extra}", message));
                let _ = kind;
            }
            SyncEvent::Failed { kind, error } => {
                self.syncing = false;
                self.set_status(StatusKind::Err, format!("{}: {error}", kind_label(kind)));
            }
            SyncEvent::Rate { remaining, limit } => {
                self.gql_remaining = remaining;
                self.gql_limit = limit;
            }
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        match self.mode {
            Mode::Help => {
                if matches!(
                    key.code,
                    KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q')
                ) {
                    self.mode = Mode::Normal;
                }
            }
            Mode::ConfirmBackfill => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.mode = Mode::Normal;
                    if self.cfg.has_token() && !self.cfg.offline {
                        let _ = self.cmd_tx.send(SyncCmd::Backfill);
                        self.set_status(StatusKind::Info, "full backfill started");
                    } else {
                        self.set_status(StatusKind::Warn, "offline / no token");
                    }
                }
                _ => self.mode = Mode::Normal,
            },
            Mode::Filter => match key.code {
                KeyCode::Esc => {
                    self.filter_buf.clear();
                    self.query.search.clear();
                    self.mode = Mode::Normal;
                    self.reload();
                }
                KeyCode::Enter => {
                    self.query.search = self.filter_buf.clone();
                    self.mode = Mode::Normal;
                    self.reload();
                }
                KeyCode::Backspace => {
                    self.filter_buf.pop();
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.filter_buf.push(c);
                }
                _ => {}
            },
            Mode::Normal => self.on_normal_key(key),
        }
    }

    fn on_normal_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char('/') => {
                self.mode = Mode::Filter;
                self.filter_buf = self.query.search.clone();
            }
            KeyCode::Esc => {
                if self.preview_open {
                    self.close_preview();
                } else if !self.query.search.is_empty() {
                    self.query.search.clear();
                    self.filter_buf.clear();
                    self.reload();
                }
            }
            KeyCode::Tab => {
                if self.preview_open {
                    self.focus = match self.focus {
                        Focus::List => Focus::Detail,
                        Focus::Detail => Focus::List,
                    };
                }
            }
            KeyCode::Char('i') => self.toggle_preview(),
            KeyCode::Char('h') | KeyCode::Left => self.shift_view(-1),
            KeyCode::Char('l') | KeyCode::Right => self.shift_view(1),
            KeyCode::Char('j') | KeyCode::Down => self.move_sel(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_sel(-1),
            KeyCode::Char('g') => self.select_abs(0),
            KeyCode::Char('G') => {
                let last = self.items.len().saturating_sub(1);
                self.select_abs(last);
            }
            KeyCode::PageDown | KeyCode::Char('d')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    || matches!(key.code, KeyCode::PageDown) =>
            {
                if self.preview_open && self.focus == Focus::Detail {
                    self.detail_scroll = self.detail_scroll.saturating_add(8);
                } else {
                    self.move_sel(10);
                }
            }
            KeyCode::PageUp | KeyCode::Char('u')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    || matches!(key.code, KeyCode::PageUp) =>
            {
                if self.preview_open && self.focus == Focus::Detail {
                    self.detail_scroll = self.detail_scroll.saturating_sub(8);
                } else {
                    self.move_sel(-10);
                }
            }
            KeyCode::Char('J') => self.detail_scroll = self.detail_scroll.saturating_add(1),
            KeyCode::Char('K') => self.detail_scroll = self.detail_scroll.saturating_sub(1),
            KeyCode::Char('t') => {
                self.query.time = self.query.time.cycle();
                self.reload();
                self.set_status(
                    StatusKind::Info,
                    format!("time {}", self.query.time.label()),
                );
            }
            KeyCode::Char('s') => {
                if self.query.view.uses_state_filter() {
                    self.query.state = self.query.state.cycle();
                    self.reload();
                    self.set_status(
                        StatusKind::Info,
                        format!("state {}", self.query.state.label()),
                    );
                }
            }
            KeyCode::Char('m') => self.toggle_read(),
            KeyCode::Char('n') => self.next_unread(1),
            KeyCode::Char('N') => self.next_unread(-1),
            KeyCode::Char('o') | KeyCode::Enter => self.open_browser(),
            KeyCode::Char('c') => self.request_comments(),
            KeyCode::Char('r') => self.refresh_selected(),
            KeyCode::Char('R') => {
                if self.cfg.has_token() && !self.cfg.offline {
                    let _ = self.cmd_tx.send(SyncCmd::Poll);
                    self.set_status(StatusKind::Info, "poll requested");
                } else {
                    self.set_status(StatusKind::Warn, "offline / no token");
                }
            }
            KeyCode::Char('F') => {
                self.mode = Mode::ConfirmBackfill;
            }
            _ => {}
        }
    }

    fn move_sel(&mut self, delta: i32) {
        if self.preview_open && self.focus == Focus::Detail {
            if delta > 0 {
                self.detail_scroll = self.detail_scroll.saturating_add(delta as u16);
            } else {
                self.detail_scroll = self.detail_scroll.saturating_sub((-delta) as u16);
            }
            return;
        }
        if self.items.is_empty() {
            return;
        }
        let next = (self.selected as i32 + delta).clamp(0, self.items.len() as i32 - 1) as usize;
        self.select_abs(next);
    }

    fn select_abs(&mut self, idx: usize) {
        if self.items.is_empty() {
            self.selected = 0;
            self.table_state.select(None);
            self.detail = None;
            return;
        }
        self.selected = idx.min(self.items.len() - 1);
        self.table_state.select(Some(self.selected));
        self.detail_scroll = 0;
        self.load_detail();
    }

    fn next_unread(&mut self, dir: i32) {
        if self.items.is_empty() {
            return;
        }
        let n = self.items.len();
        let start = self.selected;
        let mut i = start;
        for _ in 0..n {
            i = if dir > 0 {
                (i + 1) % n
            } else {
                (i + n - 1) % n
            };
            if self.items[i].unread {
                self.select_abs(i);
                return;
            }
        }
        self.set_status(StatusKind::Info, "no unread in this view");
    }

    fn toggle_read(&mut self) {
        let Some(item) = self.items.get(self.selected).cloned() else {
            return;
        };
        let next = !item.unread;
        if let Err(e) = self.db.set_unread(item.id, next) {
            self.set_status(StatusKind::Err, e.to_string());
            return;
        }
        self.reload_keep(item.id);
        self.set_status(
            StatusKind::Ok,
            if next { "marked unread" } else { "marked read" },
        );
    }

    fn open_browser(&mut self) {
        let url = self
            .items
            .get(self.selected)
            .and_then(|i| i.html_url.clone())
            .or_else(|| self.detail.as_ref().and_then(|d| d.row.html_url.clone()));
        match url {
            Some(url) => match open::that(&url) {
                Ok(()) => self.set_status(StatusKind::Ok, "opened in browser"),
                Err(e) => self.set_status(StatusKind::Err, e.to_string()),
            },
            None => self.set_status(StatusKind::Warn, "no url"),
        }
    }

    fn request_comments(&mut self) {
        let Some(item) = self.items.get(self.selected) else {
            return;
        };
        if self.cfg.offline || !self.cfg.has_token() {
            self.set_status(StatusKind::Warn, "offline / no token");
            return;
        }
        let _ = self.cmd_tx.send(SyncCmd::Comments {
            owner: item.owner.clone(),
            repo: item.repo.clone(),
            number: item.number,
            item_id: item.id,
        });
        self.set_status(StatusKind::Info, "loading comments");
    }

    fn refresh_selected(&mut self) {
        let Some(item) = self.items.get(self.selected) else {
            return;
        };
        if self.cfg.offline || !self.cfg.has_token() {
            self.set_status(StatusKind::Warn, "offline / no token");
            return;
        }
        let _ = self.cmd_tx.send(SyncCmd::Refresh {
            owner: item.owner.clone(),
            repo: item.repo.clone(),
            number: item.number,
            item_id: item.id,
        });
        self.set_status(StatusKind::Info, "refreshing item");
    }

    fn toggle_preview(&mut self) {
        if self.preview_open {
            self.close_preview();
            return;
        }
        if self.items.is_empty() {
            self.set_status(StatusKind::Info, "nothing to preview");
            return;
        }
        self.preview_open = true;
        self.focus = Focus::Detail;
        self.detail_scroll = 0;
        self.load_detail();
        if let Some(d) = &self.detail {
            if d.comments_fetched_at.is_none() {
                self.request_comments();
            }
        }
    }

    fn close_preview(&mut self) {
        self.preview_open = false;
        self.focus = Focus::List;
        self.detail_scroll = 0;
    }

    fn shift_view(&mut self, delta: i32) {
        self.query.view = self.query.view.shift(delta);
        self.reload();
    }

    fn reload(&mut self) {
        let keep = self.items.get(self.selected).map(|i| i.id);
        self.reload_keep(keep.unwrap_or(-1));
    }

    fn reload_keep(&mut self, keep_id: i64) {
        match self.db.list(&self.query) {
            Ok(items) => self.items = items,
            Err(e) => {
                self.set_status(StatusKind::Err, format!("db: {e}"));
                return;
            }
        }
        match self.db.counts_by_view(&self.query) {
            Ok(c) => self.counts = c,
            Err(e) => self.set_status(StatusKind::Warn, format!("counts: {e}")),
        }
        let idx = self.items.iter().position(|i| i.id == keep_id).unwrap_or(0);
        self.select_abs(idx);
    }

    fn load_detail(&mut self) {
        let Some(item) = self.items.get(self.selected) else {
            self.detail = None;
            return;
        };
        match self.db.get_detail(item.id) {
            Ok(d) => self.detail = d,
            Err(e) => {
                self.detail = None;
                self.set_status(StatusKind::Err, format!("detail: {e}"));
            }
        }
    }

    fn set_status(&mut self, kind: StatusKind, msg: impl Into<String>) {
        self.status = Status {
            message: msg.into(),
            kind,
            set_at: Instant::now(),
        };
    }
}

fn kind_label(k: SyncKind) -> &'static str {
    match k {
        SyncKind::Poll => "sync",
        SyncKind::Backfill => "backfill",
        SyncKind::RefreshItem => "refresh",
        SyncKind::Comments => "comments",
    }
}
