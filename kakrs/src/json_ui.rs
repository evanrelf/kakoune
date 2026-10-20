//! Port of json_ui.hh/json_ui.cc: a user interface speaking JSON-RPC on
//! stdin/stdout.

use crate::display_buffer::{DisplayAtom, DisplayBuffer, DisplayLine, DisplayLineList};
use crate::event_manager::{set_signal_handler, EventMode, FDWatcher, FdEvents, WatcherRef};
use crate::exception::{Error, Result};
use crate::face::{color_to_string, Attribute, Color, Face, RGB};
use crate::file::{fd_readable, write};
use crate::json::{parse_json, JsonValue, ToJson};
use crate::keys::{encode_coord, parse_keys, resize, str_to_button, Key, Modifiers};
use crate::units::{Codepoint, ColumnCount, DisplayCoord};
use crate::user_interface::{
    InfoStyle, MenuStyle, OnKeyCallback, OnPasteCallback, StatusStyle, UiOptions, UserInterface,
};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

fn invalid_rpc_request(message: &[u8]) -> Error {
    Error::runtime(kformat!("invalid json rpc request ({})", message))
}

impl ToJson for Color {
    fn to_json(&self) -> Vec<u8> {
        // Note: only exactly Color::RGB alpha is written in #rrggbb form,
        // other rgb(a) colors go through color_to_string.
        if self.a == RGB {
            return format!("\"#{:02x}{:02x}{:02x}\"", self.r, self.g, self.b).into_bytes();
        }
        color_to_string(*self).to_json()
    }
}

impl ToJson for Attribute {
    fn to_json(&self) -> Vec<u8> {
        const ATTRS: [(Attribute, &str); 12] = [
            (Attribute::Underline, "underline"),
            (Attribute::CurlyUnderline, "curly_underline"),
            (Attribute::DoubleUnderline, "double_underline"),
            (Attribute::Reverse, "reverse"),
            (Attribute::Blink, "blink"),
            (Attribute::Bold, "bold"),
            (Attribute::Dim, "dim"),
            (Attribute::Italic, "italic"),
            (Attribute::FinalFg, "final_fg"),
            (Attribute::FinalBg, "final_bg"),
            (Attribute::FinalAttr, "final_attr"),
            (Attribute::Strikethrough, "strikethrough"),
        ];

        let mut res = b"[".to_vec();
        let mut first = true;
        for (attr, name) in ATTRS {
            if !self.has(attr) {
                continue;
            }
            if !first {
                res.push(b',');
            }
            first = false;
            res.extend(name.to_json());
        }
        res.push(b']');
        res
    }
}

impl ToJson for Face {
    fn to_json(&self) -> Vec<u8> {
        kformat!(
            r#"\{ "fg": {}, "bg": {}, "underline": {}, "attributes": {} }"#,
            self.fg.to_json(),
            self.bg.to_json(),
            self.underline.to_json(),
            self.attributes.to_json()
        )
    }
}

impl ToJson for DisplayAtom {
    fn to_json(&self) -> Vec<u8> {
        kformat!(r#"\{ "face": {}, "contents": {} }"#, self.face.to_json(), self.content().to_json())
    }
}

impl ToJson for DisplayLine {
    fn to_json(&self) -> Vec<u8> {
        self.atoms().to_json()
    }
}

impl ToJson for DisplayCoord {
    fn to_json(&self) -> Vec<u8> {
        kformat!(r#"\{ "line": {}, "column": {} }"#, self.line, self.column)
    }
}

impl ToJson for MenuStyle {
    fn to_json(&self) -> Vec<u8> {
        match self {
            MenuStyle::Prompt => br#""prompt""#.to_vec(),
            MenuStyle::Search => br#""search""#.to_vec(),
            MenuStyle::Inline => br#""inline""#.to_vec(),
        }
    }
}

impl ToJson for InfoStyle {
    fn to_json(&self) -> Vec<u8> {
        match self {
            InfoStyle::Prompt => br#""prompt""#.to_vec(),
            InfoStyle::Inline => br#""inline""#.to_vec(),
            InfoStyle::InlineAbove => br#""inlineAbove""#.to_vec(),
            InfoStyle::InlineBelow => br#""inlineBelow""#.to_vec(),
            InfoStyle::MenuDoc => br#""menuDoc""#.to_vec(),
            InfoStyle::Modal => br#""modal""#.to_vec(),
        }
    }
}

impl ToJson for StatusStyle {
    fn to_json(&self) -> Vec<u8> {
        match self {
            StatusStyle::Status => br#""status""#.to_vec(),
            StatusStyle::Command => br#""command""#.to_vec(),
            StatusStyle::Search => br#""search""#.to_vec(),
            StatusStyle::Prompt => br#""prompt""#.to_vec(),
        }
    }
}

/// params serialized as json, separated by ", "
fn concat(args: &[&dyn ToJson]) -> Vec<u8> {
    let mut res = Vec::new();
    for (i, arg) in args.iter().enumerate() {
        if i != 0 {
            res.extend_from_slice(b", ");
        }
        res.extend(arg.to_json());
    }
    res
}

fn rpc_call_string(method: &str, args: &[&dyn ToJson]) -> Vec<u8> {
    kformat!(r#"\{ "jsonrpc": "2.0", "method": "{}", "params": [{}] }{}"#, method, concat(args), "\n")
}

fn rpc_call(method: &str, args: &[&dyn ToJson]) {
    // C++ throws on write errors, which the UserInterface interface cannot report
    let _ = write(1, &rpc_call_string(method, args));
}

// callbacks are reference counted so that they can be called without
// holding a borrow, they may reenter the ui
type SharedOnKey = Rc<dyn Fn(Key) -> Result<()>>;
type SharedOnPaste = Rc<dyn Fn(&[u8]) -> Result<()>>;

/// State shared between the JsonUI and its stdin watcher callback
struct JsonUIState {
    on_key: RefCell<Option<SharedOnKey>>,
    on_paste: RefCell<Option<SharedOnPaste>>,
    dimensions: Cell<DisplayCoord>,
    requests: RefCell<Vec<u8>>,
}

pub struct JsonUI {
    state: Rc<JsonUIState>,
    stdin_watcher: FDWatcher,
}

impl JsonUI {
    pub fn new() -> JsonUI {
        let state = Rc::new(JsonUIState::new());
        let weak: Weak<JsonUIState> = Rc::downgrade(&state);
        let stdin_watcher =
            FDWatcher::new(0, FdEvents::Read, EventMode::Urgent, move |watcher: &WatcherRef, _, mode| {
                match weak.upgrade() {
                    Some(state) => state.parse_requests(watcher, mode),
                    None => Ok(()),
                }
            });
        set_signal_handler(libc::SIGINT, libc::SIG_DFL);
        JsonUI { state, stdin_watcher }
    }
}

impl Default for JsonUI {
    fn default() -> Self {
        Self::new()
    }
}

impl UserInterface for JsonUI {
    fn is_ok(&self) -> bool {
        self.stdin_watcher.fd() != -1
    }

    fn draw(
        &self,
        display_buffer: &DisplayBuffer,
        cursor_pos: DisplayCoord,
        default_face: &Face,
        padding_face: &Face,
        widget_columns: ColumnCount,
    ) {
        rpc_call("draw", &[display_buffer.lines(), &cursor_pos, default_face, padding_face, &widget_columns]);
    }

    fn draw_status(
        &self,
        prompt: &DisplayLine,
        content: &DisplayLine,
        cursor_pos: ColumnCount,
        mode_line: &DisplayLine,
        default_face: &Face,
        style: StatusStyle,
    ) {
        rpc_call("draw_status", &[prompt, content, &cursor_pos, mode_line, default_face, &style]);
    }

    fn menu_show(&self, items: &[DisplayLine], anchor: DisplayCoord, fg: Face, bg: Face, style: MenuStyle) {
        rpc_call("menu_show", &[&items, &anchor, &fg, &bg, &style]);
    }

    fn menu_select(&self, selected: i32) {
        rpc_call("menu_select", &[&selected]);
    }

    fn menu_hide(&self) {
        rpc_call("menu_hide", &[]);
    }

    fn info_show(&self, title: &DisplayLine, content: &DisplayLineList, anchor: DisplayCoord, face: Face, style: InfoStyle) {
        rpc_call("info_show", &[title, content, &anchor, &face, &style]);
    }

    fn info_hide(&self) {
        rpc_call("info_hide", &[]);
    }

    fn refresh(&self, force: bool) {
        rpc_call("refresh", &[&force]);
    }

    fn set_ui_options(&self, options: &UiOptions) {
        rpc_call("set_ui_options", &[options]);
    }

    fn dimensions(&self) -> DisplayCoord {
        self.state.dimensions.get()
    }

    fn set_on_key(&self, callback: OnKeyCallback) {
        *self.state.on_key.borrow_mut() = Some(Rc::from(callback));
    }

    fn set_on_paste(&self, callback: OnPasteCallback) {
        *self.state.on_paste.borrow_mut() = Some(Rc::from(callback));
    }
}

impl JsonUIState {
    fn new() -> JsonUIState {
        JsonUIState {
            on_key: RefCell::new(None),
            on_paste: RefCell::new(None),
            dimensions: Cell::new(DisplayCoord::new(24, 80)),
            requests: RefCell::new(Vec::new()),
        }
    }

    fn on_key(&self, key: Key) -> Result<()> {
        // cloned so that the callback can reenter the ui
        let on_key = self.on_key.borrow().clone();
        match on_key {
            Some(on_key) => on_key(key),
            None => Ok(()),
        }
    }

    fn eval_json(&self, json: &JsonValue) -> Result<()> {
        let Some(object) = json.as_object() else {
            return Err(invalid_rpc_request(b"request is not an object"));
        };

        match object.get(b"jsonrpc".as_slice()) {
            Some(JsonValue::String(s)) if s == b"2.0" => {}
            _ => return Err(invalid_rpc_request(b"only protocol '2.0' is supported")),
        }

        let method: &[u8] = match object.get(b"method".as_slice()) {
            None => return Err(invalid_rpc_request(b"method missing")),
            Some(JsonValue::String(s)) => s,
            Some(_) => return Err(invalid_rpc_request(b"'method' is not a string")),
        };

        let params = match object.get(b"params".as_slice()) {
            None => return Err(invalid_rpc_request(b"params missing")),
            Some(JsonValue::Array(a)) => a,
            Some(_) => return Err(invalid_rpc_request(b"'params' is not an array")),
        };

        let int_param = |i: usize| params[i].as_int();

        match method {
            b"keys" => {
                for key_val in params {
                    let Some(keys) = key_val.as_string() else {
                        return Err(invalid_rpc_request(b"'keys' is not an array of strings"));
                    };
                    for key in parse_keys(keys)? {
                        self.on_key(key)?;
                    }
                }
            }
            b"paste" => {
                let content = match params.as_slice() {
                    [JsonValue::String(s)] => s,
                    _ => return Err(invalid_rpc_request(b"paste requires a string parameter")),
                };
                let on_paste = self.on_paste.borrow().clone();
                if let Some(on_paste) = on_paste {
                    on_paste(content)?;
                }
            }
            b"mouse_move" => {
                if params.len() != 2 {
                    return Err(invalid_rpc_request(b"mouse coordinates not specified"));
                }
                let (Some(line), Some(column)) = (int_param(0), int_param(1)) else {
                    return Err(invalid_rpc_request(b"mouse coordinates are not integers"));
                };
                self.on_key(Key::with_mod(Modifiers::MousePos, encode_coord(DisplayCoord::new(line, column))))?;
            }
            b"mouse_press" | b"mouse_release" => {
                if params.len() != 3 {
                    return Err(invalid_rpc_request(b"mouse button/coordinates not specified"));
                }
                let Some(button) = params[0].as_string() else {
                    return Err(invalid_rpc_request(b"mouse button is not a string"));
                };
                let (Some(line), Some(column)) = (int_param(1), int_param(2)) else {
                    return Err(invalid_rpc_request(b"mouse coordinates are not integers"));
                };

                let event = if method == b"mouse_press" { Modifiers::MousePress } else { Modifiers::MouseRelease };
                let button = str_to_button(button)?;

                self.on_key(Key::with_mod(
                    event | Key::to_modifier(button),
                    encode_coord(DisplayCoord::new(line, column)),
                ))?;
            }
            b"scroll" => {
                if params.len() != 3 {
                    return Err(invalid_rpc_request(b"scroll needs an amount and coordinates"));
                }
                let (Some(amount), Some(line), Some(column)) = (int_param(0), int_param(1), int_param(2)) else {
                    return Err(invalid_rpc_request(b"scroll parameters are not integers"));
                };
                self.on_key(Key::with_mod(
                    Modifiers::Scroll | Modifiers((amount << 16) as u32),
                    encode_coord(DisplayCoord::new(line, column)),
                ))?;
            }
            b"menu_select" => {
                if params.len() != 1 {
                    return Err(invalid_rpc_request(b"menu_select needs the item index"));
                }
                let Some(index) = int_param(0) else {
                    return Err(invalid_rpc_request(b"menu index is not an integer"));
                };
                self.on_key(Key::with_mod(Modifiers::MenuSelect, index as Codepoint))?;
            }
            b"resize" => {
                if params.len() != 2 {
                    return Err(Error::runtime("resize expects 2 parameters"));
                }
                let (Some(line), Some(column)) = (int_param(0), int_param(1)) else {
                    return Err(invalid_rpc_request(b"width and height are not integers"));
                };

                let dim = DisplayCoord::new(line, column);
                self.dimensions.set(dim);
                self.on_key(resize(dim))?;
            }
            _ => return Err(invalid_rpc_request(&kformat!("unknown method: {}", method))),
        }
        Ok(())
    }

    fn parse_requests(&self, watcher: &WatcherRef, _mode: EventMode) -> Result<()> {
        const BUFSIZE: usize = 1024;
        let mut buf = [0u8; BUFSIZE];
        while fd_readable(0) {
            let size = unsafe { libc::read(0, buf.as_mut_ptr() as *mut libc::c_void, BUFSIZE) };
            if size == -1 || size == 0 {
                watcher.close_fd();
                break;
            }

            self.requests.borrow_mut().extend_from_slice(&buf[..size as usize]);
        }

        self.process_requests()
    }

    /// Evaluate all complete requests in the request buffer, leaving any
    /// incomplete trailing request there for a later call.
    /// Runtime errors are reported on stderr, other errors (which are not
    /// runtime_errors in C++) propagate, leaving the failing request buffered.
    fn process_requests(&self) -> Result<()> {
        if self.on_key.borrow().is_none() {
            return Ok(());
        }

        loop {
            let (parsed, salvage_pos) = {
                let requests = self.requests.borrow();
                if requests.is_empty() {
                    break;
                }
                // try to salvage request by dropping its first line
                let salvage_pos = requests.iter().position(|&c| c == b'\n').map_or(requests.len(), |p| p + 1);
                (parse_json(&requests), salvage_pos)
            };

            let res = parsed.and_then(|parsed| match parsed {
                Some(parsed) => self.eval_json(&parsed.value).map(|_| Some(parsed.new_pos)),
                None => Ok(None),
            });
            let pos = match res {
                Ok(pos) => pos,
                Err(error) if !error.is_runtime() => return Err(error),
                Err(error) => {
                    let message = kformat!(
                        "error while handling requests '{}': '{}'\n",
                        self.requests.borrow().as_slice(),
                        error.what()
                    );
                    let _ = write(2, &message);
                    Some(salvage_pos)
                }
            };
            let Some(pos) = pos else {
                break; // unterminated request ?
            };

            let mut requests = self.requests.borrow_mut();
            let pos = pos.min(requests.len());
            requests.drain(..pos);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::face::NamedColor;
    use crate::keys::{key, MouseButton};

    fn line(atoms: &[(&str, Face)]) -> DisplayLine {
        DisplayLine::new(atoms.iter().map(|(s, f)| DisplayAtom::text(s.as_bytes(), *f)).collect())
    }

    fn rpc(method: &str, args: &[&dyn ToJson]) -> String {
        String::from_utf8(rpc_call_string(method, args)).unwrap()
    }

    #[test]
    fn test_json_ui_output() {
        let default = Face::default();
        let face = Face::with_attr(NamedColor::Black, NamedColor::White, Attribute::Underline | Attribute::Italic);
        let draw_line = line(&[("f", face), ("\n", default)]);
        let lines = vec![draw_line];
        assert_eq!(
            rpc("draw", &[&lines, &DisplayCoord::new(0, 0), &default, &Face::new(NamedColor::Blue, NamedColor::Default), &0]),
            concat!(
                r#"{ "jsonrpc": "2.0", "method": "draw", "params": [[[{ "face": { "fg": "black", "bg": "white", "underline": "default", "attributes": ["underline","italic"] }, "contents": "f" }, "#,
                r#"{ "face": { "fg": "default", "bg": "default", "underline": "default", "attributes": [] }, "contents": "\u000a" }]], "#,
                r#"{ "line": 0, "column": 0 }, { "fg": "default", "bg": "default", "underline": "default", "attributes": [] }, "#,
                r#"{ "fg": "blue", "bg": "default", "underline": "default", "attributes": [] }, 0] }"#,
                "\n"
            )
        );

        assert_eq!(rpc("menu_hide", &[]), "{ \"jsonrpc\": \"2.0\", \"method\": \"menu_hide\", \"params\": [] }\n");
        assert_eq!(rpc("refresh", &[&true]), "{ \"jsonrpc\": \"2.0\", \"method\": \"refresh\", \"params\": [true] }\n");
        assert_eq!(
            rpc("set_ui_options", &[&UiOptions::new()]),
            "{ \"jsonrpc\": \"2.0\", \"method\": \"set_ui_options\", \"params\": [{}] }\n"
        );
        let mut options = UiOptions::new();
        options.insert(b"a".to_vec(), b"b\"".to_vec());
        options.insert(b"c".to_vec(), b"d".to_vec());
        assert_eq!(
            rpc("set_ui_options", &[&options]),
            "{ \"jsonrpc\": \"2.0\", \"method\": \"set_ui_options\", \"params\": [{\"a\": \"b\\\"\",\"c\": \"d\"}] }\n"
        );

        let status_face = Face::new(NamedColor::Cyan, NamedColor::Default);
        assert_eq!(
            rpc(
                "draw_status",
                &[&DisplayLine::default(), &DisplayLine::default(), &-1, &line(&[("1 sel", default)]), &status_face, &StatusStyle::Status]
            ),
            concat!(
                r#"{ "jsonrpc": "2.0", "method": "draw_status", "params": [[], [], -1, [{ "face": { "fg": "default", "bg": "default", "underline": "default", "attributes": [] }, "contents": "1 sel" }], "#,
                r#"{ "fg": "cyan", "bg": "default", "underline": "default", "attributes": [] }, "status"] }"#,
                "\n"
            )
        );

        let items = vec![line(&[("foo", default)]), line(&[("bar", default)])];
        assert_eq!(
            rpc("menu_show", &[&items.as_slice(), &DisplayCoord::new(1, 2), &default, &default, &MenuStyle::Prompt]),
            concat!(
                r#"{ "jsonrpc": "2.0", "method": "menu_show", "params": [[[{ "face": { "fg": "default", "bg": "default", "underline": "default", "attributes": [] }, "contents": "foo" }], "#,
                r#"[{ "face": { "fg": "default", "bg": "default", "underline": "default", "attributes": [] }, "contents": "bar" }]], "#,
                r#"{ "line": 1, "column": 2 }, { "fg": "default", "bg": "default", "underline": "default", "attributes": [] }, "#,
                r#"{ "fg": "default", "bg": "default", "underline": "default", "attributes": [] }, "prompt"] }"#,
                "\n"
            )
        );
    }

    #[test]
    fn test_json_ui_colors() {
        assert_eq!(Color::rgba(0x12, 0xab, 0x00, RGB).unwrap().to_json(), br##""#12ab00""##);
        assert_eq!(Color::rgb(0x12, 0xab, 0x00).to_json(), br#""rgb:12ab00""#);
        assert_eq!(Color::rgba(0x12, 0xab, 0x00, 0x80).unwrap().to_json(), br#""rgba:12ab0080""#);
        assert_eq!(Color::named(NamedColor::BrightRed).to_json(), br#""bright-red""#);
        assert_eq!((Attribute::Strikethrough | Attribute::Bold | Attribute::FinalAttr).to_json(), br#"["bold","final_attr","strikethrough"]"#);
        assert_eq!(InfoStyle::InlineAbove.to_json(), br#""inlineAbove""#);
    }

    fn state_with_keys() -> (JsonUIState, Rc<RefCell<Vec<Key>>>) {
        let state = JsonUIState::new();
        let keys = Rc::new(RefCell::new(Vec::new()));
        let k = keys.clone();
        *state.on_key.borrow_mut() = Some(Rc::new(move |key| {
            k.borrow_mut().push(key);
            Ok(())
        }));
        (state, keys)
    }

    fn eval(state: &JsonUIState, request: &str) -> Result<()> {
        state.eval_json(&parse_json(request.as_bytes()).unwrap().unwrap().value)
    }

    #[test]
    fn test_json_ui_eval() {
        let (state, keys) = state_with_keys();
        eval(&state, r#"{ "jsonrpc": "2.0", "method": "keys", "params": [ "a<c-b>", "c" ] }"#).unwrap();
        assert_eq!(*keys.borrow(), vec![key('a'), crate::keys::ctrl(key('b')), key('c')]);
        keys.borrow_mut().clear();

        eval(&state, r#"{ "jsonrpc": "2.0", "method": "resize", "params": [ 40, 80 ] }"#).unwrap();
        assert_eq!(state.dimensions.get(), DisplayCoord::new(40, 80));
        assert_eq!(keys.borrow()[0], resize(DisplayCoord::new(40, 80)));
        keys.borrow_mut().clear();

        eval(&state, r#"{ "jsonrpc": "2.0", "method": "scroll", "params": [ -3, 4, 5 ] }"#).unwrap();
        let k = keys.borrow()[0];
        assert!(k.modifiers.has(Modifiers::Scroll));
        assert_eq!(k.scroll_amount(), -3);
        assert_eq!(k.coord(), DisplayCoord::new(4, 5));
        keys.borrow_mut().clear();

        eval(&state, r#"{ "jsonrpc": "2.0", "method": "mouse_press", "params": [ "right", 4, 5 ] }"#).unwrap();
        let k = keys.borrow()[0];
        assert!(k.modifiers.has(Modifiers::MousePress));
        assert_eq!(k.mouse_button(), MouseButton::Right);
        assert_eq!(k.coord(), DisplayCoord::new(4, 5));
        keys.borrow_mut().clear();

        eval(&state, r#"{ "jsonrpc": "2.0", "method": "menu_select", "params": [ 3 ] }"#).unwrap();
        assert_eq!(keys.borrow()[0], Key::with_mod(Modifiers::MenuSelect, 3));

        let err = |req: &str| String::from_utf8(eval(&state, req).unwrap_err().what().to_vec()).unwrap();
        assert_eq!(err("[]"), "invalid json rpc request (request is not an object)");
        assert_eq!(err(r#"{ "jsonrpc": "1.0" }"#), "invalid json rpc request (only protocol '2.0' is supported)");
        assert_eq!(err(r#"{ "jsonrpc": "2.0", "params": [] }"#), "invalid json rpc request (method missing)");
        assert_eq!(err(r#"{ "jsonrpc": "2.0", "method": "keys" }"#), "invalid json rpc request (params missing)");
        assert_eq!(
            err(r#"{ "jsonrpc": "2.0", "method": "foo", "params": [] }"#),
            "invalid json rpc request (unknown method: foo)"
        );
        assert_eq!(err(r#"{ "jsonrpc": "2.0", "method": "resize", "params": [1] }"#), "resize expects 2 parameters");
    }

    #[test]
    fn test_json_ui_buffering() {
        let (state, keys) = state_with_keys();
        let push = |s: &str| state.requests.borrow_mut().extend_from_slice(s.as_bytes());

        push(r#"{ "jsonrpc": "2.0", "method": "keys", "params": [ "a" ] }"#);
        push("\n");
        push(r#"{ "jsonrpc": "2.0", "method": "keys", "#);
        state.process_requests().unwrap();
        assert_eq!(*keys.borrow(), vec![key('a')]);
        // the newline following a request is only skipped when parsing the next one
        assert_eq!(state.requests.borrow().as_slice(), b"\n{ \"jsonrpc\": \"2.0\", \"method\": \"keys\", ");

        push(r#""params": [ "b" ] }"#);
        push("\n");
        state.process_requests().unwrap();
        assert_eq!(*keys.borrow(), vec![key('a'), key('b')]);
        assert_eq!(state.requests.borrow().as_slice(), b"\n");

        // invalid requests are dropped up to the end of the line
        push("garbage\n");
        push(r#"{ "jsonrpc": "2.0", "method": "keys", "params": [ "c" ] }"#);
        push("\n");
        state.process_requests().unwrap();
        assert_eq!(*keys.borrow(), vec![key('a'), key('b'), key('c')]);
        assert_eq!(state.requests.borrow().as_slice(), b"\n");
    }
}
