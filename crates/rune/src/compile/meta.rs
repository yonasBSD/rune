//! Compiler metadata for Rune.

use crate::collections::HashSet;
use crate::compile::{Item, Location, Visibility};
use crate::parse::Id;
use crate::runtime::{ConstValue, TypeCheck};
use crate::Hash;
use std::fmt;
use std::path::Path;
use std::sync::Arc;

/// Metadata about a variable captured by a clsoreu.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CaptureMeta {
    /// Identity of the captured variable.
    pub ident: Box<str>,
}

/// Information on a compile sourc.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SourceMeta {
    /// The location of the compile source.
    pub location: Location,
    /// The optional source id where the meta is declared.
    pub path: Option<Box<Path>>,
}

/// Metadata about a compiled unit.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Meta {
    /// The item of the returned compile meta.
    pub item: Arc<ItemMeta>,
    /// The kind of the compile meta.
    pub kind: MetaKind,
    /// The source of the meta.
    pub source: Option<SourceMeta>,
}

impl Meta {
    /// Get the type hash of the base type (the one to type check for) for the
    /// given compile meta.
    ///
    /// Note: Variants cannot be used for type checking, you should instead
    /// compare them against the enum type.
    pub fn type_hash_of(&self) -> Option<Hash> {
        match &self.kind {
            MetaKind::UnitStruct { type_hash, .. } => Some(*type_hash),
            MetaKind::TupleStruct { type_hash, .. } => Some(*type_hash),
            MetaKind::Struct { type_hash, .. } => Some(*type_hash),
            MetaKind::Enum { type_hash, .. } => Some(*type_hash),
            MetaKind::Function { type_hash, .. } => Some(*type_hash),
            MetaKind::Closure { type_hash, .. } => Some(*type_hash),
            MetaKind::AsyncBlock { type_hash, .. } => Some(*type_hash),
            MetaKind::UnitVariant { .. } => None,
            MetaKind::TupleVariant { .. } => None,
            MetaKind::StructVariant { .. } => None,
            MetaKind::Const { .. } => None,
            MetaKind::ConstFn { .. } => None,
            MetaKind::Import { .. } => None,
        }
    }

    /// Treat the current meta as a tuple and get the number of arguments it
    /// should receive and the type check that applies to it.
    pub(crate) fn as_tuple(&self) -> Option<(usize, TypeCheck)> {
        match &self.kind {
            MetaKind::UnitStruct { type_hash, .. } => {
                let type_check = TypeCheck::Type(*type_hash);
                Some((0, type_check))
            }
            MetaKind::TupleStruct {
                tuple, type_hash, ..
            } => {
                let type_check = TypeCheck::Type(*type_hash);
                Some((tuple.args, type_check))
            }
            MetaKind::UnitVariant { type_hash, .. } => {
                let type_check = TypeCheck::Variant(*type_hash);
                Some((0, type_check))
            }
            MetaKind::TupleVariant {
                tuple, type_hash, ..
            } => {
                let type_check = TypeCheck::Variant(*type_hash);
                Some((tuple.args, type_check))
            }
            _ => None,
        }
    }
}

impl fmt::Display for Meta {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            MetaKind::UnitStruct { .. } => {
                write!(fmt, "struct {}", self.item.item)?;
            }
            MetaKind::TupleStruct { .. } => {
                write!(fmt, "struct {}", self.item.item)?;
            }
            MetaKind::Struct { .. } => {
                write!(fmt, "struct {}", self.item.item)?;
            }
            MetaKind::UnitVariant { .. } => {
                write!(fmt, "unit variant {}", self.item.item)?;
            }
            MetaKind::TupleVariant { .. } => {
                write!(fmt, "variant {}", self.item.item)?;
            }
            MetaKind::StructVariant { .. } => {
                write!(fmt, "variant {}", self.item.item)?;
            }
            MetaKind::Enum { .. } => {
                write!(fmt, "enum {}", self.item.item)?;
            }
            MetaKind::Function { .. } => {
                write!(fmt, "fn {}", self.item.item)?;
            }
            MetaKind::Closure { .. } => {
                write!(fmt, "closure {}", self.item.item)?;
            }
            MetaKind::AsyncBlock { .. } => {
                write!(fmt, "async block {}", self.item.item)?;
            }
            MetaKind::Const { .. } => {
                write!(fmt, "const {}", self.item.item)?;
            }
            MetaKind::ConstFn { .. } => {
                write!(fmt, "const fn {}", self.item.item)?;
            }
            MetaKind::Import { .. } => {
                write!(fmt, "import {}", self.item.item)?;
            }
        }

        Ok(())
    }
}

/// Compile-time metadata kind about a unit.
#[derive(Debug, Clone)]
pub enum MetaKind {
    /// Metadata about an object.
    UnitStruct {
        /// The type hash associated with this meta kind.
        type_hash: Hash,
        /// The underlying object.
        empty: EmptyMeta,
    },
    /// Metadata about a tuple.
    TupleStruct {
        /// The type hash associated with this meta kind.
        type_hash: Hash,
        /// The underlying tuple.
        tuple: TupleMeta,
    },
    /// Metadata about an object.
    Struct {
        /// The type hash associated with this meta kind.
        type_hash: Hash,
        /// The underlying object.
        object: StructMeta,
    },
    /// Metadata about an empty variant.
    UnitVariant {
        /// The type hash associated with this meta kind.
        type_hash: Hash,
        /// The item of the enum.
        enum_item: Item,
        /// The underlying empty.
        empty: EmptyMeta,
    },
    /// Metadata about a tuple variant.
    TupleVariant {
        /// The type hash associated with this meta item.
        type_hash: Hash,
        /// The item of the enum.
        enum_item: Item,
        /// The underlying tuple.
        tuple: TupleMeta,
    },
    /// Metadata about a variant object.
    StructVariant {
        /// The type hash associated with this meta kind.
        type_hash: Hash,
        /// The item of the enum.
        enum_item: Item,
        /// The underlying object.
        object: StructMeta,
    },
    /// An enum item.
    Enum {
        /// The type hash associated with this meta kind.
        type_hash: Hash,
    },
    /// A function declaration.
    Function {
        /// The type hash associated with this meta kind.
        type_hash: Hash,

        /// Whether this function has a `#[test]` annotation
        is_test: bool,

        /// Whether this function has a `#[bench]` annotation.
        is_bench: bool,
    },
    /// A closure.
    Closure {
        /// The type hash associated with this meta kind.
        type_hash: Hash,
        /// Sequence of captured variables.
        captures: Arc<[CaptureMeta]>,
        /// If the closure moves its environment.
        do_move: bool,
    },
    /// An async block.
    AsyncBlock {
        /// The span where the async block is declared.
        type_hash: Hash,
        /// Sequence of captured variables.
        captures: Arc<[CaptureMeta]>,
        /// If the async block moves its environment.
        do_move: bool,
    },
    /// The constant expression.
    Const {
        /// The evaluated constant value.
        const_value: ConstValue,
    },
    /// A constant function.
    ConstFn {
        /// Opaque identifier for the constant function.
        id: Id,

        /// Whether this function has a test annotation
        is_test: bool,
    },
    /// Purely an import.
    Import {
        /// The module of the target.
        module: Arc<ModMeta>,
        /// The location of the import.
        location: Location,
        /// The imported target.
        target: Item,
    },
}

/// The metadata about an empty type.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct EmptyMeta {
    /// Hash of the constructor function.
    pub hash: Hash,
}

/// The metadata about a struct.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct StructMeta {
    /// Fields associated with the type.
    pub fields: HashSet<Box<str>>,
}

/// The metadata about a tuple.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TupleMeta {
    /// The number of arguments the variant takes.
    pub args: usize,
    /// Hash of the constructor function.
    pub hash: Hash,
}

/// Item and the module that the item belongs to.
#[derive(Default, Debug, Clone)]
#[non_exhaustive]
pub struct ItemMeta {
    /// The id of the item.
    pub id: Id,
    /// The location of the item.
    pub location: Location,
    /// The name of the item.
    pub item: Item,
    /// The visibility of the item.
    pub visibility: Visibility,
    /// The module associated with the item.
    pub module: Arc<ModMeta>,
}

impl ItemMeta {
    /// Test if the item is public (and should be exported).
    pub fn is_public(&self) -> bool {
        self.visibility.is_public() && self.module.is_public()
    }
}

impl From<Item> for ItemMeta {
    fn from(item: Item) -> Self {
        Self {
            id: Default::default(),
            location: Default::default(),
            item,
            visibility: Default::default(),
            module: Default::default(),
        }
    }
}

/// Module, its item and its visibility.
#[derive(Default, Debug)]
#[non_exhaustive]
pub struct ModMeta {
    /// The location of the module.
    pub location: Location,
    /// The item of the module.
    pub item: Item,
    /// The visibility of the module.
    pub visibility: Visibility,
    /// The kind of the module.
    pub parent: Option<Arc<ModMeta>>,
}

impl ModMeta {
    /// Test if the module recursively is public.
    pub fn is_public(&self) -> bool {
        let mut current = Some(self);

        while let Some(m) = current.take() {
            if !m.visibility.is_public() {
                return false;
            }

            current = m.parent.as_deref();
        }

        true
    }
}
