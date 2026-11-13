//! Interactive terminal UI. Behaviour comes from the engine; this file only
//! handles input, layout and terminal lifecycle.

use std::collections::VecDeque;
use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use nvprofile_engine::lact_config::{self, LactGpuConfig};
use nvprofile_engine::*;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Gauge, List, ListItem, ListState, Paragraph, Sparkline, Wrap};
use ratatui::{DefaultTerminal, Frame};

use crate::ctx::{run_privileged, settle, Ctx};

const HISTORY: usize = 120;
const POWER_STEP: f64 = 5.0;
const FAN_STEP: f64 = 0.01;

type R = Result<()>;

#[derive(Clone)]
enum Mode {
    Normal,
    ConfirmApply(Preset),
    ConfirmDelete(String),
    Edit(Edit),
    NameInput(Edit, String),
}

#[derive(Clone)]
struct Edit {
    preset: Preset,
    /// 0 = power cap, 1 = fan
    field: usize,
}

struct Msg {
    text: String,
    ok: bool,
}

struct App<'a> {
    ctx: &'a Ctx,
    backend: LactBackend,
    info: GpuInfo,
    limits: GpuLimits,
    path: ApplyPath,
    presets: Vec<Preset>,
    problems: usize,
    list: ListState,
    stats: Stats,
    applied: LactGpuConfig,
    active: Option<ActiveState>,
    temps: VecDeque<u64>,
    powers: VecDeque<u64>,
    fans: VecDeque<u64>,
    mode: Mode,
    msg: Msg,
    link_error: Option<String>,
}

pub fn run(ctx: &Ctx) -> R {
    let backend = ctx.backend()?;
    let mut app = App::new(ctx, backend)?;
    let mut term = ratatui::init();
    let res = event_loop(&mut term, &mut app);
    ratatui::restore();
    res.map_err(|e| EngineError::Io { path: "terminal".into(), message: e.to_string() })
}

impl<'a> App<'a> {
    fn new(ctx: &'a Ctx, backend: LactBackend) -> Result<Self> {
        let catalog = ctx.store.load_all()?;
        let mut app = Self {
            ctx,
            info: backend.gpu_info()?,
            limits: backend.limits()?,
            path: backend.apply_path(),
            backend,
            presets: catalog.presets,
            problems: catalog.problems.len(),
            list: ListState::default().with_selected(Some(0)),
            stats: Stats::default(),
            applied: LactGpuConfig::default(),
            active: None,
            temps: VecDeque::new(),
            powers: VecDeque::new(),
            fans: VecDeque::new(),
            mode: Mode::Normal,
            msg: Msg { text: "↑↓ select · Enter apply · r reset · e edit copy · d delete · q quit".into(), ok: true },
            link_error: None,
        };
        app.refresh();
        // Start on the active preset when there is one.
        if let Some(i) = app.active.as_ref().and_then(|a| app.presets.iter().position(|p| Some(&p.id) == a.preset_id.as_ref())) {
            app.list.select(Some(i));
        }
        Ok(app)
    }

    fn selected(&self) -> Option<&Preset> {
        self.list.selected().and_then(|i| self.presets.get(i))
    }

    fn say(&mut self, ok: bool, text: impl Into<String>) {
        self.msg = Msg { text: text.into(), ok };
    }

    fn reload_presets(&mut self, keep: Option<&str>) {
        match self.ctx.store.load_all() {
            Ok(c) => {
                self.problems = c.problems.len();
                self.presets = c.presets;
            }
            Err(e) => self.say(false, e.to_string()),
        }
        let idx = keep.and_then(|id| self.presets.iter().position(|p| p.id == id)).unwrap_or(0);
        self.list.select(Some(idx.min(self.presets.len().saturating_sub(1))));
    }

    fn refresh(&mut self) {
        match (self.backend.stats(), self.backend.applied()) {
            (Ok(s), Ok(a)) => {
                push(&mut self.temps, s.temp_c);
                push(&mut self.powers, s.power_draw_w);
                push(&mut self.fans, s.fan_pct);
                self.stats = s;
                self.active = detect_active(&a, &self.limits, &self.presets).ok();
                self.applied = a;
                self.link_error = None;
            }
            (Err(e), _) | (_, Err(e)) => self.link_error = Some(e.to_string()),
        }
    }

    fn apply(&mut self, term: &mut DefaultTerminal, preset: &Preset) -> io::Result<()> {
        match apply_preset(&self.backend, &self.ctx.dirs, preset, false) {
            Ok(ApplyResult { outcome, .. }) => match outcome {
                ApplyOutcome::NoChange => self.say(true, format!("'{}' is already applied", preset.id)),
                ApplyOutcome::DryRun => {}
                ApplyOutcome::Applied { verified, notes, .. } => {
                    let extra = if notes.is_empty() { String::new() } else { format!(" ({})", notes.join("; ")) };
                    self.say(verified, format!("applied '{}'{}{extra}", preset.id, if verified { ", verified" } else { ", NOT verified" }));
                }
                ApplyOutcome::PrivilegeRequired { write, .. } => {
                    let res = suspended(term, &format!("install config for '{}'", preset.id), &write.command.join(" "), || run_privileged(&write))?;
                    match res {
                        Ok(()) => {
                            settle();
                            self.say(true, format!("installed config for '{}'; lactd reloads it automatically", preset.id));
                        }
                        Err(e) => self.say(false, format!("privileged install failed: {e}")),
                    }
                }
            },
            Err(e) => self.say(false, format!("apply '{}' failed: {e}", preset.id)),
        }
        self.refresh();
        Ok(())
    }
}

fn push(q: &mut VecDeque<u64>, v: Option<f64>) {
    q.push_back(v.unwrap_or(0.0).max(0.0).round() as u64);
    if q.len() > HISTORY {
        q.pop_front();
    }
}

/// Leave the TUI, run an action that may prompt on the tty, then come back.
fn suspended<T>(term: &mut DefaultTerminal, label: &str, cmd: &str, action: impl FnOnce() -> io::Result<T>) -> io::Result<io::Result<T>> {
    ratatui::restore();
    println!("\n-- nvprofile: {label} --\n$ {cmd}\n");
    let out = action();
    match &out {
        Ok(_) => println!("\ndone. Press Enter to return to nvprofile."),
        Err(e) => println!("\nfailed: {e}\nPress Enter to return to nvprofile."),
    }
    io::stdin().read_line(&mut String::new())?;
    *term = ratatui::init();
    Ok(out)
}

fn event_loop(term: &mut DefaultTerminal, app: &mut App) -> io::Result<()> {
    let mut last = Instant::now();
    loop {
        if last.elapsed() >= Duration::from_secs(1) {
            app.refresh();
            last = Instant::now();
        }
        term.draw(|f| draw(f, app))?;
        if !event::poll(Duration::from_millis(200))? {
            continue;
        }
        let Event::Key(key) = event::read()? else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Ok(());
        }
        let mode = std::mem::replace(&mut app.mode, Mode::Normal);
        match mode {
            Mode::Normal => {
                if handle_normal(app, key.code) {
                    return Ok(());
                }
            }
            Mode::ConfirmApply(p) => {
                if matches!(key.code, KeyCode::Char('y' | 'Y')) {
                    app.apply(term, &p)?;
                } else {
                    app.say(true, "cancelled");
                }
            }
            Mode::ConfirmDelete(id) => {
                if matches!(key.code, KeyCode::Char('y' | 'Y')) {
                    match app.ctx.store.delete(&id) {
                        Ok(()) => {
                            app.say(true, format!("deleted '{id}'"));
                            app.reload_presets(None);
                            app.refresh();
                        }
                        Err(e) => app.say(false, e.to_string()),
                    }
                } else {
                    app.say(true, "cancelled");
                }
            }
            Mode::Edit(e) => handle_edit(app, e, key.code),
            Mode::NameInput(e, name) => handle_name(app, e, name, key.code),
        }
    }
}

fn handle_normal(app: &mut App, code: KeyCode) -> bool {
    let n = app.presets.len();
    let cur = app.list.selected().unwrap_or(0);
    match code {
        KeyCode::Char('q') | KeyCode::Esc => return true,
        KeyCode::Up | KeyCode::Char('k') => app.list.select(Some(cur.saturating_sub(1))),
        KeyCode::Down | KeyCode::Char('j') => app.list.select(Some((cur + 1).min(n.saturating_sub(1)))),
        KeyCode::Enter => {
            if let Some(p) = app.selected().cloned() {
                app.mode = Mode::ConfirmApply(p);
            }
        }
        KeyCode::Char('r') => {
            if let Some(stock) = app.presets.iter().find(|p| p.id == "stock").cloned() {
                app.mode = Mode::ConfirmApply(stock);
            }
        }
        KeyCode::Char('e') => {
            if let Some(p) = app.selected() {
                let mut copy = p.clone();
                copy.builtin = false;
                copy.name = format!("{} (edited)", p.name);
                copy.id = String::new();
                app.mode = Mode::Edit(Edit { preset: copy, field: 0 });
                app.say(true, "editing a copy: ↑↓ field · ←→ adjust · s save as user preset · Esc cancel");
            }
        }
        KeyCode::Char('d') => match app.selected() {
            Some(p) if p.builtin => app.say(false, format!("'{}' is built-in and cannot be deleted", p.id)),
            Some(p) => app.mode = Mode::ConfirmDelete(p.id.clone()),
            None => {}
        },
        _ => {}
    }
    false
}

fn handle_edit(app: &mut App, mut e: Edit, code: KeyCode) {
    let lim = app.limits.clone();
    match code {
        KeyCode::Esc => {
            app.say(true, "edit cancelled");
            return;
        }
        KeyCode::Up | KeyCode::Down | KeyCode::Char('k' | 'j') => e.field = 1 - e.field,
        KeyCode::Left | KeyCode::Char('h') => adjust(&mut e.preset, e.field, -1.0, &lim),
        KeyCode::Right | KeyCode::Char('l') => adjust(&mut e.preset, e.field, 1.0, &lim),
        KeyCode::Char('s') => {
            let name = e.preset.name.clone();
            app.mode = Mode::NameInput(e, name);
            app.say(true, "name the preset, Enter to save, Esc to go back");
            return;
        }
        _ => {}
    }
    app.mode = Mode::Edit(e);
}

fn adjust(p: &mut Preset, field: usize, dir: f64, lim: &GpuLimits) {
    if field == 0 {
        let base = p.power_cap_w.unwrap_or(lim.power_default_w);
        p.power_cap_w = Some((base + dir * POWER_STEP).clamp(lim.power_min_w, lim.power_max_w));
        return;
    }
    let step = dir * FAN_STEP;
    let floor = (lim.fan_min * 100.0).ceil() / 100.0;
    p.fan = match std::mem::replace(&mut p.fan, FanSpec::Auto) {
        FanSpec::Auto if dir > 0.0 => FanSpec::Static { speed: floor },
        FanSpec::Auto => FanSpec::Auto,
        FanSpec::Static { speed } => {
            let s = ((speed + step) * 100.0).round() / 100.0;
            if s < floor {
                FanSpec::Auto
            } else {
                FanSpec::Static { speed: s.min(1.0) }
            }
        }
        FanSpec::Curve { mut points, zero_rpm_below_c, hysteresis_c, spindown_delay_ms } => {
            for pt in &mut points {
                pt.speed = (((pt.speed + step) * 100.0).round() / 100.0).clamp(floor, 1.0);
            }
            FanSpec::Curve { points, zero_rpm_below_c, hysteresis_c, spindown_delay_ms }
        }
    };
}

fn handle_name(app: &mut App, mut e: Edit, mut name: String, code: KeyCode) {
    match code {
        KeyCode::Esc => {
            app.mode = Mode::Edit(e);
            return;
        }
        KeyCode::Backspace => {
            name.pop();
        }
        KeyCode::Char(c) if name.chars().count() < 40 => name.push(c),
        KeyCode::Enter => {
            e.preset.id = slugify(&name);
            e.preset.name = name.trim().to_owned();
            match app.ctx.store.save(&e.preset, &app.limits, false) {
                Ok(path) => {
                    let id = e.preset.id.clone();
                    app.say(true, format!("saved '{id}' to {}", path.display()));
                    app.reload_presets(Some(&id));
                    app.refresh();
                }
                Err(err) => {
                    app.say(false, err.to_string());
                    app.mode = Mode::NameInput(e, name);
                }
            }
            return;
        }
        _ => {}
    }
    app.mode = Mode::NameInput(e, name);
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

fn ratio(v: f64, max: f64) -> f64 {
    if max <= 0.0 {
        0.0
    } else {
        (v / max).clamp(0.0, 1.0)
    }
}

fn gauge(f: &mut Frame, area: Rect, title: &str, r: f64, label: String, color: Color) {
    f.render_widget(
        Gauge::default()
            .block(Block::bordered().title(title.to_owned()))
            .gauge_style(Style::new().fg(color))
            .ratio(r)
            .label(label),
        area,
    );
}

fn draw(f: &mut Frame, app: &mut App) {
    let [head, body, details, foot] =
        Layout::vertical([Constraint::Length(3), Constraint::Min(9), Constraint::Length(8), Constraint::Length(4)]).areas(f.area());

    let path = match app.path {
        ApplyPath::DaemonSocket => "LACT socket",
        ApplyPath::PrivilegedInstall => "sudo install",
    };
    let mut head_spans = vec![
        Span::styled(app.info.name.clone(), Style::new().add_modifier(Modifier::BOLD)),
        Span::raw(format!("   driver {}   apply via {path}", app.info.driver.as_deref().unwrap_or("?"))),
    ];
    if app.problems > 0 {
        head_spans.push(Span::styled(format!("   {} broken user preset file(s)", app.problems), Style::new().fg(Color::Yellow)));
    }
    if let Some(e) = &app.link_error {
        head_spans.push(Span::styled(format!("   LINK ERROR: {e}"), Style::new().fg(Color::Red)));
    }
    f.render_widget(Paragraph::new(Line::from(head_spans)).block(Block::bordered().title(" nvprofile ")), head);

    let [left, right] = Layout::horizontal([Constraint::Length(34), Constraint::Min(30)]).areas(body);
    draw_list(f, left, app);
    draw_live(f, right, app);
    draw_details(f, details, app);
    draw_footer(f, foot, app);
}

fn draw_list(f: &mut Frame, area: Rect, app: &mut App) {
    let active = app.active.as_ref().and_then(|a| a.preset_id.clone());
    let mut items: Vec<ListItem> = app
        .presets
        .iter()
        .map(|p| {
            let on = active.as_deref() == Some(p.id.as_str());
            let mut spans = vec![
                Span::styled(if on { "● " } else { "  " }, Style::new().fg(Color::Green)),
                Span::styled(p.name.clone(), if on { Style::new().fg(Color::Green).add_modifier(Modifier::BOLD) } else { Style::new() }),
            ];
            if !p.builtin {
                spans.push(Span::styled(" [user]", Style::new().fg(Color::DarkGray)));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    if app.active.as_ref().is_some_and(|a| a.preset_id.is_none()) {
        items.push(ListItem::new(Line::from(Span::styled("  applied config: custom", Style::new().fg(Color::Yellow)))));
    }
    let list = List::new(items)
        .block(Block::bordered().title(" presets "))
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .highlight_symbol("");
    f.render_stateful_widget(list, area, &mut app.list);
}

fn draw_live(f: &mut Frame, area: Rect, app: &App) {
    let [grid, load, spark] = Layout::vertical([Constraint::Length(6), Constraint::Length(3), Constraint::Min(0)]).areas(area);
    let [row1, row2] = Layout::vertical([Constraint::Length(3), Constraint::Length(3)]).areas(grid);
    let [g_temp, g_pow] = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(row1);
    let [g_fan, g_clk] = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(row2);
    let s = &app.stats;

    let temp = s.temp_c.unwrap_or(0.0);
    let tcolor = if temp < 70.0 { Color::Green } else if temp < 83.0 { Color::Yellow } else { Color::Red };
    gauge(f, g_temp, " temp ", ratio(temp, 100.0), format!("{temp:.0} °C{}", s.hotspot_c.map_or(String::new(), |h| format!(" (hotspot {h:.0})"))), tcolor);

    let cap = s.power_cap_w.unwrap_or(app.limits.power_default_w);
    let draw_w = s.power_draw_w.unwrap_or(0.0);
    gauge(f, g_pow, " power vs cap ", ratio(draw_w, cap), format!("{draw_w:.0} / {cap:.0} W"), Color::Cyan);

    gauge(
        f,
        g_fan,
        " fan ",
        ratio(s.fan_pct.unwrap_or(0.0), 100.0),
        format!("{:.0} %  {}", s.fan_pct.unwrap_or(0.0), s.fan_rpm.map_or(String::new(), |r| format!("{r} rpm"))),
        Color::Blue,
    );
    let clk = f64::from(s.gpu_clock_mhz.unwrap_or(0));
    gauge(f, g_clk, " core clock ", ratio(clk, f64::from(app.limits.clock_max_mhz)), format!("{clk:.0} / {} MHz", app.limits.clock_max_mhz), Color::Magenta);

    let throttle = if s.throttle.is_empty() { Span::styled("none", Style::new().fg(Color::Green)) } else { Span::styled(s.throttle.join(", "), Style::new().fg(Color::Yellow)) };
    let load_line = Line::from(vec![
        Span::raw(format!(
            " util {} %  ·  vram {}/{} MB  ·  {} mV  ·  throttle: ",
            s.util_pct.map_or("?".into(), |v| v.to_string()),
            s.vram_used_mb.map_or("?".into(), |v| v.to_string()),
            s.vram_total_mb.map_or("?".into(), |v| v.to_string()),
            s.voltage_mv.map_or("?".into(), |v| v.to_string()),
        )),
        throttle,
    ]);
    f.render_widget(Paragraph::new(load_line).block(Block::bordered().title(" load ")), load);

    if spark.height >= 3 {
        let [a, b, c] = Layout::horizontal([Constraint::Ratio(1, 3); 3]).areas(spark);
        let spk = |f: &mut Frame, r: Rect, title: &str, d: &VecDeque<u64>, max: u64, color: Color| {
            let data: Vec<u64> = d.iter().copied().collect();
            f.render_widget(Sparkline::default().block(Block::bordered().title(title.to_owned())).data(&data).max(max).style(Style::new().fg(color)), r);
        };
        spk(f, a, " temp 0-100 °C ", &app.temps, 100, Color::Yellow);
        spk(f, b, &format!(" power 0-{:.0} W ", app.limits.power_default_w), &app.powers, app.limits.power_default_w as u64, Color::Cyan);
        spk(f, c, " fan 0-100 % ", &app.fans, 100, Color::Blue);
    }
}

fn draw_details(f: &mut Frame, area: Rect, app: &App) {
    let dim = Style::new().fg(Color::DarkGray);
    let (title, lines): (String, Vec<Line>) = match &app.mode {
        Mode::Edit(e) | Mode::NameInput(e, _) => {
            let row = |sel: bool, label: &str, value: String| {
                let st = if sel { Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD) } else { Style::new() };
                Line::from(vec![Span::styled(format!(" {label:<10}"), st), Span::styled(format!(" ◀ {value} ▶"), st)])
            };
            let mut l = vec![
                row(e.field == 0, "power cap", power_summary(e.preset.power_cap_w)),
                row(e.field == 1, "fan", e.preset.fan.summary()),
                Line::from(Span::styled(format!(" v/f        {}", e.preset.vf.summary()), dim)),
            ];
            let issues = validate::issues(&Preset { id: "draft".into(), ..e.preset.clone() }, &app.limits);
            for i in issues.iter().take(2) {
                l.push(Line::from(Span::styled(format!(" ! {i}"), Style::new().fg(Color::Red))));
            }
            (" editing a copy ".into(), l)
        }
        _ => match app.selected() {
            Some(p) => {
                let active = app.active.as_ref().and_then(|a| a.preset_id.as_deref()) == Some(p.id.as_str());
                let mut l = vec![Line::from(vec![
                    Span::styled(p.name.clone(), Style::new().add_modifier(Modifier::BOLD)),
                    Span::styled(if active { "  ● ACTIVE" } else { "" }, Style::new().fg(Color::Green)),
                ])];
                l.push(Line::from(format!(" power {}   fan {}   v/f {}", power_summary(p.power_cap_w), p.fan.summary(), p.vf.summary())));
                match lact_config::render(p, &app.limits) {
                    Ok(target) => {
                        let ch = app.applied.changes_to(&target, app.limits.power_default_w);
                        if ch.is_empty() {
                            l.push(Line::from(Span::styled(" matches the applied config", dim)));
                        } else {
                            let fields = ch.iter().map(|c| c.field.as_str()).collect::<Vec<_>>().join(", ");
                            l.push(Line::from(Span::styled(format!(" would change: {fields}"), dim)));
                        }
                    }
                    Err(e) => l.push(Line::from(Span::styled(format!(" cannot render: {e}"), Style::new().fg(Color::Red)))),
                }
                for i in validate::issues(p, &app.limits).iter().take(2) {
                    l.push(Line::from(Span::styled(format!(" ! {i}"), Style::new().fg(Color::Red))));
                }
                l.push(Line::from(Span::styled(format!(" {}", p.description), dim)));
                (format!(" {} ", p.id), l)
            }
            None => (" details ".into(), vec![]),
        },
    };
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(Block::bordered().title(title)), area);
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let (help, line) = match &app.mode {
        Mode::Normal => (
            " ↑↓ select · Enter apply · r reset to stock · e edit a copy · d delete user preset · q quit".to_owned(),
            Line::from(Span::styled(format!(" {}", app.msg.text), Style::new().fg(if app.msg.ok { Color::Cyan } else { Color::Red }))),
        ),
        Mode::ConfirmApply(p) => (
            " y confirm · any other key cancels".to_owned(),
            Line::from(Span::styled(
                format!(" Apply '{}' ({}, {})? [y/N]", p.name, p.power_cap_w.map_or("driver default power".into(), |w| format!("{w:.0} W")), p.fan.summary()),
                Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            )),
        ),
        Mode::ConfirmDelete(id) => (
            " y confirm · any other key cancels".to_owned(),
            Line::from(Span::styled(format!(" Delete user preset '{id}'? [y/N]"), Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD))),
        ),
        Mode::Edit(_) => (
            " ↑↓ field · ←→ adjust (power ±5 W, fan ±1%) · s save as user preset · Esc cancel".to_owned(),
            Line::from(Span::styled(format!(" {}", app.msg.text), Style::new().fg(if app.msg.ok { Color::Cyan } else { Color::Red }))),
        ),
        Mode::NameInput(_, name) => (
            " type a name · Enter save · Backspace · Esc back".to_owned(),
            Line::from(vec![Span::styled(" name: ", Style::new().fg(Color::Yellow)), Span::raw(format!("{name}▏")), Span::styled(format!("   {}", if app.msg.ok { "" } else { &app.msg.text }), Style::new().fg(Color::Red))]),
        ),
    };
    f.render_widget(Paragraph::new(vec![Line::from(Span::styled(help, Style::new().fg(Color::DarkGray))), line]).block(Block::bordered()), area);
}
