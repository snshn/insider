//! insider — a realtime TUI hardware inspector that draws your mainboard.

mod board;
mod color;
mod json;
mod model;
mod platform;
mod probe;
mod probe_macos;
mod probe_windows;
mod ui;

use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, MouseButton, MouseEventKind,
};
use ratatui::crossterm::execute;
use std::io;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

const HELP: &str = "\
insider — realtime TUI hardware inspector

USAGE: insider [OPTIONS]

OPTIONS:
  -b, --board <FILE>          use a layout file (for authoring layouts)
  -l, --list-boards           list known board layouts and exit
  -d, --dump                  print everything detected (plain text) and exit
  -e, --exploded              start in exploded view
  -i, --interval <MS>         refresh interval in milliseconds (default 1000)
      --color <truecolor|256> force colour depth (default: detect;
                              also settable via INSIDER_COLOR)
      --no-nvidia-smi         never run nvidia-smi
  -h, --help                  show this help
  -V, --version               show version
";

fn help() -> String {
    let dirs: Vec<String> = board::board_dirs()
        .iter()
        .map(|d| format!("  {}", d.display()))
        .collect();
    format!(
        "{HELP}\nLayouts (*.toml) are read from:\n{}\n\n{}\n",
        dirs.join("\n"),
        platform::elevate_hint()
    )
}

struct Args {
    board: Option<String>,
    list: bool,
    dump: bool,
    exploded: bool,
    interval: u64,
    nvidia: bool,
    color: Option<color::Depth>,
}

fn parse_args() -> Args {
    let mut a = Args {
        board: None,
        list: false,
        dump: false,
        exploded: false,
        interval: 1000,
        nvidia: true,
        color: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-b" | "--board" => a.board = it.next(),
            "-l" | "--list-boards" => a.list = true,
            "-d" | "--dump" => a.dump = true,
            "-e" | "--exploded" => a.exploded = true,
            "-i" | "--interval" => {
                a.interval = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(1000)
                    .max(200)
            }
            "--no-nvidia-smi" => a.nvidia = false,
            "--color" | "--colour" => {
                let v = it.next().unwrap_or_default();
                match color::parse_depth(&v) {
                    Some(d) => a.color = Some(d),
                    None => {
                        eprintln!("insider: --color takes \"truecolor\" or \"256\", not {v:?}");
                        std::process::exit(2)
                    }
                }
            }
            "-h" | "--help" => {
                print!("{}", help());
                std::process::exit(0)
            }
            "-V" | "--version" => {
                println!("insider {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0)
            }
            other => {
                eprintln!("insider: unknown option {other:?}\n\n{}", help());
                std::process::exit(2)
            }
        }
    }
    a
}

fn main() -> io::Result<()> {
    let args = parse_args();
    let (mut boards, load_errors) = board::load_all();
    for e in &load_errors {
        eprintln!("insider: layout error: {e}");
    }
    if boards.is_empty() {
        eprintln!("insider: no usable board layouts");
        std::process::exit(1);
    }
    if args.list {
        for b in &boards {
            println!("{:<20} {:<36} {}", b.id, b.name, b.source);
        }
        println!("\nsearch path:");
        for d in board::board_dirs() {
            println!("  {}", d.display());
        }
        return Ok(());
    }

    let mut prober = platform::Prober::new(platform::Options {
        nvidia_smi: args.nvidia,
    });
    let first = prober.gather();
    let (idx, reason) = match board::choose(&mut boards, &first, args.board.as_deref()) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("insider: {e}");
            std::process::exit(1)
        }
    };

    if args.dump {
        prober.settle(); // wait for slow, background-collected details
        thread::sleep(Duration::from_millis(700)); // second sample, for rates
        let snap = prober.gather();
        dump(&snap, &boards[idx], &reason);
        return Ok(());
    }

    let (tx, rx) = mpsc::channel();
    let interval = Duration::from_millis(args.interval);
    thread::spawn(move || loop {
        thread::sleep(interval);
        if tx.send(prober.gather()).is_err() {
            break;
        }
    });

    let mut app = ui::App::new(boards, idx, load_errors, first, args.exploded);
    let depth = args.color.unwrap_or_else(color::detect);
    let mut terminal = ratatui::init();
    let _ = execute!(io::stdout(), EnableMouseCapture);
    let res = run(&mut terminal, &mut app, &rx, depth);
    let _ = execute!(io::stdout(), DisableMouseCapture);
    ratatui::restore();
    res
}

fn run(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut ui::App,
    rx: &Receiver<model::Snapshot>,
    depth: color::Depth,
) -> io::Result<()> {
    loop {
        while let Ok(s) = rx.try_recv() {
            app.set_snapshot(s);
        }
        terminal.draw(|f| {
            ui::draw_frame(f, app);
            if depth == color::Depth::Ansi256 {
                color::downsample(f.buffer_mut());
            }
        })?;
        if event::poll(Duration::from_millis(60))? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => app.on_key(k.code, k.modifiers),
                Event::Mouse(m) => match m.kind {
                    MouseEventKind::Down(MouseButton::Left) => app.on_click(m.column, m.row),
                    MouseEventKind::ScrollDown => app.on_scroll(true),
                    MouseEventKind::ScrollUp => app.on_scroll(false),
                    _ => {}
                },
                _ => {}
            }
        }
        if app.quit {
            return Ok(());
        }
    }
}

fn dump(snap: &model::Snapshot, b: &board::BoardDef, reason: &str) {
    println!(
        "layout: {} [{}, {}] ({}){} — {}",
        b.name,
        b.form,
        b.id,
        b.source,
        if b.generic { " generic stand-in" } else { "" },
        reason
    );
    let mut dmi: Vec<_> = snap.dmi.iter().collect();
    dmi.sort();
    for (k, v) in dmi {
        println!("dmi.{k} = {v}");
    }
    let asg = board::assign(b, &snap.parts);
    println!();
    for (si, s) in b.slots.iter().enumerate() {
        let ids: Vec<&str> = asg.slots[si]
            .iter()
            .map(|&i| snap.parts[i].id.as_str())
            .collect();
        println!(
            "slot {:<8} {:<13} -> {}",
            s.id,
            s.kind().name(),
            if ids.is_empty() {
                "-".into()
            } else {
                ids.join(", ")
            }
        );
    }
    if !asg.unplaced.is_empty() {
        let ids: Vec<&str> = asg
            .unplaced
            .iter()
            .map(|&i| snap.parts[i].id.as_str())
            .collect();
        println!("unplaced: {}", ids.join(", "));
    }
    for p in &snap.parts {
        println!(
            "\n[{}] {}  ({}){}",
            p.kind.name(),
            p.title,
            p.id,
            if p.present { "" } else { "  [not present]" }
        );
        if let Some(t) = p.temp {
            println!("    temp: {}", model::fmt_temp(t));
        }
        if !p.metrics.is_empty() {
            println!("    {}", p.metrics.join(" · "));
        }
        if !p.bind.is_empty() {
            println!("    bind keys: {}", p.bind.join(", "));
        }
        for (k, v) in &p.props {
            println!("    {k}: {v}");
        }
    }
    if !snap.notes.is_empty() {
        println!("\nnotes:");
        for n in &snap.notes {
            println!("  • {n}");
        }
    }
}
