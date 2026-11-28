//! The window: title bar, sign-in, the chat list, the composer, the chatter card and toasts.
//!
//! The chat list lays each line out once (words, emotes, badges placed by hand so emotes sit
//! inline) and keeps that until the window width changes. Only rows on screen are painted.
//! While the pointer moves over chat, or chat is scrolled up, new lines wait below so nothing
//! slides out from under the mouse.

use crate::backend::Backend;
use crate::images::{Icons, Images};
use crate::lines::{ChatEvent, Part};
use crate::theme::{self, *};
use crate::twitch::DeviceCode;
use crate::ui_events::{Ui as Event, User};
use eframe::egui::{
    self, pos2, vec2, Align2, Color32, CornerRadius, CursorIcon, Galley, Id, Key, Modifiers, Order, Pos2, Rect, Response,
    Sense, Stroke, TextureId, UiBuilder, ViewportCommand,
};
use eframe::egui::text::{LayoutJob, TextFormat};
use crate::live::{HypeTrain, Live, Participant, Poll, Prediction, TrainKind};
use std::collections::{HashSet, VecDeque};
use std::sync::mpsc::Receiver;
use std::sync::Arc;

const MAX_ROWS: usize = 600;
const MAX_HELD: usize = 2000;
const PAD_X: f32 = 16.0;
const LINE_H: f32 = 22.0;
const EMOTE_H: f32 = 28.0;
const BADGE: f32 = 18.0;
const TEXT_SIZE: f32 = 13.5;
const COMPOSER_H: f32 = 64.0;

// ---------- Layout of one line ----------

enum ItemKind {
    Galley(Arc<Galley>),
    Image { url: String, layers: Vec<(String, egui::Vec2)> },
    Chip(Arc<Galley>),
    Icon(&'static str),
}

/// What clicking an item does.
#[derive(Clone)]
enum Hit {
    Name(String),
    Link(String),
}

struct Item {
    rect: Rect,
    kind: ItemKind,
    hit: Option<Hit>,
    dim: bool,
}

struct Layout {
    width: f32,
    height: f32,
    items: Vec<Item>,
    /// Laid out before some emote's size was known; redo once it is.
    provisional: Vec<String>,
    /// The event card's background, relative to the row.
    card: Option<Rect>,
}

struct Row {
    seq: u64,
    ev: ChatEvent,
    gone: bool,
    layout: Option<Layout>,
}

impl Row {
    fn login(&self) -> Option<&str> {
        match &self.ev {
            ChatEvent::Msg { login, .. } => Some(login),
            ChatEvent::Event { login, parts, .. } if !login.is_empty() && !parts.is_empty() => Some(login),
            _ => None,
        }
    }
    fn id(&self) -> Option<&str> {
        match &self.ev {
            ChatEvent::Msg { id, .. } | ChatEvent::Event { id, .. } if !id.is_empty() => Some(id),
            _ => None,
        }
    }
    /// "name: text" with emotes as their codes, for copying.
    fn plain(&self) -> String {
        match &self.ev {
            ChatEvent::Msg { name, parts, .. } | ChatEvent::Event { name, parts, .. } => format!("{name}: {}", plain(parts)),
            ChatEvent::Sys { text } | ChatEvent::Divider { text } => text.clone(),
            _ => String::new(),
        }
    }
}

fn plain(parts: &[Part]) -> String {
    parts
        .iter()
        .map(|p| match p {
            Part::Text { v } | Part::Link { v, .. } | Part::Mention { v, .. } => v.clone(),
            Part::Emote { code, layers, .. } => {
                let mut s = code.clone();
                for l in layers {
                    s.push(' ');
                    s.push_str(&l.code);
                }
                s
            }
        })
        .collect()
}

fn clock(ms: u64) -> String {
    use chrono::TimeZone;
    let t = if ms == 0 { chrono::Local::now() } else { chrono::Local.timestamp_millis_opt(ms as i64).single().unwrap_or_else(chrono::Local::now) };
    t.format("%-H:%M").to_string()
}

/// Lays out words, emotes and badges in lines that wrap at `right`.
struct Flow<'a> {
    painter: &'a egui::Painter,
    images: &'a Images,
    left: f32,
    right: f32,
    x: f32,
    y: f32,
    line: Vec<Item>,
    items: Vec<Item>,
    provisional: Vec<String>,
    dim: bool,
}

impl<'a> Flow<'a> {
    fn new(painter: &'a egui::Painter, images: &'a Images, left: f32, right: f32, y: f32) -> Self {
        Self { painter, images, left, right, x: left, y, line: Vec::new(), items: Vec::new(), provisional: Vec::new(), dim: false }
    }

    fn break_line(&mut self) {
        let h = self.line.iter().map(|i| i.rect.height() - 6.0).fold(LINE_H, f32::max);
        for mut it in self.line.drain(..) {
            let dy = self.y + (h - it.rect.height()) / 2.0 - it.rect.top();
            it.rect = it.rect.translate(vec2(0.0, dy));
            self.items.push(it);
        }
        self.y += h;
        self.x = self.left;
    }

    fn place(&mut self, size: egui::Vec2, kind: ItemKind, hit: Option<Hit>, gap_after: f32) {
        if self.x > self.left && self.x + size.x > self.right {
            self.break_line();
        }
        let rect = Rect::from_min_size(pos2(self.x, 0.0), size);
        self.line.push(Item { rect, kind, hit, dim: self.dim });
        self.x += size.x + gap_after;
    }

    fn galley(&mut self, g: Arc<Galley>, hit: Option<Hit>) {
        let size = g.size();
        if size.x > self.right - self.left {
            // A word longer than the whole line: wrap it on its own.
            if self.x > self.left {
                self.break_line();
            }
        }
        self.place(size, ItemKind::Galley(g), hit, 0.0);
    }

    fn text(&mut self, text: &str, format: TextFormat, hit: Option<Hit>) {
        // One galley per word (with its trailing space) so lines can wrap between words.
        let width = self.right - self.left;
        for word in text.split_inclusive(' ') {
            let mut job = LayoutJob::single_section(word.to_string(), format.clone());
            job.wrap.max_width = width;
            job.wrap.break_anywhere = true;
            let g = self.painter.layout_job(job);
            self.galley(g, hit.clone());
        }
    }

    fn emote(&mut self, url: &str, w: u32, h: u32, layers: Vec<(String, egui::Vec2)>, gap: f32) {
        let size = self.emote_size(url, w, h);
        self.place(size, ItemKind::Image { url: url.to_string(), layers }, None, gap);
    }

    fn emote_size(&mut self, url: &str, w: u32, h: u32) -> egui::Vec2 {
        if w > 0 && h > 0 {
            return vec2(EMOTE_H * w as f32 / h as f32, EMOTE_H);
        }
        match self.images.get(url) {
            Some(a) if a.size.y > 0.0 => vec2(EMOTE_H * a.size.x / a.size.y, EMOTE_H),
            _ => {
                self.provisional.push(url.to_string());
                vec2(EMOTE_H, EMOTE_H)
            }
        }
    }

    fn finish(mut self) -> (Vec<Item>, f32, Vec<String>) {
        if !self.line.is_empty() {
            self.break_line();
        }
        (self.items, self.y, self.provisional)
    }
}

fn fmt(font: egui::FontId, color: Color32) -> TextFormat {
    TextFormat { font_id: font, color, ..Default::default() }
}

fn layout_parts(flow: &mut Flow, parts: &[Part], color: Color32, gone: bool) {
    let text_color = if gone { FAINT } else { color };
    let strike = |mut f: TextFormat| {
        if gone {
            f.strikethrough = Stroke::new(1.0, FAINT);
        }
        f
    };
    for p in parts {
        match p {
            Part::Text { v } => flow.text(v, strike(fmt(regular(TEXT_SIZE), text_color)), None),
            Part::Mention { v, login } => flow.text(v, strike(fmt(semibold(TEXT_SIZE), text_color)), Some(Hit::Name(login.clone()))),
            Part::Link { v, url } => {
                let mut f = fmt(regular(TEXT_SIZE), if gone { FAINT } else { SAGE });
                f.underline = Stroke::new(1.0, Color32::from_rgba_unmultiplied(0xd6, 0xef, 0xc2, 100));
                flow.text(v, strike(f), Some(Hit::Link(url.clone())));
            }
            Part::Emote { e, layers, .. } => {
                let layers = layers.iter().map(|l| (l.e.src.clone(), vec2(l.e.w as f32, l.e.h as f32))).collect();
                flow.dim = gone;
                flow.emote(&e.src, e.w, e.h, layers, 0.0);
                flow.dim = false;
                // Keep a normal space after an emote.
                flow.x += 4.0;
            }
        }
    }
}

fn layout_row(row: &Row, width: f32, painter: &egui::Painter, images: &Images) -> Layout {
    let right = width - PAD_X;
    match &row.ev {
        ChatEvent::Msg { login, name, color, badges, parts, action, first, reply, ts, source, paid, .. } => {
            let mut flow = Flow::new(painter, images, PAD_X, right, 3.0);
            if let Some(r) = reply {
                let mut job = LayoutJob::single_section(format!("Replying to @{}: {}", r.name, r.body), fmt(regular(12.0), FAINT));
                job.wrap.max_width = right - PAD_X - 22.0;
                job.wrap.max_rows = 1;
                job.wrap.break_anywhere = true;
                let g = painter.layout_job(job);
                flow.items.push(Item { rect: Rect::from_min_size(pos2(PAD_X + 2.0, flow.y + 3.0), vec2(12.0, 12.0)), kind: ItemKind::Icon("reply"), hit: None, dim: false });
                flow.items.push(Item { rect: Rect::from_min_size(pos2(PAD_X + 22.0, flow.y), g.size()), kind: ItemKind::Galley(g), hit: None, dim: false });
                flow.y += 18.0;
            }
            let ts = painter.layout_no_wrap(clock(*ts), mono(11.0), FAINT);
            flow.place(ts.size(), ItemKind::Galley(ts), None, 6.0);
            let chips = [first.then(|| "First message".to_string()), source.clone(), paid.as_ref().map(|p| format!("Hype Chat {p}"))];
            for c in chips.into_iter().flatten() {
                let g = painter.layout_no_wrap(c, medium(11.0), TEXT_2);
                flow.place(g.size() + vec2(12.0, 18.0 - g.size().y), ItemKind::Chip(g), None, 6.0);
            }
            for b in badges {
                flow.place(vec2(BADGE, BADGE), ItemKind::Image { url: b.src.clone(), layers: Vec::new() }, None, 4.0);
            }
            let name_color = theme::parse_hex(color);
            let g = painter.layout_no_wrap(name.clone(), bold(TEXT_SIZE), name_color);
            flow.place(g.size(), ItemKind::Galley(g), Some(Hit::Name(login.clone())), 0.0);
            if !*action {
                let g = painter.layout_no_wrap(": ".into(), regular(TEXT_SIZE), TEXT);
                flow.place(g.size(), ItemKind::Galley(g), None, 0.0);
            } else {
                flow.x += 5.0;
            }
            let body_color = if *action { name_color } else if row_is_old(&row.ev) { TEXT_2 } else { TEXT };
            layout_parts(&mut flow, parts, body_color, row.gone);
            let (items, h, provisional) = flow.finish();
            Layout { width, height: h + 3.0, items, provisional, card: None }
        }
        ChatEvent::Event { head, login, name, color, badges, parts, accent, .. } => {
            let (l, r) = (8.0 + 10.0, width - 8.0 - 10.0);
            let mut flow = Flow::new(painter, images, l, r, 4.0 + 8.0);
            let head_color = accent.as_deref().map_or(TEXT, theme::parse_hex);
            let g = painter.layout(head.clone(), semibold(13.0), head_color, r - l);
            flow.items.push(Item { rect: Rect::from_min_size(pos2(l, flow.y), g.size()), kind: ItemKind::Galley(g.clone()), hit: None, dim: false });
            flow.y += g.size().y.max(20.0) + 2.0;
            if !parts.is_empty() {
                for b in badges {
                    flow.place(vec2(BADGE, BADGE), ItemKind::Image { url: b.src.clone(), layers: Vec::new() }, None, 4.0);
                }
                let g = painter.layout_no_wrap(name.clone(), bold(TEXT_SIZE), theme::parse_hex(color));
                flow.place(g.size(), ItemKind::Galley(g), Some(Hit::Name(login.clone())), 0.0);
                let g = painter.layout_no_wrap(": ".into(), regular(TEXT_SIZE), TEXT);
                flow.place(g.size(), ItemKind::Galley(g), None, 0.0);
                layout_parts(&mut flow, parts, TEXT, row.gone);
            }
            let (items, h, provisional) = flow.finish();
            let card = Rect::from_min_max(pos2(8.0, 4.0), pos2(width - 8.0, h + 8.0));
            Layout { width, height: h + 8.0 + 4.0, items, provisional, card: Some(card) }
        }
        ChatEvent::Sys { text } => {
            let g = painter.layout(text.clone(), regular(12.5), MUTED, width - 2.0 * PAD_X);
            let h = g.size().y + 6.0;
            Layout { width, height: h, items: vec![Item { rect: Rect::from_min_size(pos2(PAD_X, 3.0), g.size()), kind: ItemKind::Galley(g), hit: None, dim: false }], provisional: vec![], card: None }
        }
        ChatEvent::Divider { text } => {
            let g = painter.layout_no_wrap(text.clone(), regular(12.0), FAINT);
            let x = (width - g.size().x) / 2.0;
            Layout { width, height: 34.0, items: vec![Item { rect: Rect::from_min_size(pos2(x, 9.0), g.size()), kind: ItemKind::Galley(g), hit: None, dim: false }], provisional: vec![], card: None }
        }
        _ => Layout { width, height: 0.0, items: vec![], provisional: vec![], card: None },
    }
}

fn row_is_old(ev: &ChatEvent) -> bool {
    matches!(ev, ChatEvent::Msg { old: true, .. })
}

// ---------- The app ----------

enum Screen {
    Starting,
    SignIn { client_id: String, prefilled: bool, code: Option<DeviceCode>, busy: bool },
    Chat,
}

struct Toast {
    text: String,
    bad: bool,
    until: f64,
}

struct Card {
    login: String,
    at: Pos2,
    ban_armed_until: f64,
}

struct Completion {
    start: usize,
    after: String,
    options: Vec<String>,
    i: usize,
}

pub struct App {
    rt: tokio::runtime::Handle,
    backend: Arc<Backend>,
    rx: Receiver<Event>,
    images: Images,
    icons: Icons,
    screen: Screen,
    me: Option<User>,
    rows: VecDeque<Row>,
    held: VecDeque<Row>,
    seq: u64,
    width: f32,
    pause_until: f64,
    at_bottom: bool,
    jump: bool,
    loading: bool,
    hover_row: Option<u64>,
    rail_rect: Option<Rect>,
    ban_armed: Option<(u64, f64)>,
    card: Option<Card>,
    composer: String,
    reply: Option<(String, String)>,
    sent: Vec<String>,
    sent_at: Option<usize>,
    completion: Option<Completion>,
    emote_codes: Vec<String>,
    recent_names: Vec<String>,
    toasts: Vec<Toast>,
    update: Option<String>,
    updating: bool,
    pinned: bool,
    menu: bool,
    saved_client_id: String,
    focus_composer: bool,
    pending_codes: Option<std::sync::mpsc::Receiver<Vec<String>>>,
    live: LiveState,
}

/// Hype train, poll, prediction, Shared Chat and the pinned message, shown above chat.
#[derive(Default)]
struct LiveState {
    /// Each with the time (ms since 1970) it stops showing.
    train: Option<(HypeTrain, u64)>,
    poll: Option<(Poll, u64)>,
    prediction: Option<(Prediction, u64)>,
    shared: Vec<Participant>,
    pin: Option<Pin>,
    dismissed: HashSet<String>,
    /// Folded by hand, and cards the person folded or opened by hand.
    collapsed: HashSet<String>,
    touched: HashSet<String>,
}

struct Pin {
    name: String,
    color: Color32,
    text: String,
}

fn wall_ms() -> u64 {
    chrono::Utc::now().timestamp_millis().max(0) as u64
}

/// "4:05" for time left; "0:00" once it has passed.
fn left(until_ms: u64, now_ms: u64) -> String {
    let s = until_ms.saturating_sub(now_ms).div_ceil(1000);
    format!("{}:{:02}", s / 60, s % 60)
}

fn num(n: u64) -> String {
    match n {
        0..=9_999 => n.to_string(),
        10_000..=999_999 => format!("{:.1}K", n as f64 / 1000.0).replace(".0K", "K"),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0).replace(".0M", "M"),
    }
}

fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{} {}", num(n), if n == 1 { one } else { many })
}

struct CardSpec {
    key: String,
    title: String,
    title_color: Color32,
    /// Time left, top right.
    right: String,
    body: Vec<Block>,
    dismiss: &'static str,
}

impl CardSpec {
    fn new(key: &str, title: String, title_color: Color32) -> Self {
        Self { key: key.to_string(), title, title_color, right: String::new(), body: Vec::new(), dismiss: "Hide" }
    }
}

enum Block {
    Line(String, Color32),
    Job(LayoutJob),
    Progress { frac: f32, fill: Color32 },
    Choice { title: String, value: String, frac: f32, fill: Color32, lead: bool },
}

fn join_names(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [a] => a.to_string(),
        [a, b] => format!("{a} and {b}"),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

const BLUE: Color32 = Color32::from_rgb(0x8f, 0xb8, 0xff);
const PINK: Color32 = Color32::from_rgb(0xf5, 0xa3, 0xc7);

fn train_tint(k: TrainKind) -> Color32 {
    match k {
        TrainKind::Regular => TEXT_2,
        TrainKind::GoldenKappa => Color32::from_rgb(0xf2, 0xc9, 0x4c),
        TrainKind::Treasure => Color32::from_rgb(0xf5, 0xb3, 0x6b),
        TrainKind::Mythic => Color32::from_rgb(0x8f, 0xdc, 0xf0),
    }
}

const PINNED_KEY: &str = "pinned";
const CLIENT_KEY: &str = "client-id";

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, rt: tokio::runtime::Handle) -> Self {
        theme::install(&cc.egui_ctx);
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = cc.egui_ctx.clone();
        let notify: crate::ui_events::Notify = Arc::new(move |e| {
            let _ = tx.send(e);
            ctx.request_repaint();
        });
        let backend = Backend::new(notify);
        let pinned = cc.storage.and_then(|s| s.get_string(PINNED_KEY)).as_deref() == Some("1");
        let saved_client_id = cc.storage.and_then(|s| s.get_string(CLIENT_KEY)).unwrap_or_default();
        if pinned {
            cc.egui_ctx.send_viewport_cmd(ViewportCommand::WindowLevel(egui::WindowLevel::AlwaysOnTop));
        }
        {
            let b = backend.clone();
            rt.spawn(async move {
                b.restore().await;
                b.check_for_update().await;
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(6 * 3600)).await;
                    b.check_for_update().await;
                }
            });
        }
        Self {
            images: Images::new(cc.egui_ctx.clone(), rt.clone()),
            icons: Icons::load(&cc.egui_ctx),
            rt,
            backend,
            rx,
            screen: Screen::Starting,
            me: None,
            rows: VecDeque::new(),
            held: VecDeque::new(),
            seq: 0,
            width: 0.0,
            pause_until: 0.0,
            at_bottom: true,
            jump: false,
            loading: true,
            hover_row: None,
            rail_rect: None,
            ban_armed: None,
            card: None,
            composer: String::new(),
            reply: None,
            sent: Vec::new(),
            sent_at: None,
            completion: None,
            emote_codes: Vec::new(),
            recent_names: Vec::new(),
            toasts: Vec::new(),
            update: None,
            updating: false,
            pinned,
            menu: false,
            saved_client_id,
            focus_composer: false,
            pending_codes: None,
            live: LiveState::default(),
        }
    }

    fn toast(&mut self, ctx: &egui::Context, text: impl Into<String>, bad: bool) {
        let now = ctx.input(|i| i.time);
        self.toasts.push(Toast { text: text.into(), bad, until: now + if bad { 5.0 } else { 2.6 } });
        if self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
    }

    fn spawn<F: std::future::Future<Output = ()> + Send + 'static>(&self, f: F) {
        self.rt.spawn(f);
    }

    /// Runs a chat action and shows what it says (or its error) as a toast.
    fn act<F>(&self, f: F)
    where
        F: std::future::Future<Output = Result<Option<String>, String>> + Send + 'static,
    {
        let b = self.backend.clone();
        self.spawn(async move {
            match f.await {
                Ok(Some(text)) => b.toast(text, false),
                Ok(None) => {}
                Err(e) => b.toast(e, true),
            }
        });
    }

    fn drain_events(&mut self, ctx: &egui::Context) {
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                Event::SignedIn(user) => {
                    self.me = Some(user);
                    self.screen = Screen::Chat;
                    self.rows.clear();
                    self.held.clear();
                    self.live = LiveState::default();
                    self.loading = true;
                    self.focus_composer = true;
                }
                Event::SignedOut { suggested_client_id } => {
                    self.me = None;
                    let suggested = suggested_client_id.unwrap_or_default();
                    let client_id = if self.saved_client_id.is_empty() { suggested.clone() } else { self.saved_client_id.clone() };
                    let prefilled = self.saved_client_id.is_empty() && !suggested.is_empty();
                    self.screen = Screen::SignIn { client_id, prefilled, code: None, busy: false };
                }
                Event::DeviceCode(code) => {
                    if let Screen::SignIn { code: c, .. } = &mut self.screen {
                        *c = Some(code);
                    }
                }
                Event::SignInFailed(e) => {
                    if let Screen::SignIn { code, busy, .. } = &mut self.screen {
                        *code = None;
                        *busy = false;
                    }
                    self.toast(ctx, e, true);
                }
                Event::ChatReady => {
                    self.loading = false;
                    let b = self.backend.clone();
                    let (tx, rx) = std::sync::mpsc::channel();
                    let c = ctx.clone();
                    self.spawn(async move {
                        let _ = tx.send(b.emote_codes().await);
                        c.request_repaint();
                    });
                    self.pending_codes = Some(rx);
                }
                Event::Chat(batch) => {
                    self.loading = false;
                    for ev in batch {
                        self.apply(ev);
                    }
                }
                Event::Toast { text, bad } => self.toast(ctx, text, bad),
                Event::UpdateReady(v) => self.update = Some(v),
                Event::Live(l) => self.live_event(l),
            }
        }
        if let Some(rx) = &self.pending_codes {
            if let Ok(codes) = rx.try_recv() {
                self.emote_codes = codes;
                self.pending_codes = None;
            }
        }
    }

    fn apply(&mut self, ev: ChatEvent) {
        let all = self.rows.iter_mut().chain(self.held.iter_mut());
        match &ev {
            ChatEvent::Deleted { id } => {
                for r in all {
                    if r.id() == Some(id.as_str()) {
                        r.gone = true;
                        r.layout = None;
                    }
                }
                return;
            }
            ChatEvent::UserCleared { login } => {
                for r in all {
                    if r.login() == Some(login.as_str()) {
                        r.gone = true;
                        r.layout = None;
                    }
                }
                return;
            }
            ChatEvent::Cleared => {
                for r in all {
                    if matches!(r.ev, ChatEvent::Msg { .. }) {
                        r.gone = true;
                        r.layout = None;
                    }
                }
                return;
            }
            ChatEvent::Msg { name, .. } => {
                if let Some(i) = self.recent_names.iter().position(|n| n == name) {
                    self.recent_names.remove(i);
                }
                self.recent_names.push(name.clone());
                if self.recent_names.len() > 300 {
                    self.recent_names.remove(0);
                }
            }
            _ => {}
        }
        self.seq += 1;
        self.held.push_back(Row { seq: self.seq, ev, gone: false, layout: None });
        if self.held.len() > MAX_HELD {
            self.held.pop_front();
        }
    }

    /// Moves waiting lines into the list when chat is following.
    fn release_held(&mut self, now: f64) {
        let paused = now < self.pause_until || self.card.is_some() || !self.at_bottom;
        if paused || self.held.is_empty() {
            return;
        }
        self.rows.extend(self.held.drain(..));
        while self.rows.len() > MAX_ROWS {
            self.rows.pop_front();
        }
    }

    // ---------- Title bar ----------

    fn win_button(&self, ui: &mut egui::Ui, rect: Rect, icon: &str, danger: bool, pressed: bool, tip: &str) -> Response {
        let r = ui.interact(rect, ui.scope_id().with(("wb", icon)), Sense::click()).on_hover_text(tip);
        let bg = if r.hovered() && danger {
            DANGER_SOLID
        } else if r.hovered() || pressed {
            BTN
        } else {
            Color32::TRANSPARENT
        };
        ui.painter().rect_filled(rect, R_CONTROL, bg);
        let tint = if r.hovered() || pressed { if danger { Color32::WHITE } else { TEXT } } else { MUTED };
        let s = 13.0;
        ui.painter().image(self.icons.get(icon).id(), Rect::from_center_size(rect.center(), vec2(s, s)), uv(), tint);
        if r.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        r
    }

    fn titlebar(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let ctx = ui.ctx().clone();
        let p = ui.painter().clone();
        // Drag anywhere that is not a button.
        let drag = ui.interact(rect, Id::unique("titlebar"), Sense::click_and_drag());
        if drag.double_clicked() {
            let max = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
            ctx.send_viewport_cmd(ViewportCommand::Maximized(!max));
        } else if drag.drag_started_by(egui::PointerButton::Primary) {
            ctx.send_viewport_cmd(ViewportCommand::StartDrag);
        }
        let mut x = rect.left() + 16.0;
        let cy = rect.center().y;
        p.image(self.icons.get("leaf").id(), Rect::from_min_size(pos2(x, cy - 11.0), vec2(16.0, 22.0)), uv(), Color32::WHITE);
        x += 25.0;
        let g = p.layout_no_wrap("Sylva Chat".into(), semibold(14.0), TEXT);
        p.galley(pos2(x, cy - g.size().y / 2.0), g.clone(), TEXT);
        x += g.size().x + 9.0;
        if let (Some(me), Screen::Chat) = (&self.me, &self.screen) {
            let g = p.layout_no_wrap(format!("#{}", me.login), mono(12.5), SAGE);
            let tag = Rect::from_min_size(pos2(x, cy - 12.0), vec2(g.size().x + 16.0, 24.0));
            p.rect_filled(tag, R_CONTROL, TAG_BG);
            p.galley(pos2(x + 8.0, cy - g.size().y / 2.0), g, SAGE);
        }

        // Right side, from the right edge inwards.
        let mut r = rect.right() - 8.0;
        let mut button = |w: f32| {
            r -= w;
            let b = Rect::from_min_size(pos2(r, cy - 15.0), vec2(w, 30.0));
            r -= 2.0;
            b
        };
        let close = button(36.0);
        let max = button(36.0);
        let min = button(36.0);
        r -= 6.0;
        if self.win_button(ui, close, "x", true, false, "Close").clicked() {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
        if self.win_button(ui, max, "square", false, false, "Maximize").clicked() {
            let m = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
            ctx.send_viewport_cmd(ViewportCommand::Maximized(!m));
        }
        if self.win_button(ui, min, "minus", false, false, "Minimize").clicked() {
            ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
        }
        let mut button = |w: f32| {
            r -= w;
            let b = Rect::from_min_size(pos2(r, cy - 15.0), vec2(w, 30.0));
            r -= 2.0;
            b
        };
        if matches!(self.screen, Screen::Chat) {
            let acct = button(36.0);
            let resp = self.win_button(ui, acct, "user", false, self.menu, "Account");
            if resp.clicked() {
                self.menu = !self.menu;
            }
        }
        let pin = button(36.0);
        if self.win_button(ui, pin, "pin", false, self.pinned, if self.pinned { "Stop keeping on top" } else { "Keep on top" }).clicked() {
            self.pinned = !self.pinned;
            let level = if self.pinned { egui::WindowLevel::AlwaysOnTop } else { egui::WindowLevel::Normal };
            ctx.send_viewport_cmd(ViewportCommand::WindowLevel(level));
        }
        if let Some(v) = self.update.clone() {
            let g = p.layout_no_wrap("Update".into(), medium(12.5), TEXT);
            let w = g.size().x + 14.0 + 7.0 + 20.0;
            let b = button(w);
            let resp = ui.interact(b, Id::unique("update-btn"), Sense::click()).on_hover_text(format!("Sylva Chat {v} is ready"));
            p.rect_filled(b, R_CONTROL, if resp.hovered() { BTN } else { Color32::TRANSPARENT });
            p.image(self.icons.get("update").id(), Rect::from_min_size(pos2(b.left() + 10.0, cy - 7.0), vec2(14.0, 14.0)), uv(), if resp.hovered() { TEXT } else { MUTED });
            p.galley(pos2(b.left() + 31.0, cy - g.size().y / 2.0), g, TEXT);
            if resp.clicked() && !self.updating {
                self.updating = true;
                let bk = self.backend.clone();
                self.spawn(async move { bk.install_update().await });
            }
        }
    }

    fn account_menu(&mut self, ctx: &egui::Context, anchor: Pos2) {
        if !self.menu {
            return;
        }
        let login = self.me.as_ref().map(|m| m.login.clone()).unwrap_or_default();
        let area = egui::Area::new(Id::unique("menu")).order(Order::Foreground).fixed_pos(anchor).show(ctx, |ui| {
            popup_frame(12, 6).show(ui, |ui| {
                ui.set_width(200.0);
                let mut job = LayoutJob::default();
                job.append("Signed in as ", 0.0, fmt(regular(12.5), MUTED));
                job.append(&login, 0.0, fmt(semibold(12.5), TEXT));
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.add_space(10.0);
                    ui.label(job);
                });
                ui.add_space(4.0);
                if menu_item(ui, &self.icons, "logout", "Sign out").clicked() {
                    self.menu = false;
                    let b = self.backend.clone();
                    self.spawn(async move { b.sign_out().await });
                }
            });
        });
        let clicked_outside = ctx.input(|i| i.pointer.any_pressed())
            && !area.response.rect.contains(ctx.input(|i| i.pointer.interact_pos().unwrap_or_default()))
            && !area.response.hovered();
        if clicked_outside && !ctx.input(|i| i.pointer.interact_pos()).is_some_and(|p| p.y < TITLEBAR) {
            self.menu = false;
        }
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.menu = false;
        }
    }

    // ---------- Sign in ----------

    fn sign_in(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let ctx = ui.ctx().clone();
        let Screen::SignIn { client_id, prefilled, code, busy } = &mut self.screen else { return };
        let w = (rect.width() - 32.0).min(400.0);
        let inner = Rect::from_min_size(pos2(rect.center().x - w / 2.0, rect.top() + 32.0), vec2(w, rect.height() - 32.0));
        let mut start: Option<String> = None;
        let mut cancel = false;
        let mut open_url: Option<String> = None;
        ui.scope_builder(UiBuilder::new().max_rect(inner), |ui| {
            ui.spacing_mut().item_spacing.y = 16.0;
            ui.label(egui::RichText::new("Sign in to Twitch").font(medium(24.0)).color(TEXT));
            ui.label(
                egui::RichText::new("Sylva Chat reads your chat, sends messages as you, and deletes, times out and bans for you.")
                    .font(regular(14.0))
                    .color(MUTED),
            );
            if *prefilled {
                ui.label(egui::RichText::new("Sylva Control's Client ID is filled in. Keep it, or paste another.").font(regular(12.5)).color(FAINT));
            } else if client_id.is_empty() {
                ui.spacing_mut().item_spacing.y = 8.0;
                let steps = [
                    "Open dev.twitch.tv/console/apps and register an application.",
                    "Set Client Type to Public. For the redirect URL, use http://localhost.",
                    "Paste the Client ID below.",
                ];
                for (i, s) in steps.iter().enumerate() {
                    ui.horizontal_wrapped(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
                        ui.painter().rect_filled(r, R_CONTROL, TAG_BG);
                        ui.painter().text(r.center(), Align2::CENTER_CENTER, (i + 1).to_string(), mono(11.5), SAGE);
                        let resp = ui.label(egui::RichText::new(*s).font(regular(13.5)).color(MUTED));
                        if i == 0 && resp.interact(Sense::click()).on_hover_cursor(CursorIcon::PointingHand).clicked() {
                            open_url = Some("https://dev.twitch.tv/console/apps".into());
                        }
                    });
                }
                ui.spacing_mut().item_spacing.y = 16.0;
            }
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                ui.label(egui::RichText::new("Client ID").font(regular(13.0)).color(MUTED));
                field(ui, client_id, "a1b2c3d4...", true, Id::unique("client-id"));
            });
            if primary_button(ui, &self.icons, "twitch", "Continue on Twitch", !*busy, w).clicked() {
                *busy = true;
                start = Some(client_id.clone());
            }
            if let Some(c) = code {
                egui::Frame::new().fill(SURFACE).corner_radius(R_ITEM).inner_margin(16).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.spacing_mut().item_spacing.y = 8.0;
                    ui.label(egui::RichText::new("Enter this code on the Twitch page that opened:").font(regular(13.5)).color(TEXT_2));
                    let mut f = fmt(mono(26.0), TEXT);
                    f.extra_letter_spacing = 3.6;
                    let job = LayoutJob::single_section(c.user_code.clone(), f);
                    ui.vertical_centered(|ui| ui.label(job));
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        ui.label(egui::RichText::new("Waiting for you to approve. ").font(regular(12.5)).color(FAINT));
                        if link(ui, "Open the page again").clicked() {
                            open_url = Some(c.verification_uri.clone());
                        }
                        ui.label(egui::RichText::new(" or ").font(regular(12.5)).color(FAINT));
                        if link(ui, "cancel").clicked() {
                            cancel = true;
                        }
                        ui.label(egui::RichText::new(".").font(regular(12.5)).color(FAINT));
                    });
                });
            }
        });
        if let Some(url) = open_url {
            let _ = open::that_detached(url);
        }
        if cancel {
            *code = None;
            *busy = false;
            let b = self.backend.clone();
            self.rt.spawn(async move { b.cancel_sign_in().await });
        }
        if let Some(id) = start {
            self.saved_client_id = id.trim().to_string();
            let b = self.backend.clone();
            self.rt.spawn(async move { b.sign_in(id).await });
        }
        let _ = ctx;
    }

    // ---------- Chat ----------

    fn chat(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let ctx = ui.ctx().clone();
        let now = ctx.input(|i| i.time);
        let strip = self.live_strip(ui, rect);
        let list_rect = Rect::from_min_max(pos2(rect.left(), rect.top() + strip), pos2(rect.right(), rect.bottom() - COMPOSER_H - if self.reply.is_some() { 38.0 } else { 0.0 }));
        let composer_rect = Rect::from_min_max(pos2(rect.left(), list_rect.bottom()), rect.max);

        // Pause while the pointer moves over chat.
        let pointer = ctx.input(|i| i.pointer.hover_pos());
        if let Some(p) = pointer {
            if list_rect.contains(p) && ctx.input(|i| i.pointer.delta() != egui::Vec2::ZERO) {
                self.pause_until = now + 1.2;
            }
            if self.rail_rect.is_some_and(|r| r.expand(2.0).contains(p)) {
                self.pause_until = now + 1.2;
            }
        }
        if pointer.is_none_or(|p| !list_rect.contains(p)) && self.card.is_none() {
            self.pause_until = self.pause_until.min(now);
        }
        self.release_held(now);
        if now < self.pause_until {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(self.pause_until - now + 0.01));
        }

        // Lay out rows that are new or changed, or all of them when the width changed.
        let width = list_rect.width();
        if (width - self.width).abs() > 0.5 {
            self.width = width;
            for r in &mut self.rows {
                r.layout = None;
            }
        }
        let painter = ui.painter_at(list_rect);
        for r in &mut self.rows {
            if let Some(l) = &r.layout {
                if l.width == width && (l.provisional.is_empty() || !l.provisional.iter().any(|u| self.images.get(u).is_some())) {
                    continue;
                }
            }
            r.layout = Some(layout_row(r, width, &painter, &self.images));
        }
        let total: f32 = self.rows.iter().map(|r| r.layout.as_ref().map_or(0.0, |l| l.height)).sum::<f32>() + 16.0;

        let mut clicked: Option<(Hit, Pos2)> = None;
        let mut hovered_row: Option<(u64, Rect)> = None;
        let mut next_frame: Option<u64> = None;
        let now_ms = (now * 1000.0) as u64;
        let jump = std::mem::take(&mut self.jump);
        let mut area = egui::ScrollArea::vertical().id_salt("chat").stick_to_bottom(true).auto_shrink([false, false]);
        if jump {
            area = area.vertical_scroll_offset(total);
        }
        let mut at_bottom = self.at_bottom;
        ui.scope_builder(UiBuilder::new().max_rect(list_rect), |ui| {
            let out = area.show_viewport(ui, |ui, viewport| {
                ui.set_min_height(total);
                at_bottom = viewport.max.y >= total - 4.0 || total <= viewport.height();
                let origin = ui.min_rect().min;
                let mut y = 8.0;
                for row in &self.rows {
                    let Some(l) = &row.layout else { continue };
                    let top = y;
                    y += l.height;
                    if y < viewport.min.y || top > viewport.max.y {
                        continue;
                    }
                    let rrect = Rect::from_min_size(origin + vec2(0.0, top), vec2(width, l.height));
                    let resp = ui.interact(rrect, Id::unique(("row", row.seq)), Sense::click());
                    let is_hover = resp.hovered() || self.hover_row == Some(row.seq) && self.rail_rect.is_some_and(|r| pointer.is_some_and(|p| r.contains(p)));
                    let hl = matches!(row.ev, ChatEvent::Msg { hl: true, .. });
                    if let Some(card) = l.card {
                        ui.painter().rect_filled(card.translate(rrect.min.to_vec2()), R_ITEM, SURFACE);
                    } else if hl {
                        ui.painter().rect_filled(rrect, 0.0, if is_hover { HL_HOVER } else { TAG_BG });
                    } else if is_hover && row.login().is_some() {
                        ui.painter().rect_filled(rrect, 0.0, ROW_HOVER);
                    }
                    if is_hover && row.login().is_some() {
                        hovered_row = Some((row.seq, rrect));
                    }
                    for it in &l.items {
                        let r = it.rect.translate(rrect.min.to_vec2());
                        match &it.kind {
                            ItemKind::Galley(g) => ui.painter().galley(r.min, g.clone(), TEXT),
                            ItemKind::Chip(g) => {
                                ui.painter().rect_filled(r, CornerRadius::same(6), SURFACE_2);
                                ui.painter().galley(r.center() - g.size() / 2.0, g.clone(), TEXT_2);
                            }
                            ItemKind::Icon(name) => {
                                ui.painter().image(self.icons.get(name).id(), r, uv(), FAINT);
                            }
                            ItemKind::Image { url, layers } => {
                                let tint = if it.dim { Color32::from_white_alpha(90) } else { Color32::WHITE };
                                for (u, _) in std::iter::once((url, &egui::Vec2::ZERO)).chain(layers.iter().map(|(u, s)| (u, s))) {
                                    if let Some(a) = self.images.get(u) {
                                        let (tex, next) = a.frame(now_ms);
                                        let fit = fit_rect(r, a.size);
                                        ui.painter().image(tex.id(), fit, uv(), tint);
                                        if let Some(n) = next {
                                            next_frame = Some(next_frame.map_or(n, |m| m.min(n)));
                                        }
                                    }
                                }
                            }
                        }
                        if it.hit.is_some() && pointer.is_some_and(|p| r.contains(p)) {
                            ctx.set_cursor_icon(CursorIcon::PointingHand);
                            if resp.clicked() {
                                clicked = Some((it.hit.clone().unwrap(), r.left_bottom()));
                            }
                        }
                    }
                    if resp.secondary_clicked() {
                        ctx.copy_text(row.plain());
                        self.backend.toast("Copied.", false);
                    }
                }
            });
            let _ = out;
        });
        self.at_bottom = at_bottom;
        if let Some(ms) = next_frame {
            ctx.request_repaint_after(std::time::Duration::from_millis(ms.max(16)));
        }

        // The action rail on the hovered message.
        let rail_hovered = self.rail_rect.is_some_and(|r| pointer.is_some_and(|p| r.contains(p)));
        if let Some((seq, rrect)) = hovered_row {
            self.hover_row = Some(seq);
            self.rail(ui, seq, rrect, list_rect, now);
        } else if !rail_hovered {
            self.hover_row = None;
            self.rail_rect = None;
            self.ban_armed = None;
        } else if let Some(seq) = self.hover_row {
            if let Some(rr) = self.rail_rect {
                let rrect = Rect::from_min_max(pos2(list_rect.left(), rr.top() + 14.0), pos2(list_rect.right(), rr.bottom() + 14.0));
                self.rail(ui, seq, rrect, list_rect, now);
            }
        }

        if let Some((hit, at)) = clicked {
            match hit {
                Hit::Link(url) => {
                    let _ = open::that_detached(url);
                }
                Hit::Name(login) => self.card = Some(Card { login, at, ban_armed_until: 0.0 }),
            }
        }

        // "New messages" pill.
        if !self.held.is_empty() || !self.at_bottom {
            let n = self.held.len();
            let text = if n > 0 { format!("{} new message{}", if n > 99 { "99+".into() } else { n.to_string() }, if n == 1 { "" } else { "s" }) } else { "Back to live chat".into() };
            let g = ui.painter().layout_no_wrap(text, medium(12.5), TEXT);
            let w = g.size().x + 24.0 + 20.0;
            let pill = Rect::from_center_size(pos2(list_rect.center().x, list_rect.bottom() - 28.0), vec2(w, 32.0));
            let resp = ui.interact(pill, Id::unique("jump"), Sense::click());
            let p = ui.painter().clone().with_layer_id(egui::LayerId::new(Order::Foreground, Id::unique("jump-layer")));
            p.add(shadow().as_shape(pill, CornerRadius::same(16)));
            p.rect_filled(pill, CornerRadius::same(16), if resp.hovered() { SURFACE_3 } else { POP });
            p.image(self.icons.get("down").id(), Rect::from_min_size(pos2(pill.left() + 12.0, pill.center().y - 6.5), vec2(13.0, 13.0)), uv(), TEXT);
            p.galley(pos2(pill.left() + 32.0, pill.center().y - g.size().y / 2.0), g, TEXT);
            if resp.clicked() {
                self.pause_until = 0.0;
                self.at_bottom = true;
                self.jump = true;
                self.card = None;
                self.release_held(now);
                ctx.request_repaint();
            }
        }

        if self.loading && self.rows.is_empty() && self.held.is_empty() {
            let a = 0.55 + 0.45 * ((now * std::f64::consts::PI / 2.2).sin() * 0.5 + 0.5) as f32;
            ui.painter().text(list_rect.center(), Align2::CENTER_CENTER, "Loading chat", regular(13.0), FAINT.gamma_multiply(a));
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }

        self.composer(ui, composer_rect);
        self.chatter_card(&ctx, now);
    }

    // ---------- Live: hype train, poll, prediction, Shared Chat, pinned ----------

    fn live_event(&mut self, l: Live) {
        let now = wall_ms();
        match l {
            Live::Train(t) => {
                let until = if t.ended { now + 30_000 } else { t.expires_ms.max(now) + 10_000 };
                self.live.train = Some((t, until));
            }
            Live::Poll(p) => {
                let until = if p.ended { now + 30_000 } else if p.ends_ms > 0 { p.ends_ms + 60_000 } else { u64::MAX };
                self.live.poll = Some((p, until));
            }
            Live::Prediction(p) => {
                let until = if p.ended { now + 60_000 } else { u64::MAX };
                self.live.prediction = Some((p, until));
            }
            Live::Shared(people) => {
                let me = self.me.as_ref().map(|m| m.user_id.clone()).unwrap_or_default();
                self.live.shared = people.into_iter().filter(|p| p.id != me).collect();
            }
        }
    }

    fn live_cards(&mut self, now: u64) -> (Vec<CardSpec>, bool) {
        let l = &mut self.live;
        if l.train.as_ref().is_some_and(|t| now > t.1) {
            l.train = None;
        }
        if l.poll.as_ref().is_some_and(|t| now > t.1) {
            l.poll = None;
        }
        if l.prediction.as_ref().is_some_and(|t| now > t.1) {
            l.prediction = None;
        }
        let mut cards = Vec::new();
        let mut timed = false;
        if !l.shared.is_empty() {
            let names: Vec<&str> = l.shared.iter().map(|p| p.name.as_str()).collect();
            cards.push(CardSpec::new("shared", format!("Shared Chat with {}", join_names(&names)), TEXT_2));
        }
        if let Some(pin) = &l.pin {
            let mut c = CardSpec::new("pin", "Pinned".into(), MUTED);
            let mut job = LayoutJob::default();
            job.append(&pin.name, 0.0, fmt(bold(13.0), pin.color));
            job.append(": ", 0.0, fmt(regular(13.0), TEXT));
            job.append(&pin.text, 0.0, fmt(regular(13.0), TEXT));
            job.wrap.max_rows = 4;
            c.body.push(Block::Job(job));
            c.dismiss = "Unpin";
            cards.push(c);
        }
        if let Some((t, _)) = &l.train {
            timed |= !t.ended;
            let tint = train_tint(t.kind);
            let name = t.kind.title(!t.shared_with.is_empty());
            let mut c = if t.ended {
                CardSpec::new(&format!("train:{}", t.id), format!("{name} ended at level {}", t.level), if t.kind == TrainKind::Regular { TEXT } else { tint })
            } else {
                let mut c = CardSpec::new(&format!("train:{}", t.id), format!("{name}, level {}", t.level), if t.kind == TrainKind::Regular { TEXT } else { tint });
                c.right = left(t.expires_ms, now);
                c
            };
            if !t.ended {
                let frac = if t.goal > 0 { t.progress as f32 / t.goal as f32 } else { 0.0 };
                c.body.push(Block::Progress { frac, fill: tint });
                c.body.push(Block::Line(format!("{} of {} to level {}", num(t.progress), num(t.goal), t.level + 1), MUTED));
                if t.record.is_some_and(|r| t.level > r) {
                    c.body.push(Block::Line("A new record for this channel".into(), tint));
                }
            } else if t.cooldown_ms > now {
                let mins = (t.cooldown_ms - now).div_ceil(60_000);
                c.body.push(Block::Line(format!("The next one can start in {}", plural(mins, "minute", "minutes")), MUTED));
            }
            if !t.shared_with.is_empty() {
                let names: Vec<&str> = t.shared_with.iter().map(String::as_str).collect();
                c.body.push(Block::Line(format!("Shared with {}", join_names(&names)), MUTED));
            }
            if !t.top.is_empty() {
                let top: Vec<String> = t.top.iter().take(3).map(|x| x.describe()).collect();
                c.body.push(Block::Line(format!("Top: {}", top.join(", ")), MUTED));
            }
            cards.push(c);
        }
        if let Some((p, _)) = &l.prediction {
            timed |= !p.locked;
            let mut c = CardSpec::new(&format!("pred:{}", p.id), p.title.clone(), TEXT);
            if !p.locked {
                c.right = left(p.locks_ms, now);
            }
            let total: u64 = p.outcomes.iter().map(|o| o.points).sum();
            let users: u64 = p.outcomes.iter().map(|o| o.users).sum();
            for o in &p.outcomes {
                let frac = if total > 0 { o.points as f32 / total as f32 } else { 0.0 };
                let won = p.winner.as_deref() == Some(o.id.as_str());
                let base = if o.blue { BLUE } else { PINK };
                let lost = p.winner.is_some() && !won;
                c.body.push(Block::Choice {
                    title: o.title.clone(),
                    value: format!("{}%", (frac * 100.0).round() as u32),
                    frac,
                    fill: base.gamma_multiply(if won { 0.55 } else if lost { 0.15 } else { 0.32 }),
                    lead: won,
                });
            }
            let footer = if p.canceled {
                ("Prediction canceled. Points were refunded.".to_string(), MUTED)
            } else if let Some(w) = p.outcomes.iter().find(|o| p.winner.as_deref() == Some(o.id.as_str())) {
                let biggest = w.top.as_ref().map(|(n, _)| format!(" Biggest winner: {n}.")).unwrap_or_default();
                (format!("{} won.{biggest}", w.title), if w.blue { BLUE } else { PINK })
            } else if p.locked {
                (format!("Locked with {} and {} points. Waiting for the result.", plural(users, "person", "people"), num(total)), MUTED)
            } else {
                (format!("Prediction: {} so far, {} points", plural(users, "person", "people"), num(total)), MUTED)
            };
            c.body.push(Block::Line(footer.0, footer.1));
            cards.push(c);
        }
        if let Some((p, _)) = &l.poll {
            timed |= !p.ended;
            let mut c = CardSpec::new(&format!("poll:{}", p.id), p.title.clone(), TEXT);
            if !p.ended {
                c.right = left(p.ends_ms, now);
            }
            let total: u64 = p.choices.iter().map(|x| x.votes).sum();
            let max = p.choices.iter().map(|x| x.votes).max().unwrap_or(0);
            for ch in &p.choices {
                let frac = if total > 0 { ch.votes as f32 / total as f32 } else { 0.0 };
                let lead = max > 0 && ch.votes == max;
                c.body.push(Block::Choice {
                    title: ch.title.clone(),
                    value: format!("{}%", (frac * 100.0).round() as u32),
                    frac,
                    fill: if lead { Color32::from_rgb(0x3a, 0x3a, 0x38) } else { Color32::from_rgb(0x2b, 0x2b, 0x2a) },
                    lead: lead && p.ended,
                });
            }
            let votes = plural(total, "vote", "votes");
            let footer = match (p.ended, p.terminated) {
                (false, _) => format!("Poll with {votes} so far"),
                (true, false) => format!("Poll ended with {votes}"),
                (true, true) => format!("Poll ended early with {votes}"),
            };
            c.body.push(Block::Line(footer, MUTED));
            cards.push(c);
        }
        (cards.into_iter().filter(|c| !self.live.dismissed.contains(&c.key)).collect(), timed)
    }

    /// Draws the live cards above chat and returns the height they take.
    fn live_strip(&mut self, ui: &mut egui::Ui, rect: Rect) -> f32 {
        let now = wall_ms();
        let (cards, timed) = self.live_cards(now);
        if cards.is_empty() {
            return 0.0;
        }
        if timed {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(1000 - now % 1000 + 5));
        }
        // Keep chat readable: fold cards nobody opened by hand, oldest kind first, until the
        // cards take under about 45% of the window.
        let estimate = |c: &CardSpec| {
            48.0 + c.body.iter().map(|b| match b {
                Block::Choice { .. } => 36.0,
                Block::Progress { .. } => 16.0,
                _ => 20.0,
            }).sum::<f32>()
        };
        let budget = rect.height() * 0.45;
        let mut total: f32 = cards.iter().map(|c| if self.folded(c) { 48.0 } else { estimate(c) }).sum();
        let mut auto = HashSet::new();
        for c in &cards {
            if total <= budget {
                break;
            }
            if !self.live.touched.contains(&c.key) && !c.body.is_empty() {
                total -= estimate(c) - 48.0;
                auto.insert(c.key.clone());
            }
        }
        let (x0, x1) = (rect.left() + 8.0, rect.right() - 8.0);
        let mut y = rect.top() + 8.0;
        for c in cards {
            let fold = auto.contains(&c.key) || self.folded(&c);
            y += self.live_card(ui, c, x0, x1, y, fold) + 8.0;
        }
        y - rect.top()
    }

    fn folded(&self, c: &CardSpec) -> bool {
        self.live.collapsed.contains(&c.key)
    }

    fn live_card(&mut self, ui: &mut egui::Ui, c: CardSpec, x0: f32, x1: f32, top: f32, fold: bool) -> f32 {
        const PAD: f32 = 12.0;
        let p = ui.painter().clone();
        let inner = x1 - x0 - 2.0 * PAD;
        let has_body = !c.body.is_empty();
        let collapsed = fold && has_body;
        let right = p.layout_no_wrap(c.right.clone(), mono(12.0), MUTED);
        let title_w = inner - right.size().x - 28.0 - 8.0;
        let mut job = LayoutJob::single_section(c.title.clone(), fmt(semibold(13.0), c.title_color));
        job.wrap.max_width = title_w;
        job.wrap.max_rows = if collapsed { 1 } else { 3 };
        let title = p.layout_job(job);
        let head_h = title.size().y.max(20.0);
        let mut shapes: Vec<egui::Shape> = Vec::new();
        let mut y = top + 10.0;
        shapes.push(egui::Shape::galley(pos2(x0 + PAD, y + (head_h - title.size().y) / 2.0), title, TEXT));
        let x_rect = Rect::from_min_size(pos2(x1 - 6.0 - 28.0, top + 6.0), vec2(28.0, 28.0));
        if !c.right.is_empty() {
            shapes.push(egui::Shape::galley(pos2(x_rect.left() - 4.0 - right.size().x, y + (head_h - right.size().y) / 2.0), right, TEXT));
        }
        y += head_h;
        let head_rect = Rect::from_min_max(pos2(x0, top), pos2(x_rect.left(), y + 4.0));
        if !collapsed {
            for b in c.body {
                match b {
                    Block::Line(text, color) => {
                        y += 4.0;
                        let g = p.layout(text, regular(12.5), color, inner);
                        let h = g.size().y;
                        shapes.push(egui::Shape::galley(pos2(x0 + PAD, y), g, TEXT));
                        y += h;
                    }
                    Block::Job(mut job) => {
                        y += 4.0;
                        job.wrap.max_width = inner;
                        let g = p.layout_job(job);
                        let h = g.size().y;
                        shapes.push(egui::Shape::galley(pos2(x0 + PAD, y), g, TEXT));
                        y += h;
                    }
                    Block::Progress { frac, fill } => {
                        y += 8.0;
                        let track = Rect::from_min_size(pos2(x0 + PAD, y), vec2(inner, 6.0));
                        shapes.push(egui::Shape::rect_filled(track, CornerRadius::same(3), SURFACE_2));
                        let w = (inner * frac.clamp(0.0, 1.0)).max(6.0);
                        shapes.push(egui::Shape::rect_filled(Rect::from_min_size(track.min, vec2(w, 6.0)), CornerRadius::same(3), fill));
                        y += 6.0 + 2.0;
                    }
                    Block::Choice { title, value, frac, fill, lead } => {
                        y += 6.0;
                        let row = Rect::from_min_size(pos2(x0 + PAD, y), vec2(inner, 30.0));
                        shapes.push(egui::Shape::rect_filled(row, CornerRadius::same(8), SURFACE_2));
                        if frac > 0.0 {
                            let w = (inner * frac.clamp(0.0, 1.0)).max(8.0);
                            shapes.push(egui::Shape::rect_filled(Rect::from_min_size(row.min, vec2(w, 30.0)), CornerRadius::same(8), fill));
                        }
                        let v = p.layout_no_wrap(value, mono(12.0), TEXT_2);
                        let mut job = LayoutJob::single_section(title, fmt(if lead { semibold(12.5) } else { regular(12.5) }, TEXT));
                        job.wrap.max_width = inner - 20.0 - v.size().x - 8.0;
                        job.wrap.max_rows = 1;
                        job.wrap.break_anywhere = true;
                        let g = p.layout_job(job);
                        shapes.push(egui::Shape::galley(pos2(row.left() + 10.0, row.center().y - g.size().y / 2.0), g, TEXT));
                        shapes.push(egui::Shape::galley(pos2(row.right() - 10.0 - v.size().x, row.center().y - v.size().y / 2.0), v, TEXT));
                        y += 30.0;
                    }
                }
            }
        }
        y += 10.0;
        let card = Rect::from_min_max(pos2(x0, top), pos2(x1, y));
        p.rect_filled(card, R_ITEM, SURFACE);
        p.extend(shapes);

        // Click the title to fold a card down to one line; the x hides it.
        if has_body {
            let r = ui.interact(head_rect, Id::unique(("live-head", &c.key)), Sense::click()).on_hover_text(if collapsed { "Show more" } else { "Show less" });
            if r.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            if r.clicked() {
                self.live.touched.insert(c.key.clone());
                if collapsed {
                    self.live.collapsed.remove(&c.key);
                } else {
                    self.live.collapsed.insert(c.key.clone());
                }
            }
        }
        let x = ui.interact(x_rect, Id::unique(("live-x", &c.key)), Sense::click()).on_hover_text(c.dismiss);
        if x.hovered() {
            p.rect_filled(x_rect, R_CONTROL, BTN);
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        p.image(self.icons.get("x").id(), Rect::from_center_size(x_rect.center(), vec2(12.0, 12.0)), uv(), if x.hovered() { TEXT } else { FAINT });
        if x.clicked() {
            if c.key == "pin" {
                self.live.pin = None;
            } else {
                self.live.dismissed.insert(c.key);
            }
        }
        card.height()
    }

    fn rail(&mut self, ui: &mut egui::Ui, seq: u64, rrect: Rect, list_rect: Rect, now: f64) {
        let Some(row) = self.rows.iter().find(|r| r.seq == seq) else { return };
        let Some(me) = &self.me else { return };
        let login = row.login().unwrap_or_default().to_string();
        let id = row.id().map(str::to_string);
        let own = login == me.login;
        let mut acts: Vec<(&'static str, &'static str)> = Vec::new();
        if id.is_some() {
            acts.push(("reply", "Reply"));
        }
        acts.push(("copy", "Copy message"));
        if matches!(row.ev, ChatEvent::Msg { .. }) {
            acts.push(("pin", "Pin above chat"));
        }
        if id.is_some() && (own || login != me.login) {
            acts.push(("trash", "Delete message"));
        }
        if !own {
            acts.push(("clock", "Time out for 10 minutes"));
            acts.push(("ban", "Ban"));
        }
        let armed = self.ban_armed.is_some_and(|(s, until)| s == seq && until > now);
        let ban_w = if armed { 64.0 } else { 28.0 };
        let w = acts.iter().map(|a| if a.0 == "ban" { ban_w } else { 28.0 }).sum::<f32>() + 2.0 * (acts.len() as f32 - 1.0) + 4.0;
        let top = (rrect.top() - 14.0).max(list_rect.top() + 2.0);
        let rail = Rect::from_min_size(pos2(rrect.right() - 12.0 - w, top), vec2(w, 32.0));
        self.rail_rect = Some(rail);
        let layer = egui::LayerId::new(Order::Foreground, Id::unique("rail"));
        let p = ui.ctx().layer_painter(layer);
        p.add(shadow().as_shape(rail, R_CONTROL));
            p.rect_filled(rail, R_CONTROL, POP);
        let mut x = rail.left() + 2.0;
        let plain = row.plain();
        for (icon, tip) in acts {
            let bw = if icon == "ban" { ban_w } else { 28.0 };
            let b = Rect::from_min_size(pos2(x, rail.top() + 2.0), vec2(bw, 28.0));
            x += bw + 2.0;
            let resp = ui.interact(b, Id::unique(("rail", icon)), Sense::click()).on_hover_text(tip);
            let danger = matches!(icon, "trash" | "ban");
            let confirm = icon == "ban" && armed;
            let bg = if confirm { DANGER_SOLID } else if resp.hovered() { BTN } else { Color32::TRANSPARENT };
            p.rect_filled(b, CornerRadius::same(6), bg);
            let tint = if confirm { Color32::WHITE } else if resp.hovered() { if danger { DANGER } else { TEXT } } else { MUTED };
            let icon_rect = if confirm { Rect::from_min_size(pos2(b.left() + 7.0, b.center().y - 7.0), vec2(14.0, 14.0)) } else { Rect::from_center_size(b.center(), vec2(14.0, 14.0)) };
            p.image(self.icons.get(icon).id(), icon_rect, uv(), tint);
            if confirm {
                p.text(pos2(b.left() + 26.0, b.center().y), Align2::LEFT_CENTER, "Ban?", semibold(12.0), Color32::WHITE);
            }
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            if !resp.clicked() {
                continue;
            }
            match icon {
                "reply" => {
                    let name = match &row.ev {
                        ChatEvent::Msg { name, .. } | ChatEvent::Event { name, .. } => name.clone(),
                        _ => login.clone(),
                    };
                    self.reply = Some((id.clone().unwrap_or_default(), name));
                    self.focus_composer = true;
                }
                "pin" => {
                    if let ChatEvent::Msg { name, color, parts, .. } = &row.ev {
                        self.live.pin = Some(Pin { name: name.clone(), color: theme::parse_hex(color), text: self::plain(parts) });
                    }
                }
                "copy" => {
                    ui.ctx().copy_text(plain.clone());
                    self.backend.toast("Copied.", false);
                }
                "trash" => {
                    let (b, id) = (self.backend.clone(), id.clone());
                    let login = login.clone();
                    self.act(async move { b.moderate("delete", login, id, None).await });
                }
                "clock" => {
                    let (b, login) = (self.backend.clone(), login.clone());
                    self.act(async move { b.moderate("timeout", login, None, Some(600)).await });
                }
                "ban" if !armed => self.ban_armed = Some((seq, now + 3.0)),
                "ban" => {
                    self.ban_armed = None;
                    let (b, login) = (self.backend.clone(), login.clone());
                    self.act(async move { b.moderate("ban", login, None, None).await });
                }
                _ => {}
            }
        }
        if armed {
            ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64((self.ban_armed.map_or(now, |a| a.1) - now).max(0.0) + 0.01));
        }
    }

    fn composer(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let ctx = ui.ctx().clone();
        let mut inner = rect.shrink2(vec2(12.0, 0.0));
        inner.min.y += 8.0;
        inner.max.y -= 12.0;
        if let Some((_, name)) = self.reply.clone() {
            let bar = Rect::from_min_size(inner.min, vec2(inner.width(), 32.0));
            ui.painter().rect_filled(bar, R_ITEM, SURFACE);
            ui.painter().image(self.icons.get("reply").id(), Rect::from_min_size(pos2(bar.left() + 10.0, bar.center().y - 6.5), vec2(13.0, 13.0)), uv(), MUTED);
            let mut job = LayoutJob::default();
            job.append("Replying to ", 0.0, fmt(regular(12.5), MUTED));
            job.append(&format!("@{name}"), 0.0, fmt(semibold(12.5), TEXT));
            let g = ui.painter().layout_job(job);
            ui.painter().galley(pos2(bar.left() + 31.0, bar.center().y - g.size().y / 2.0), g, TEXT);
            let x = Rect::from_min_size(pos2(bar.right() - 32.0, bar.top()), vec2(32.0, 32.0));
            if self.win_button(ui, x, "x", false, false, "Cancel reply").clicked() {
                self.reply = None;
            }
            inner.min.y += 38.0;
        }
        let send_rect = Rect::from_min_size(pos2(inner.right() - 44.0, inner.top()), vec2(44.0, 44.0));
        let field_rect = Rect::from_min_max(inner.min, pos2(send_rect.left() - 8.0, inner.top() + 44.0));
        let id = Id::unique("composer");
        let focused = ctx.memory(|m| m.has_focus(id));

        // Keys the text field would otherwise take: Tab completes, Up and Down walk history.
        if focused {
            if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Tab)) {
                self.complete(&ctx, id, false);
            } else if ctx.input_mut(|i| i.consume_key(Modifiers::SHIFT, Key::Tab)) {
                self.complete(&ctx, id, true);
            } else if !self.sent.is_empty() && (self.composer.is_empty() || self.sent_at.is_some()) && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowUp)) {
                let at = self.sent_at.map_or(self.sent.len() - 1, |a| a.saturating_sub(1));
                self.sent_at = Some(at);
                self.composer = self.sent[at].clone();
                set_cursor_end(&ctx, id, &self.composer);
            } else if self.sent_at.is_some() && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowDown)) {
                let at = self.sent_at.unwrap() + 1;
                if at >= self.sent.len() {
                    self.sent_at = None;
                    self.composer.clear();
                } else {
                    self.sent_at = Some(at);
                    self.composer = self.sent[at].clone();
                }
                set_cursor_end(&ctx, id, &self.composer);
            } else if ctx.input(|i| i.key_pressed(Key::Escape)) {
                if self.reply.is_some() {
                    self.reply = None;
                } else {
                    ctx.memory_mut(|m| m.surrender_focus(id));
                }
            }
        }

        ui.painter().rect_filled(field_rect, R_ITEM, SURFACE);
        if focused {
            ui.painter().rect_stroke(field_rect.shrink(1.0), R_ITEM, Stroke::new(2.0, SAGE), egui::StrokeKind::Inside);
        }
        let mut submit = false;
        ui.scope_builder(UiBuilder::new().max_rect(field_rect.shrink2(vec2(14.0, 0.0))), |ui| {
            let before = self.composer.clone();
            let resp = ui.add_sized(
                vec2(field_rect.width() - 28.0, 44.0),
                egui::TextEdit::singleline(&mut self.composer)
                    .id(id)
                    .frame(egui::Frame::NONE)
                    .hint_text(egui::RichText::new("Send a message").color(FAINT))
                    .font(regular(14.0))
                    .text_color(TEXT)
                    .char_limit(500)
                    .align(egui::Align2::LEFT_CENTER),
            );
            if self.composer != before {
                self.completion = None;
            }
            if resp.lost_focus() && ctx.input(|i| i.key_pressed(Key::Enter)) {
                submit = true;
            }
        });
        if self.focus_composer {
            self.focus_composer = false;
            ctx.memory_mut(|m| m.request_focus(id));
        }
        let resp = ui.interact(send_rect, Id::unique("send"), Sense::click()).on_hover_text("Send");
        ui.painter().rect_filled(send_rect, R_ITEM, if resp.hovered() { SAGE_HOVER } else { SAGE });
        ui.painter().image(self.icons.get("send").id(), Rect::from_center_size(send_rect.center(), vec2(17.0, 17.0)), uv(), ON_SAGE);
        if resp.hovered() {
            ctx.set_cursor_icon(CursorIcon::PointingHand);
        }
        if resp.clicked() {
            submit = true;
        }
        if submit {
            let text = self.composer.trim().to_string();
            if !text.is_empty() {
                self.sent.push(text.clone());
                if self.sent.len() > 50 {
                    self.sent.remove(0);
                }
                self.sent_at = None;
                self.composer.clear();
                let reply = self.reply.take().map(|r| r.0);
                let b = self.backend.clone();
                self.act(async move { b.send_chat(text, reply).await });
            }
            ctx.memory_mut(|m| m.request_focus(id));
        }
    }

    fn complete(&mut self, ctx: &egui::Context, id: Id, back: bool) {
        if self.completion.is_none() {
            let caret = egui::TextEdit::load_state(ctx, id)
                .and_then(|s| s.cursor.char_range())
                .map_or(self.composer.chars().count(), |r| r.primary.index.into());
            let byte = self.composer.char_indices().nth(caret).map_or(self.composer.len(), |(b, _)| b);
            let before = &self.composer[..byte];
            let start = before.rfind(' ').map_or(0, |i| i + 1);
            let word = &before[start..];
            if word.is_empty() {
                return;
            }
            let at = word.starts_with('@');
            let q = word.trim_start_matches('@').to_lowercase();
            let mut options: Vec<String> = Vec::new();
            if !at {
                options.extend(self.emote_codes.iter().filter(|c| c.to_lowercase().starts_with(&q)).take(40).cloned());
            }
            options.extend(
                self.recent_names.iter().rev().filter(|n| n.to_lowercase().starts_with(&q)).take(20).map(|n| if at { format!("@{n}") } else { n.clone() }),
            );
            if options.is_empty() {
                return;
            }
            self.completion = Some(Completion { start, after: self.composer[byte..].to_string(), options, i: usize::MAX });
        }
        let c = self.completion.as_mut().unwrap();
        let n = c.options.len();
        c.i = if c.i == usize::MAX { if back { n - 1 } else { 0 } } else if back { (c.i + n - 1) % n } else { (c.i + 1) % n };
        let pick = format!("{} ", c.options[c.i]);
        self.composer = format!("{}{}{}", &self.composer[..c.start], pick, c.after);
        let pos = self.composer[..c.start].chars().count() + pick.chars().count();
        let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
        state.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(pos))));
        state.store(ctx, id);
        // Keep the completion going: the text changed, but by us.
        let keep = self.completion.take();
        self.completion = keep;
    }

    fn chatter_card(&mut self, ctx: &egui::Context, now: f64) {
        let Some(card) = &self.card else { return };
        let login = card.login.clone();
        let me = self.me.clone();
        let screen = ctx.content_rect();
        let w = 300.0_f32.min(screen.width() - 24.0);
        let x = card.at.x.clamp(12.0, screen.right() - w - 12.0);
        let y = (card.at.y + 6.0).max(TITLEBAR + 8.0);
        let (name, color) = self
            .rows
            .iter()
            .rev()
            .find_map(|r| match &r.ev {
                ChatEvent::Msg { login: l, name, color, .. } if *l == login => Some((name.clone(), theme::parse_hex(color))),
                _ => None,
            })
            .unwrap_or((login.clone(), TEXT));
        let recent: Vec<(String, bool)> = self
            .rows
            .iter()
            .filter(|r| r.login() == Some(login.as_str()))
            .filter_map(|r| match &r.ev {
                ChatEvent::Msg { parts, ts, .. } => Some((format!("{}  {}", clock(*ts), plain(parts)), r.gone)),
                _ => None,
            })
            .rev()
            .take(12)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let is_self = me.as_ref().is_some_and(|m| m.login == login);
        let armed = card.ban_armed_until > now;
        let mut close = false;
        let mut action: Option<(&'static str, Option<u64>)> = None;
        let mut arm = false;
        let area = egui::Area::new(Id::unique("card")).order(Order::Foreground).fixed_pos(pos2(x, y)).show(ctx, |ui| {
            popup_frame(R_PANEL, 14).show(ui, |ui| {
                ui.set_width(w - 28.0);
                ui.spacing_mut().item_spacing = vec2(6.0, 10.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(&name).font(bold(17.0)).color(color));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::hover());
                        if self.win_button(ui, r, "x", false, false, "Close").clicked() {
                            close = true;
                        }
                    });
                });
                ui.label(egui::RichText::new(&login).font(mono(12.0)).color(FAINT));
                if !is_self {
                    ui.horizontal_wrapped(|ui| {
                        for (label, secs) in [("1m", 60), ("10m", 600), ("1h", 3600), ("1d", 86400)] {
                            if small_button(ui, label, false).clicked() {
                                action = Some(("timeout", Some(secs)));
                            }
                        }
                        if small_button(ui, if armed { "Ban, sure?" } else { "Ban" }, true).clicked() {
                            if armed {
                                action = Some(("ban", None));
                            } else {
                                arm = true;
                            }
                        }
                        if small_button(ui, "Unban", false).clicked() {
                            action = Some(("unban", None));
                        }
                    });
                }
                ui.label(egui::RichText::new("Recent messages").font(regular(12.0)).color(MUTED));
                egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    if recent.is_empty() {
                        ui.label(egui::RichText::new("Nothing in chat yet.").font(regular(13.0)).color(FAINT));
                    }
                    for (text, gone) in &recent {
                        let mut rt = egui::RichText::new(text).font(regular(13.0)).color(if *gone { FAINT } else { TEXT_2 });
                        if *gone {
                            rt = rt.strikethrough();
                        }
                        ui.add(egui::Label::new(rt).wrap());
                    }
                });
                let open_card = ui.add(egui::Button::image_and_text(
                    egui::Image::new((self.icons.get("external").id(), vec2(14.0, 14.0))).tint(MUTED),
                    egui::RichText::new("Viewer card on Twitch").font(medium(12.5)).color(MUTED),
                ).fill(Color32::TRANSPARENT));
                if open_card.clicked() {
                    if let Some(me) = &me {
                        let _ = open::that_detached(format!("https://www.twitch.tv/popout/{}/viewercard/{}", me.login, login));
                    }
                }
            });
        });
        if arm {
            if let Some(c) = &mut self.card {
                c.ban_armed_until = now + 3.0;
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(3010));
        }
        if let Some((what, secs)) = action {
            let b = self.backend.clone();
            let l = login.clone();
            self.act(async move { b.moderate(what, l, None, secs).await });
            if what == "ban" {
                if let Some(c) = &mut self.card {
                    c.ban_armed_until = 0.0;
                }
            }
        }
        let pressed_outside = ctx.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !area.response.rect.contains(p)));
        if close || pressed_outside || ctx.input(|i| i.key_pressed(Key::Escape)) {
            // A press on a name opens the card again for that name.
            self.card = None;
        }
    }

    fn toasts(&mut self, ctx: &egui::Context, bottom: f32) {
        let now = ctx.input(|i| i.time);
        self.toasts.retain(|t| t.until > now);
        if self.toasts.is_empty() {
            return;
        }
        let screen = ctx.content_rect();
        let mut y = bottom;
        let mut soonest = f64::MAX;
        for (i, t) in self.toasts.iter().enumerate().rev() {
            let alpha = ((t.until - now) / 0.15).clamp(0.0, 1.0) as f32;
            soonest = soonest.min(t.until - now - 0.15);
            let p = ctx.layer_painter(egui::LayerId::new(Order::Tooltip, Id::unique(("toast", i))));
            let g = p.layout(t.text.clone(), regular(13.5), if t.bad { DANGER } else { TEXT }, (screen.width() - 60.0).min(420.0));
            let size = g.size() + vec2(28.0, 20.0);
            let rect = Rect::from_min_size(pos2(screen.center().x - size.x / 2.0, y - size.y), size);
            y -= size.y + 8.0;
            p.add(shadow().as_shape(rect, R_ITEM));
            p.rect_filled(rect, R_ITEM, POP.gamma_multiply(alpha));
            p.galley_with_override_text_color(rect.min + vec2(14.0, 10.0), g, if t.bad { DANGER } else { TEXT }.gamma_multiply(alpha));
        }
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(soonest.max(0.016)));
    }

    /// Resizing from the window edges (the window has no system frame).
    fn edges(&self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().maximized.unwrap_or(false)) {
            return;
        }
        let Some(p) = ctx.input(|i| i.pointer.hover_pos()) else { return };
        let r = ctx.content_rect();
        let m = 5.0;
        let (l, rt, t, b) = (p.x < r.left() + m, p.x > r.right() - m, p.y < r.top() + m, p.y > r.bottom() - m);
        use egui::ResizeDirection as D;
        let dir = match (l, rt, t, b) {
            (true, _, true, _) => Some((D::NorthWest, CursorIcon::ResizeNorthWest)),
            (_, true, true, _) => Some((D::NorthEast, CursorIcon::ResizeNorthEast)),
            (true, _, _, true) => Some((D::SouthWest, CursorIcon::ResizeSouthWest)),
            (_, true, _, true) => Some((D::SouthEast, CursorIcon::ResizeSouthEast)),
            (true, ..) => Some((D::West, CursorIcon::ResizeWest)),
            (_, true, ..) => Some((D::East, CursorIcon::ResizeEast)),
            (_, _, true, _) => Some((D::North, CursorIcon::ResizeNorth)),
            (_, _, _, true) => Some((D::South, CursorIcon::ResizeSouth)),
            _ => None,
        };
        if let Some((d, cursor)) = dir {
            ctx.set_cursor_icon(cursor);
            if ctx.input(|i| i.pointer.primary_pressed()) {
                ctx.send_viewport_cmd(ViewportCommand::BeginResize(d));
            }
        }
    }
}

// ---------- Small pieces ----------

fn uv() -> Rect {
    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))
}

fn shadow() -> egui::Shadow {
    egui::Shadow { offset: [0, 8], blur: 24, spread: 0, color: Color32::from_black_alpha(120) }
}

fn popup_frame(radius: u8, pad: i8) -> egui::Frame {
    egui::Frame::new().fill(POP).corner_radius(CornerRadius::same(radius)).inner_margin(pad).shadow(shadow())
}

fn fit_rect(slot: Rect, size: egui::Vec2) -> Rect {
    if size.x <= 0.0 || size.y <= 0.0 {
        return slot;
    }
    let s = (slot.width() / size.x).min(slot.height() / size.y);
    Rect::from_center_size(slot.center(), size * s)
}

fn set_cursor_end(ctx: &egui::Context, id: Id, text: &str) {
    let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
    state.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(text.chars().count()))));
    state.store(ctx, id);
}

fn link(ui: &mut egui::Ui, text: &str) -> Response {
    let r = ui.add(egui::Label::new(egui::RichText::new(text).font(regular(12.5)).color(SAGE)).sense(Sense::click()));
    r.on_hover_cursor(CursorIcon::PointingHand)
}

fn field(ui: &mut egui::Ui, text: &mut String, hint: &str, mono_font: bool, id: Id) -> Response {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::hover());
    let focused = ui.ctx().memory(|m| m.has_focus(id));
    ui.painter().rect_filled(rect, R_ITEM, Color32::from_rgb(0x1a, 0x1a, 0x19));
    if focused {
        ui.painter().rect_stroke(rect.shrink(1.0), R_ITEM, Stroke::new(2.0, SAGE), egui::StrokeKind::Inside);
    }
    ui.scope_builder(UiBuilder::new().max_rect(rect.shrink2(vec2(12.0, 0.0))), |ui| {
        ui.add_sized(
            vec2(rect.width() - 24.0, 36.0),
            egui::TextEdit::singleline(text)
                .id(id)
                .frame(egui::Frame::NONE)
                .hint_text(egui::RichText::new(hint).color(FAINT))
                .font(if mono_font { mono(13.0) } else { regular(13.5) })
                .text_color(TEXT)
                .align(egui::Align2::LEFT_CENTER),
        )
    })
    .inner
}

fn primary_button(ui: &mut egui::Ui, icons: &Icons, icon: &str, text: &str, enabled: bool, width: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 44.0), if enabled { Sense::click() } else { Sense::hover() });
    let fill = if !enabled { SAGE.gamma_multiply(0.45) } else if resp.hovered() { SAGE_HOVER } else { SAGE };
    ui.painter().rect_filled(rect, R_ITEM, fill);
    let g = ui.painter().layout_no_wrap(text.into(), medium(14.0), ON_SAGE);
    let total = 15.0 + 7.0 + g.size().x;
    let x = rect.center().x - total / 2.0;
    ui.painter().image(icons.get(icon).id(), Rect::from_min_size(pos2(x, rect.center().y - 7.5), vec2(15.0, 15.0)), uv(), ON_SAGE);
    ui.painter().galley(pos2(x + 22.0, rect.center().y - g.size().y / 2.0), g, ON_SAGE);
    if enabled && resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    resp
}

fn small_button(ui: &mut egui::Ui, text: &str, danger: bool) -> Response {
    let g = ui.painter().layout_no_wrap(text.into(), medium(12.5), if danger { DANGER } else { TEXT });
    let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 20.0, 30.0), Sense::click());
    ui.painter().rect_filled(rect, R_CONTROL, if resp.hovered() { BTN_IN_HOVER } else { BTN_IN });
    ui.painter().galley(rect.center() - g.size() / 2.0, g, TEXT);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    resp
}

fn menu_item(ui: &mut egui::Ui, icons: &Icons, icon: &str, text: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, R_CONTROL, BTN);
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    ui.painter().image(icons.get(icon).id(), Rect::from_min_size(pos2(rect.left() + 10.0, rect.center().y - 7.5), vec2(15.0, 15.0)), uv(), MUTED);
    ui.painter().text(pos2(rect.left() + 35.0, rect.center().y), Align2::LEFT_CENTER, text, regular(13.5), TEXT);
    resp
}

#[allow(dead_code)]
fn tex(id: TextureId) -> TextureId {
    id
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.drain_events(&ctx);
        let full = ctx.content_rect();
        ui.painter().rect_filled(full, 0.0, FRAME);
        let bar = Rect::from_min_size(full.min, vec2(full.width(), TITLEBAR));
        let panel = Rect::from_min_max(pos2(full.left(), bar.bottom()), full.max);
        ui.painter().rect_filled(panel, CornerRadius { nw: R_PANEL, ne: R_PANEL, sw: 0, se: 0 }, BG);
        self.titlebar(ui, bar);
        match self.screen {
            Screen::Starting => {}
            Screen::SignIn { .. } => self.sign_in(ui, panel),
            Screen::Chat => self.chat(ui, panel),
        }
        self.account_menu(&ctx, pos2(full.right() - 208.0, TITLEBAR - 4.0));
        let toast_bottom = if matches!(self.screen, Screen::Chat) { full.bottom() - COMPOSER_H - 12.0 } else { full.bottom() - 20.0 };
        self.toasts(&ctx, toast_bottom);
        self.edges(&ctx);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string(PINNED_KEY, if self.pinned { "1".into() } else { String::new() });
        storage.set_string(CLIENT_KEY, self.saved_client_id.clone());
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        FRAME.to_normalized_gamma_f32()
    }
}
