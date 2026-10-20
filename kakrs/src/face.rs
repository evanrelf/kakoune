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
    // A base before the background (e.g. "@a,b") makes C++ build a reversed
    // StringView for the background color, which is undefined behaviour
    // (it crashes), report the description as invalid instead.
    if bg_it != end && colors_end <= bg_it {
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
            // With a base before the attributes (e.g. "red@a+b") C++ reads
            // past the end of the description, finding the terminating nul
            // of the String in practice: report it as an invalid attribute.
            let c = if attr_it < end { facedesc[attr_it] } else { 0 };
            let attr = match c {
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

#[cfg(test)]
mod tests {
    use super::*;
    use hegel::generators as gs;
    use hegel::{Generator as _, TestCase};

    fn named_color() -> impl hegel::PrintableGenerator<Color> {
        gs::sampled_from(COLOR_VALUES.to_vec()).map(Color::named)
    }

    fn rgb_color() -> impl hegel::PrintableGenerator<Color> {
        hegel::compose!(|tc| {
            let r = tc.draw(gs::integers::<u8>());
            let g = tc.draw(gs::integers::<u8>());
            let b = tc.draw(gs::integers::<u8>());
            let a = tc.draw(hegel::one_of!(gs::just(255u8), gs::integers::<u8>().min_value(RGB)));
            Color::rgba(r, g, b, a).unwrap()
        })
    }

    fn color() -> impl hegel::PrintableGenerator<Color> {
        hegel::one_of!(named_color(), rgb_color())
    }

    const ATTRIBUTES: [Attribute; 13] = [
        Attribute::Underline,
        Attribute::CurlyUnderline,
        Attribute::DoubleUnderline,
        Attribute::Reverse,
        Attribute::Blink,
        Attribute::Bold,
        Attribute::Dim,
        Attribute::Italic,
        Attribute::Strikethrough,
        Attribute::FinalFg,
        Attribute::FinalBg,
        Attribute::FinalAttr,
        Attribute::Final,
    ];

    fn attributes() -> impl hegel::PrintableGenerator<Attribute> {
        gs::vecs(gs::sampled_from(ATTRIBUTES.to_vec()))
            .map(|v| v.into_iter().fold(Attribute::Normal, |a, b| a | b))
    }

    fn face() -> impl hegel::PrintableGenerator<Face> {
        hegel::compose!(|tc| {
            Face {
                fg: tc.draw(color()),
                bg: tc.draw(color()),
                attributes: tc.draw(attributes()),
                underline: tc.draw(color()),
            }
        })
    }

    #[hegel::test(test_cases = 1000)]
    fn str_to_color_inverts_color_to_string(tc: TestCase) {
        let c = tc.draw(color());
        assert_eq!(str_to_color(&color_to_string(c)).unwrap(), c);
    }

    #[hegel::test(test_cases = 1000)]
    fn rgb_strings_roundtrip_case_insensitively(tc: TestCase) {
        let bytes = tc.draw(gs::vecs(gs::integers::<u8>()).min_size(3).max_size(4));
        let alpha = bytes.len() == 4;
        tc.assume(!alpha || bytes[3] >= RGB);
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let upper = tc.draw(gs::booleans());
        let s = format!("{}:{}", if alpha { "rgba" } else { "rgb" }, if upper { hex.to_uppercase() } else { hex.clone() });
        let c = str_to_color(s.as_bytes()).unwrap();
        assert_eq!((c.r, c.g, c.b), (bytes[0], bytes[1], bytes[2]));
        assert_eq!(c.a, if alpha { bytes[3] } else { 255 });
        // printing normalizes to lowercase, and to rgb: for opaque colors
        let expected = if alpha && bytes[3] != 255 { format!("rgba:{hex}") } else { format!("rgb:{}", &hex[..6]) };
        assert_eq!(color_to_string(c), expected.as_bytes());
    }

    #[hegel::test(test_cases = 1000)]
    fn rgba_with_low_alpha_is_rejected(tc: TestCase) {
        let a = tc.draw(gs::integers::<u8>().max_value(RGB - 1));
        assert!(str_to_color(format!("rgba:123456{a:02x}").as_bytes()).is_err());
        assert!(Color::rgba(1, 2, 3, a).is_err());
    }

    #[hegel::test(test_cases = 1000)]
    fn invalid_color_strings_are_rejected(tc: TestCase) {
        let s = tc.draw(hegel::one_of!(
            gs::binary(),
            gs::from_regex("rgba?:[0-9a-fA-Fg-zG-Z]{0,9}").fullmatch(true).map(String::into_bytes),
        ));
        let valid = is_color_name(&s)
            || (s.len() == 10 && s.starts_with(b"rgb:") && s[4..].iter().all(u8::is_ascii_hexdigit))
            || (s.len() == 13
                && s.starts_with(b"rgba:")
                && s[5..].iter().all(u8::is_ascii_hexdigit)
                && u8::from_str_radix(std::str::from_utf8(&s[11..]).unwrap(), 16).unwrap() >= RGB);
        assert_eq!(str_to_color(&s).is_ok(), valid);
    }

    #[hegel::test(test_cases = 1000)]
    fn parse_face_inverts_face_to_string(tc: TestCase) {
        let f = tc.draw(face());
        let spec = parse_face(&face_to_string(&f)).unwrap();
        assert_eq!(spec, FaceSpec { face: f, base: Vec::new() });
    }

    #[hegel::test(test_cases = 1000)]
    fn parse_face_with_base(tc: TestCase) {
        let f = tc.draw(face());
        let base = tc.draw(gs::from_regex("[A-Za-z0-9_]*").fullmatch(true));
        let desc = [face_to_string(&f), b"@".to_vec(), base.clone().into_bytes()].concat();
        assert_eq!(parse_face(&desc).unwrap(), FaceSpec { face: f, base: base.into_bytes() });
    }

    #[hegel::test(test_cases = 1000)]
    fn word_face_descriptions_are_base_names(tc: TestCase) {
        let name = tc.draw(gs::from_regex("[A-Za-z0-9_]*").fullmatch(true));
        let spec = parse_face(name.as_bytes()).unwrap();
        if is_color_name(name.as_bytes()) {
            assert_eq!(spec.face.fg, str_to_color(name.as_bytes()).unwrap());
            assert!(spec.base.is_empty());
        } else {
            assert_eq!(spec, FaceSpec { face: Face::default(), base: name.into_bytes() });
        }
    }

    #[hegel::test(test_cases = 1000)]
    fn unknown_attribute_is_rejected(tc: TestCase) {
        let f = tc.draw(face());
        let c = tc.draw(gs::integers::<u8>().filter(|c| !b"ucUrbBdisfgaF@".contains(c)));
        let desc = [face_to_string(&f), b"+".to_vec(), vec![c]].concat();
        assert!(parse_face(&desc).is_err());
    }

    /// Descriptions built from the face syntax's tokens, often malformed
    fn face_desc() -> impl hegel::PrintableGenerator<Vec<u8>> {
        hegel::one_of!(
            gs::binary(),
            gs::vecs(gs::sampled_from(vec![
                &b"red"[..], b"rgb:ff0000", b"rgba:00ff0080", b",", b"+", b"@", b"b", b"u", b"F", b"x", b"default", b"_",
            ]))
            .map(|v| v.concat()),
        )
    }

    // C++ has undefined behaviour (crashes, or reads past the end) when '@'
    // comes before ',' or '+', the port must report an error, not panic.
    #[hegel::test(test_cases = 1000)]
    fn parse_face_never_panics(tc: TestCase) {
        let desc = tc.draw(face_desc());
        let _ = parse_face(&desc);
    }

    #[hegel::test(test_cases = 1000)]
    fn base_before_background_or_attributes_is_an_error(tc: TestCase) {
        let fg = tc.draw(hegel::one_of!(gs::just(String::new()), gs::just("red".to_string())));
        let base = tc.draw(gs::from_regex("[A-Za-z_]+").fullmatch(true));
        let tail = tc.draw(gs::sampled_from(vec![",blue", "+b", ",blue+u", "+"]));
        assert!(parse_face(format!("{fg}@{base}{tail}").as_bytes()).is_err());
    }

    // parsing is idempotent through printing
    #[hegel::test(test_cases = 1000)]
    fn face_to_string_of_parsed_face_reparses_identically(tc: TestCase) {
        let desc = tc.draw(face_desc());
        if let Ok(spec) = parse_face(&desc) {
            assert_eq!(parse_face(&face_to_string(&spec.face)).unwrap().face, spec.face);
        }
    }

    #[hegel::test(test_cases = 1000)]
    fn attributes_to_string_roundtrips(tc: TestCase) {
        let a = tc.draw(attributes());
        let s = attributes_to_string(a);
        assert_eq!(s.is_empty(), a == Attribute::Normal);
        let parsed = parse_face(&[&b"red"[..], &s].concat()).unwrap();
        assert_eq!(parsed.face.attributes, a);
    }

    #[hegel::test(test_cases = 1000)]
    fn merging_default_face_is_identity(tc: TestCase) {
        let f = tc.draw(face());
        assert_eq!(merge_faces(&f, &Face::default()), f);
        assert_eq!(merge_faces(&Face::default(), &f), f);
    }

    #[hegel::test(test_cases = 1000)]
    fn final_colors_win(tc: TestCase) {
        let base = tc.draw(face());
        let face = tc.draw(face());
        let merged = merge_faces(&base, &face);
        if face.attributes.has(Attribute::FinalFg) {
            assert_eq!(merged.fg, face.fg);
        } else if base.attributes.has(Attribute::FinalFg) {
            assert_eq!(merged.fg, base.fg);
        }
        if face.attributes.has(Attribute::FinalBg) {
            assert_eq!(merged.bg, face.bg);
        } else if base.attributes.has(Attribute::FinalBg) {
            assert_eq!(merged.bg, base.bg);
        }
    }

    #[hegel::test(test_cases = 1000)]
    fn non_final_attributes_are_combined(tc: TestCase) {
        let base = tc.draw(face());
        let face = tc.draw(face());
        let merged = merge_faces(&base, &face);
        if face.attributes.has(Attribute::FinalAttr) {
            assert_eq!(merged.attributes, face.attributes | (base.attributes & Attribute::Final));
        } else if base.attributes.has(Attribute::FinalAttr) {
            assert_eq!(merged.attributes, base.attributes);
        } else {
            assert_eq!(merged.attributes, face.attributes | base.attributes);
        }
    }

    #[hegel::test(test_cases = 1000)]
    fn opaque_or_named_colors_override(tc: TestCase) {
        let base = tc.draw(face());
        let mut face = tc.draw(face());
        face.attributes = Attribute::Normal;
        tc.assume(!base.attributes.has(Attribute::FinalFg));
        tc.assume(face.fg != Color::DEFAULT && (!face.fg.is_rgb() || face.fg.a == 255 || !base.fg.is_rgb()));
        assert_eq!(merge_faces(&base, &face).fg, face.fg);
    }

    // Blending a translucent color over an rgb one gives a convex combination
    // of the two colors, and a result at least as opaque as either.
    #[hegel::test(test_cases = 1000)]
    fn alpha_blending_is_between_both_colors(tc: TestCase) {
        let base_color = tc.draw(rgb_color());
        let color = tc.draw(rgb_color().filter(|c| c.a != 255));
        let base = Face { fg: base_color, ..Face::default() };
        let face = Face { fg: color, ..Face::default() };
        let res = merge_faces(&base, &face).fg;
        for (r, b, c) in [(res.r, base_color.r, color.r), (res.g, base_color.g, color.g), (res.b, base_color.b, color.b)] {
            assert!(b.min(c) <= r && r <= b.max(c), "{r} not between {b} and {c}");
        }
        assert!(res.a >= color.a.max(base_color.a));
    }
}
