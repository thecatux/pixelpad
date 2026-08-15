//! Lua plugin system.
//!
//! Each `.lua` file in the `plugins/` folder next to the executable gets
//! its own sandboxed `mlua::Lua` runtime. Unlike the Python version (which
//! used `lupa` to reflect live Python objects into Lua and therefore had
//! to explicitly block underscore/dunder attribute access to avoid a
//! sandbox escape), `mlua`'s `UserData` mechanism only ever exposes
//! methods we explicitly register below -- there is no generic attribute
//! reflection, so that whole class of escape doesn't exist here. We still
//! strip the dangerous bits of the Lua standard library itself (io, most
//! of os, dofile/loadfile/load/require) so a plugin can't touch the
//! filesystem, spawn processes, or load arbitrary bytecode.

use std::cell::RefCell;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use mlua::{HookTriggers, Lua, RegistryKey, Table, UserData, UserDataMethods, Value};

/// Hard wall-clock limit on a single plugin invocation. Protects against a
/// runaway plugin (an infinite loop, accidental or malicious) hanging the
/// whole editor -- there's no way to Ctrl-C out since the terminal is in
/// raw mode. Checked every `PLUGIN_HOOK_INSTRUCTIONS` VM instructions
/// rather than on every one, to keep normal plugins fast.
const PLUGIN_TIME_LIMIT: Duration = Duration::from_secs(3);
const PLUGIN_HOOK_INSTRUCTIONS: u32 = 10_000;

use crate::editor::{PixelPad, PluginMeta, RESERVED_HOTKEYS};
use crate::ui;

/// The `pixelpad` object exposed to Lua plugins. Only these methods are
/// reachable from plugin code -- plugins never get a direct reference to
/// the PixelPad instance, the terminal, or the filesystem. Buffer lines
/// are 1-indexed on the Lua side, matching Lua convention.
struct PluginApi {
    editor: Rc<RefCell<PixelPad>>,
}

impl UserData for PluginApi {
    fn add_methods<'lua, M: UserDataMethods<'lua, Self>>(methods: &mut M) {
        methods.add_method("get_line_count", |_, this, ()| {
            Ok(this.editor.borrow().lines.len() as i64)
        });

        methods.add_method("get_line", |_, this, i: i64| {
            let ed = this.editor.borrow();
            let idx = i.saturating_sub(1);
            if idx >= 0 && (idx as usize) < ed.lines.len() {
                Ok(ed.lines[idx as usize].iter().collect::<String>())
            } else {
                Ok(String::new())
            }
        });

        methods.add_method("set_line", |_, this, (i, text): (i64, Option<String>)| {
            let mut ed = this.editor.borrow_mut();
            let idx = i.saturating_sub(1);
            if idx >= 0 && (idx as usize) < ed.lines.len() {
                ed.lines[idx as usize] = text.unwrap_or_default().chars().collect();
            }
            Ok(())
        });

        methods.add_method(
            "insert_line",
            |_, this, (i, text): (i64, Option<String>)| {
                let mut ed = this.editor.borrow_mut();
                let len = ed.lines.len() as i64;
                let idx = i.saturating_sub(1).clamp(0, len) as usize;
                ed.lines
                    .insert(idx, text.unwrap_or_default().chars().collect());
                Ok(())
            },
        );

        methods.add_method("remove_line", |_, this, i: i64| {
            let mut ed = this.editor.borrow_mut();
            let idx = i.saturating_sub(1);
            if idx >= 0 && (idx as usize) < ed.lines.len() && ed.lines.len() > 1 {
                ed.lines.remove(idx as usize);
            }
            Ok(())
        });

        // Return every buffer line as a 1-indexed Lua table (a copy).
        methods.add_method("get_lines", |lua, this, ()| {
            let ed = this.editor.borrow();
            let table = lua.create_table()?;
            for (i, line) in ed.lines.iter().enumerate() {
                table.set(i + 1, line.iter().collect::<String>())?;
            }
            Ok(table)
        });

        // Replace the whole buffer from a 1-indexed Lua table of strings.
        methods.add_method("set_lines", |_, this, table: Option<Table>| {
            let mut lines: Vec<Vec<char>> = Vec::new();
            if let Some(t) = table {
                let n = t.raw_len();
                for i in 1..=n {
                    let val: Option<String> = t.get(i)?;
                    lines.push(val.unwrap_or_default().chars().collect());
                }
            }
            let mut ed = this.editor.borrow_mut();
            ed.lines = if lines.is_empty() {
                vec![Vec::new()]
            } else {
                lines
            };
            Ok(())
        });

        // Returns row, col -- both 1-indexed.
        methods.add_method("get_cursor", |_, this, ()| {
            let ed = this.editor.borrow();
            Ok((ed.cy as i64 + 1, ed.cx as i64 + 1))
        });

        methods.add_method("set_cursor", |_, this, (row, col): (i64, i64)| {
            let mut ed = this.editor.borrow_mut();
            let max_row = ed.lines.len() as i64 - 1;
            ed.cy = row.saturating_sub(1).clamp(0, max_row) as usize;
            let max_col = ed.lines[ed.cy].len() as i64;
            ed.cx = col.saturating_sub(1).clamp(0, max_col) as usize;
            Ok(())
        });

        // Insert text at the current cursor position; '\n' starts a new line.
        methods.add_method("insert_text", |_, this, text: String| {
            let mut ed = this.editor.borrow_mut();
            for ch in text.chars() {
                if ch == '\n' {
                    ed.newline();
                } else {
                    ed.insert_char(ch);
                }
            }
            Ok(())
        });

        methods.add_method("filename", |_, this, ()| {
            Ok(this.editor.borrow().filename.clone())
        });

        methods.add_method("filetype", |_, this, ()| {
            let ed = this.editor.borrow();
            let ext = PathBuf::from(&ed.filename)
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            Ok(ext)
        });

        methods.add_method("message", |_, this, text: String| {
            this.editor.borrow_mut().set_message(text, 3);
            Ok(())
        });

        // Ask the user for a line of input via the status bar; blocking.
        methods.add_method("prompt", |_, _this, text: String| Ok(ui::prompt(&text, "")));
    }
}

/// A loaded plugin's runnable state: its own Lua VM plus a registry handle
/// to `plugin.run`. Kept separate from `PixelPad::plugins` (which only
/// holds display metadata) so that invoking a plugin never requires
/// holding a borrow of the editor across the call -- Lua callbacks
/// re-borrow the editor themselves via `PluginApi`.
pub struct PluginRuntime {
    pub name: String,
    pub lua: Lua,
    pub run_key: RegistryKey,
}

/// Strip filesystem / process / shell access from a fresh Lua runtime
/// before any plugin code runs in it. Plugins can still use string/table/
/// math and safe bits of os (os.date, os.time, os.clock) -- just nothing
/// that touches disk, the environment, or spawns processes.
fn sandbox_lua_runtime(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();
    globals.set("io", Value::Nil)?;
    if let Ok(os_table) = globals.get::<_, Table>("os") {
        for danger in [
            "execute", "remove", "rename", "tmpname", "exit", "getenv", "setenv",
        ] {
            let _ = os_table.set(danger, Value::Nil);
        }
    }
    for danger in [
        "dofile",
        "loadfile",
        "load",
        "loadstring",
        "require",
        "collectgarbage",
    ] {
        let _ = globals.set(danger, Value::Nil);
    }
    Ok(())
}

/// Turn a 'ctrl-x' string from a plugin's metadata into a hotkey char, or
/// None if it's missing/malformed.
fn parse_hotkey(hotkey: &str) -> Option<char> {
    let h = hotkey.trim().to_lowercase();
    if h.starts_with("ctrl-") && h.len() == 6 {
        let c = h.chars().nth(5)?;
        if c.is_ascii_alphabetic() {
            return Some(c);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hotkey_valid() {
        assert_eq!(parse_hotkey("ctrl-x"), Some('x'));
        assert_eq!(parse_hotkey("Ctrl-X"), Some('x')); // case-insensitive
        assert_eq!(parse_hotkey("  ctrl-r  "), Some('r')); // trims whitespace
    }

    #[test]
    fn test_parse_hotkey_invalid() {
        assert_eq!(parse_hotkey("ctrl-1"), None); // not alphabetic
        assert_eq!(parse_hotkey("ctrl-"), None); // missing char
        assert_eq!(parse_hotkey("ctrl-xy"), None); // too long
        assert_eq!(parse_hotkey("alt-x"), None); // wrong prefix
        assert_eq!(parse_hotkey(""), None);
    }

    #[test]
    fn test_reserved_hotkeys_cover_all_bound_ctrl_letters() {
        // Every letter bound to a built-in Ctrl-<letter> command in
        // main.rs must appear in RESERVED_HOTKEYS, or a plugin could
        // silently steal a built-in shortcut.
        let bound: &[char] = &[
            'q', 's', 'a', 'o', 'n', 'g', 'f', 'k', 'c', 'u', 'z', 'y', 't', 'p',
        ];
        for c in bound {
            assert!(
                RESERVED_HOTKEYS.contains(c),
                "built-in shortcut ctrl-{} is missing from RESERVED_HOTKEYS",
                c
            );
        }
    }
}

/// Scan `editor.plugins_dir` for `*.lua` files and load each into its own
/// sandboxed Lua runtime. A broken plugin only disables itself; it never
/// prevents the editor from starting. Populates `editor.plugins` (display
/// metadata) and returns the runnable `PluginRuntime`s, index-aligned with
/// `editor.plugins`.
pub fn load_plugins(editor: &Rc<RefCell<PixelPad>>) -> Vec<PluginRuntime> {
    let plugins_dir = editor.borrow().plugins_dir.clone();
    let mut metas: Vec<PluginMeta> = Vec::new();
    let mut runtimes: Vec<PluginRuntime> = Vec::new();

    let Ok(entries) = fs::read_dir(&plugins_dir) else {
        return runtimes;
    };

    let mut fnames: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|f| f.to_lowercase().ends_with(".lua"))
        .collect();
    fnames.sort();

    let mut loaded = 0usize;
    for fname in fnames {
        let path = plugins_dir.join(&fname);
        let load_result = (|| -> mlua::Result<PluginRuntime> {
            let lua = Lua::new();
            sandbox_lua_runtime(&lua)?;
            let api = PluginApi {
                editor: Rc::clone(editor),
            };
            lua.globals().set("pixelpad", api)?;

            let code = fs::read_to_string(&path)
                .map_err(|e| mlua::Error::RuntimeError(format!("{}", e)))?;
            lua.load(&code).exec()?;

            let (name, description, raw_hotkey, run_key) = {
                let plugin_table: Table = lua.globals().get("plugin")?;
                let run_fn: mlua::Function = plugin_table.get("run")?;

                let name: String = plugin_table
                    .get::<_, Option<String>>("name")?
                    .unwrap_or_else(|| {
                        PathBuf::from(&fname)
                            .file_stem()
                            .unwrap()
                            .to_string_lossy()
                            .to_string()
                    });
                let description: String = plugin_table
                    .get::<_, Option<String>>("description")?
                    .unwrap_or_default();
                let raw_hotkey: Option<String> = plugin_table.get("hotkey")?;
                let run_key = lua.create_registry_value(run_fn)?;
                (name, description, raw_hotkey, run_key)
            };

            let mut hotkey = raw_hotkey.as_deref().and_then(parse_hotkey);
            if raw_hotkey.is_some() && hotkey.is_none() {
                editor.borrow_mut().set_message(
                    format!("Plugin {}: invalid hotkey '{}'", fname, raw_hotkey.unwrap()),
                    3,
                );
            }
            if let Some(h) = hotkey {
                if RESERVED_HOTKEYS.contains(&h) {
                    hotkey = None; // collides with a built-in shortcut
                }
            }

            metas.push(PluginMeta {
                file: fname.clone(),
                name: name.clone(),
                description,
                hotkey,
            });
            Ok(PluginRuntime { name, lua, run_key })
        })();

        match load_result {
            Ok(rt) => {
                runtimes.push(rt);
                loaded += 1;
            }
            Err(e) => {
                editor
                    .borrow_mut()
                    .set_message(format!("Error loading plugin {}: {}", fname, e), 3);
            }
        }
    }

    if loaded > 0 {
        editor
            .borrow_mut()
            .set_message(format!("{} plugin(s) loaded from plugins/", loaded), 3);
    }
    editor.borrow_mut().plugins = metas;
    runtimes
}

/// Run a plugin. Mirrors `PixelPad.run_plugin` in the Python version:
/// snapshot for undo, run, and mark modified only if the buffer actually
/// changed (otherwise drop the now-useless undo snapshot).
pub fn run_plugin(editor: &Rc<RefCell<PixelPad>>, rt: &PluginRuntime) -> bool {
    editor.borrow_mut().push_undo();
    let before = editor.borrow().lines.clone();

    let run_fn: mlua::Function = match rt.lua.registry_value(&rt.run_key) {
        Ok(f) => f,
        Err(e) => {
            editor
                .borrow_mut()
                .set_message(format!("Plugin error ({}): {}", rt.name, e), 3);
            return false;
        }
    };

    let deadline = Instant::now() + PLUGIN_TIME_LIMIT;
    rt.lua.set_hook(
        HookTriggers {
            every_nth_instruction: Some(PLUGIN_HOOK_INSTRUCTIONS),
            ..Default::default()
        },
        move |_lua, _debug| {
            if Instant::now() > deadline {
                Err(mlua::Error::RuntimeError(
                    "plugin exceeded time limit (3s)".to_string(),
                ))
            } else {
                Ok(())
            }
        },
    );

    let ok = match run_fn.call::<_, ()>(()) {
        Ok(()) => true,
        Err(e) => {
            editor
                .borrow_mut()
                .set_message(format!("Plugin error ({}): {}", rt.name, e), 3);
            false
        }
    };

    rt.lua.remove_hook();

    let mut ed = editor.borrow_mut();
    if ed.lines != before {
        ed.modified = true;
    } else if !ed.undo_stack.is_empty() {
        ed.undo_stack.pop_back();
    }
    ed.clamp_cursor();
    ok
}
