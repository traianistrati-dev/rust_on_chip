//! Real pointer and key input for scripted checks of the running IDE - debug
//! builds only, and only with `EIDE_AUTOMATION=<absolute folder>` set.
//!
//! Windows UI Automation reaches the buttons AccessKit names, but not a
//! clickable label (a virtual module's row) or a shortcut (Ctrl+S), and
//! window messages posted to the IDE never become an egui click. This feeds
//! egui itself, through `App::raw_input_hook`, without touching the real
//! mouse or keyboard.
//!
//! The folder is the whole channel: a script writes `cmd.txt` - whole, by
//! renaming a finished file over it -
//!
//! ```text
//! seq 7
//! click 1940 1776
//! key ctrl+s
//! type hello
//! paste some text
//! end
//! ```
//!
//! `seq` must grow for a file to be taken, and a file without its `end` line
//! is one still being written. `click` is in the window's client area, in
//! physical pixels (what `ScreenToClient` gives); `key` is an egui key name
//! with `ctrl+` / `shift+` / `alt+` in front (`ctrl+c` / `ctrl+x` copy and
//! cut, as egui-winit sends them); `type` and `paste` take the rest of the
//! line as it is. The IDE plays one event a frame - a click is a move, a
//! press and a release, so egui sees it as a click - and, a frame after the
//! last, writes the `seq` to `done.txt`, with the reason when the file could
//! not be read (then none of it is played). The real mouse moving over the
//! window while a click plays can still turn it into a drag.
//!
//! A start removes what an earlier run left in the folder, so an old
//! `cmd.txt` is never played against a project opened since.

use eframe::egui;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Duration;

/// How often the folder is looked at while nothing is being played.
const POLL: Duration = Duration::from_millis(100);

pub(super) struct Automation {
    dir: PathBuf,
    seq: u64,
    queue: VecDeque<Step>,
    /// The queue was played out: `done.txt` is written next frame, once egui
    /// has run a pass over the last event.
    finishing: bool,
    /// While a scripted shortcut is down: the modifiers to give back with its
    /// release - the real keyboard's, which egui's own state no longer shows.
    restore: Option<egui::Modifiers>,
    /// The last `cmd.txt` played or refused - read again, it is no news.
    handled: String,
}

#[derive(Clone, Debug, PartialEq)]
enum Step {
    /// The pointer to this client pixel.
    Move(egui::Pos2),
    /// The primary button down or up where the pointer is.
    Button(bool),
    Key(egui::Key, egui::Modifiers, bool),
    /// A whole event of its own: text, Copy, Cut, Paste.
    Event(egui::Event),
}

impl Automation {
    /// The channel `EIDE_AUTOMATION` names; never in a release build, and
    /// never for an empty or relative path - one that would land wherever
    /// the IDE was started.
    pub(super) fn from_env() -> Option<Self> {
        Self::at(PathBuf::from(std::env::var_os("EIDE_AUTOMATION")?))
    }

    fn at(dir: PathBuf) -> Option<Self> {
        if !cfg!(debug_assertions) || !dir.is_absolute() {
            return None;
        }
        std::fs::create_dir_all(&dir).ok()?;
        for stale in ["cmd.txt", "done.txt"] {
            let _ = std::fs::remove_file(dir.join(stale));
        }
        Some(Self {
            dir,
            seq: 0,
            queue: VecDeque::new(),
            finishing: false,
            restore: None,
            handled: String::new(),
        })
    }

    /// Takes a new `cmd.txt`, and plays the next step into `raw`.
    pub(super) fn hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        // A window minimized or covered runs the hook but no pass (eframe's
        // logic-only frame): nothing would see a step played now.
        let hidden = raw
            .viewports
            .get(&raw.viewport_id)
            .and_then(|v| v.visible())
            == Some(false);
        if hidden {
            return;
        }
        // The real keyboard's modifiers, when they changed this frame.
        let real_now = raw.events.iter().rev().find_map(|e| match e {
            egui::Event::ModifiersChanged(m) => Some(*m),
            _ => None,
        });
        if let (Some(r), Some(m)) = (&mut self.restore, real_now) {
            *r = m;
        }
        if self.finishing {
            self.finishing = false;
            self.report("");
        }
        if self.queue.is_empty() {
            self.take_commands();
        }
        let Some(step) = self.queue.pop_front() else {
            return;
        };
        let ppp = ctx.pixels_per_point();
        let (pointer, egui_now) = ctx.input(|i| (i.pointer.latest_pos(), i.modifiers));
        let real = real_now.unwrap_or(egui_now);
        let held = match &step {
            Step::Key(_, m, true) if !m.is_none() => *self.restore.insert(real),
            Step::Key(_, m, false) if !m.is_none() => self.restore.take().unwrap_or(real),
            _ => self.restore.unwrap_or(real),
        };
        raw.events
            .extend(events(step, ppp, pointer.unwrap_or_default(), held));
        self.finishing = self.queue.is_empty();
    }

    /// Keeps passes coming: the next step at once, a new `cmd.txt` within
    /// [`POLL`]. Called from `App::logic` - a repaint asked for in the input
    /// hook, before the pass begins, is dropped when it does.
    pub(super) fn keep_polling(&self, ctx: &egui::Context) {
        ctx.request_repaint_after(if self.queue.is_empty() && !self.finishing {
            POLL
        } else {
            Duration::ZERO
        });
    }

    fn take_commands(&mut self) {
        let Ok(text) = std::fs::read_to_string(self.dir.join("cmd.txt")) else {
            return;
        };
        // The file last played or refused stays where it is until the next:
        // looked at again, it is no news.
        if text == self.handled {
            return;
        }
        let Some((seq, steps)) = parse(&text) else {
            return;
        };
        self.handled = text;
        // A whole file that cannot be played is said, so the script waiting
        // on it stops: no `seq` to take, or one taken already.
        if seq == 0 || seq <= self.seq {
            let why = match steps {
                Err(e) if seq == 0 => e,
                _ => format!("seq {seq} is not above {}, the last taken", self.seq),
            };
            let _ = std::fs::write(self.dir.join("done.txt"), format!("{seq}\n{why}"));
            return;
        }
        self.seq = seq;
        match steps {
            Ok(steps) if steps.is_empty() => self.report(""),
            Ok(steps) => self.queue.extend(steps),
            Err(e) => self.report(&e),
        }
    }

    /// `done.txt`: the last `seq` finished, and what went wrong with it.
    fn report(&self, error: &str) {
        let _ = std::fs::write(self.dir.join("done.txt"), format!("{}\n{error}", self.seq));
    }
}

/// `(seq, its steps or the first line that could not be read)`, or `None`
/// for a file that is not whole yet - no `end` last. A whole file whose first
/// line is not `seq N` (N above 0) comes back as seq 0 with the reason.
fn parse(text: &str) -> Option<(u64, Result<Vec<Step>, String>)> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines: Vec<&str> = text
        .lines()
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .filter(|l| !l.trim().is_empty())
        .collect();
    if lines.last().map(|l| l.trim()) != Some("end") {
        return None;
    }
    lines.pop();
    let Some(seq) = lines
        .first()
        .and_then(|l| l.trim().strip_prefix("seq "))
        .and_then(|n| n.trim().parse::<u64>().ok())
        .filter(|&n| n > 0)
    else {
        return Some((
            0,
            Err("the first line is not `seq N` (N above 0)".to_owned()),
        ));
    };
    let mut steps = Vec::new();
    for line in &lines[1..] {
        let line = line.trim_start();
        let (cmd, rest) = line.split_once(' ').unwrap_or((line, ""));
        match cmd {
            "click" => {
                let mut xy = rest.split_whitespace().map(str::parse::<f32>);
                let (Some(Ok(x)), Some(Ok(y)), None) = (xy.next(), xy.next(), xy.next()) else {
                    return Some((seq, Err(format!("click wants two numbers: {line}"))));
                };
                if !(x.is_finite() && y.is_finite()) {
                    return Some((seq, Err(format!("click wants finite numbers: {line}"))));
                }
                let at = egui::pos2(x, y);
                steps.extend([Step::Move(at), Step::Button(true), Step::Button(false)]);
            }
            "key" => match key(rest.trim()) {
                Some((k, m)) => match clipboard(k, m) {
                    Some(Clip::Copy) => steps.push(Step::Event(egui::Event::Copy)),
                    Some(Clip::Cut) => steps.push(Step::Event(egui::Event::Cut)),
                    Some(Clip::Paste) => {
                        return Some((
                            seq,
                            Err(format!("a paste has no text - use paste: {line}")),
                        ));
                    }
                    None => steps.extend([Step::Key(k, m, true), Step::Key(k, m, false)]),
                },
                None => return Some((seq, Err(format!("unknown key: {line}")))),
            },
            "type" => steps.push(Step::Event(egui::Event::Text(rest.to_owned()))),
            "paste" => steps.push(Step::Event(egui::Event::Paste(rest.to_owned()))),
            _ => return Some((seq, Err(format!("unknown command: {line}")))),
        }
    }
    Some((seq, Ok(steps)))
}

enum Clip {
    Copy,
    Cut,
    Paste,
}

/// The keys egui-winit turns into a clipboard event instead of a key one
/// (its `is_copy_command` and the others): Ctrl with C / X / V whatever else
/// is held, and Ctrl+Insert, Shift+Delete, Shift+Insert.
fn clipboard(k: egui::Key, m: egui::Modifiers) -> Option<Clip> {
    use egui::Key;
    match k {
        Key::C if m.command => Some(Clip::Copy),
        Key::X if m.command => Some(Clip::Cut),
        Key::V if m.command => Some(Clip::Paste),
        Key::Insert if m.command => Some(Clip::Copy),
        Key::Delete if m.shift => Some(Clip::Cut),
        Key::Insert if m.shift => Some(Clip::Paste),
        _ => None,
    }
}

/// `ctrl+shift+s` → (S, Ctrl+Shift). Ctrl is also the command key: that is
/// what egui reports for Ctrl on Windows and Linux.
fn key(spec: &str) -> Option<(egui::Key, egui::Modifiers)> {
    let mut mods = egui::Modifiers::NONE;
    let mut parts: Vec<&str> = spec.split('+').map(str::trim).collect();
    let name = parts.pop()?;
    for m in parts {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" => {
                mods.ctrl = true;
                mods.command = true;
            }
            "shift" => mods.shift = true,
            "alt" => mods.alt = true,
            _ => return None,
        }
    }
    // egui names keys `Insert`, `F5`, `A`: tried as written, capitalized,
    // and in capitals.
    let mut chars = name.chars();
    let capital: String = chars
        .next()
        .map(|c| {
            c.to_uppercase()
                .chain(chars.flat_map(char::to_lowercase))
                .collect()
        })
        .unwrap_or_default();
    let k = [name.to_owned(), capital, name.to_uppercase()]
        .iter()
        .find_map(|n| egui::Key::from_name(n))?;
    Some((k, mods))
}

/// The egui events of one step; `pointer` is where the pointer is now, in
/// points, `held` the modifiers the real keyboard holds. A key with
/// modifiers holds them in egui's own state too, for the pass of the press -
/// shortcuts read `i.modifiers` as often as the event's - and gives the real
/// ones back with the release.
fn events(step: Step, ppp: f32, pointer: egui::Pos2, held: egui::Modifiers) -> Vec<egui::Event> {
    match step {
        Step::Move(px) => vec![egui::Event::PointerMoved(px / ppp)],
        Step::Button(pressed) => vec![egui::Event::PointerButton {
            pos: pointer,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: held,
        }],
        Step::Key(key, modifiers, pressed) => {
            let key = egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers,
            };
            if modifiers.is_none() {
                vec![key]
            } else if pressed {
                vec![egui::Event::ModifiersChanged(modifiers), key]
            } else {
                vec![key, egui::Event::ModifiersChanged(held)]
            }
        }
        Step::Event(e) => vec![e],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTRL: egui::Modifiers = egui::Modifiers {
        alt: false,
        ctrl: true,
        shift: false,
        mac_cmd: false,
        command: true,
    };

    #[test]
    fn reads_a_command_file() {
        let (seq, steps) = parse(
            "\u{feff}seq 3\r\nclick 10 20\r\n\r\nkey ctrl+s\r\ntype  two spaces \r\n\
             key ctrl+c\r\npaste x\r\nend\r\n",
        )
        .unwrap();
        assert_eq!(seq, 3);
        assert_eq!(
            steps.unwrap(),
            [
                Step::Move(egui::pos2(10.0, 20.0)),
                Step::Button(true),
                Step::Button(false),
                Step::Key(egui::Key::S, CTRL, true),
                Step::Key(egui::Key::S, CTRL, false),
                Step::Event(egui::Event::Text(" two spaces ".into())),
                Step::Event(egui::Event::Copy),
                Step::Event(egui::Event::Paste("x".into())),
            ]
        );
    }

    #[test]
    fn waits_for_a_whole_file_and_refuses_what_it_cannot_read() {
        assert_eq!(parse(""), None, "still being written");
        assert_eq!(parse("seq 7\nclick 1940 17"), None, "no end: cut mid-write");
        for no_seq in ["click 1 2\nend", "seq 0\nend", "seq x\nend"] {
            let (seq, steps) = parse(no_seq).expect("whole");
            assert!(seq == 0 && steps.is_err(), "{no_seq}");
        }
        for bad in [
            "click 1",
            "click a b",
            "click NaN 2",
            "click 1e40 2",
            "key hyper+s",
            "key nokey",
            "key ctrl+v",
            "wave",
        ] {
            let (_, steps) = parse(&format!("seq 1\n{bad}\nend")).unwrap();
            assert!(steps.is_err(), "{bad}");
        }
        assert_eq!(key("F5").map(|(k, _)| k), Some(egui::Key::F5));
        assert_eq!(key("Escape").map(|(k, _)| k), Some(egui::Key::Escape));
    }

    /// The clipboard keys play as egui-winit sends them: an event of their
    /// own, whatever else is held; a paste needs its text.
    #[test]
    fn clipboard_keys_play_as_egui_winit_sends_them() {
        let one = |k: &str| parse(&format!("seq 1\nkey {k}\nend")).unwrap().1;
        for (k, want) in [
            ("ctrl+c", egui::Event::Copy),
            ("ctrl+shift+c", egui::Event::Copy),
            ("ctrl+insert", egui::Event::Copy),
            ("ctrl+x", egui::Event::Cut),
            ("shift+delete", egui::Event::Cut),
        ] {
            assert_eq!(one(k).unwrap(), [Step::Event(want)], "{k}");
        }
        for k in ["ctrl+v", "ctrl+shift+v", "shift+insert"] {
            assert!(one(k).is_err(), "{k}");
        }
        assert_eq!(one("delete").unwrap().len(), 2, "a plain Delete is a key");
    }

    /// A click lands where it was aimed, in points, and the press and the
    /// release follow the pointer there; a shortcut holds its modifiers in
    /// egui's state for the press and gives the real ones back.
    #[test]
    fn events_are_aimed_and_held_as_egui_reads_them() {
        let none = egui::Modifiers::NONE;
        let ev = events(
            Step::Move(egui::pos2(300.0, 150.0)),
            1.5,
            egui::Pos2::ZERO,
            none,
        );
        assert_eq!(ev, [egui::Event::PointerMoved(egui::pos2(200.0, 100.0))]);
        let at = egui::pos2(200.0, 100.0);
        let ev = events(Step::Button(true), 1.5, at, none);
        assert!(
            matches!(ev[0], egui::Event::PointerButton { pos, pressed: true, .. } if pos == at)
        );

        let down = events(Step::Key(egui::Key::S, CTRL, true), 1.0, at, none);
        assert_eq!(down[0], egui::Event::ModifiersChanged(CTRL));
        let up = events(Step::Key(egui::Key::S, CTRL, false), 1.0, at, none);
        assert_eq!(up.last(), Some(&egui::Event::ModifiersChanged(none)));
    }

    /// Through real egui passes: a scripted Ctrl+S holds Ctrl only for
    /// itself - the click after it carries no modifier, and egui's state is
    /// clean again.
    #[cfg(debug_assertions)]
    #[test]
    fn a_shortcut_gives_the_modifiers_back() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = Automation::at(dir.path().to_owned()).unwrap();
        std::fs::write(
            dir.path().join("cmd.txt"),
            "seq 1\nkey ctrl+s\nclick 10 10\nend\n",
        )
        .unwrap();
        let ctx = egui::Context::default();
        let mut played = Vec::new();
        for _ in 0..6 {
            let mut raw = egui::RawInput::default();
            a.hook(&ctx, &mut raw);
            played.extend(raw.events.clone());
            let _ = crate::headless::run_ui(&ctx, raw, |_| {});
        }
        let click = played
            .iter()
            .find_map(|e| match e {
                egui::Event::PointerButton {
                    pressed: true,
                    modifiers,
                    ..
                } => Some(*modifiers),
                _ => None,
            })
            .expect("the click was played");
        assert!(click.is_none(), "{click:?}");
        assert!(ctx.input(|i| i.modifiers.is_none()));
    }

    /// The channel end to end: a start clears an earlier run's files, a file
    /// is played once and reported a frame after its last event, a file that
    /// cannot be read is reported and not played, a reused seq is said once,
    /// a relative folder is refused.
    #[cfg(debug_assertions)]
    #[test]
    fn plays_each_file_once_and_reports_it() {
        assert!(Automation::at(PathBuf::from("auto")).is_none(), "relative");
        let dir = tempfile::tempdir().unwrap();
        let cmd = dir.path().join("cmd.txt");
        let done = || std::fs::read_to_string(dir.path().join("done.txt")).unwrap_or_default();
        std::fs::write(&cmd, "seq 5\nkey F5\nend\n").unwrap();
        std::fs::write(dir.path().join("done.txt"), "5\n").unwrap();
        let mut a = Automation::at(dir.path().to_owned()).expect("a debug test build");
        assert!(!cmd.exists() && done().is_empty(), "an earlier run's files");

        let ctx = egui::Context::default();
        let frame = |a: &mut Automation| {
            let mut raw = egui::RawInput::default();
            a.hook(&ctx, &mut raw);
            raw.events
        };
        std::fs::write(&cmd, "seq 5\nkey F5\nend\n").unwrap();
        assert_eq!(frame(&mut a).len(), 1, "the press");
        assert_eq!(frame(&mut a).len(), 1, "the release");
        assert_eq!(done(), "", "reported a frame after the last event");
        assert!(frame(&mut a).is_empty());
        assert_eq!(done(), "5\n");
        assert!(frame(&mut a).is_empty(), "seq 5 is not played twice");

        std::fs::write(&cmd, "seq 6\nwave\nend\n").unwrap();
        assert!(frame(&mut a).is_empty());
        assert_eq!(done(), "6\nunknown command: wave");

        std::fs::write(&cmd, "seq 6\nkey F5\nend\n").unwrap();
        assert!(frame(&mut a).is_empty(), "seq 6 again: not played");
        assert_eq!(done(), "6\nseq 6 is not above 6, the last taken");
    }
}
