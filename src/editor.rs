//! Core editor state and buffer operations. Deliberately has no knowledge
//! of the terminal (crossterm) or of Lua (mlua) -- it only manages the
//! in-memory document, matching the data half of the Python `PixelPad`
//! class.

use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub const UNDO_LIMIT: usize = 200;

/// Consecutive "typing" edits (insert/backspace/delete) that happen within
/// this window of each other share a single undo checkpoint, instead of
/// cloning the whole buffer on every keystroke. Structural edits (newline,
/// cut/paste, plugin runs) always get their own checkpoint.
const UNDO_COALESCE_MS: u64 = 700;

/// Ctrl codes already bound to a built-in editor command; a plugin-declared
/// hotkey that collides with one of these is ignored (menu access still works).
pub const RESERVED_HOTKEYS: &[char] = &[
    'a', 'c', 'f', 'g', 'k', 'n', 'o', 'p', 'q', 's', 't', 'u', 'y', 'z',
];

/// Metadata about a loaded Lua plugin, used for display / hotkey dispatch.
/// The runnable Lua state itself lives outside of `PixelPad` (see
/// `plugin::PluginRuntime`) so that running a plugin never needs to hold a
/// borrow of the editor while Lua callbacks re-borrow it.
#[derive(Clone)]
pub struct PluginMeta {
    pub file: String,
    pub name: String,
    pub description: String,
    pub hotkey: Option<char>,
}

type UndoSnapshot = (Vec<Vec<char>>, usize, usize);

pub struct PixelPad {
    pub filename: String,
    pub clipboard: String,
    pub lines: Vec<Vec<char>>,
    pub cx: usize,
    pub cy: usize,
    pub scroll_x: usize,
    pub scroll_y: usize,
    pub msg: String,
    pub msg_until: Option<Instant>,
    pub running: bool,
    pub modified: bool,
    pub last_search: String,
    pub undo_stack: VecDeque<UndoSnapshot>,
    pub redo_stack: VecDeque<UndoSnapshot>,
    last_undo_push: Option<Instant>,
    pub syntax_enabled: bool,
    pub colors_available: bool,
    pub plugins_dir: PathBuf,
    pub plugins: Vec<PluginMeta>,
}

impl PixelPad {
    pub fn new(plugins_dir: PathBuf, filename: Option<String>) -> Self {
        let mut ed = PixelPad {
            filename: filename.clone().unwrap_or_else(|| "Untitled".to_string()),
            clipboard: String::new(),
            lines: vec![Vec::new()],
            cx: 0,
            cy: 0,
            scroll_x: 0,
            scroll_y: 0,
            msg: String::new(),
            msg_until: None,
            running: true,
            modified: false,
            last_search: String::new(),
            undo_stack: VecDeque::new(),
            redo_stack: VecDeque::new(),
            last_undo_push: None,
            syntax_enabled: true,
            colors_available: false,
            plugins_dir,
            plugins: Vec::new(),
        };
        if let Some(f) = filename {
            if PathBuf::from(&f).exists() {
                ed.load_file(&f);
            }
        }
        ed
    }

    // ---------------------------------------------------------- file i/o --

    pub fn load_file(&mut self, path: &str) {
        match fs::read_to_string(path) {
            Ok(data) => {
                let lines: Vec<Vec<char>> = if data.is_empty() {
                    vec![Vec::new()]
                } else {
                    let split: Vec<Vec<char>> = data
                        .split('\n')
                        .map(|l| l.strip_suffix('\r').unwrap_or(l).chars().collect())
                        .collect();
                    if split.is_empty() {
                        vec![Vec::new()]
                    } else {
                        split
                    }
                };
                self.lines = lines;
                self.filename = path.to_string();
                self.cx = 0;
                self.cy = 0;
                self.scroll_x = 0;
                self.scroll_y = 0;
                self.modified = false;
                self.undo_stack.clear();
                self.redo_stack.clear();
                self.set_message(format!("Opened: {}", path), 3);
            }
            Err(e) => {
                self.set_message(format!("Error opening {}: {}", path, e), 3);
            }
        }
    }

    pub fn save_file(&mut self, path: Option<&str>) {
        let path = path
            .map(|p| p.to_string())
            .unwrap_or_else(|| self.filename.clone());
        let text: String = self
            .lines
            .iter()
            .map(|l| l.iter().collect::<String>())
            .collect::<Vec<String>>()
            .join("\n");
        match fs::write(&path, text) {
            Ok(()) => {
                self.filename = path.clone();
                self.modified = false;
                self.set_message(format!("Saved: {}", path), 3);
            }
            Err(e) => {
                self.set_message(format!("Error saving {}: {}", path, e), 3);
            }
        }
    }

    pub fn new_file(&mut self) {
        self.lines = vec![Vec::new()];
        self.filename = "Untitled".to_string();
        self.cx = 0;
        self.cy = 0;
        self.scroll_x = 0;
        self.scroll_y = 0;
        self.modified = false;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.set_message("New file".to_string(), 3);
    }

    // ------------------------------------------------------------- misc --

    pub fn set_message(&mut self, text: String, duration_secs: u64) {
        self.msg = text;
        self.msg_until = Some(Instant::now() + Duration::from_secs(duration_secs));
    }

    pub fn clear_message_if_expired(&mut self) {
        if !self.msg.is_empty() {
            if let Some(until) = self.msg_until {
                if Instant::now() > until {
                    self.msg.clear();
                }
            }
        }
    }

    // ------------------------------------------------------- edit stack --

    pub fn clamp_cursor(&mut self) {
        self.cy = self.cy.min(self.lines.len().saturating_sub(1));
        self.cx = self.cx.min(self.lines[self.cy].len());
    }

    /// Always create a new undo checkpoint. Used for structural edits
    /// (newline, cut/paste, plugin runs) where each action should be
    /// individually undoable.
    pub fn push_undo(&mut self) {
        self.undo_stack
            .push_back((self.lines.clone(), self.cx, self.cy));
        if self.undo_stack.len() > UNDO_LIMIT {
            self.undo_stack.pop_front();
        }
        self.redo_stack.clear();
        self.last_undo_push = Some(Instant::now());
    }

    /// Like `push_undo`, but consecutive calls within `UNDO_COALESCE_MS`
    /// of each other share a single checkpoint instead of each cloning
    /// the full buffer. This keeps a burst of ordinary typing (which
    /// calls this once per character) from being O(buffer size) per key
    /// and from bloating the undo history with a snapshot per character;
    /// undo instead steps back a whole burst at a time, similar to most
    /// editors.
    pub fn push_undo_coalesced(&mut self) {
        let now = Instant::now();
        let within_burst = matches!(
            self.last_undo_push,
            Some(t) if now.duration_since(t) <= Duration::from_millis(UNDO_COALESCE_MS)
        );
        if within_burst {
            // Reuse the existing checkpoint at the top of the stack; only
            // bump the timestamp and drop any now-stale redo history.
            self.redo_stack.clear();
            self.last_undo_push = Some(now);
        } else {
            self.push_undo();
        }
    }

    pub fn undo(&mut self) {
        if self.undo_stack.is_empty() {
            self.set_message("Nothing to undo".to_string(), 3);
            return;
        }
        self.redo_stack
            .push_back((self.lines.clone(), self.cx, self.cy));
        let (lines, cx, cy) = self.undo_stack.pop_back().unwrap();
        self.lines = lines;
        self.cx = cx;
        self.cy = cy;
        self.modified = true;
        self.clamp_cursor();
        // Force the next typing edit to open a fresh checkpoint rather
        // than coalescing into whatever was just undone/redone.
        self.last_undo_push = None;
        self.set_message("Undo".to_string(), 3);
    }

    pub fn redo(&mut self) {
        if self.redo_stack.is_empty() {
            self.set_message("Nothing to redo".to_string(), 3);
            return;
        }
        self.undo_stack
            .push_back((self.lines.clone(), self.cx, self.cy));
        let (lines, cx, cy) = self.redo_stack.pop_back().unwrap();
        self.lines = lines;
        self.cx = cx;
        self.cy = cy;
        self.modified = true;
        self.clamp_cursor();
        self.last_undo_push = None;
        self.set_message("Redo".to_string(), 3);
    }

    // ------------------------------------------------------------ edits --

    pub fn insert_char(&mut self, ch: char) {
        self.push_undo_coalesced();
        self.lines[self.cy].insert(self.cx, ch);
        self.cx += 1;
        self.modified = true;
    }

    pub fn backspace(&mut self) {
        if self.cx == 0 && self.cy == 0 {
            return;
        }
        self.push_undo_coalesced();
        if self.cx > 0 {
            self.lines[self.cy].remove(self.cx - 1);
            self.cx -= 1;
        } else {
            let cur = self.lines.remove(self.cy);
            self.cy -= 1;
            self.cx = self.lines[self.cy].len();
            self.lines[self.cy].extend(cur);
        }
        self.modified = true;
    }

    pub fn delete_char(&mut self) {
        let line_len = self.lines[self.cy].len();
        if self.cx >= line_len && self.cy >= self.lines.len() - 1 {
            return;
        }
        self.push_undo_coalesced();
        if self.cx < line_len {
            self.lines[self.cy].remove(self.cx);
        } else {
            let next = self.lines.remove(self.cy + 1);
            self.lines[self.cy].extend(next);
        }
        self.modified = true;
    }

    pub fn newline(&mut self) {
        self.push_undo();
        let right: Vec<char> = self.lines[self.cy].split_off(self.cx);
        self.lines.insert(self.cy + 1, right);
        self.cy += 1;
        self.cx = 0;
        self.modified = true;
    }

    // ------------------------------------------------- clipboard / find --

    pub fn cut_line(&mut self) {
        self.push_undo();
        self.clipboard = self.lines[self.cy].iter().collect();
        if self.lines.len() == 1 {
            self.lines[self.cy].clear();
        } else {
            self.lines.remove(self.cy);
            if self.cy >= self.lines.len() {
                self.cy = self.lines.len() - 1;
            }
        }
        self.cx = 0;
        self.modified = true;
        self.set_message("Cut line".to_string(), 3);
    }

    pub fn copy_line(&mut self) {
        self.clipboard = self.lines[self.cy].iter().collect();
        self.set_message("Copied line".to_string(), 3);
    }

    pub fn paste_line(&mut self) {
        self.push_undo();
        self.lines.insert(self.cy, self.clipboard.chars().collect());
        self.modified = true;
        self.set_message("Pasted line".to_string(), 3);
    }

    pub fn find(&mut self, term: &str) {
        if term.is_empty() {
            return;
        }
        self.last_search = term.to_string();
        let n = self.lines.len();
        let term_chars: Vec<char> = term.chars().collect();

        // First, look for the next match on the current line, after the
        // cursor.
        if let Some(idx) = find_sub(&self.lines[self.cy], &term_chars, self.cx + 1) {
            self.cx = idx;
            self.set_message(format!("Found: {}", term), 3);
            return;
        }

        // Then wrap through the other lines, in order.
        for offset in 1..n {
            let row = (self.cy + offset) % n;
            if let Some(idx) = find_sub(&self.lines[row], &term_chars, 0) {
                self.cy = row;
                self.cx = idx;
                self.set_message(format!("Found: {}", term), 3);
                return;
            }
        }

        // Finally, wrap back around to the start of the current line
        // (covers a match at or before the original cursor position).
        if let Some(idx) = find_sub(&self.lines[self.cy], &term_chars, 0) {
            self.cx = idx;
            self.set_message(format!("Found: {}", term), 3);
        } else {
            self.set_message(format!("Not found: {}", term), 3);
        }
    }

    // ------------------------------------------------------------ scroll --

    pub fn ensure_scroll(&mut self, text_h: usize, text_w: usize) {
        if self.cy < self.scroll_y {
            self.scroll_y = self.cy;
        } else if self.cy >= self.scroll_y + text_h {
            self.scroll_y = self.cy + 1 - text_h;
        }
        if self.cx < self.scroll_x {
            self.scroll_x = self.cx;
        } else if self.cx >= self.scroll_x + text_w {
            self.scroll_x = self.cx + 1 - text_w;
        }
    }
}

/// Find `needle` inside `haystack` starting at char index `from`, returning
/// the char index of the first match (like Python's str.find).
fn find_sub(haystack: &[char], needle: &[char], from: usize) -> Option<usize> {
    if needle.is_empty() || from > haystack.len() {
        return None;
    }
    if needle.len() > haystack.len() {
        return None;
    }
    for start in from..=(haystack.len() - needle.len()) {
        if haystack[start..start + needle.len()] == *needle {
            return Some(start);
        }
    }
    None
}
