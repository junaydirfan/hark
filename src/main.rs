//! hark: a lean terminal podcast player.
//!
//! `hark` opens the player. Two developer commands exercise the foundations on their own:
//!   hark feed <url|file>                            fetch + parse a feed, with timings
//!   hark play <file|url> [speed] [start_s] [volume] play through the in-process engine

mod app;
mod engine;
mod feed;
mod paths;
mod store;
mod stream;
mod theme;
mod ui;

use std::time::{Duration, Instant};

use engine::{Cmd, Engine, Event};

/// Hands freed heap pages back to the OS (glibc only): a 20 MB feed would otherwise stay in RSS.
pub fn trim_heap() {
    // SAFETY: malloc_trim only releases free heap memory.
    #[cfg(target_os = "linux")]
    unsafe {
        libc::malloc_trim(0);
    }
}

fn main() {
    // Every helper thread gets its own malloc arena that is never handed back; two is plenty.
    // SAFETY: called before any other threads exist.
    #[cfg(target_os = "linux")]
    unsafe {
        libc::mallopt(libc::M_ARENA_MAX, 2);
        // Keep big buffers (a 20 MB feed body) in their own mappings, returned the moment they're
        // freed. Without a fixed threshold glibc raises it after the first one and they fragment the heap.
        libc::mallopt(libc::M_MMAP_THRESHOLD, 256 * 1024);
    }
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime");
    rt.block_on(async_main());
}

async fn async_main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let res = match args.first().map(String::as_str) {
        Some("feed") if args.len() > 1 => show_feed(&args[1]).await,
        Some("play") if args.len() > 1 => play(&args[1..]).await,
        None => run().await,
        _ => Err("usage: hark | hark feed <url|file> | hark play <file|url> [speed] [start_s] [volume]".into()),
    };
    if let Err(e) = res {
        eprintln!("hark: {e}");
        std::process::exit(1);
    }
}

/// The player: one async thread for the screen and the network, input on its own thread,
/// audio on its own thread. Redraws only when something changed.
async fn run() -> Result<(), String> {
    let (eng_tx, mut eng_rx) = tokio::sync::mpsc::unbounded_channel();
    let (msg_tx, mut msg_rx) = tokio::sync::mpsc::unbounded_channel();
    let (key_tx, mut key_rx) = tokio::sync::mpsc::unbounded_channel();
    // sleeps in read() until there's input, so it costs nothing while idle
    std::thread::Builder::new()
        .name("input".into())
        .spawn(move || {
            while let Ok(ev) = crossterm::event::read() {
                if key_tx.send(ev).is_err() {
                    break;
                }
            }
        })
        .map_err(|e| e.to_string())?;
    let mut app = app::App::new(Engine::start(eng_tx), msg_tx);
    let mut term = ratatui::init();
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture);
    // ratatui's panic hook restores the terminal but not mouse reporting, which would leak into the shell
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |p| {
        let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
        hook(p)
    }));
    app.refresh(false);
    let mut dirty = true;
    while !app.quit {
        if dirty {
            let _ = term.draw(|f| ui::draw(f, &mut app));
            dirty = false;
        }
        tokio::select! {
            Some(ev) = key_rx.recv() => match ev {
                crossterm::event::Event::Key(k) => { app.on_key(k); dirty = true; }
                crossterm::event::Event::Resize(..) => dirty = true,
                crossterm::event::Event::Mouse(m) => dirty |= app.on_mouse(m),
                _ => {}
            },
            Some(ev) = eng_rx.recv() => dirty |= app.on_engine(ev),
            Some(m) = msg_rx.recv() => { app.on_msg(m); dirty = true; }
            else => break,
        }
    }
    app.shutdown();
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
    ratatui::restore();
    Ok(())
}

async fn show_feed(src: &str) -> Result<(), String> {
    let t0 = Instant::now();
    let bytes = if src.starts_with("http://") || src.starts_with("https://") {
        let resp = reqwest::Client::new().get(src).send().await.map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("{src}: HTTP {}", resp.status()));
        }
        resp.bytes().await.map_err(|e| e.to_string())?.to_vec()
    } else {
        std::fs::read(src).map_err(|e| format!("{src}: {e}"))?
    };
    let fetched = t0.elapsed();
    let t1 = Instant::now();
    let show = feed::parse(&bytes)?;
    let parsed = t1.elapsed();
    let with = |f: fn(&feed::Episode) -> bool| show.episodes.iter().filter(|e| f(e)).count();
    println!("{} — {}", show.title, show.author);
    println!(
        "{} episodes from {:.1} MB: fetch {:?}, parse {:?} | chapters {}, transcripts {}, no duration {}",
        show.episodes.len(),
        bytes.len() as f64 / 1e6,
        fetched,
        parsed,
        with(|e| e.chapters.is_some()),
        with(|e| e.transcript.is_some()),
        with(|e| e.duration_s == 0),
    );
    for e in show.episodes.iter().take(3) {
        println!("  {} | {}s | {} | {} | notes {} chars", e.published, e.duration_s, e.title, e.mime, e.notes.len());
    }
    Ok(())
}

async fn play(a: &[String]) -> Result<(), String> {
    let num = |i: usize, d: f64| a.get(i).and_then(|s| s.parse().ok()).unwrap_or(d);
    let (speed, start, volume) = (num(1, 1.0) as f32, num(2, 0.0), num(3, 1.0) as f32);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let eng = Engine::start(tx);
    eng.send(Cmd::Volume(volume));
    eng.send(Cmd::Speed(speed));
    let t0 = Instant::now();
    eng.send(Cmd::Load { src: a[0].clone(), at_s: start });
    let script = [(4.0, "seek +60"), (6.0, "pause"), (9.0, "resume"), (12.0, "end")];
    let mut step = 0;
    let mut cpu_mark = None;
    loop {
        let next = Duration::from_secs_f64(script[step].0).saturating_sub(t0.elapsed());
        match tokio::time::timeout(next, rx.recv()).await {
            Ok(Some(ev)) => {
                println!("{:>6.3}s {ev:?}", t0.elapsed().as_secs_f64());
                if matches!(ev, Event::Ended | Event::Error(_)) {
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => {
                let (pss, cpu) = usage();
                println!("{:>6.3}s -- {} (PSS {pss:.1} MB)", t0.elapsed().as_secs_f64(), script[step].1);
                match script[step].1 {
                    "seek +60" => eng.send(Cmd::Seek(start + 60.0)),
                    "pause" => {
                        eng.send(Cmd::Pause(true));
                        cpu_mark = Some(cpu);
                    }
                    "resume" => {
                        if let Some(c) = cpu_mark {
                            println!("        CPU while paused: {:.2}% of a core", (cpu - c) / 3.0 * 100.0);
                        }
                        eng.send(Cmd::Pause(false));
                        cpu_mark = Some(cpu);
                    }
                    _ => {
                        if let Some(c) = cpu_mark {
                            println!("        CPU while playing: {:.2}% of a core", (cpu - c) / 3.0 * 100.0);
                        }
                        break;
                    }
                }
                step += 1;
            }
        }
    }
    Ok(())
}

/// (PSS in MB, CPU seconds used so far) for this process; Linux only, for the dev CLI.
fn usage() -> (f64, f64) {
    let pss = std::fs::read_to_string("/proc/self/smaps_rollup").ok().and_then(|s| {
        s.lines().find(|l| l.starts_with("Pss:"))?.split_whitespace().nth(1)?.parse::<f64>().ok()
    });
    let cpu = std::fs::read_to_string("/proc/self/stat").ok().and_then(|s| {
        let f: Vec<&str> = s.rsplit(')').next()?.split_whitespace().collect();
        Some((f.get(11)?.parse::<f64>().ok()? + f.get(12)?.parse::<f64>().ok()?) / 100.0)
    });
    (pss.unwrap_or(0.0) / 1024.0, cpu.unwrap_or(0.0))
}
