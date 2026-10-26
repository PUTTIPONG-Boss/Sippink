#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::RichText;
use tray_icon::Icon;

mod files;
mod job;
mod media;
mod pinterest;
mod theme;
mod tools;
mod tray;
mod x;
mod youtube;

use files::human;
use job::{Evt, Job};
use media::{Fmt, Kind, Probe, Quality};
use theme::{AMBER, DIM, FG, RED};
use tools::{Tool, Tools};

const COMPACT: [f32; 2] = [560.0, 510.0];
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

/// Downloads running at once.
const WORKERS: usize = 4;
/// Above this many items the chooser toggles whole kinds, not single items.
const MANY: usize = 12;

/// Where the link box is in its probe -> choose -> download cycle.
enum Stage {
    Idle,
    Probing { token: u64, url: String, status: String, cancel: Arc<AtomicBool> },
    Ready { probe: Probe, pick: Vec<bool> },
    Failed(String),
}

struct App {
    /// Set by the tray when it showed the window; sized on the next frame.
    tray_request: tray::Request,
    ctx: egui::Context,
    mode: Mode,
    positioned: bool,
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

/// assets/icon.png (also baked into the exe by build.rs) at `size` px.
fn icon_rgba(size: u32) -> (Vec<u8>, u32, u32) {
    let img = image::load_from_memory(include_bytes!("../assets/icon.png")).expect("icon.png");
    // Pixel art: nearest-neighbour keeps the edges sharp.
    let img = img.resize_exact(size, size, image::imageops::FilterType::Nearest).to_rgba8();
    let (w, h) = img.dimensions();
    (img.into_raw(), w, h)
}

/// The main window's HWND, for showing it from the tray while eframe is idle.
fn window_handle(cc: &eframe::CreationContext<'_>) -> isize {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match cc.window_handle().map(|h| h.as_raw()) {
        Ok(RawWindowHandle::Win32(w)) => w.hwnd.get(),
        _ => 0,
    }
}

fn tray_icon() -> Icon {
    let (rgba, w, h) = icon_rgba(32);
    Icon::from_rgba(rgba, w, h).expect("icon")
}

fn is_link(s: &str) -> bool {
    let s = s.trim();
    s.starts_with("http://") || s.starts_with("https://")
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);
        let tray_request = tray::install(tray_icon(), cc.egui_ctx.clone(), window_handle(cc));

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

        App {
            tray_request,
            ctx: cc.egui_ctx.clone(),
            mode: Mode::Compact,
            positioned: false,
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
        self.positioned = false;
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
        self.probe_with(false);
    }

    /// `deep`: search big Pinterest accounts without the time budget.
    fn probe_with(&mut self, deep: bool) {
        let url = self.url.trim().to_string();
        if !is_link(&url) {
            return;
        }
        let token = self.next_token;
        self.next_token += 1;
        let cancel = Arc::new(AtomicBool::new(false));
        self.stage = Stage::Probing { token, url: url.clone(), status: String::new(), cancel: cancel.clone() };
        let report = self.reporter();
        let tools = self.tools.clone();
        std::thread::spawn(move || {
            let progress = |text| report(Evt::ProbeStatus { token, text });
            let ctx = job::Ctx { progress: &progress, cancel: &cancel, deep };
            let res = job::probe(&job::agent(), &tools, &url, &ctx);
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
            let done = |j: &Job, res: Result<PathBuf, String>| {
                report(match res {
                    Ok(path) => Evt::Done { id: j.id, path },
                    Err(msg) => Evt::Error { id: j.id, msg },
                })
            };
            job::run_all(&tools, &folder, jobs, WORKERS, &report, &done);
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
                Evt::ProbeStatus { token, text } => {
                    if let Stage::Probing { token: t, status, .. } = &mut self.stage {
                        if *t == token {
                            *status = text;
                        }
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
        let mut deeper = false;
        match &mut self.stage {
            Stage::Idle => {
                theme::pane(ui, "", |ui| {
                    ui.label(RichText::new("waiting for a link_").color(DIM));
                });
            }
            Stage::Probing { url, status, cancel: stop, .. } => {
                let dots = ".".repeat(1 + (ui.input(|i| i.time) * 3.0) as usize % 3);
                theme::pane(ui, "SCAN", |ui| {
                    ui.label(format!("looking inside{dots}"));
                    if !status.is_empty() {
                        ui.add(egui::Label::new(RichText::new(status.as_str()).color(AMBER)).truncate(true));
                    }
                    ui.add(egui::Label::new(RichText::new(url.as_str()).color(DIM)).truncate(true));
                    if ui.button("CANCEL").clicked() {
                        stop.store(true, Ordering::Relaxed);
                        cancel = true;
                    }
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
                    if !probe.note.is_empty() {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(format!("! {}", probe.note)).color(AMBER));
                            if probe.note.contains("SEARCH ALL") && ui.small_button("SEARCH ALL").clicked() {
                                deeper = true;
                            }
                        });
                    }
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
                    if probe.items.len() > MANY {
                        // Hundreds of pins: toggle a whole kind at a time.
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new("TAKE").color(AMBER));
                            for k in &kinds {
                                let idx: Vec<usize> = (0..probe.items.len()).filter(|i| probe.items[*i].kind == *k).collect();
                                let on = idx.iter().filter(|i| pick[**i]).count();
                                let mark = match on {
                                    0 => " ",
                                    n if n == idx.len() => "x",
                                    _ => "~",
                                };
                                let txt = format!("[{mark}] {} {}", idx.len(), k.label());
                                if ui.selectable_label(on > 0, txt).clicked() {
                                    let to = on == 0;
                                    for i in idx {
                                        pick[i] = to;
                                    }
                                }
                            }
                        });
                    } else if probe.items.len() > 1 {
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
        } else if deeper {
            self.probe_with(true);
        } else if cancel {
            self.stage = Stage::Idle;
        }
    }

    fn download_list(&mut self, ui: &mut egui::Ui, compact: bool) {
        let mut clear = false;
        let ok = self.downloads.iter().filter(|r| r.stat == Stat::Done).count();
        let bad = self.downloads.iter().filter(|r| r.stat == Stat::Error).count();
        let title = match self.downloads.len() {
            0 => "QUEUE".to_string(),
            n if bad > 0 => format!("QUEUE {ok}/{n} done, {bad} failed"),
            n => format!("QUEUE {ok}/{n} done"),
        };
        theme::pane(ui, &title, |ui| {
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

        // The tray already showed the window; size it for what was asked.
        let asked = self.tray_request.lock().unwrap().take();
        match asked {
            Some(tray::Show::Full) => self.show_full(),
            Some(tray::Show::Compact) => self.show_compact(),
            None => {}
        }

        if ctx.input(|i| i.viewport().close_requested()) {
            dbg_log("close_requested -> cancel+hide");
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.hide();
        }

        // Compact sits above the tray corner; full is centred so it never
        // grows off-screen from the compact spot.
        if !self.positioned {
            if let Some(m) = ctx.input(|i| i.viewport().monitor_size) {
                let pos = match self.mode {
                    Mode::Compact => {
                        let [w, h] = COMPACT;
                        egui::pos2((m.x - w - 24.0).max(0.0), (m.y - h - 64.0).max(0.0))
                    }
                    Mode::Full => {
                        let [w, h] = FULL;
                        egui::pos2(((m.x - w) / 2.0).max(0.0), ((m.y - h) / 2.0).max(0.0))
                    }
                };
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
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
    let mut deep = false;
    let mut limit = usize::MAX;
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
            "--deep" => deep = true,
            "--limit" => limit = it.next().and_then(|n| n.parse().ok()).unwrap_or(usize::MAX),
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
        eprintln!("       sippink --get <url> [--fmt mp4|mp3|m4a|gif|webm|jpg|png|webp] [--quality best|medium|small|1080p|...] [--out dir] [--probe] [--all] [--limit n] [--deep]");
        return 2;
    };
    let agent = job::agent();
    let tools = Tools::detect();
    let progress = |text: String| eprint!("\r  {text:<70}");
    let never = AtomicBool::new(false);
    let ctx = job::Ctx { progress: &progress, cancel: &never, deep };
    let probe = job::probe(&agent, &tools, &url, &ctx);
    eprint!("\r{:<74}\r", "");
    let probe = match probe {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    println!("{}", probe.title);
    if !probe.note.is_empty() {
        println!("  ! {}", probe.note);
    }
    for (i, m) in probe.items.iter().enumerate().take(if only_probe { usize::MAX } else { 10 }) {
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
    if probe.items.len() > 10 {
        println!("  ... {} items in all", probe.items.len());
    }
    let jobs: Vec<Job> = probe
        .items
        .into_iter()
        .enumerate()
        .filter(|(_, m)| all || m.default_on)
        .take(limit)
        .map(|(i, m)| {
            let fmt = media::usable_fmt(&m, fmt, tools.ffmpeg.is_some());
            Job { id: i as u64 + 1, media: m, fmt, quality }
        })
        .collect();
    let failed = std::sync::atomic::AtomicUsize::new(0);
    let report = |e: Evt| {
        if let Evt::Phase { id, text } = e {
            if !text.is_empty() {
                println!("     #{id} {text}...");
            }
        }
    };
    let done = |j: &Job, res: Result<PathBuf, String>| match res {
        Ok(p) => {
            let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            println!("  ok  {}  ({})", p.display(), human(size));
        }
        Err(e) => {
            failed.fetch_add(1, Ordering::Relaxed);
            println!("  ERR item {}: {e}", j.id);
        }
    };
    job::run_all(&tools, &out, jobs, WORKERS, &report, &done);
    i32::from(failed.load(Ordering::Relaxed) > 0)
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
            .with_icon({
                let (rgba, width, height) = icon_rgba(64);
                egui::IconData { rgba, width, height }
            })
            .with_inner_size(COMPACT)
            .with_min_inner_size([420.0, 260.0])
            .with_resizable(true),
        ..Default::default()
    };
    eframe::run_native("Sippink", options, Box::new(|cc| Box::new(App::new(cc))))
}
