//! Port of word_db.hh/cc

use crate::buffer::{Buffer, StringDataPtr};
use crate::exception::Result;
use crate::line_modification::compute_line_modifications;
use crate::option_manager::{Opt, OptionWatcher};
use crate::option_types::CodepointOpt;
use crate::ranked_match::{used_letters, RankedMatch, UsedLetters, WordSplitter};
use crate::units::Codepoint;
use crate::value::{get_free_value_id, ValueId};
use indexmap::IndexMap;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

struct WordInfo {
    letters: UsedLetters,
    refcount: i32,
}

#[derive(Default)]
struct WordDBData {
    timestamp: usize,
    words: IndexMap<Rc<[u8]>, WordInfo>,
    lines: Vec<StringDataPtr>,
}

/// maintain a database of words available in a buffer
pub struct WordDB {
    buffer: Weak<Buffer>,
    data: RefCell<WordDBData>,
    registered: Cell<bool>,
}

thread_local! {
    static WORD_DB_ID: ValueId = get_free_value_id();
}

pub fn get_word_db(buffer: &Buffer) -> Rc<WordDB> {
    let id = WORD_DB_ID.with(|id| *id);
    if let Some(db) = buffer.values().get::<WordDB>(id) {
        return db;
    }
    let db = Rc::new(WordDB::new(buffer));
    buffer.values().insert(id, db.clone());
    buffer.options().register_watcher(Rc::downgrade(&db) as Weak<dyn OptionWatcher>);
    db.registered.set(true);
    db
}

fn get_extra_word_chars(buffer: &Buffer) -> Vec<Codepoint> {
    buffer.options().val::<Vec<CodepointOpt>>("extra_word_chars").iter().map(|c| c.0).collect()
}

impl WordDB {
    pub fn new(buffer: &Buffer) -> Self {
        let db = WordDB { buffer: buffer.weak(), data: RefCell::new(WordDBData::default()), registered: Cell::new(false) };
        db.rebuild_db(buffer);
        db
    }

    fn add_words(words: &mut IndexMap<Rc<[u8]>, WordInfo>, line: &[u8], extra_word_chars: &[Codepoint]) {
        for w in WordSplitter::new(line, extra_word_chars) {
            if let Some(info) = words.get_mut(w) {
                info.refcount += 1;
            } else {
                words.insert(Rc::from(w), WordInfo { letters: used_letters(w), refcount: 1 });
            }
        }
    }

    fn remove_words(words: &mut IndexMap<Rc<[u8]>, WordInfo>, line: &[u8], extra_word_chars: &[Codepoint]) {
        for w in WordSplitter::new(line, extra_word_chars) {
            if let Some(info) = words.get_mut(w) {
                info.refcount -= 1;
                if info.refcount == 0 {
                    words.swap_remove(w);
                }
            }
        }
    }

    fn rebuild_db(&self, buffer: &Buffer) {
        let mut data = self.data.borrow_mut();
        data.words.clear();
        data.lines.clear();
        let extra_word_chars = get_extra_word_chars(buffer);
        let lines: Vec<StringDataPtr> = buffer.lines().to_vec();
        for line in &lines {
            Self::add_words(&mut data.words, line, &extra_word_chars);
        }
        data.lines = lines;
        data.timestamp = buffer.timestamp();
    }

    fn update_db(&self) {
        let Some(buffer) = self.buffer.upgrade() else { return };
        let mut guard = self.data.borrow_mut();
        let data = &mut *guard;

        let modifs = compute_line_modifications(&buffer, data.timestamp);
        data.timestamp = buffer.timestamp();
        if modifs.is_empty() {
            return;
        }

        let buffer_lines = buffer.lines();
        let mut new_lines: Vec<StringDataPtr> = Vec::with_capacity(buffer_lines.len());
        let extra_word_chars = get_extra_word_chars(&buffer);

        let old_lines = std::mem::take(&mut data.lines);
        let mut old_line = 0usize;
        for modif in &modifs {
            while (old_line as i32) < modif.old_line {
                new_lines.push(old_lines[old_line].clone());
                old_line += 1;
            }
            while (old_line as i32) < modif.old_line + modif.num_removed {
                Self::remove_words(&mut data.words, &old_lines[old_line], &extra_word_chars);
                old_line += 1;
            }
            for l in 0..modif.num_added {
                let line = buffer_lines[(modif.new_line + l) as usize].clone();
                Self::add_words(&mut data.words, &line, &extra_word_chars);
                new_lines.push(line);
            }
        }
        while old_line != old_lines.len() {
            new_lines.push(old_lines[old_line].clone());
            old_line += 1;
        }
        data.lines = new_lines;
    }

    pub fn get_word_occurences(&self, word: &[u8]) -> i32 {
        self.data.borrow().words.get(word).map_or(0, |w| w.refcount)
    }

    /// Returns the matching words, as owned strings with their ranked match
    pub fn find_matching(&self, query: &[u8]) -> Vec<(Rc<[u8]>, RankedMatchInfo)> {
        self.update_db();
        let letters = used_letters(query);
        let data = self.data.borrow();
        let mut res = Vec::new();
        for (word, info) in &data.words {
            let m = RankedMatch::with_letters(word, info.letters, query, letters);
            if m.is_match() {
                res.push((word.clone(), RankedMatchInfo { letters: info.letters }));
            }
        }
        res
    }
}

/// Information to rebuild a RankedMatch over a word returned by find_matching
#[derive(Clone, Copy)]
pub struct RankedMatchInfo {
    pub letters: UsedLetters,
}

impl OptionWatcher for WordDB {
    fn on_option_changed(&self, option: &Opt) -> Result<()> {
        if option.name() == b"extra_word_chars"
            && let Some(buffer) = self.buffer.upgrade() {
                self.rebuild_db(&buffer);
            }
        Ok(())
    }
}

/// Port of for_n_best: call func on the best matches (according to less)
/// until it has accepted count of them.
pub fn for_n_best<T>(mut items: Vec<T>, mut count: usize, less: impl Fn(&T, &T) -> bool, mut func: impl FnMut(&T) -> bool) {
    items.sort_by(|a, b| {
        if less(a, b) {
            std::cmp::Ordering::Less
        } else if less(b, a) {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    });
    for item in &items {
        if count == 0 {
            break;
        }
        if func(item) {
            count -= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::make_buffer;
    use crate::units::BufferCoord;

    #[test]
    fn test_word_db() {
        let buffer = make_buffer(b"tchou mutch\ntchou kanaky tchou\n\ntchaa tchaa\nallo\n");
        let word_db = WordDB::new(&buffer);
        let words = |db: &WordDB| {
            let mut res: Vec<Vec<u8>> = db.find_matching(b"").into_iter().map(|(w, _)| w.to_vec()).collect();
            res.sort();
            res
        };
        let list = |w: &[&str]| w.iter().map(|s| s.as_bytes().to_vec()).collect::<Vec<_>>();
        assert_eq!(words(&word_db), list(&["allo", "kanaky", "mutch", "tchaa", "tchou"]));
        assert_eq!(word_db.get_word_occurences(b"tchou"), 3);
        assert_eq!(word_db.get_word_occurences(b"allo"), 1);
        buffer.erase(BufferCoord::new(1, 6), BufferCoord::new(4, 0)).unwrap();
        assert_eq!(words(&word_db), list(&["allo", "mutch", "tchou"]));
        buffer.insert(BufferCoord::new(1, 0), b"re").unwrap();
        assert_eq!(words(&word_db), list(&["allo", "mutch", "retchou", "tchou"]));
    }
}
