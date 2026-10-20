//! Port of flags.hh: a small bitflags macro

/// Declares a bitflags newtype:
/// kak_flags! { pub struct Foo: u32 { const A = 1; const B = 2; } }
#[macro_export]
macro_rules! kak_flags {
    ($(#[$meta:meta])* $vis:vis struct $name:ident : $t:ty { $(const $flag:ident = $value:expr;)* }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Debug, PartialOrd, Ord)]
        $vis struct $name(pub $t);
        #[allow(non_upper_case_globals)]
        impl $name {
            $(pub const $flag: $name = $name($value);)*
            pub const fn empty() -> Self { $name(0) }
            pub const fn bits(self) -> $t { self.0 }
            pub const fn is_empty(self) -> bool { self.0 == 0 }
            /// true if any bit of other is set in self (C++ `flags & other`)
            pub const fn has(self, other: $name) -> bool { (self.0 & other.0) != 0 }
            /// true if all bits of other are set in self
            pub const fn contains(self, other: $name) -> bool { (self.0 & other.0) == other.0 }
            pub fn insert(&mut self, other: $name) { self.0 |= other.0; }
            pub fn remove(&mut self, other: $name) { self.0 &= !other.0; }
            pub fn set(&mut self, other: $name, value: bool) { if value { self.insert(other) } else { self.remove(other) } }
        }
        impl std::ops::BitOr for $name { type Output = $name; fn bitor(self, o: $name) -> $name { $name(self.0 | o.0) } }
        impl std::ops::BitOrAssign for $name { fn bitor_assign(&mut self, o: $name) { self.0 |= o.0; } }
        impl std::ops::BitAnd for $name { type Output = $name; fn bitand(self, o: $name) -> $name { $name(self.0 & o.0) } }
        impl std::ops::BitAndAssign for $name { fn bitand_assign(&mut self, o: $name) { self.0 &= o.0; } }
        impl std::ops::BitXor for $name { type Output = $name; fn bitxor(self, o: $name) -> $name { $name(self.0 ^ o.0) } }
        impl std::ops::Not for $name { type Output = $name; fn not(self) -> $name { $name(!self.0) } }
    };
}
