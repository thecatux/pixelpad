//! Terminal rendering, built on crossterm instead of curses. Functions
//! here mirror the drawing / prompting methods of the Python `PixelPad`
//! class, but operate on a plain `&PixelPad` (or nothing, for
//! self-contained prompts) rather than being methods on the editor --
//! that's what lets the Lua plugin API call `ui::prompt` without needing
//! a reference back into the editor.

use std::io::{stdout, Write};

use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{read, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor};
use crossterm::terminal::{self, Clear, ClearType};
use crossterm::{execute, queue};

use crate::editor::PixelPad;
use crate::syntax::{self, ColorPair};

pub const PIXEL_CHAR: &str = "\u{2588}"; // "█"

/// Full-screen clear. Called once at startup and on terminal resize --
/// NOT on every frame, since a per-frame `Clear(ClearType::All)` followed
/// by a full repaint is what causes visible flicker/tearing on Windows
/// terminals. Every normal frame already overwrites every cell it touches
/// (border, padded text lines, padded status/message lines), so no clear
/// is needed between keystrokes.
pub fn clear_screen() -> std::io::Result<()> {
    let mut out = stdout();
    queue!(out, Clear(ClearType::All))?;
    out.flush()
}

/// Block for the next terminal event, transparently discarding the
/// `Release` half of Press/Release keystroke pairs. Windows' console API
/// and terminals with the "enhanced keyboard protocol" (Kitty, some
/// iTerm2/WezTerm configs) emit a `KeyEventKind::Release` event per
/// keystroke in addition to the `Press`; every blocking read in the app
/// (the main loop, `prompt`, `show_overlay`) goes through this one
/// function so a stray Release can't be misread as a real keystroke (e.g.
/// double characters, or an Enter that closes a prompt nobody typed).
pub fn read_key_event() -> std::io::Result<Event> {
    loop {
        let ev = read()?;
        if let Event::Key(key) = &ev {
            if key.kind == KeyEventKind::Release {
                continue;
            }
        }
        return Ok(ev);
    }
}

pub fn get_text_dims() -> (usize, usize) {
    let (w, h) = terminal::size().unwrap_or((80, 24));
    let text_h = (h as isize - 6).max(1) as usize;
    let text_w = (w as isize - 4).max(1) as usize;
    (text_h, text_w)
}

fn color_for(pair: ColorPair) -> Option<(Color, Attribute)> {
    match pair {
        ColorPair::Default => None,
        ColorPair::Keyword => Some((Color::Cyan, Attribute::Bold)),
        ColorPair::String => Some((Color::Green, Attribute::NoBold)),
        ColorPair::Comment => Some((Color::Blue, Attribute::Dim)),
        ColorPair::Number => Some((Color::Yellow, Attribute::NoBold)),
    }
}

pub fn render(ed: &PixelPad) -> std::io::Result<()> {
    let mut out = stdout();

    let (w, h) = terminal::size()?;
    let (w, h) = (w as usize, h as usize);

    if h < 8 || w < 20 {
        // This branch doesn't fully repaint the screen, so it's the one
        // case where we do need an explicit clear first.
        queue!(
            out,
            Clear(ClearType::All),
            MoveTo(0, 0),
            SetAttribute(Attribute::Bold),
            Print("Window too small. Increase terminal size."),
            SetAttribute(Attribute::Reset)
        )?;
        out.flush()?;
        return Ok(());
    }

    draw_border_and_title(&mut out, ed, w, h)?;

    queue!(
        out,
        MoveTo(2, 1),
        SetAttribute(Attribute::Bold),
        Print(format!("{} PIXELPAD {}", PIXEL_CHAR, PIXEL_CHAR)),
        SetAttribute(Attribute::Reset)
    )?;

    let text_top = 3usize;
    let text_left = 2usize;
    let (text_h, text_w) = get_text_dims();

    for row in 0..text_h {
        let buf_row = ed.scroll_y + row;
        queue!(out, MoveTo(text_left as u16, (text_top + row) as u16))?;
        if buf_row >= ed.lines.len() {
            queue!(out, Print(" ".repeat(text_w)))?;
        } else {
            draw_text_line(&mut out, ed, &ed.lines[buf_row], text_w)?;
        }
    }

    let modified_flag = if ed.modified { "*" } else { "" };
    let syntax_flag = if ed.syntax_enabled && ed.colors_available {
        "on"
    } else {
        "off"
    };
    let basename = std::path::Path::new(&ed.filename)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| ed.filename.clone());
    let status = format!(
        " {}{} \u{2014} Ln {}, Col {}  Syntax:{} ",
        basename,
        modified_flag,
        ed.cy + 1,
        ed.cx + 1,
        syntax_flag
    );
    let controls = "^S Save ^O Open ^F Find ^Z/^Y Undo/Redo ^T Syntax ^P Plugins ^G Help";
    let pad_len =
        (w as isize - 2 - status.chars().count() as isize - controls.chars().count() as isize)
            .max(0) as usize;
    let mut stat_line: String = format!("{}{}{}", status, " ".repeat(pad_len), controls);
    let max_w = w.saturating_sub(2);
    stat_line = truncate_chars(&stat_line, max_w);
    queue!(
        out,
        MoveTo(1, (h - 3) as u16),
        SetAttribute(Attribute::Reverse),
        Print(stat_line),
        SetAttribute(Attribute::Reset)
    )?;

    if !ed.msg.is_empty() {
        let msg = truncate_chars(&format!(" {} ", ed.msg), max_w);
        queue!(out, MoveTo(1, (h - 2) as u16), Print(msg))?;
    } else {
        queue!(out, MoveTo(1, (h - 2) as u16), Print(" ".repeat(max_w)))?;
    }

    let screen_y = text_top as isize + (ed.cy as isize - ed.scroll_y as isize);
    let screen_x = text_left as isize + (ed.cx as isize - ed.scroll_x as isize);
    if screen_y >= 0 && screen_x >= 0 {
        queue!(out, MoveTo(screen_x as u16, screen_y as u16), Show)?;
    }

    out.flush()
}

fn draw_border_and_title<W: Write>(
    out: &mut W,
    ed: &PixelPad,
    w: usize,
    h: usize,
) -> std::io::Result<()> {
    let top_bottom = PIXEL_CHAR.repeat(w);
    queue!(out, MoveTo(0, 0), Print(&top_bottom))?;
    queue!(out, MoveTo(0, (h - 1) as u16), Print(&top_bottom))?;
    for y in 0..h {
        queue!(out, MoveTo(0, y as u16), Print(PIXEL_CHAR))?;
        queue!(out, MoveTo((w - 1) as u16, y as u16), Print(PIXEL_CHAR))?;
    }

    let modified_flag = if ed.modified { "*" } else { "" };
    let basename = std::path::Path::new(&ed.filename)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| ed.filename.clone());
    let title = format!(" PIXELPAD - {}{} ", basename, modified_flag);
    let start = ((w as isize - title.chars().count() as isize) / 2).max(2) as usize;
    if start + title.chars().count() < w.saturating_sub(2) {
        queue!(
            out,
            MoveTo(start as u16, 0),
            SetAttribute(Attribute::Reverse),
            Print(&title),
            SetAttribute(Attribute::Reset)
        )?;
    }
    Ok(())
}

fn draw_text_line<W: Write>(
    out: &mut W,
    ed: &PixelPad,
    line: &[char],
    text_w: usize,
) -> std::io::Result<()> {
    let use_colors = ed.syntax_enabled && ed.colors_available;
    let start = ed.scroll_x.min(line.len());
    let end = (ed.scroll_x + text_w).min(line.len());

    // The tokenizer only scans left-to-right (it never needs characters
    // past its current position), so it's correct -- and much cheaper for
    // long lines with horizontal scroll -- to tokenize only up through
    // the visible window instead of the whole line every frame.
    let colors_prefix = if use_colors {
        syntax::get_line_colors(&line[..end])
    } else {
        vec![ColorPair::Default; end]
    };

    let visible_chars = &line[start..end];
    let visible_colors = &colors_prefix[start..end];

    // Expand tabs to 4 spaces, keeping each expanded space the same color
    // as the tab character it came from.
    let mut chars: Vec<char> = Vec::with_capacity(text_w);
    let mut chcolors: Vec<ColorPair> = Vec::with_capacity(text_w);
    for (&ch, &col) in visible_chars.iter().zip(visible_colors.iter()) {
        if ch == '\t' {
            for _ in 0..4 {
                chars.push(' ');
                chcolors.push(col);
            }
        } else {
            chars.push(ch);
            chcolors.push(col);
        }
    }

    if chars.len() < text_w {
        let pad = text_w - chars.len();
        chars.extend(std::iter::repeat(' ').take(pad));
        chcolors.extend(std::iter::repeat(ColorPair::Default).take(pad));
    } else {
        chars.truncate(text_w);
        chcolors.truncate(text_w);
    }

    if !use_colors {
        let s: String = chars.into_iter().collect();
        queue!(out, Print(s))?;
        return Ok(());
    }

    // Draw in runs of consecutive same-color characters to minimize the
    // number of writes.
    let mut run_start = 0usize;
    for i in 1..=chcolors.len() {
        if i == chcolors.len() || chcolors[i] != chcolors[run_start] {
            let run: String = chars[run_start..i].iter().collect();
            match color_for(chcolors[run_start]) {
                Some((color, attr)) => {
                    queue!(
                        out,
                        SetForegroundColor(color),
                        SetAttribute(attr),
                        Print(run),
                        SetAttribute(Attribute::Reset),
                        ResetColor
                    )?;
                }
                None => {
                    queue!(out, Print(run))?;
                }
            }
            run_start = i;
        }
    }
    Ok(())
}

fn truncate_chars(s: &str, max_w: usize) -> String {
    s.chars().take(max_w).collect()
}

// ------------------------------------------------------------- prompts --

/// Prompt the user for a line of input on the bottom status area;
/// blocking. Returns `default` if the user cancels (Esc) or submits empty
/// input, matching the Python original's `prompt(..., default=...)`.
pub fn prompt(prompt_text: &str, default: &str) -> String {
    let mut out = stdout();
    let (w, h) = terminal::size().unwrap_or((80, 24));
    let y = h.saturating_sub(2);

    let mut input = String::new();
    loop {
        let _ = queue!(out, MoveTo(1, y), Clear(ClearType::CurrentLine));
        let line = format!("{}{}", prompt_text, input);
        let max_w = (w as usize).saturating_sub(2);
        let _ = queue!(out, Print(truncate_chars(&line, max_w)), Show);
        let _ = out.flush();

        match read_key_event() {
            Ok(Event::Key(key)) => match key.code {
                KeyCode::Enter => break,
                KeyCode::Esc => {
                    input.clear();
                    break;
                }
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Char(c) => {
                    if key.modifiers.contains(KeyModifiers::CONTROL) {
                        continue;
                    }
                    input.push(c);
                }
                _ => {}
            },
            Ok(_) => {}
            Err(_) => break,
        }
    }

    if input.is_empty() {
        default.to_string()
    } else {
        input
    }
}

pub fn confirm(prompt_text: &str) -> bool {
    prompt(prompt_text, "")
        .trim()
        .to_lowercase()
        .starts_with('y')
}

// ------------------------------------------------------------ overlays --

/// Draw a centered box of lines and wait for any key press.
pub fn show_overlay(lines: &[String]) {
    let mut out = stdout();
    let (w, h) = terminal::size().unwrap_or((80, 24));
    let (w, h) = (w as usize, h as usize);

    let box_w =
        (w.saturating_sub(4)).min(lines.iter().map(|l| l.chars().count()).max().unwrap_or(20) + 4);
    let box_h = (h.saturating_sub(4)).min(lines.len() + 2);
    let y = ((h.saturating_sub(box_h)) / 2).max(1);
    let x = ((w.saturating_sub(box_w)) / 2).max(1);

    let _ = execute!(out, Hide);
    for row in 0..box_h {
        let _ = queue!(out, MoveTo(x as u16, (y + row) as u16));
        if row == 0 || row == box_h - 1 {
            let _ = queue!(
                out,
                Print("+".to_string() + &"-".repeat(box_w.saturating_sub(2)) + "+")
            );
        } else {
            let content = lines.get(row - 1).cloned().unwrap_or_default();
            let content = truncate_chars(&content, box_w.saturating_sub(4));
            let padded = format!("| {:<width$} |", content, width = box_w.saturating_sub(4));
            let _ = queue!(
                out,
                SetAttribute(Attribute::Bold),
                Print(padded),
                SetAttribute(Attribute::Reset)
            );
        }
    }
    let _ = out.flush();
    let _ = read_key_event();
    let _ = execute!(out, Show);
}

pub fn draw_help_overlay() {
    let lines: Vec<String> = vec![
        "PixelPad - Controls:".to_string(),
        "  Ctrl-S Save   Ctrl-A Save As   Ctrl-O Open   Ctrl-N New".to_string(),
        "  Ctrl-F Find   Ctrl-K Cut line  Ctrl-C Copy line  Ctrl-U Paste".to_string(),
        "  Ctrl-Z Undo   Ctrl-Y Redo      Ctrl-T Syntax  Ctrl-G Help".to_string(),
        "  Ctrl-P Run Lua plugin (plugins/ folder)   Ctrl-Q Quit".to_string(),
        "  Arrow keys : Move   Home/End : line start/end".to_string(),
        "  PageUp/PageDown : scroll   Backspace/Delete : erase".to_string(),
        "".to_string(),
        "Press any key to continue...".to_string(),
    ];
    show_overlay(&lines);
}

pub fn draw_plugin_list_overlay(ed: &PixelPad) {
    let mut lines: Vec<String> = vec!["Available plugins:".to_string(), "".to_string()];
    for (i, p) in ed.plugins.iter().enumerate() {
        let hk = p
            .hotkey
            .map(|c| format!("  [ctrl-{}]", c))
            .unwrap_or_default();
        lines.push(format!("{}. {}{}", i + 1, p.name, hk));
        if !p.description.is_empty() {
            lines.push(format!("      {}", p.description));
        }
    }
    lines.push("".to_string());
    lines.push("Type a number or name at the prompt, Enter to cancel.".to_string());
    show_overlay(&lines);
}
