#![windows_subsystem = "windows"]
//! Spotter GUI: search files by name, text, size and date, skipping dependency, build and system folders.

mod icons;

use eframe::egui::{self, Align, Color32, Key, Layout, RichText, Sense, Vec2};
use egui_extras::{Column, TableBuilder};
use spotter::drives;
use spotter::fmt::{self, count};
use spotter::ignore::{IgnoreList, DEFAULT_RULES};
use spotter::search::{self, Hit, NameMatcher, Progress, Query};
use spotter::settings::{remember, Settings};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("Spotter").with_inner_size([1500.0, 860.0]).with_min_inner_size([900.0, 500.0]),
        ..Default::default()
    };
    // `spotter.exe [folder] [name]`: open on a folder, and search it straight away when a name is given.
    let args: Vec<String> = std::env::args().skip(1).collect();
    eframe::run_native("Spotter", options, Box::new(move |_cc| Ok(Box::new(App::new(&args)))))
}

/// Lines shown per file on the Hits tab.
const MAX_HIT_LINES: usize = 500;
/// Errors listed on the Summary tab.
const SHOWN_ERRORS: usize = 200;
/// Width of the labels in front of the search fields.
const LABEL_W: f32 = 110.0;
const SIZE_UNITS: [(&str, u64); 3] = [("KB", 1024), ("MB", 1024 * 1024), ("GB", 1024 * 1024 * 1024)];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Main,
    Options,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SideTab {
    Summary,
    Hits,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Col {
    Name,
    Location,
    Size,
    Hits,
    Type,
    Modified,
}

enum Status {
    Idle,
    Running,
    Completed,
    Stopped,
}

struct Job {
    rx: Receiver<Hit>,
    handle: JoinHandle<()>,
}

fn type_of(h: &Hit) -> String {
    if h.is_dir {
        return "File Folder".into();
    }
    match h.path.extension() {
        Some(e) => format!("{} File", e.to_string_lossy().to_uppercase()),
        None => "File".into(),
    }
}

fn open(path: &Path) {
    let _ = std::process::Command::new("explorer").arg(path).spawn();
}

fn reveal(path: &Path) {
    let _ = std::process::Command::new("explorer").raw_arg(format!("/select,\"{}\"", path.display())).spawn();
}

/// (label, value) choices offered under Look in: all local drives, then each drive.
fn drive_choices(list: &[drives::Drive]) -> Vec<(String, String)> {
    let local = drives::local_roots(list);
    let mut v = vec![(format!("Local drives  ({local})"), local)];
    v.extend(list.iter().map(|d| (d.describe(), d.root.clone())));
    v
}

struct App {
    s: Settings,
    tab: Tab,
    side: SideTab,
    min_size: String,
    max_size: String,
    size_unit: usize,
    after: String,
    before: String,
    form_error: Option<String>,

    job: Option<Job>,
    prog: Arc<Progress>,
    status: Status,
    started: Instant,
    elapsed: Duration,
    completed_at: String,
    /// The search that produced `results`, for the title bar and the Hits tab.
    searched: Query,
    /// "name / text" of that search.
    searched_label: String,

    results: Vec<Hit>,
    sort: (Col, bool),
    selected: Option<usize>,
    hit_lines: Option<(PathBuf, Vec<(usize, String)>)>,
    title: String,
    /// (label, value) choices offered under Look in, read once at startup.
    drives: Vec<(String, String)>,
    /// Search on the first frame (a name was given on the command line).
    start_now: bool,
    icons: icons::Icons,
}

impl App {
    fn new(args: &[String]) -> App {
        let list = drives::list();
        let mut s = Settings::load();
        if let Some(folder) = args.first() {
            s.look_in = folder.clone();
        }
        if let Some(name) = args.get(1) {
            s.name = name.clone();
        }
        if s.look_in.trim().is_empty() {
            s.look_in = drives::local_roots(&list);
        }
        App {
            s,
            tab: Tab::Main,
            side: SideTab::Summary,
            min_size: String::new(),
            max_size: String::new(),
            size_unit: 0,
            after: String::new(),
            before: String::new(),
            form_error: None,
            job: None,
            prog: Arc::new(Progress::default()),
            status: Status::Idle,
            started: Instant::now(),
            elapsed: Duration::ZERO,
            completed_at: String::new(),
            searched: Query::default(),
            searched_label: String::new(),
            results: Vec::new(),
            sort: (Col::Name, true),
            selected: None,
            hit_lines: None,
            title: String::new(),
            drives: drive_choices(&list),
            start_now: args.len() >= 2,
            icons: icons::Icons::new(),
        }
    }

    fn build_query(&self) -> Result<Query, String> {
        let roots: Vec<PathBuf> = self.s.look_in.split(';').map(str::trim).filter(|s| !s.is_empty()).map(PathBuf::from).collect();
        if roots.is_empty() {
            return Err("Choose a folder to look in.".into());
        }
        let unit = SIZE_UNITS[self.size_unit].1;
        Ok(Query {
            roots,
            name: NameMatcher::new(&self.s.name, self.s.name_case),
            text: self.s.text.clone(),
            text_case: self.s.text_case,
            subfolders: self.s.subfolders,
            skip_ignored: self.s.skip_ignored,
            ignore: IgnoreList::parse(&self.s.ignore_rules),
            max_text_bytes: self.s.max_text_mb * 1024 * 1024,
            min_size: fmt::parse_size(&self.min_size, unit)?,
            max_size: fmt::parse_size(&self.max_size, unit)?,
            after: fmt::parse_date(&self.after)?,
            before: fmt::parse_date(&self.before)?,
            folders: self.s.folders,
        })
    }

    fn start(&mut self, ctx: &egui::Context) {
        if self.job.is_some() {
            return;
        }
        let q = match self.build_query() {
            // Nothing to match would list every file on the drives.
            Ok(q) if q.name.is_empty() && q.text.is_empty() => {
                self.form_error = Some("Enter a file name or text to look for (* finds everything).".into());
                return;
            }
            Ok(q) => q,
            Err(e) => {
                self.form_error = Some(e);
                return;
            }
        };
        self.form_error = None;
        remember(&mut self.s.name_history, &self.s.name);
        remember(&mut self.s.text_history, &self.s.text);
        remember(&mut self.s.look_in_history, &self.s.look_in);
        self.s.save();

        self.results.clear();
        self.selected = None;
        self.hit_lines = None;
        self.prog = Arc::new(Progress::default());
        self.status = Status::Running;
        self.started = Instant::now();
        self.searched = q.clone();
        self.searched_label = [self.s.name.trim(), self.s.text.trim()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" / ");
        let (tx, rx) = std::sync::mpsc::channel();
        let prog = self.prog.clone();
        let ctx = ctx.clone();
        let handle = std::thread::spawn(move || {
            search::search(&q, &prog, &tx);
            ctx.request_repaint();
        });
        self.job = Some(Job { rx, handle });
    }

    fn stop(&self) {
        self.prog.cancel.store(true, Ordering::Relaxed);
    }

    fn poll(&mut self) {
        let Some(job) = &self.job else { return };
        let done = job.handle.is_finished();
        self.results.extend(job.rx.try_iter());
        if done {
            self.job = None;
            self.elapsed = self.started.elapsed();
            self.completed_at = fmt::now_string();
            self.status = if self.prog.cancel.load(Ordering::Relaxed) { Status::Stopped } else { Status::Completed };
            self.sort_results();
        } else {
            self.elapsed = self.started.elapsed();
        }
    }

    fn sort_results(&mut self) {
        let keep = self.selected.and_then(|i| self.results.get(i)).map(|h| h.path.clone());
        let (col, asc) = self.sort;
        let r = &mut self.results;
        match col {
            Col::Name => r.sort_by_cached_key(|h| h.name().to_lowercase()),
            Col::Location => r.sort_by_cached_key(|h| h.path.to_string_lossy().to_lowercase()),
            Col::Size => r.sort_by_key(|h| h.size),
            Col::Hits => r.sort_by_key(|h| h.hits),
            Col::Type => r.sort_by_cached_key(|h| (type_of(h), h.name().to_lowercase())),
            Col::Modified => r.sort_by_key(|h| h.modified),
        }
        if !asc {
            r.reverse();
        }
        self.selected = keep.and_then(|p| self.results.iter().position(|h| h.path == p));
    }

    fn select(&mut self, i: usize) {
        self.selected = Some(i);
        self.hit_lines = None;
    }

    // ---------------------------------------------------------------- search form

    fn form(&mut self, ui: &mut egui::Ui) {
        let running = self.job.is_some();
        let mut go = false;
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.tab, Tab::Main, "Main");
            ui.selectable_value(&mut self.tab, Tab::Options, "Options");
        });
        ui.separator();
        let form_w = (ui.available_width() - 110.0).max(500.0);
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(form_w);
                match self.tab {
                    Tab::Main => go |= self.main_tab(ui),
                    Tab::Options => self.options_tab(ui),
                }
            });
            ui.vertical(|ui| {
                let big = Vec2::new(90.0, 28.0);
                if ui.add_enabled(!running, egui::Button::new(RichText::new("Start").strong()).min_size(big)).clicked() {
                    go = true;
                }
                if ui.add_enabled(running, egui::Button::new("Stop").min_size(big)).clicked() {
                    self.stop();
                }
            });
        });
        if let Some(e) = &self.form_error {
            ui.label(RichText::new(format!("[ERROR] {e}")).color(ui.visuals().error_fg_color));
        }
        if go {
            self.start(ui.ctx());
        }
    }

    /// Returns true when Enter was pressed in a field.
    fn main_tab(&mut self, ui: &mut egui::Ui) -> bool {
        let mut enter = false;
        let mut submit = |r: &egui::Response, ui: &egui::Ui| {
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                enter = true;
            }
        };
        let field_w = (ui.available_width() - LABEL_W - 300.0).max(200.0);
        let label = |ui: &mut egui::Ui, text: &str| {
            ui.add_sized([LABEL_W, 20.0], egui::Label::new(text).halign(Align::Min));
        };
        ui.horizontal(|ui| {
            label(ui, "File name:");
            let r = ui.add(egui::TextEdit::singleline(&mut self.s.name).desired_width(field_w).hint_text("e.g. report  or  *.rs;*.toml"));
            submit(&r, ui);
            history(ui, "name_hist", &mut self.s.name, &self.s.name_history, &[]);
            ui.toggle_value(&mut self.s.name_case, "Aa").on_hover_text("Match case");
        });
        ui.horizontal(|ui| {
            label(ui, "Containing text:");
            let r = ui.add(egui::TextEdit::singleline(&mut self.s.text).desired_width(field_w).hint_text("leave empty to search names only"));
            submit(&r, ui);
            history(ui, "text_hist", &mut self.s.text, &self.s.text_history, &[]);
            ui.toggle_value(&mut self.s.text_case, "Aa").on_hover_text("Match case");
        });
        ui.horizontal(|ui| {
            label(ui, "Look in:");
            let r = ui.add(egui::TextEdit::singleline(&mut self.s.look_in).desired_width(field_w).hint_text(r"D:\Github;C:\Users  (separate folders with ;)"));
            submit(&r, ui);
            history(ui, "look_hist", &mut self.s.look_in, &self.s.look_in_history, &self.drives);
            ui.checkbox(&mut self.s.subfolders, "Subfolders");
            ui.checkbox(&mut self.s.skip_ignored, "Skip ignored folders").on_hover_text("node_modules, site-packages, .git, build output, WinSxS ... (edit the list on the Options tab)");
        });
        ui.horizontal(|ui| {
            label(ui, "Size:");
            ui.label(">");
            let r = ui.add(egui::TextEdit::singleline(&mut self.min_size).desired_width(70.0).hint_text("any"));
            submit(&r, ui);
            ui.label("<");
            let r = ui.add(egui::TextEdit::singleline(&mut self.max_size).desired_width(70.0).hint_text("any"));
            submit(&r, ui);
            egui::ComboBox::from_id_salt("unit").width(56.0).selected_text(SIZE_UNITS[self.size_unit].0).show_ui(ui, |ui| {
                for (i, (u, _)) in SIZE_UNITS.iter().enumerate() {
                    ui.selectable_value(&mut self.size_unit, i, *u);
                }
            });
            ui.add_space(24.0);
            ui.label("Modified after:");
            let r = ui.add(egui::TextEdit::singleline(&mut self.after).desired_width(110.0).hint_text("YYYY-MM-DD or 7d"));
            submit(&r, ui);
            ui.label("before:");
            let r = ui.add(egui::TextEdit::singleline(&mut self.before).desired_width(110.0).hint_text("YYYY-MM-DD or today"));
            submit(&r, ui);
        });
        enter
    }

    fn options_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.s.skip_ignored, "Skip ignored folders");
            ui.checkbox(&mut self.s.folders, "Find folders by name too");
            ui.add_space(16.0);
            ui.label("Read files up to");
            ui.add(egui::DragValue::new(&mut self.s.max_text_mb).range(1..=4096).suffix(" MB"));
            ui.label("for a text search");
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new("Ignored folders, one per line:  name   |   name if ../marker   |   name if marker-inside   |   Windows/WinSxS (path ending)").small().weak());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Reset to defaults").clicked() {
                    self.s.ignore_rules = DEFAULT_RULES.to_string();
                }
            });
        });
        egui::ScrollArea::vertical().max_height(150.0).show(ui, |ui| {
            ui.add(egui::TextEdit::multiline(&mut self.s.ignore_rules).code_editor().desired_width(f32::INFINITY).desired_rows(8));
        });
    }

    // ---------------------------------------------------------------- results

    fn results_table(&mut self, ui: &mut egui::Ui) {
        let mut clicked: Option<usize> = None;
        let mut action: Option<(Action, PathBuf)> = None;
        let mut resort: Option<Col> = None;
        let selected = self.selected;
        let sort = self.sort;
        let header = |ui: &mut egui::Ui, col: Col, label: &str, resort: &mut Option<Col>| {
            let arrow = if sort.0 == col { if sort.1 { "  ^" } else { "  v" } } else { "" };
            let b = egui::Button::new(RichText::new(format!("{label}{arrow}")).strong()).frame(false);
            if ui.add(b).clicked() {
                *resort = Some(col);
            }
        };
        let text_h = egui::TextStyle::Body.resolve(ui.style()).size + 6.0;
        let ctx = ui.ctx().clone();
        TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::initial(240.0).at_least(80.0).clip(true))
            .column(Column::remainder().at_least(120.0).clip(true))
            .column(Column::initial(85.0).at_least(50.0))
            .column(Column::initial(50.0).at_least(36.0))
            .column(Column::initial(90.0).at_least(50.0).clip(true))
            .column(Column::initial(120.0).at_least(110.0))
            .header(text_h + 2.0, |mut h| {
                for (col, label) in [(Col::Name, "Name"), (Col::Location, "Location"), (Col::Size, "Size"), (Col::Hits, "Hits"), (Col::Type, "Type"), (Col::Modified, "Modified")] {
                    h.col(|ui| header(ui, col, label, &mut resort));
                }
            })
            .body(|body| {
                body.rows(text_h, self.results.len(), |mut row| {
                    let i = row.index();
                    let h = &self.results[i];
                    row.set_selected(selected == Some(i));
                    row.col(|ui| {
                        match self.icons.get(&ctx, &h.path, h.is_dir) {
                            Some(tex) => drop(ui.add(egui::Image::new(tex).fit_to_exact_size(Vec2::splat(16.0)))),
                            None => file_icon(ui, h.is_dir),
                        }
                        ui.add(egui::Label::new(h.name()).truncate());
                    });
                    row.col(|ui| drop(ui.add(egui::Label::new(h.location()).truncate())));
                    row.col(|ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| if !h.is_dir { ui.label(fmt::kb(h.size)); });
                    });
                    row.col(|ui| {
                        if h.hits > 0 {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(count(h.hits as u64)));
                        }
                    });
                    row.col(|ui| drop(ui.add(egui::Label::new(type_of(h)).truncate())));
                    row.col(|ui| drop(ui.label(fmt::date(h.modified))));
                    let resp = row.response();
                    if resp.clicked() || resp.secondary_clicked() {
                        clicked = Some(i);
                    }
                    if resp.double_clicked() {
                        action = Some((Action::Open, h.path.clone()));
                    }
                    resp.context_menu(|ui| {
                        ui.label(RichText::new(h.path.display().to_string()).monospace().small());
                        for (a, label) in [(Action::Open, "Open"), (Action::Reveal, "Show in Explorer"), (Action::CopyPath, "Copy path"), (Action::CopyName, "Copy name")] {
                            if ui.button(label).clicked() {
                                action = Some((a, h.path.clone()));
                                ui.close();
                            }
                        }
                        ui.separator();
                        if ui.button("Copy all result paths").clicked() {
                            action = Some((Action::CopyAll, h.path.clone()));
                            ui.close();
                        }
                    });
                });
            });

        if let Some(i) = clicked {
            self.select(i);
        }
        if let Some(col) = resort {
            self.sort = if self.sort.0 == col { (col, !self.sort.1) } else { (col, !matches!(col, Col::Size | Col::Hits | Col::Modified)) };
            self.sort_results();
        }
        if let Some((a, p)) = action {
            match a {
                Action::Open => open(&p),
                Action::Reveal => reveal(&p),
                Action::CopyPath => ui.ctx().copy_text(p.display().to_string()),
                Action::CopyName => ui.ctx().copy_text(p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
                Action::CopyAll => ui.ctx().copy_text(self.results.iter().map(|h| h.path.display().to_string()).collect::<Vec<_>>().join("\r\n")),
            }
        }
    }

    fn keys(&mut self, ctx: &egui::Context) {
        if ctx.memory(|m| m.focused().is_some()) || self.results.is_empty() {
            return;
        }
        let (down, up, enter, copy) = ctx.input(|i| (i.key_pressed(Key::ArrowDown), i.key_pressed(Key::ArrowUp), i.key_pressed(Key::Enter), i.modifiers.command && i.key_pressed(Key::C)));
        let last = self.results.len() - 1;
        if down {
            self.select(self.selected.map_or(0, |i| (i + 1).min(last)));
        }
        if up {
            self.select(self.selected.map_or(0, |i| i.saturating_sub(1)));
        }
        if let Some(h) = self.selected.and_then(|i| self.results.get(i)) {
            if enter {
                open(&h.path);
            }
            if copy {
                ctx.copy_text(h.path.display().to_string());
            }
        }
    }

    // ---------------------------------------------------------------- side panel

    fn side_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.side, SideTab::Summary, "Summary");
            ui.selectable_value(&mut self.side, SideTab::Hits, "Hits");
        });
        ui.separator();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match self.side {
            SideTab::Summary => self.summary(ui),
            SideTab::Hits => self.hits(ui),
        });
    }

    fn summary(&self, ui: &mut egui::Ui) {
        let p = &self.prog;
        let n = |a: &std::sync::atomic::AtomicU64| count(a.load(Ordering::Relaxed));
        ui.label(RichText::new("Search statistics").size(16.0).strong());
        ui.add_space(4.0);
        let status = match self.status {
            Status::Idle => "Ready".to_string(),
            Status::Running => format!("Searching ({:.0} s)", self.elapsed.as_secs_f64()),
            Status::Completed => format!("Completed ({:.2} s)", self.elapsed.as_secs_f64()),
            Status::Stopped => format!("Stopped ({:.2} s)", self.elapsed.as_secs_f64()),
        };
        egui::Grid::new("stats").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
            let found_bytes: u64 = self.results.iter().map(|h| h.size).sum();
            ui.label("Found:");
            ui.label(format!("{} items ({})", count(self.results.len() as u64), fmt::human(found_bytes)));
            ui.end_row();
            ui.label("Checked:");
            ui.label(format!("{} items in {} folders", n(&p.checked), n(&p.dirs)));
            ui.end_row();
            ui.label("Skipped:");
            ui.label(format!("{} ignored folders", n(&p.ignored)));
            ui.end_row();
            if p.links.load(Ordering::Relaxed) > 0 {
                ui.label("Links:");
                ui.label(format!("{} not followed", n(&p.links)));
                ui.end_row();
            }
            if p.online_only.load(Ordering::Relaxed) > 0 {
                ui.label("Cloud:");
                ui.label(format!("{} online-only folders not opened", n(&p.online_only)));
                ui.end_row();
            }
            if p.online_files.load(Ordering::Relaxed) > 0 {
                ui.label("Cloud:");
                ui.label(format!("{} online-only files not read", n(&p.online_files)));
                ui.end_row();
            }
            if !self.searched.text.is_empty() {
                ui.label("Not read:");
                ui.label(format!("{} files (binary or too large)", n(&p.not_read)));
                ui.end_row();
            }
            ui.label("Status:");
            ui.label(status);
            ui.end_row();
            if !self.completed_at.is_empty() && self.job.is_none() {
                ui.label("Completed:");
                ui.label(&self.completed_at);
                ui.end_row();
            }
        });

        let mut ignored: Vec<(String, u64)> = p.ignored_by_rule.lock().map(|m| m.iter().map(|(k, v)| (k.clone(), *v)).collect()).unwrap_or_default();
        if !ignored.is_empty() {
            ignored.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            ui.add_space(12.0);
            ui.label(RichText::new("Skipped folders").size(16.0).strong());
            egui::Grid::new("ignored").num_columns(2).spacing([16.0, 2.0]).show(ui, |ui| {
                for (rule, c) in ignored {
                    ui.label(RichText::new(rule).monospace());
                    ui.label(count(c));
                    ui.end_row();
                }
            });
        }

        let denied = p.denied.load(Ordering::Relaxed);
        if denied > 0 {
            ui.add_space(12.0);
            let list = p.denied_list.lock().map(|v| v.iter().take(SHOWN_ERRORS).cloned().collect::<Vec<_>>()).unwrap_or_default();
            egui::CollapsingHeader::new(RichText::new(format!("No access ({})", count(denied))).size(16.0).strong())
                .id_salt("denied")
                .show(ui, |ui| {
                    ui.label(RichText::new("This account may not open these (mostly system and vendor folders that need administrator), so they were not searched.").small().weak());
                    for path in list {
                        ui.label(RichText::new(path).monospace().small());
                    }
                    if denied as usize > SHOWN_ERRORS {
                        ui.label(RichText::new(format!("... and {} more", count(denied - SHOWN_ERRORS as u64))).small().weak());
                    }
                });
        }

        let errors = p.errors.load(Ordering::Relaxed);
        if errors > 0 {
            ui.add_space(12.0);
            ui.label(RichText::new(format!("Errors reported ({})", count(errors))).size(16.0).strong().color(ui.visuals().error_fg_color));
            let list = p.error_list.lock().map(|v| v.iter().take(SHOWN_ERRORS).cloned().collect::<Vec<_>>()).unwrap_or_default();
            for (path, msg) in list {
                ui.label(RichText::new(path).monospace().small());
                ui.label(RichText::new(format!("    {msg}")).small().weak());
            }
            if errors as usize > SHOWN_ERRORS {
                ui.label(RichText::new(format!("... and {} more", count(errors - SHOWN_ERRORS as u64))).small().weak());
            }
        }
    }

    fn hits(&mut self, ui: &mut egui::Ui) {
        let Some(h) = self.selected.and_then(|i| self.results.get(i)) else {
            ui.label("Select a result to see where the text was found.");
            return;
        };
        ui.label(RichText::new(h.name()).size(16.0).strong());
        ui.label(RichText::new(h.location()).small().weak());
        ui.add_space(6.0);
        if self.searched.text.is_empty() || h.is_dir {
            ui.label("Lines are shown for a search with 'Containing text'.");
            return;
        }
        if self.hit_lines.as_ref().is_none_or(|(p, _)| p != &h.path) {
            self.hit_lines = Some((h.path.clone(), search::matching_lines(&h.path, &self.searched.text, self.searched.text_case, MAX_HIT_LINES)));
        }
        let Some((_, lines)) = &self.hit_lines else { return };
        if lines.is_empty() {
            ui.label("No matching lines (the text may span lines, or the file changed).");
        }
        let accent = ui.visuals().hyperlink_color;
        for (no, line) in lines {
            ui.horizontal_top(|ui| {
                ui.label(RichText::new(format!("{no:>6}")).monospace().color(accent));
                ui.add(egui::Label::new(RichText::new(line.trim_start()).monospace()).wrap());
            });
        }
        if lines.len() == MAX_HIT_LINES {
            ui.label(RichText::new(format!("(first {MAX_HIT_LINES} lines)")).small().weak());
        }
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if self.job.is_some() {
                ui.spinner();
                ui.label(format!(
                    "Searching ...  {} checked, {} found, {} folders skipped",
                    count(self.prog.checked.load(Ordering::Relaxed)),
                    count(self.results.len() as u64),
                    count(self.prog.ignored.load(Ordering::Relaxed))
                ));
            } else {
                ui.label(format!("{} items", count(self.results.len() as u64)));
                if let Some(h) = self.selected.and_then(|i| self.results.get(i)) {
                    ui.separator();
                    ui.label(h.path.display().to_string());
                }
            }
        });
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let title = match self.status {
            Status::Idle => "Spotter".to_string(),
            _ => {
                let roots = self.searched.roots.iter().map(|r| r.display().to_string()).collect::<Vec<_>>().join(";");
                format!("{} found - Search: {roots} ({})", count(self.results.len() as u64), self.searched_label)
            }
        };
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }
}

#[derive(Clone, Copy)]
enum Action {
    Open,
    Reveal,
    CopyPath,
    CopyName,
    CopyAll,
}

/// A small painted folder or page icon, for when the shell has no icon for a type.
fn file_icon(ui: &mut egui::Ui, is_dir: bool) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(16.0, 14.0), Sense::hover());
    let p = ui.painter();
    if is_dir {
        let fill = Color32::from_rgb(0xe8, 0xb8, 0x4a);
        let tab = egui::Rect::from_min_size(rect.min + Vec2::new(1.0, 1.5), Vec2::new(6.0, 3.0));
        p.rect_filled(tab, 1.0, fill);
        p.rect_filled(egui::Rect::from_min_max(rect.min + Vec2::new(1.0, 3.5), rect.max - Vec2::new(1.0, 1.0)), 1.5, fill);
    } else {
        let stroke = egui::Stroke::new(1.0, ui.visuals().weak_text_color());
        let r = egui::Rect::from_min_max(rect.min + Vec2::new(3.5, 0.5), rect.max - Vec2::new(3.5, 0.5));
        p.rect_filled(r, 1.0, ui.visuals().extreme_bg_color);
        p.rect_stroke(r, 1.0, stroke, egui::StrokeKind::Inside);
    }
}

/// Drop-down of `extra` (label, value) choices and earlier values for a text field.
fn history(ui: &mut egui::Ui, id: &str, value: &mut String, past: &[String], extra: &[(String, String)]) {
    ui.push_id(id, |ui| {
        ui.add_enabled_ui(!past.is_empty() || !extra.is_empty(), |ui| {
            ui.menu_button("v", |ui| {
                let mut pick = |ui: &mut egui::Ui, label: &str, v: &str| {
                    if ui.button(label).clicked() {
                        *value = v.to_string();
                        ui.close();
                    }
                };
                for (label, v) in extra {
                    pick(ui, label, v);
                }
                if !extra.is_empty() && !past.is_empty() {
                    ui.separator();
                }
                for v in past {
                    pick(ui, v, v);
                }
            })
            .response
            .on_hover_text("Recent values");
        });
    });
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if std::mem::take(&mut self.start_now) {
            self.start(&ctx);
        }
        self.poll();
        if self.job.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if ctx.input(|i| i.viewport().close_requested()) {
            self.stop();
            self.s.save();
        }
        self.keys(&ctx);
        self.update_title(&ctx);
        egui::Panel::top("form").show(ui, |ui| {
            ui.add_space(4.0);
            self.form(ui);
            ui.add_space(4.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        egui::Panel::right("side").resizable(true).default_size(440.0).min_size(260.0).show(ui, |ui| self.side_panel(ui));
        egui::CentralPanel::default().show(ui, |ui| {
            if self.results.is_empty() && self.job.is_none() {
                ui.centered_and_justified(|ui| {
                    ui.label(RichText::new(if matches!(self.status, Status::Idle) { "Enter a name or text and press Start" } else { "Nothing found" }).weak());
                });
            } else {
                self.results_table(ui);
            }
        });
    }
}
