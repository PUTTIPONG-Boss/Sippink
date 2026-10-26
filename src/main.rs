#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use egui::RichText;
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

mod files;
mod job;
mod media;
mod pinterest;
mod theme;
mod tools;
mod x;
mod youtube;

use files::human;
use job::{Evt, Job};
use media::{Fmt, Kind, Probe, Quality};
use theme::{AMBER, DIM, FG, RED};
use tools::{Tool, Tools};

const COMPACT: [f32; 2] = [560.0, 470.0];
const FULL: [f32; 2] = [820.0, 600.0];

fn dbg_log(msg: &str) {
    let path = std::env::temp_dir().join("sippink.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let _ = writeln!(f, "{t} {msg}");
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    Compact,
    Full,
}

#[derive(PartialEq)]
enum Stat {
    Running,
    Done,
    Error,
}

struct DlRow {
    id: u64,
    name: String,
    done: u64,
    total: u64,
    stat: Stat,
    phase: String,
    err: String,
    path: Option<PathBuf>,
    started: Instant,
}

/// Where the link box is in its probe -> choose -> download cycle.
enum Stage {
    Idle,
    Probing { token: u64, url: String },
    Ready { probe: Probe, pick: Vec<bool> },
    Failed(String),
}

struct App {
    _tray: TrayIcon,
    menu_open: MenuId,
    menu_quit: MenuId,
    ctx: egui::Context,
    mode: Mode,
    positioned: bool,
    really_quit: bool,
    url: String,
    folder: PathBuf,
    focus_input: bool,
    pending_url: Option<String>,
    tx: Sender<Evt>,
    rx: Receiver<Evt>,
    next_id: u64,
    next_token: u64,
    stage: Stage,
    /// Last format chosen per kind; becomes the default next time.
    fmt_for: HashMap<Kind, Fmt>,
    quality: Quality,
    /// Skip the chooser and download with the remembered formats.
    auto: bool,
    tools: Tools,
    /// One-line status from tool installs / updates.
    notice: String,
    downloads: Vec<DlRow>,
}

fn make_icon() -> Icon {
    let (w, h) = (32u32, 32u32);
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    let bgc = [theme::BG.r(), theme::BG.g(), theme::BG.b()];
    let fgc = [FG.r(), FG.g(), FG.b()];
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let i = ((y as u32 * w + x as u32) * 4) as usize;
            let cx = 16i32;
            // 2px frame, then a down arrow: shaft + head
            let frame = !(2..30).contains(&x) || !(2..30).contains(&y);
            let shaft = (cx - 2..=cx + 1).contains(&x) && (6..=18).contains(&y);
            let half = 25 - y + 1;
            let head = (17..=25).contains(&y) && x >= cx - half && x <= cx + half - 1;
            let c = if frame || shaft || head { fgc } else { bgc };
            rgba[i..i + 3].copy_from_slice(&c);
            rgba[i + 3] = 255;
        }
    }
    Icon::from_rgba(rgba, w, h).expect("icon")
}

fn is_link(s: &str) -> bool {
    let s = s.trim();
    s.starts_with("http://") || s.starts_with("https://")
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);
        let menu = Menu::new();
        let open = MenuItem::new("Open full window", true, None);
        let quit = MenuItem::new("Quit", true, None);
        menu.append(&open).unwrap();
        menu.append(&quit).unwrap();
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("Sippink — sip a link. Double-click to paste one")
            .with_icon(make_icon())
            .build()
            .expect("tray");

        let (tx, rx) = std::sync::mpsc::channel::<Evt>();
        // `sippink [url] [--out dir]`
        let args: Vec<String> = std::env::args().skip(1).collect();
        let folder = args
            .iter()
            .position(|a| a == "--out")
            .and_then(|i| args.get(i + 1))
            .map(PathBuf::from)
            .or_else(dirs::download_dir)
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."));
        let pending_url = args.into_iter().find(|a| is_link(a));

        // Tray clicks only fill a background channel; they don't wake winit.
        // While hidden in the tray the window gets no repaints, so update()
        // would never run to drain those events. This ticker wakes the event
        // loop via eframe's proxy so tray actions are always processed.
        {
            let ctx = cc.egui_ctx.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_millis(120));
                ctx.request_repaint();
            });
        }

        App {
            _tray: tray,
            menu_open: open.id().clone(),
            menu_quit: quit.id().clone(),
            ctx: cc.egui_ctx.clone(),
            mode: Mode::Compact,
            positioned: false,
            really_quit: false,
            url: String::new(),
            folder,
            focus_input: true,
            pending_url,
            tx,
            rx,
            next_id: 1,
            next_token: 1,
            stage: Stage::Idle,
            fmt_for: HashMap::new(),
            quality: Quality::BEST,
            auto: false,
            tools: Tools::detect(),
            notice: String::new(),
            downloads: Vec::new(),
        }
    }

    fn show(&mut self, size: [f32; 2]) {
        use egui::ViewportCommand as V;
        self.ctx.send_viewport_cmd(V::Visible(true));
        self.ctx.send_viewport_cmd(V::Minimized(false));
        self.ctx.send_viewport_cmd(V::InnerSize(size.into()));
        self.ctx.send_viewport_cmd(V::Focus);
        self.ctx.request_repaint();
    }

    fn show_compact(&mut self) {
        dbg_log("show_compact");
        self.mode = Mode::Compact;
        self.positioned = false;
        self.focus_input = true;
        self.show(COMPACT);
    }

    fn show_full(&mut self) {
        dbg_log("show_full");
        self.mode = Mode::Full;
        self.focus_input = true;
        self.show(FULL);
    }

    fn hide(&self) {
        dbg_log("hide");
        self.ctx
            .send_viewport_cmd(egui::ViewportCommand::Visible(false));
    }

    fn reporter(&self) -> impl Fn(Evt) + Send + 'static {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        move |e| {
            let _ = tx.send(e);
            ctx.request_repaint();
        }
    }

    /// Step 1: find out what the link is.
    fn start_probe(&mut self) {
        let url = self.url.trim().to_string();
        if !is_link(&url) {
            return;
        }
        let token = self.next_token;
        self.next_token += 1;
        self.stage = Stage::Probing { token, url: url.clone() };
        let report = self.reporter();
        let tools = self.tools.clone();
        std::thread::spawn(move || {
            let res = job::probe(&job::agent(), &tools, &url);
            report(Evt::Probed { token, res });
        });
    }

    /// Step 2: download every picked item with the chosen formats.
    fn start_jobs(&mut self) {
        let Stage::Ready { probe, pick } = std::mem::replace(&mut self.stage, Stage::Idle) else {
            return;
        };
        let mut jobs = Vec::new();
        for (m, _) in probe.items.into_iter().zip(pick).filter(|(_, p)| *p) {
            let id = self.next_id;
            self.next_id += 1;
            self.downloads.push(DlRow {
                id,
                name: m.stem.clone(),
                done: 0,
                total: 0,
                stat: Stat::Running,
                phase: String::new(),
                err: String::new(),
                path: None,
                started: Instant::now(),
            });
            let fmt = media::usable_fmt(&m, self.fmt_for.get(&m.kind).copied(), self.tools.ffmpeg.is_some());
            jobs.push(Job { id, media: m, fmt, quality: self.quality });
        }
        let report = self.reporter();
        let (folder, tools) = (self.folder.clone(), self.tools.clone());
        std::thread::spawn(move || {
            let agent = job::agent();
            for j in jobs {
                let evt = match job::run(&agent, &tools, &folder, &j, &report) {
                    Ok(path) => Evt::Done { id: j.id, path },
                    Err(msg) => Evt::Error { id: j.id, msg },
                };
                report(evt);
            }
        });
        self.url.clear();
        self.focus_input = true;
    }

    fn row(&mut self, id: u64) -> Option<&mut DlRow> {
        self.downloads.iter_mut().find(|r| r.id == id)
    }

    fn drain(&mut self) {
        while let Ok(e) = self.rx.try_recv() {
            match e {
                Evt::Probed { token, res } => {
                    let current = matches!(self.stage, Stage::Probing { token: t, .. } if t == token);
                    if !current {
                        continue;
                    }
                    self.stage = match res {
                        Ok(probe) => {
                            let pick = probe.items.iter().map(|m| m.default_on).collect();
                            Stage::Ready { probe, pick }
                        }
                        Err(e) => Stage::Failed(e),
                    };
                    if self.auto && matches!(self.stage, Stage::Ready { .. }) {
                        self.start_jobs();
                    }
                }
                Evt::Named { id, name } => {
                    if let Some(r) = self.row(id) {
                        r.name = name;
                    }
                }
                Evt::Progress { id, done, total } => {
                    if let Some(r) = self.row(id) {
                        r.done = done;
                        r.total = total;
                    }
                }
                Evt::Phase { id, text } => {
                    if let Some(r) = self.row(id) {
                        r.phase = text;
                    }
                }
                Evt::Done { id, path } => {
                    if let Some(r) = self.row(id) {
                        r.stat = Stat::Done;
                        // after a conversion the file differs from what was fetched
                        r.done = std::fs::metadata(&path).map_or(r.done, |m| m.len());
                        r.path = Some(path);
                    }
                }
                Evt::ToolReady { tool, res } => {
                    self.notice = match res {
                        Ok(_) => format!("{} ready", tool.name()),
                        Err(e) => format!("{} install failed: {e}", tool.name()),
                    };
                    self.tools = Tools::detect();
                }
                Evt::Notice(text) => self.notice = text,
                Evt::Error { id, msg } => {
                    if let Some(r) = self.row(id) {
                        r.stat = Stat::Error;
                        r.err = msg;
                    }
                }
            }
        }
    }

    fn input_bar(&mut self, ui: &mut egui::Ui) {
        theme::pane(ui, "LINK", |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(">").color(AMBER));
                let te = egui::TextEdit::singleline(&mut self.url)
                    .hint_text("paste a YouTube / X / Pinterest link or any file link")
                    .desired_width(ui.available_width() - 70.0);
                let r = ui.add(te);
                if self.focus_input {
                    r.request_focus();
                    self.focus_input = false;
                }
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let can = is_link(&self.url);
                if (ui.add_enabled(can, egui::Button::new("SIP")).clicked() || enter) && can {
                    self.start_probe();
                }
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("dir").color(DIM));
                if ui.button("CHANGE").clicked() {
                    if let Some(d) = rfd::FileDialog::new()
                        .set_directory(&self.folder)
                        .pick_folder()
                    {
                        self.folder = d;
                    }
                }
                let folder_txt = self.folder.to_string_lossy().into_owned();
                ui.add(egui::Label::new(RichText::new(folder_txt).color(DIM)).truncate(true));
            });
        });
    }

    fn chooser(&mut self, ui: &mut egui::Ui) {
        let mut go = false;
        let mut cancel = false;
        match &mut self.stage {
            Stage::Idle => {
                theme::pane(ui, "", |ui| {
                    ui.label(RichText::new("waiting for a link_").color(DIM));
                });
            }
            Stage::Probing { url, .. } => {
                let dots = ".".repeat(1 + (ui.input(|i| i.time) * 3.0) as usize % 3);
                theme::pane(ui, "SCAN", |ui| {
                    ui.label(format!("looking inside{dots}"));
                    ui.add(egui::Label::new(RichText::new(url.as_str()).color(DIM)).truncate(true));
                });
            }
            Stage::Failed(msg) => {
                theme::pane(ui, "ERROR", |ui| {
                    ui.label(RichText::new(format!("x {msg}")).color(RED));
                });
            }
            Stage::Ready { probe, pick } => {
                let kinds = probe.kinds();
                let summary: Vec<String> = kinds
                    .iter()
                    .map(|k| {
                        let n = probe.items.iter().filter(|m| m.kind == *k).count();
                        format!("{n}x {}", k.label())
                    })
                    .collect();
                let title = format!("FOUND: {}", summary.join(" + "));
                let fmt_for = &mut self.fmt_for;
                let quality = &mut self.quality;
                let ffmpeg = self.tools.ffmpeg.is_some();
                theme::pane(ui, &title, |ui| {
                    ui.add(egui::Label::new(RichText::new(&probe.title).color(DIM)).truncate(true))
                        .on_hover_text(&probe.title);
                    egui::Grid::new("fmt").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                        for k in &kinds {
                            let sample = probe.items.iter().find(|m| m.kind == *k).unwrap();
                            ui.label(RichText::new(k.label()).color(AMBER));
                            ui.horizontal_wrapped(|ui| {
                                let cur = media::usable_fmt(sample, fmt_for.get(k).copied(), ffmpeg);
                                for f in k.formats() {
                                    let blocked = !ffmpeg && media::needs_ffmpeg(sample, *f);
                                    let r = ui.add_enabled(
                                        !blocked,
                                        egui::SelectableLabel::new(cur == *f, f.label()),
                                    );
                                    let r = r.on_disabled_hover_text(
                                        "needs ffmpeg: press GET FFMPEG below",
                                    );
                                    if r.clicked() {
                                        fmt_for.insert(*k, *f);
                                    }
                                }
                            });
                            ui.end_row();
                        }
                        // Quality only matters when the source offers several sizes.
                        if let Some(m) = probe.items.iter().find(|m| m.variants.len() > 1) {
                            ui.label(RichText::new("QUALITY").color(AMBER));
                            ui.horizontal_wrapped(|ui| {
                                let n = m.variants.len();
                                for (i, v) in m.variants.iter().enumerate() {
                                    let on = quality.index(n) == i;
                                    if ui.selectable_label(on, &v.label).clicked() {
                                        *quality = Quality::at(i, n);
                                    }
                                }
                            });
                            ui.end_row();
                        }
                    });
                    if probe.items.len() > 1 {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new("ITEMS").color(AMBER));
                            for (i, (m, p)) in probe.items.iter().zip(pick.iter_mut()).enumerate() {
                                let mark = if *p { "x" } else { " " };
                                let txt = format!("[{mark}] {} {}", i + 1, m.kind.label());
                                if ui.selectable_label(*p, txt).clicked() {
                                    *p = !*p;
                                }
                            }
                        });
                    }
                    ui.horizontal(|ui| {
                        let n = pick.iter().filter(|p| **p).count();
                        let btn = egui::Button::new(RichText::new(format!(">> DOWNLOAD {n}")).color(AMBER));
                        if ui.add_enabled(n > 0, btn).clicked() {
                            go = true;
                        }
                        if ui.button("CANCEL").clicked() {
                            cancel = true;
                        }
                    });
                });
            }
        }
        if go {
            self.start_jobs();
        } else if cancel {
            self.stage = Stage::Idle;
        }
    }

    fn download_list(&mut self, ui: &mut egui::Ui, compact: bool) {
        let mut clear = false;
        theme::pane(ui, "QUEUE", |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
                let take = if compact { 3 } else { usize::MAX };
                for r in self.downloads.iter().rev().take(take) {
                    let frac = if r.total > 0 { r.done as f32 / r.total as f32 } else { 0.0 };
                    let secs = r.started.elapsed().as_secs_f64().max(0.001);
                    let speed = r.done as f64 / secs;
                    ui.horizontal(|ui| {
                        let (bar, color) = match r.stat {
                            Stat::Done => (theme::ascii_bar(1.0, 12), FG),
                            Stat::Error => ("[!! FAILED !]".to_string(), RED),
                            Stat::Running => (theme::ascii_bar(frac, 12), AMBER),
                        };
                        ui.label(RichText::new(bar).color(color));
                        let detail = match r.stat {
                            // numbers first so truncating a long name never hides them
                            Stat::Done => format!("{}  {}", human(r.done), r.name),
                            Stat::Error => format!("{}  {}", r.err, r.name),
                            Stat::Running if !r.phase.is_empty() => format!("{}...  {}", r.phase, r.name),
                            Stat::Running if r.total > 0 => format!(
                                "{}/{}  {}/s  {}",
                                human(r.done),
                                human(r.total),
                                human(speed as u64),
                                r.name
                            ),
                            Stat::Running => format!("{}  {}/s  {}", human(r.done), human(speed as u64), r.name),
                        };
                        if let Some(p) = &r.path {
                            if ui.small_button("OPEN").clicked() {
                                let _ = std::process::Command::new("explorer")
                                    .arg(format!("/select,{}", p.display()))
                                    .spawn();
                            }
                        }
                        ui.add(egui::Label::new(RichText::new(detail).color(color)).truncate(true));
                    });
                }
                if self.downloads.is_empty() {
                    ui.label(RichText::new("nothing yet").color(DIM));
                }
            });
            if !compact && self.downloads.iter().any(|r| r.stat != Stat::Running) {
                clear = ui.button("CLEAR FINISHED").clicked();
            }
        });
        if clear {
            self.downloads.retain(|r| r.stat == Stat::Running);
        }
    }

    fn footer(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if self.mode == Mode::Compact {
                if ui.button("FULL").clicked() {
                    self.show_full();
                }
            } else if ui.button("TO TRAY").clicked() {
                self.hide();
            }
            let auto = if self.auto { "[x] AUTO" } else { "[ ] AUTO" };
            let r = ui.selectable_label(self.auto, auto);
            if r.on_hover_text("skip the chooser and use the last formats").clicked() {
                self.auto = !self.auto;
            }
        });
        ui.horizontal_wrapped(|ui| {
            for t in Tool::ALL {
                if self.tools.get(t).is_some() {
                    ui.label(RichText::new(format!("{}:ok", t.name())).color(DIM));
                } else if ui
                    .button(RichText::new(format!("GET {}", t.name().to_uppercase())).color(AMBER))
                    .on_hover_text(match t {
                        Tool::YtDlp => "needed for YouTube",
                        Tool::Ffmpeg => "needed for MP3 / GIF / WEBM and YouTube above 360p",
                        Tool::Deno => "optional: helps yt-dlp get every YouTube format",
                    })
                    .clicked()
                {
                    self.install(t);
                }
            }
            if let Some(exe) = self.tools.ytdlp.clone() {
                let r = ui.small_button("UPDATE YT-DLP");
                if r.on_hover_text("run this when YouTube downloads start failing").clicked() {
                    self.notice = "updating yt-dlp...".into();
                    let report = self.reporter();
                    std::thread::spawn(move || {
                        let msg = tools::update_ytdlp(&exe).unwrap_or_else(|e| format!("update failed: {e}"));
                        report(Evt::Notice(msg));
                    });
                }
            }
        });
        if !self.notice.is_empty() {
            ui.label(RichText::new(&self.notice).color(AMBER));
        }
    }

    /// Downloads a helper into Sippink's bin folder, shown as a queue row.
    fn install(&mut self, tool: Tool) {
        let id = self.next_id;
        self.next_id += 1;
        self.downloads.push(DlRow {
            id,
            name: format!("installing {}", tool.name()),
            done: 0,
            total: 0,
            stat: Stat::Running,
            phase: String::new(),
            err: String::new(),
            path: None,
            started: Instant::now(),
        });
        let report = self.reporter();
        std::thread::spawn(move || {
            let res = tools::install(&job::agent(), tool, id, &report);
            report(match &res {
                Ok(path) => Evt::Done { id, path: path.clone() },
                Err(msg) => Evt::Error { id, msg: msg.clone() },
            });
            report(Evt::ToolReady { tool, res });
        });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // keep the loop alive to poll tray events even while hidden
        ctx.request_repaint_after(Duration::from_millis(150));
        self.drain();

        if let Some(u) = self.pending_url.take() {
            self.url = u;
            self.start_probe();
        }

        while let Ok(ev) = MenuEvent::receiver().try_recv() {
            dbg_log(&format!("menu_event id={:?}", ev.id));
            if ev.id == self.menu_open {
                self.show_full();
            } else if ev.id == self.menu_quit {
                dbg_log("quit requested");
                self.really_quit = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        while let Ok(ev) = TrayIconEvent::receiver().try_recv() {
            match ev {
                TrayIconEvent::DoubleClick { .. } => {
                    dbg_log("tray DoubleClick");
                    self.show_compact();
                }
                TrayIconEvent::Click { button, .. } => {
                    dbg_log(&format!("tray Click {:?}", button));
                    if button == tray_icon::MouseButton::Left {
                        self.show_compact();
                    }
                }
                other => dbg_log(&format!("tray {:?}", std::mem::discriminant(&other))),
            }
        }

        if ctx.input(|i| i.viewport().close_requested()) && !self.really_quit {
            dbg_log("close_requested -> cancel+hide");
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.hide();
        }

        if self.mode == Mode::Compact && !self.positioned {
            if let Some(m) = ctx.input(|i| i.viewport().monitor_size) {
                let [w, h] = COMPACT;
                let x = (m.x - w - 24.0).max(0.0);
                let y = (m.y - h - 64.0).max(0.0);
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(x, y)));
                self.positioned = true;
            }
        }

        let compact = self.mode == Mode::Compact;
        let frame = egui::Frame::none()
            .fill(theme::BG)
            .stroke(theme::px(2.0, DIM))
            .inner_margin(10.0);
        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("SIPPINK").heading().color(FG));
                ui.label(RichText::new("v0.1 :: sip a link").color(DIM));
            });
            self.input_bar(ui);
            self.chooser(ui);
            self.download_list(ui, compact);
            self.footer(ui);
        });
    }
}

/// `sippink --get <url> [--fmt mp4] [--quality best] [--out dir] [--probe]`
/// Same probe/download path as the window, printed to stdout. Handy for
/// scripting and for testing extractors.
fn cli(args: &[String]) -> i32 {
    let mut url = None;
    let mut fmt = None;
    let mut quality_arg = String::from("BEST");
    let mut out = dirs::download_dir().unwrap_or_else(|| PathBuf::from("."));
    let mut only_probe = false;
    let mut all = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--get" => url = it.next().cloned(),
            "--fmt" => {
                let v = it.next().map(|s| s.to_ascii_uppercase()).unwrap_or_default();
                fmt = [Fmt::Mp4, Fmt::Webm, Fmt::Mp3, Fmt::M4a, Fmt::Gif, Fmt::Jpg, Fmt::Png, Fmt::Webp, Fmt::Original]
                    .into_iter()
                    .find(|f| f.label() == v || (v == "ORIGINAL" && *f == Fmt::Original));
                if fmt.is_none() {
                    eprintln!("unknown --fmt {v}");
                    return 2;
                }
            }
            "--quality" => quality_arg = it.next().map(|s| s.to_ascii_uppercase()).unwrap_or_default(),
            "--out" => out = it.next().map(PathBuf::from).unwrap_or(out),
            "--probe" => only_probe = true,
            "--all" => all = true,
            "--install" => {
                let name = it.next().map(String::as_str).unwrap_or("");
                let Some(t) = Tool::ALL.into_iter().find(|t| t.name() == name) else {
                    eprintln!("--install yt-dlp|ffmpeg|deno");
                    return 2;
                };
                return match tools::install(&job::agent(), t, 0, &|_| {}) {
                    Ok(p) => {
                        println!("installed {}", p.display());
                        0
                    }
                    Err(e) => {
                        eprintln!("error: {e}");
                        1
                    }
                };
            }
            _ => {}
        }
    }
    let Some(url) = url else {
        eprintln!("usage: sippink --install yt-dlp|ffmpeg|deno");
        eprintln!("       sippink --get <url> [--fmt mp4|mp3|m4a|gif|webm|jpg|png|webp] [--quality best|medium|small|1080p|...] [--out dir] [--probe] [--all]");
        return 2;
    };
    let agent = job::agent();
    let tools = Tools::detect();
    let probe = match job::probe(&agent, &tools, &url) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    println!("{}", probe.title);
    for (i, m) in probe.items.iter().enumerate() {
        let sizes: Vec<&str> = m.variants.iter().map(|v| v.label.as_str()).collect();
        let fmts: Vec<&str> = m.kind.formats().iter().map(|f| f.label()).collect();
        println!("  {}. {:<5} {}  sizes={sizes:?}  formats={fmts:?}", i + 1, m.kind.label(), m.stem);
    }
    if only_probe {
        return 0;
    }
    // best / medium / small, or a size label of any item (1080p, ORIG...)
    let quality = match quality_arg.as_str() {
        "BEST" => Quality::BEST,
        "MEDIUM" => Quality(0.5),
        "SMALL" => Quality(1.0),
        label => {
            let found = probe.items.iter().find_map(|m| {
                let i = m.variants.iter().position(|v| v.label.eq_ignore_ascii_case(label))?;
                Some(Quality::at(i, m.variants.len()))
            });
            match found {
                Some(q) => q,
                None => {
                    eprintln!("no size {label} here");
                    return 2;
                }
            }
        }
    };
    let _ = std::fs::create_dir_all(&out);
    let mut failed = 0;
    for (i, m) in probe.items.into_iter().enumerate().filter(|(_, m)| all || m.default_on) {
        let f = media::usable_fmt(&m, fmt, tools.ffmpeg.is_some());
        let j = Job { id: i as u64 + 1, media: m, fmt: f, quality };
        let last = std::cell::RefCell::new(String::new());
        let report = |e: Evt| {
            if let Evt::Phase { text, .. } = e {
                if !text.is_empty() && *last.borrow() != text {
                    println!("     {text}...");
                    *last.borrow_mut() = text;
                }
            }
        };
        match job::run(&agent, &tools, &out, &j, &report) {
            Ok(p) => {
                let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                println!("  ok  {}  ({})", p.display(), human(size));
            }
            Err(e) => {
                failed += 1;
                println!("  ERR item {}: {e}", i + 1);
            }
        }
    }
    i32::from(failed > 0)
}

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--get" || a == "--install") {
        std::process::exit(cli(&args));
    }
    let _ = std::fs::remove_file(std::env::temp_dir().join("sippink.log"));
    dbg_log("app start");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Sippink")
            .with_inner_size(COMPACT)
            .with_min_inner_size([420.0, 260.0])
            .with_resizable(true),
        ..Default::default()
    };
    eframe::run_native("Sippink", options, Box::new(|cc| Box::new(App::new(cc))))
}
