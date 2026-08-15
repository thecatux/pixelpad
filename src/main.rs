//! PixelPad - A tiny pixel-styled terminal text editor.
//! Rust port of the original Python (curses) implementation.
//!
//! Usage:
//!     pixelpad [optional_filename]
//!
//! Controls:
//!     Ctrl-S : Save
//!     Ctrl-A : Save As
//!     Ctrl-O : Open (prompt)
//!     Ctrl-N : New file
//!     Ctrl-F : Find (search forward, wraps around)
//!     Ctrl-K : Cut current line (into clipboard)
//!     Ctrl-C : Copy current line (into clipboard)
//!     Ctrl-U : Paste clipboard (as line above cursor)
//!     Ctrl-Z : Undo
//!     Ctrl-Y : Redo
//!     Ctrl-T : Toggle syntax highlighting
//!     Ctrl-P : Run a Lua plugin (from the plugins/ folder next to this binary)
//!     Ctrl-G : Help
//!     Ctrl-Q : Quit (asks to confirm if there are unsaved changes)
//!     Arrow keys : Move cursor
//!     Home / End : line start/end
//!     PageUp / PageDown : scroll
//!     Backspace / Delete : remove text

mod editor;
mod plugin;
mod syntax;
mod ui;

use std::cell::RefCell;
use std::io::stdout;
use std::rc::Rc;

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{execute, ExecutableCommand};

use editor::PixelPad;
use plugin::PluginRuntime;

fn main() {
    let filename = std::env::args().nth(1);

    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let plugins_dir = exe_dir.join("plugins");

    let mut out = stdout();
    if enable_raw_mode().is_err() {
        eprintln!("This terminal does not support raw mode.");
        std::process::exit(1);
    }

    // If anything panics from here on, the terminal is left in raw mode
    // inside the alternate screen -- unusable until the user blindly runs
    // `reset`. Restore it first, then let the default hook print the
    // panic message normally.
    let default_panic_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
        default_panic_hook(info);
    }));

    let _ = execute!(out, EnterAlternateScreen, crossterm::cursor::Show);
    let _ = ui::clear_screen();

    let editor = Rc::new(RefCell::new(PixelPad::new(plugins_dir, filename)));
    editor.borrow_mut().colors_available = std::env::var("NO_COLOR").is_err();

    let runtimes = plugin::load_plugins(&editor);
    editor
        .borrow_mut()
        .set_message("Welcome to PixelPad! Ctrl-G for help".to_string(), 4);

    run(&editor, &runtimes);

    let _ = execute!(out, LeaveAlternateScreen);
    let _ = out.execute(crossterm::cursor::Show);
    let _ = disable_raw_mode();
}

fn run(editor: &Rc<RefCell<PixelPad>>, runtimes: &[PluginRuntime]) {
    loop {
        {
            let mut ed = editor.borrow_mut();
            ed.clear_message_if_expired();
        }
        if ui::render(&editor.borrow()).is_err() {
            break;
        }
        if !editor.borrow().running {
            break;
        }

        match ui::read_key_event() {
            Ok(event) => handle_event(editor, runtimes, event),
            Err(_) => break,
        }

        if !editor.borrow().running {
            break;
        }
    }
}

fn handle_event(editor: &Rc<RefCell<PixelPad>>, runtimes: &[PluginRuntime], event: Event) {
    match event {
        Event::Resize(_, _) => {
            let _ = ui::clear_screen();
            let (text_h, text_w) = ui::get_text_dims();
            let mut ed = editor.borrow_mut();
            ed.clamp_cursor();
            ed.ensure_scroll(text_h, text_w);
        }
        // Only act on key-down (Press/Repeat). Windows' console API and
        // terminals with the "enhanced keyboard protocol" (Kitty, some
        // iTerm2/WezTerm configs) also emit a KeyEventKind::Release event
        // per keystroke; without this guard every character gets handled
        // twice (once on press, once on release).
        Event::Key(key) if key.kind != KeyEventKind::Release => {
            handle_key(editor, runtimes, key.code, key.modifiers)
        }
        _ => {}
    }
}

fn handle_key(
    editor: &Rc<RefCell<PixelPad>>,
    runtimes: &[PluginRuntime],
    code: KeyCode,
    modifiers: KeyModifiers,
) {
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);

    if ctrl {
        if let KeyCode::Char(c) = code {
            match c.to_ascii_lowercase() {
                'q' => {
                    let modified = editor.borrow().modified;
                    if modified && !ui::confirm("Unsaved changes! Quit anyway? (y/n): ") {
                        editor
                            .borrow_mut()
                            .set_message("Quit canceled".to_string(), 3);
                        return;
                    }
                    editor.borrow_mut().running = false;
                    return;
                }
                's' => {
                    let filename = editor.borrow().filename.clone();
                    if filename == "Untitled" {
                        let path = ui::prompt("Save as: ", "");
                        if !path.is_empty() {
                            editor.borrow_mut().save_file(Some(&path));
                        }
                    } else {
                        editor.borrow_mut().save_file(None);
                    }
                    return;
                }
                'a' => {
                    let filename = editor.borrow().filename.clone();
                    let path = ui::prompt("Save as: ", &filename);
                    if !path.is_empty() {
                        editor.borrow_mut().save_file(Some(&path));
                    }
                    return;
                }
                'o' => {
                    let modified = editor.borrow().modified;
                    if modified && !ui::confirm("Unsaved changes! Open anyway? (y/n): ") {
                        editor
                            .borrow_mut()
                            .set_message("Open canceled".to_string(), 3);
                        return;
                    }
                    let path = ui::prompt("Open file: ", "");
                    if !path.is_empty() {
                        editor.borrow_mut().load_file(&path);
                    }
                    return;
                }
                'n' => {
                    let modified = editor.borrow().modified;
                    if modified && !ui::confirm("Discard unsaved changes? (y/n): ") {
                        editor
                            .borrow_mut()
                            .set_message("New file canceled".to_string(), 3);
                        return;
                    }
                    editor.borrow_mut().new_file();
                    return;
                }
                'g' => {
                    ui::draw_help_overlay();
                    return;
                }
                'f' => {
                    let last = editor.borrow().last_search.clone();
                    let term = ui::prompt("Find: ", &last);
                    editor.borrow_mut().find(&term);
                    return;
                }
                'k' => {
                    let (text_h, text_w) = ui::get_text_dims();
                    let mut ed = editor.borrow_mut();
                    ed.cut_line();
                    ed.clamp_cursor();
                    ed.ensure_scroll(text_h, text_w);
                    return;
                }
                'c' => {
                    editor.borrow_mut().copy_line();
                    return;
                }
                'u' => {
                    let (text_h, text_w) = ui::get_text_dims();
                    let mut ed = editor.borrow_mut();
                    ed.paste_line();
                    ed.clamp_cursor();
                    ed.ensure_scroll(text_h, text_w);
                    return;
                }
                'z' => {
                    let (text_h, text_w) = ui::get_text_dims();
                    let mut ed = editor.borrow_mut();
                    ed.undo();
                    ed.ensure_scroll(text_h, text_w);
                    return;
                }
                'y' => {
                    let (text_h, text_w) = ui::get_text_dims();
                    let mut ed = editor.borrow_mut();
                    ed.redo();
                    ed.ensure_scroll(text_h, text_w);
                    return;
                }
                't' => {
                    let mut ed = editor.borrow_mut();
                    if !ed.colors_available {
                        ed.set_message("Terminal has no color support".to_string(), 3);
                    } else {
                        ed.syntax_enabled = !ed.syntax_enabled;
                        let on = ed.syntax_enabled;
                        ed.set_message(
                            format!("Syntax highlighting {}", if on { "on" } else { "off" }),
                            3,
                        );
                    }
                    return;
                }
                'p' => {
                    open_plugin_menu(editor, runtimes);
                    return;
                }
                _ => {}
            }
        }

        // Plugin-declared hotkeys.
        if let KeyCode::Char(c) = code {
            let c = c.to_ascii_lowercase();
            let idx = editor
                .borrow()
                .plugins
                .iter()
                .position(|p| p.hotkey == Some(c));
            if let Some(idx) = idx {
                let name = editor.borrow().plugins[idx].name.clone();
                if plugin::run_plugin(editor, &runtimes[idx]) {
                    editor
                        .borrow_mut()
                        .set_message(format!("Ran plugin: {}", name), 3);
                }
                return;
            }
        }
        return;
    }

    // Navigation and editing (no modifier).
    let (text_h, text_w) = ui::get_text_dims();
    let mut ed = editor.borrow_mut();
    match code {
        KeyCode::Left => {
            if ed.cx > 0 {
                ed.cx -= 1;
            } else if ed.cy > 0 {
                ed.cy -= 1;
                ed.cx = ed.lines[ed.cy].len();
            }
        }
        KeyCode::Right => {
            if ed.cx < ed.lines[ed.cy].len() {
                ed.cx += 1;
            } else if ed.cy < ed.lines.len() - 1 {
                ed.cy += 1;
                ed.cx = 0;
            }
        }
        KeyCode::Up => {
            if ed.cy > 0 {
                ed.cy -= 1;
                ed.cx = ed.cx.min(ed.lines[ed.cy].len());
            }
        }
        KeyCode::Down => {
            if ed.cy < ed.lines.len() - 1 {
                ed.cy += 1;
                ed.cx = ed.cx.min(ed.lines[ed.cy].len());
            }
        }
        KeyCode::Home => {
            ed.cx = 0;
        }
        KeyCode::End => {
            ed.cx = ed.lines[ed.cy].len();
        }
        KeyCode::PageDown => {
            let step = text_h.max(1);
            ed.cy = (ed.cy + step).min(ed.lines.len() - 1);
            ed.cx = ed.cx.min(ed.lines[ed.cy].len());
        }
        KeyCode::PageUp => {
            let step = text_h.max(1);
            ed.cy = ed.cy.saturating_sub(step);
            ed.cx = ed.cx.min(ed.lines[ed.cy].len());
        }
        KeyCode::Backspace => ed.backspace(),
        KeyCode::Delete => ed.delete_char(),
        KeyCode::Enter => ed.newline(),
        KeyCode::Tab => ed.insert_char('\t'),
        KeyCode::Char(c) => ed.insert_char(c),
        _ => {}
    }

    ed.clamp_cursor();
    ed.ensure_scroll(text_h, text_w);
}

fn open_plugin_menu(editor: &Rc<RefCell<PixelPad>>, runtimes: &[PluginRuntime]) {
    let empty = editor.borrow().plugins.is_empty();
    if empty {
        let dir = editor.borrow().plugins_dir.display().to_string();
        editor
            .borrow_mut()
            .set_message(format!("No plugins found in {}", dir), 3);
        return;
    }

    ui::draw_plugin_list_overlay(&editor.borrow());
    let choice = ui::prompt("Run plugin (number or name): ", "")
        .trim()
        .to_string();
    if choice.is_empty() {
        editor.borrow_mut().set_message("Canceled".to_string(), 3);
        return;
    }

    let idx = if let Ok(n) = choice.parse::<usize>() {
        if n >= 1 && n <= runtimes.len() {
            Some(n - 1)
        } else {
            None
        }
    } else {
        let choice_l = choice.to_lowercase();
        editor
            .borrow()
            .plugins
            .iter()
            .position(|p| p.name.to_lowercase().contains(&choice_l))
    };

    match idx {
        Some(i) => {
            let name = editor.borrow().plugins[i].name.clone();
            if plugin::run_plugin(editor, &runtimes[i]) {
                editor
                    .borrow_mut()
                    .set_message(format!("Ran plugin: {}", name), 3);
            }
        }
        None => {
            editor
                .borrow_mut()
                .set_message(format!("Plugin not found: {}", choice), 3);
        }
    }
}
