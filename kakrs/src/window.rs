//! Port of window.hh/cc

use crate::buffer::{Buffer, BufferFlags, BufferIterator, ChangeType};
use crate::client::Client;
use crate::context::{Context, ContextFlags};
use crate::debug::{write_to_debug_buffer, DebugFlags, ProfileScope};
use crate::display_buffer::{get_iterator_coord, AtomType, DisplayAtom, DisplayBuffer, DisplayLine};
use crate::exception::Result;
use crate::face::FaceRegistry;
use crate::highlighter::{DisplaySetup, HighlightContext, HighlightPass};
use crate::highlighter_group::Highlighters;
use crate::hook_manager::Hook;
use crate::input_handler::InputHandler;
use crate::option_manager::{Opt, OptionWatcher};
use crate::scope::Scope;
use crate::selection::{BasicSelection, Selection, SelectionList};
use crate::units::{BufferCoord, BufferRange, ColumnCount, DisplayCoord, LineCount};
use crate::utf8;
use std::cell::{Cell, Ref, RefCell};
use std::hash::{Hash, Hasher};
use std::rc::{Rc, Weak};

#[derive(Clone, Default, PartialEq)]
struct Setup {
    position: DisplayCoord,
    dimensions: DisplayCoord,
    timestamp: usize,
    faces_hash: u64,
    main_selection: usize,
    selections: Vec<BasicSelection>,
}

/// A Window is a view onto a Buffer
pub struct Window {
    scope: Scope,
    buffer: Rc<Buffer>,
    client: RefCell<Weak<Client>>,
    position: Cell<DisplayCoord>,
    dimensions: Cell<DisplayCoord>,
    display_buffer: RefCell<DisplayBuffer>,
    builtin_highlighters: Highlighters,
    resize_hook_pending: Cell<bool>,
    last_display_setup: Cell<DisplaySetup>,
    last_setup: RefCell<Setup>,
    self_weak: Weak<Window>,
}

fn compute_faces_hash(faces: &FaceRegistry) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for (_, face) in faces.flatten_faces() {
        if face.base.is_empty() {
            face.face.hash(&mut hasher);
        } else {
            face.base.hash(&mut hasher);
        }
    }
    hasher.finish()
}

impl Window {
    pub fn new(buffer: &Rc<Buffer>) -> Result<Rc<Window>> {
        let scope = Scope::new(buffer.scope());
        let builtin_highlighters = Highlighters::new(scope.highlighters());
        let window = Rc::new_cyclic(|w| Window {
            scope,
            buffer: buffer.clone(),
            client: RefCell::new(Weak::new()),
            position: Cell::new(DisplayCoord::default()),
            dimensions: Cell::new(DisplayCoord::default()),
            display_buffer: RefCell::new(DisplayBuffer::default()),
            builtin_highlighters,
            resize_hook_pending: Cell::new(false),
            last_display_setup: Cell::new(DisplaySetup::default()),
            last_setup: RefCell::new(Setup::default()),
            self_weak: w.clone(),
        });

        window.run_hook_in_own_context(Hook::WinCreate, &buffer.name(), Vec::new())?;
        window.options().register_watcher(window.self_weak.clone() as Weak<dyn OptionWatcher>);

        crate::highlighters::setup_builtin_highlighters(window.builtin_highlighters.group())?;

        // gather as on_option_changed can mutate the option managers
        for option in window.options().flatten_options() {
            window.on_option_changed(&option)?;
        }
        Ok(window)
    }

    pub fn rc(&self) -> Rc<Window> {
        self.self_weak.upgrade().expect("window is alive")
    }

    pub fn scope(&self) -> &Scope {
        &self.scope
    }
    pub fn options(&self) -> &Rc<crate::option_manager::OptionManager> {
        self.scope.options()
    }
    pub fn hooks(&self) -> &Rc<crate::hook_manager::HookManager> {
        self.scope.hooks()
    }

    pub fn buffer(&self) -> &Rc<Buffer> {
        &self.buffer
    }

    pub fn position(&self) -> DisplayCoord {
        self.position.get()
    }

    pub fn set_position(&self, position: DisplayCoord) {
        let line = position.line.clamp(0, self.buffer.line_count() - 1);
        self.position.set(DisplayCoord::new(line, position.column.max(0)));
    }

    pub fn dimensions(&self) -> DisplayCoord {
        self.dimensions.get()
    }

    pub fn set_dimensions(&self, dimensions: DisplayCoord) {
        if self.dimensions.get() != dimensions {
            self.dimensions.set(dimensions);
            self.resize_hook_pending.set(true);
        }
    }

    pub fn set_client(&self, client: Option<&Rc<Client>>) {
        *self.client.borrow_mut() = client.map(Rc::downgrade).unwrap_or_default();
    }

    pub fn set_client_weak(&self, client: Weak<Client>) {
        *self.client.borrow_mut() = client;
    }

    pub fn scroll_lines(&self, offset: LineCount) {
        let mut p = self.position.get();
        p.line = (p.line + offset).max(0);
        self.position.set(p);
    }

    pub fn display_line_at(&self, buffer_line: LineCount, display_line: LineCount) {
        if display_line >= 0 || display_line < self.dimensions.get().line {
            let mut p = self.position.get();
            p.line = (buffer_line - display_line).max(0);
            self.position.set(p);
        }
    }

    pub fn center_line(&self, buffer_line: LineCount) {
        self.display_line_at(buffer_line, self.dimensions.get().line / 2);
    }

    pub fn scroll_columns(&self, offset: ColumnCount) {
        let mut p = self.position.get();
        p.column = (p.column + offset).max(0);
        self.position.set(p);
    }

    pub fn display_column_at(&self, buffer_column: ColumnCount, display_column: ColumnCount) {
        if display_column >= 0 || display_column < self.dimensions.get().column {
            let mut p = self.position.get();
            p.column = (buffer_column - display_column).max(0);
            self.position.set(p);
        }
    }

    pub fn center_column(&self, buffer_column: ColumnCount) {
        self.display_column_at(buffer_column, self.dimensions.get().column / 2);
    }

    fn build_setup(&self, context: &Context) -> Setup {
        let timestamp = context.buffer().timestamp();
        let faces_hash = compute_faces_hash(&context.faces_allow_local(false));
        let sels = context.selections();
        Setup {
            position: self.position.get(),
            dimensions: self.dimensions.get(),
            timestamp,
            faces_hash,
            main_selection: sels.main_index(),
            selections: sels.iter().map(|s| s.basic()).collect(),
        }
    }

    pub fn needs_redraw(&self, context: &Context) -> bool {
        let last = self.last_setup.borrow();
        let faces_hash = compute_faces_hash(&context.faces_allow_local(false));
        let timestamp = context.buffer().timestamp();
        let selections = context.selections();
        self.position.get() != last.position
            || self.dimensions.get() != last.dimensions
            || timestamp != last.timestamp
            || selections.main_index() != last.main_selection
            || selections.size() != last.selections.len()
            || faces_hash != last.faces_hash
            || !selections.iter().zip(last.selections.iter()).all(|(a, b)| a.basic() == *b)
    }

    pub fn display_buffer(&self) -> Ref<'_, DisplayBuffer> {
        self.display_buffer.borrow()
    }

    pub fn update_display_buffer(&self, context: &Context) -> Result<Ref<'_, DisplayBuffer>> {
        let debug_flags = context.options().val::<DebugFlags>("debug");
        let buffer = self.buffer.clone();
        let _profile = ProfileScope::new(
            debug_flags,
            |duration| {
                write_to_debug_buffer(&kformat!(
                    "window display update for '{}' took {} us",
                    buffer.display_name(),
                    duration.as_micros() as usize
                ))
            },
            !self.buffer.flags().has(BufferFlags::Debug),
        );

        // Work on a detached display buffer, so that highlighters can query the window
        let mut display_buffer = std::mem::take(&mut *self.display_buffer.borrow_mut());

        if display_buffer.timestamp() != usize::MAX {
            let mut position = self.position.get();
            for change in self.buffer.changes_since(display_buffer.timestamp()) {
                if change.type_ == ChangeType::Insert && change.begin.line < position.line {
                    position.line += change.end.line - change.begin.line;
                }
                if change.type_ == ChangeType::Erase && change.begin.line < position.line {
                    position.line = (position.line - (change.end.line - change.begin.line)).max(change.begin.line);
                }
            }
            self.position.set(position);
        }

        display_buffer.set_timestamp(self.buffer.timestamp());
        display_buffer.lines_mut().clear();

        let dimensions = self.dimensions.get();
        if dimensions.line == 0 || dimensions.column == 0 {
            *self.display_buffer.borrow_mut() = display_buffer;
            return Ok(self.display_buffer.borrow());
        }

        let mut setup = self.compute_display_setup(context);
        let last = self.last_display_setup.get();
        if setup.line_count != last.line_count || setup.widget_columns != last.widget_columns {
            // Technically the window has not resized, but most things that hook WinResize probably want to be notfied anyway
            self.resize_hook_pending.set(true);
        }

        for line in 0..setup.line_count {
            let buffer_line = setup.first_line + line;
            if buffer_line >= self.buffer.line_count() {
                break;
            }
            let len = self.buffer.line_length(buffer_line);
            display_buffer.lines_mut().push(DisplayLine::new(vec![DisplayAtom::range(
                &self.buffer,
                BufferRange::new(BufferCoord::line(buffer_line), BufferCoord::new(buffer_line, len)),
                Default::default(),
            )]));
        }

        display_buffer.compute_range();
        let range = BufferRange::new(BufferCoord::new(0, 0), self.buffer.end_coord());
        for pass in [HighlightPass::Replace, HighlightPass::Wrap, HighlightPass::Move] {
            self.builtin_highlighters.highlight(
                &HighlightContext { context, setup: &setup, pass, disabled_ids: &[] },
                &mut display_buffer,
                range,
            );
        }

        if context.ensure_cursor_visible.get() {
            let cursor = context.selections().main().cursor().coord;
            let cursor_pos = display_coord_in(&display_buffer, &self.buffer, cursor);
            if let Some(cursor_pos) = cursor_pos {
                let line_overflow = cursor_pos.line - dimensions.line + setup.scroll_offset.line + 1;
                if line_overflow > 0 {
                    let lines = display_buffer.lines_mut();
                    let n = (line_overflow as usize).min(lines.len());
                    lines.drain(..n);
                    if let Some(first) = lines.first() {
                        setup.first_line = first.range().begin.line;
                    }
                }

                let max_first_column = cursor_pos.column - (setup.widget_columns + setup.scroll_offset.column);
                setup.first_column = setup.first_column.min(max_first_column).max(0);

                let min_first_column = cursor_pos.column - (dimensions.column - setup.scroll_offset.column) + 1;
                setup.first_column = setup.first_column.max(min_first_column);
            }
        }

        for line in display_buffer.lines_mut() {
            line.trim_from(setup.widget_columns, setup.first_column, dimensions.column);
        }
        if display_buffer.lines().len() > dimensions.line as usize {
            display_buffer.lines_mut().truncate(dimensions.line as usize);
        }

        self.builtin_highlighters.highlight(
            &HighlightContext { context, setup: &setup, pass: HighlightPass::Colorize, disabled_ids: &[] },
            &mut display_buffer,
            range,
        );

        display_buffer.optimize();

        *self.display_buffer.borrow_mut() = display_buffer;

        self.set_position(DisplayCoord::new(setup.first_line, setup.first_column));
        let new_setup = self.build_setup(context);
        *self.last_setup.borrow_mut() = new_setup;
        self.last_display_setup.set(setup);

        Ok(self.display_buffer.borrow())
    }

    pub fn run_resize_hook_ifn(&self) -> Result<()> {
        if self.resize_hook_pending.get() {
            self.resize_hook_pending.set(false);
            let d = self.dimensions.get();
            self.run_hook_in_own_context(Hook::WinResize, &kformat!("{}.{}", d.line, d.column), Vec::new())?;
        }
        Ok(())
    }

    pub fn last_display_setup(&self) -> DisplaySetup {
        self.last_display_setup.get()
    }

    fn compute_display_setup(&self, context: &Context) -> DisplaySetup {
        let mut offset = self.options().val::<DisplayCoord>("scrolloff");
        let dimensions = self.dimensions.get();
        offset.line = offset.line.min((dimensions.line + 1) / 2);
        offset.column = offset.column.min((dimensions.column + 1) / 2);

        let cursor = context.selections().main().cursor().coord;
        let position = self.position.get();
        let mut setup = DisplaySetup {
            first_line: position.line,
            line_count: dimensions.line,
            first_column: position.column,
            widget_columns: 0,
            scroll_offset: offset,
        };
        let line_count = self.buffer.line_count();
        if context.ensure_cursor_visible.get() && cursor.line - offset.line < setup.first_line {
            setup.first_line = (cursor.line - offset.line).clamp(0, line_count - 1);
        }

        for pass in [HighlightPass::Move, HighlightPass::Wrap, HighlightPass::Replace] {
            let current = setup;
            self.builtin_highlighters.compute_display_setup(
                &HighlightContext { context, setup: &current, pass, disabled_ids: &[] },
                &mut setup,
            );
        }

        if context.ensure_cursor_visible.get() && cursor.line + offset.line >= setup.first_line + setup.line_count {
            setup.first_line = (cursor.line + offset.line - setup.line_count + 1).min(line_count - 1);
        }
        setup.first_line = setup.first_line.min(line_count - 1);
        setup
    }

    pub fn display_coord(&self, coord: BufferCoord) -> Option<DisplayCoord> {
        display_coord_in(&self.display_buffer.borrow(), &self.buffer, coord)
    }

    pub fn buffer_coord(&self, coord: DisplayCoord) -> Option<BufferCoord> {
        let display_buffer = self.display_buffer.borrow();
        if display_buffer.timestamp() != self.buffer.timestamp() || display_buffer.lines().is_empty() {
            return None;
        }
        if coord.line < 0 {
            return None;
        }
        if coord.line as usize >= display_buffer.lines().len() {
            return None;
        }
        Some(find_buffer_coord(&display_buffer.lines()[coord.line as usize], &self.buffer, coord.column))
    }

    pub fn clear_display_buffer(&self) {
        *self.display_buffer.borrow_mut() = DisplayBuffer::default();
    }

    pub fn run_hook_in_own_context(&self, hook: Hook, param: &[u8], client_name: Vec<u8>) -> Result<()> {
        if self.buffer.flags().has(BufferFlags::NoHooks) {
            return Ok(());
        }
        let hook_handler = InputHandler::new(
            SelectionList::new(self.buffer.clone(), Selection::new(BufferCoord::default())),
            ContextFlags::Draft,
            client_name,
        );
        hook_handler.context().set_window(&self.rc());
        let client = self.client.borrow().upgrade();
        if let Some(client) = client {
            hook_handler.context().set_client(&client);
        }
        self.hooks().run_hook(hook, param, hook_handler.context())
    }
}

impl OptionWatcher for Window {
    fn on_option_changed(&self, option: &Opt) -> Result<()> {
        self.run_hook_in_own_context(
            Hook::WinSetOption,
            &kformat!("{}={}", option.name(), option.get_desc_string()),
            Vec::new(),
        )
    }
}

fn display_coord_in(display_buffer: &DisplayBuffer, buffer: &Buffer, coord: BufferCoord) -> Option<DisplayCoord> {
    if display_buffer.timestamp() != buffer.timestamp() {
        return None;
    }
    for (l, line) in display_buffer.lines().iter().enumerate() {
        let range = line.range();
        if range.begin <= coord && coord < range.end {
            return Some(DisplayCoord::new(l as LineCount, find_display_column(line, buffer, coord)));
        }
    }
    None
}

fn find_display_column(line: &DisplayLine, buffer: &Buffer, coord: BufferCoord) -> ColumnCount {
    let mut column = 0;
    for atom in line.iter() {
        if atom.has_buffer_range() && coord >= atom.begin() && coord < atom.end() {
            if atom.type_() == AtomType::Range {
                let lines = buffer.lines();
                column += utf8::it_column_distance(
                    &BufferIterator::new(&lines, get_iterator_coord(buffer, atom.begin())),
                    &BufferIterator::new(&lines, get_iterator_coord(buffer, coord)),
                );
            }
            return column;
        }
        column += atom.length();
    }
    column
}

fn find_buffer_coord(line: &DisplayLine, buffer: &Buffer, mut column: ColumnCount) -> BufferCoord {
    let range = line.range();
    for atom in line.iter() {
        let len = atom.length();
        if atom.has_buffer_range() && column < len {
            if atom.type_() == AtomType::Range {
                let coord = {
                    let lines = buffer.lines();
                    let mut it = BufferIterator::new(&lines, get_iterator_coord(buffer, atom.begin()));
                    let end = BufferIterator::new(&lines, get_iterator_coord(buffer, range.end));
                    // utf8::advance(it, end, ColumnCount)
                    let mut d = column.max(0);
                    if it != end && d > 0 {
                        let begin = it;
                        while it != end && d > 0 {
                            d -= crate::unicode::codepoint_width(utf8::it_read_codepoint(&mut it, &end));
                            if it != end && d < 0 {
                                utf8::it_to_previous(&mut it, &begin);
                            }
                        }
                    }
                    it.coord()
                };
                return buffer.clamp(coord);
            }
            return buffer.clamp(atom.begin());
        }
        column -= len;
    }
    if range.end == BufferCoord::new(0, 0) { range.end } else { buffer.prev(range.end) }
}
