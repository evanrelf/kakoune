//! Port of color.hh/cc, face.hh, face_registry.hh/cc

use crate::exception::{Error, Result};
use crate::format::{FormatParam, Hex};
use crate::unicode::is_word_default;
use indexmap::IndexMap;
use std::borrow::Cow;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
pub enum NamedColor {
    Default = 0,
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    BrightBlack,
    BrightRed,
    BrightGreen,
    BrightYellow,
    BrightBlue,
    BrightMagenta,
    BrightCyan,
    BrightWhite,
}

pub const RGB: u8 = 17;

/// `a` holds either the named color index (< 17) or the alpha value of an rgb color
#[derive(Clone, Copy, Debug, Hash)]
pub struct Color {
    pub a: u8,
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const DEFAULT: Color = Color::named(NamedColor::Default);

    pub const fn named(c: NamedColor) -> Color {
        Color { a: c as u8, r: 0, g: 0, b: 0 }
    }
    pub fn rgba(r: u8, g: u8, b: u8, a: u8) -> Result<Color> {
        if a < RGB {
            return Err(Error::runtime("Colors alpha must be > 16"));
        }
        Ok(Color { a, r, g, b })
    }
    pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color { a: 255, r, g, b }
    }
    pub const fn is_rgb(&self) -> bool {
        self.a >= RGB
    }
    pub fn named_color(&self) -> Option<NamedColor> {
        if self.is_rgb() { None } else { Some(COLOR_VALUES[self.a as usize]) }
    }
}

impl Default for Color {
    fn default() -> Self {
        Color::DEFAULT
    }
}

impl PartialEq for Color {
    fn eq(&self, o: &Color) -> bool {
        self.a == o.a && self.r == o.r && self.g == o.g && self.b == o.b
    }
}
impl Eq for Color {}

impl From<NamedColor> for Color {
    fn from(c: NamedColor) -> Color {
        Color::named(c)
    }
}

const COLOR_VALUES: [NamedColor; 17] = [
    NamedColor::Default,
    NamedColor::Black,
    NamedColor::Red,
    NamedColor::Green,
    NamedColor::Yellow,
    NamedColor::Blue,
    NamedColor::Magenta,
    NamedColor::Cyan,
    NamedColor::White,
    NamedColor::BrightBlack,
    NamedColor::BrightRed,
    NamedColor::BrightGreen,
    NamedColor::BrightYellow,
    NamedColor::BrightBlue,
    NamedColor::BrightMagenta,
    NamedColor::BrightCyan,
    NamedColor::BrightWhite,
];

pub const COLOR_NAMES: [&str; 17] = [
    "default",
    "black",
    "red",
    "green",
    "yellow",
    "blue",
    "magenta",
    "cyan",
    "white",
    "bright-black",
    "bright-red",
    "bright-green",
    "bright-yellow",
    "bright-blue",
    "bright-magenta",
    "bright-cyan",
    "bright-white",
];

pub fn is_color_name(color: &[u8]) -> bool {
    COLOR_NAMES.iter().any(|c| c.as_bytes() == color)
}

pub fn str_to_color(color: &[u8]) -> Result<Color> {
    if let Some(i) = COLOR_NAMES.iter().position(|c| c.as_bytes() == color) {
        return Ok(Color::named(COLOR_VALUES[i]));
    }
    let hval = |c: u8| -> Result<u8> {
        match c {
            b'A'..=b'F' => Ok(10 + c - b'A'),
            b'a'..=b'f' => Ok(10 + c - b'a'),
            b'0'..=b'9' => Ok(c - b'0'),
            _ => Err(rt_err!("invalid digit '{}' in '{}'", [c], color)),
        }
    };
    if color.len() == 10 && &color[..4] == b"rgb:" {
        return Color::rgba(
            hval(color[4])? * 16 + hval(color[5])?,
            hval(color[6])? * 16 + hval(color[7])?,
            hval(color[8])? * 16 + hval(color[9])?,
            255,
        );
    }
    if color.len() == 13 && &color[..5] == b"rgba:" {
        return Color::rgba(
            hval(color[5])? * 16 + hval(color[6])?,
            hval(color[7])? * 16 + hval(color[8])?,
            hval(color[9])? * 16 + hval(color[10])?,
            hval(color[11])? * 16 + hval(color[12])?,
        );
    }
    Err(rt_err!("unable to parse color: '{}'", color))
}

pub fn color_to_string(color: Color) -> Vec<u8> {
    if color.is_rgb() {
        if color.a == 255 {
            kformat!("rgb:{:02}{:02}{:02}", Hex(color.r as usize), Hex(color.g as usize), Hex(color.b as usize))
        } else {
            kformat!(
                "rgba:{:02}{:02}{:02}{:02}",
                Hex(color.r as usize),
                Hex(color.g as usize),
                Hex(color.b as usize),
                Hex(color.a as usize)
            )
        }
    } else {
        COLOR_NAMES[color.a as usize].as_bytes().to_vec()
    }
}

impl FormatParam for Color {
    fn format_param(&self) -> Cow<'_, [u8]> {
        Cow::Owned(color_to_string(*self))
    }
}

crate::kak_flags! {
    pub struct Attribute: u32 {
        const Normal          = 0;
        const Underline       = 1 << 1;
        const CurlyUnderline  = 1 << 2;
        const DoubleUnderline = 1 << 3;
        const Reverse         = 1 << 4;
        const Blink           = 1 << 5;
        const Bold            = 1 << 6;
        const Dim             = 1 << 7;
        const Italic          = 1 << 8;
        const Strikethrough   = 1 << 9;
        const FinalFg         = 1 << 10;
        const FinalBg         = 1 << 11;
        const FinalAttr       = 1 << 12;
        const Final           = (1 << 10) | (1 << 11) | (1 << 12);
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct Face {
    pub fg: Color,
    pub bg: Color,
    pub attributes: Attribute,
    pub underline: Color,
}

impl Face {
    pub fn new(fg: impl Into<Color>, bg: impl Into<Color>) -> Face {
        Face { fg: fg.into(), bg: bg.into(), attributes: Attribute::Normal, underline: Color::DEFAULT }
    }
    pub fn with_attr(fg: impl Into<Color>, bg: impl Into<Color>, attributes: Attribute) -> Face {
        Face { fg: fg.into(), bg: bg.into(), attributes, underline: Color::DEFAULT }
    }
    pub fn attr(attributes: Attribute) -> Face {
        Face { attributes, ..Default::default() }
    }
}

pub fn merge_faces(base: &Face, face: &Face) -> Face {
    let alpha_blend = |base: Color, color: Color| -> Color {
        let blend = |b: u8, c: u8| -> u8 {
            let blended = (b as i32 * (255 - color.a as i32) + c as i32 * color.a as i32) / 255;
            if blended <= 255 { blended as u8 } else { 255 }
        };
        let alpha = color.a as i32 + base.a as i32 * (255 - color.a as i32) / 255;
        Color {
            r: blend(base.r, color.r),
            g: blend(base.g, color.g),
            b: blend(base.b, color.b),
            a: if alpha <= 255 { alpha as u8 } else { 255 },
        }
    };
    let choose = |get: fn(&Face) -> Color, final_attr: Attribute| -> Color {
        if face.attributes.has(final_attr) {
            return get(face);
        }
        if base.attributes.has(final_attr) {
            return get(base);
        }
        if get(face) == Color::DEFAULT {
            return get(base);
        }
        if get(base).is_rgb() && get(face).is_rgb() && get(face).a != 255 {
            return alpha_blend(get(base), get(face));
        }
        get(face)
    };
    Face {
        fg: choose(|f| f.fg, Attribute::FinalFg),
        bg: choose(|f| f.bg, Attribute::FinalBg),
        attributes: if face.attributes.has(Attribute::FinalAttr) {
            face.attributes | (base.attributes & Attribute::Final)
        } else if base.attributes.has(Attribute::FinalAttr) {
            base.attributes
        } else {
            face.attributes | base.attributes
        },
        underline: choose(|f| f.underline, Attribute(0)),
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct FaceSpec {
    pub face: Face,
    pub base: Vec<u8>,
}

/// Mimic C++ `is_word(char)` where char is signed (non-ascii bytes are never words)
fn is_word_char(c: u8) -> bool {
    c < 0x80 && is_word_default(c as u32)
}

const INVALID_FACE_ERROR: &str =
    "invalid face description, expected [<fg>][,<bg>[,<underline>]][+<attr>][@base] or just [base]";

pub fn parse_face(facedesc: &[u8]) -> Result<FaceSpec> {
    if facedesc.iter().all(|&c| is_word_char(c)) && !is_color_name(facedesc) {
        return Ok(FaceSpec { face: Face::default(), base: facedesc.to_vec() });
    }
    let end = facedesc.len();
    let find = |c: u8, from: usize| facedesc[from..].iter().position(|&x| x == c).map(|p| p + from).unwrap_or(end);
    let bg_it = find(b',', 0);
    let underline_it = if bg_it == end { bg_it } else { find(b',', bg_it + 1) };
    let mut attr_it = find(b'+', 0);
    let base_it = find(b'@', 0);

    if bg_it != end && (attr_it < bg_it || bg_it + 1 == end) {
        return Err(Error::runtime(INVALID_FACE_ERROR));
    }
    if attr_it != end && attr_it + 1 == end {
        return Err(Error::runtime(INVALID_FACE_ERROR));
    }
    let colors_end = attr_it.min(base_it);
    if underline_it != end && underline_it > colors_end {
        return Err(Error::runtime(INVALID_FACE_ERROR));
    }

    let parse_color = |spec: &[u8]| -> Result<Color> {
        if spec.is_empty() { Ok(Color::DEFAULT) } else { str_to_color(spec) }
    };

    let mut spec = FaceSpec::default();
    let face = &mut spec.face;
    face.fg = parse_color(&facedesc[..bg_it.min(colors_end)])?;
    if bg_it != end {
        face.bg = parse_color(&facedesc[bg_it + 1..underline_it.min(colors_end)])?;
        if underline_it != end {
            face.underline = parse_color(&facedesc[underline_it + 1..colors_end])?;
        }
    }
    if attr_it != end {
        attr_it += 1;
        while attr_it != base_it {
            let attr = match facedesc[attr_it] {
                b'u' => Attribute::Underline,
                b'c' => Attribute::CurlyUnderline,
                b'U' => Attribute::DoubleUnderline,
                b'r' => Attribute::Reverse,
                b'b' => Attribute::Bold,
                b'B' => Attribute::Blink,
                b'd' => Attribute::Dim,
                b'i' => Attribute::Italic,
                b's' => Attribute::Strikethrough,
                b'f' => Attribute::FinalFg,
                b'g' => Attribute::FinalBg,
                b'a' => Attribute::FinalAttr,
                b'F' => Attribute::Final,
                c => return Err(rt_err!("no such face attribute: '{}'", [c])),
            };
            face.attributes |= attr;
            attr_it += 1;
        }
    }
    if base_it != end {
        spec.base = facedesc[base_it + 1..].to_vec();
    }
    Ok(spec)
}

pub fn attributes_to_string(mut attributes: Attribute) -> Vec<u8> {
    if attributes == Attribute::Normal {
        return Vec::new();
    }
    let attrs: [(Attribute, &str); 13] = [
        (Attribute::Underline, "u"),
        (Attribute::CurlyUnderline, "c"),
        (Attribute::DoubleUnderline, "U"),
        (Attribute::Reverse, "r"),
        (Attribute::Blink, "B"),
        (Attribute::Bold, "b"),
        (Attribute::Dim, "d"),
        (Attribute::Italic, "i"),
        (Attribute::Strikethrough, "s"),
        (Attribute::Final, "F"),
        (Attribute::FinalFg, "f"),
        (Attribute::FinalBg, "g"),
        (Attribute::FinalAttr, "a"),
    ];
    let mut res = b"+".to_vec();
    for (attr, name) in attrs {
        if (attributes & attr) != attr {
            continue;
        }
        attributes.remove(attr);
        res.extend_from_slice(name.as_bytes());
    }
    res
}

pub fn face_to_string(face: &Face) -> Vec<u8> {
    kformat!(
        "{},{},{}{}",
        color_to_string(face.fg),
        color_to_string(face.bg),
        color_to_string(face.underline),
        attributes_to_string(face.attributes)
    )
}

impl FormatParam for Face {
    fn format_param(&self) -> Cow<'_, [u8]> {
        Cow::Owned(face_to_string(self))
    }
}

pub type FaceMap = IndexMap<Vec<u8>, FaceSpec>;

pub struct FaceRegistry {
    parent: RefCell<Option<Rc<FaceRegistry>>>,
    faces: RefCell<FaceMap>,
}

impl FaceRegistry {
    pub fn new(parent: &Rc<FaceRegistry>) -> FaceRegistry {
        FaceRegistry { parent: RefCell::new(Some(parent.clone())), faces: RefCell::new(IndexMap::new()) }
    }

    pub fn new_root() -> FaceRegistry {
        use NamedColor::*;
        let f = |fg: NamedColor, bg: NamedColor| FaceSpec { face: Face::new(fg, bg), base: Vec::new() };
        let fa = |fg: NamedColor, bg: NamedColor, attr: Attribute| FaceSpec {
            face: Face::with_attr(fg, bg, attr),
            base: Vec::new(),
        };
        let based = |base: &str| FaceSpec { face: Face::default(), base: base.as_bytes().to_vec() };
        let entries: Vec<(&str, FaceSpec)> = vec![
            ("Default", f(Default, Default)),
            ("PrimarySelection", f(White, Blue)),
            ("SecondarySelection", f(Black, Blue)),
            ("PrimaryCursor", f(Black, White)),
            ("SecondaryCursor", f(Black, White)),
            ("PrimaryCursorEol", f(Black, Cyan)),
            ("SecondaryCursorEol", f(Black, Cyan)),
            ("LineNumbers", f(Default, Default)),
            ("LineNumberCursor", fa(Default, Default, Attribute::Reverse)),
            ("LineNumbersWrapped", fa(Default, Default, Attribute::Italic)),
            ("WrapMarker", f(Blue, Default)),
            ("MenuForeground", f(White, Blue)),
            ("MenuBackground", f(Blue, White)),
            ("MenuInfo", f(Cyan, Default)),
            ("Information", f(Black, Yellow)),
            ("InlineInformation", based("Information")),
            ("Error", f(Black, Red)),
            ("DiagnosticError", f(Red, Default)),
            ("DiagnosticWarning", f(Yellow, Default)),
            ("StatusLine", f(Cyan, Default)),
            ("StatusLineMode", f(Yellow, Default)),
            ("StatusLineInfo", f(Blue, Default)),
            ("StatusLineValue", f(Green, Default)),
            ("StatusCursor", f(Black, Cyan)),
            ("Prompt", f(Yellow, Default)),
            ("MatchingChar", fa(Default, Default, Attribute::Bold)),
            ("BufferPadding", f(Blue, Default)),
            ("Whitespace", fa(Default, Default, Attribute::FinalFg)),
            ("WhitespaceIndent", based("Whitespace")),
        ];
        FaceRegistry {
            parent: RefCell::new(None),
            faces: RefCell::new(entries.into_iter().map(|(k, v)| (k.as_bytes().to_vec(), v)).collect()),
        }
    }

    pub fn reparent(&self, parent: &Rc<FaceRegistry>) {
        *self.parent.borrow_mut() = Some(parent.clone());
    }

    pub fn parent(&self) -> Option<Rc<FaceRegistry>> {
        self.parent.borrow().clone()
    }

    /// operator[](StringView facedesc)
    pub fn get(&self, facedesc: &[u8]) -> Result<Face> {
        Ok(self.resolve_spec(&parse_face(facedesc)?))
    }

    /// Like get, but for known-valid face names, returning default on error
    pub fn face(&self, facedesc: &str) -> Face {
        self.get(facedesc.as_bytes()).unwrap_or_default()
    }

    pub fn get_spec(&self, spec: &FaceSpec) -> Face {
        self.resolve_spec(spec)
    }

    fn resolve_spec(&self, spec: &FaceSpec) -> Face {
        if spec.base.is_empty() {
            return spec.face;
        }
        let mut base = spec.base.clone();
        let mut face = spec.face;
        // returns Some(result) when resolution is finished at this registry
        let mut step = |r: &FaceRegistry| -> Option<Face> {
            let found = r.faces.borrow().get(&base).cloned();
            if let Some(value) = found {
                if value.base.is_empty() {
                    return Some(merge_faces(&value.face, &face));
                }
                if value.base != base {
                    return Some(merge_faces(&r.resolve_spec(&value), &face));
                }
                face = merge_faces(&value.face, &face);
                base = value.base.clone();
            }
            None
        };
        if let Some(res) = step(self) {
            return res;
        }
        let mut reg = self.parent();
        while let Some(r) = reg {
            if let Some(res) = step(&r) {
                return res;
            }
            reg = r.parent();
        }
        face
    }

    pub fn add_face(&self, name: &[u8], facedesc: &[u8], override_: bool) -> Result<()> {
        if !override_ && self.faces.borrow().contains_key(name) {
            return Err(rt_err!("face '{}' already defined", name));
        }
        if name.is_empty() || is_color_name(name) || name.iter().any(|&c| !is_word_char(c)) {
            return Err(rt_err!("invalid face name: '{}'", name));
        }
        let spec = parse_face(facedesc)?;
        let mut faces = self.faces.borrow_mut();
        if spec.base == name {
            if let Some(v) = faces.get_mut(&spec.base) {
                v.face = merge_faces(&v.face, &spec.face);
                v.base = spec.base;
                return Ok(());
            }
        }
        let mut it = faces.get(&spec.base);
        while let Some(v) = it {
            if v.base.is_empty() {
                break;
            }
            if v.base == name {
                return Err(Error::runtime("face cycle detected"));
            }
            it = faces.get(&v.base);
        }
        faces.insert(name.to_vec(), spec);
        Ok(())
    }

    pub fn remove_face(&self, name: &[u8]) {
        self.faces.borrow_mut().shift_remove(name);
    }

    /// Merge faces of grand parent, parent and self, children overriding
    pub fn flatten_faces(&self) -> Vec<(Vec<u8>, FaceSpec)> {
        let parent = self.parent();
        let grand_parent = parent.as_ref().and_then(|p| p.parent());
        let mut layers: Vec<FaceMap> = Vec::new();
        if let Some(gp) = &grand_parent {
            layers.push(gp.faces.borrow().clone());
        }
        if let Some(p) = &parent {
            layers.push(p.faces.borrow().clone());
        }
        layers.push(self.faces.borrow().clone());
        merge_layers(layers)
    }
}

/// Port of the flatten_* merge helpers: items of earlier layers that are
/// overridden in a later layer are dropped, order is preserved otherwise.
pub fn merge_layers<K: Clone + Eq + std::hash::Hash, V: Clone>(layers: Vec<IndexMap<K, V>>) -> Vec<(K, V)> {
    let mut res: Vec<(K, V)> = Vec::new();
    for layer in layers {
        res.retain(|(k, _)| !layer.contains_key(k));
        res.extend(layer.into_iter());
    }
    res
}
