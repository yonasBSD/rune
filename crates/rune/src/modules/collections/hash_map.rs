use core::fmt::{self, Write};

use crate::no_std::collections;
use crate::no_std::prelude::*;

use crate as rune;
use crate::runtime::{FromValue, Iterator, Key, Protocol, Value, VmErrorKind, VmResult};
use crate::{Any, ContextError, Module};

pub(super) fn setup(module: &mut Module) -> Result<(), ContextError> {
    module.ty::<HashMap>()?;
    module.function_meta(HashMap::new)?;
    module.function_meta(HashMap::with_capacity)?;
    module.function_meta(HashMap::len)?;
    module.function_meta(HashMap::insert)?;
    module.function_meta(HashMap::get)?;
    module.function_meta(HashMap::contains_key)?;
    module.function_meta(HashMap::remove)?;
    module.function_meta(HashMap::clear)?;
    module.function_meta(HashMap::is_empty)?;
    module.function_meta(HashMap::iter)?;
    module.function_meta(HashMap::keys)?;
    module.function_meta(HashMap::values)?;
    module.function_meta(HashMap::extend)?;
    module.function_meta(from)?;
    module.function_meta(clone)?;
    module.associated_function(Protocol::INTO_ITER, HashMap::__rune_fn__iter)?;
    module.associated_function(Protocol::INDEX_SET, HashMap::index_set)?;
    module.associated_function(Protocol::INDEX_GET, HashMap::index_get)?;
    module.associated_function(Protocol::STRING_DEBUG, HashMap::string_debug)?;
    module.associated_function(Protocol::PARTIAL_EQ, HashMap::partial_eq)?;
    module.associated_function(Protocol::EQ, HashMap::eq)?;
    Ok(())
}

#[derive(Any, Clone)]
#[rune(module = crate, item = ::std::collections)]
pub(crate) struct HashMap {
    map: collections::HashMap<Key, Value>,
}

impl HashMap {
    /// Creates an empty `HashMap`.
    ///
    /// The hash map is initially created with a capacity of 0, so it will not
    /// allocate until it is first inserted into.
    ///
    /// # Examples
    ///
    /// ```rune
    /// use std::collections::HashMap;
    /// let map = HashMap::new();
    /// ```
    #[rune::function(path = Self::new)]
    fn new() -> Self {
        Self {
            map: collections::HashMap::new(),
        }
    }

    /// Creates an empty `HashMap` with at least the specified capacity.
    ///
    /// The hash map will be able to hold at least `capacity` elements without
    /// reallocating. This method is allowed to allocate for more elements than
    /// `capacity`. If `capacity` is 0, the hash map will not allocate.
    ///
    /// # Examples
    ///
    /// ```rune
    /// use std::collections::HashMap;
    /// let map = HashMap::with_capacity(10);
    /// ```
    #[rune::function(path = Self::with_capacity)]
    fn with_capacity(capacity: usize) -> Self {
        Self {
            map: collections::HashMap::with_capacity(capacity),
        }
    }

    /// Returns the number of elements in the map.
    ///
    /// # Examples
    ///
    /// ```rune
    /// use std::collections::HashMap;
    ///
    /// let a = HashMap::new();
    /// assert_eq!(a.len(), 0);
    /// a.insert(1, "a");
    /// assert_eq!(a.len(), 1);
    /// ```
    #[rune::function]
    fn len(&self) -> usize {
        self.map.len()
    }

    /// Returns the number of elements the map can hold without reallocating.
    ///
    /// This number is a lower bound; the `HashMap<K, V>` might be able to hold
    /// more, but is guaranteed to be able to hold at least this many.
    ///
    /// # Examples
    ///
    /// ```rune
    /// use std::collections::HashMap;
    /// let map = HashMap::with_capacity(100);
    /// assert!(map.capacity() >= 100);
    /// ```
    #[rune::function]
    fn capacity(&self) -> usize {
        self.map.capacity()
    }

    /// Returns `true` if the map contains no elements.
    ///
    /// # Examples
    ///
    /// ```rune
    /// use std::collections::HashMap;
    ///
    /// let a = HashMap::new();
    /// assert!(a.is_empty());
    /// a.insert(1, "a");
    /// assert!(!a.is_empty());
    /// ```
    #[rune::function]
    fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    #[rune::function]
    fn iter(&self) -> Iterator {
        let iter = self.map.clone().into_iter();
        Iterator::from("std::collections::map::Iter", iter)
    }

    /// Inserts a key-value pair into the map.
    ///
    /// If the map did not have this key present, [`None`] is returned.
    ///
    /// If the map did have this key present, the value is updated, and the old
    /// value is returned. The key is not updated, though; this matters for
    /// types that can be `==` without being identical. See the [module-level
    /// documentation] for more.
    ///
    /// [module-level documentation]: crate::collections#insert-and-complex-keys
    ///
    /// # Examples
    ///
    /// ```rune
    /// use std::collections::HashMap;
    ///
    /// let map = HashMap::new();
    /// assert_eq!(map.insert(37, "a"), None);
    /// assert_eq!(map.is_empty(), false);
    ///
    /// map.insert(37, "b");
    /// assert_eq!(map.insert(37, "c"), Some("b"));
    /// assert_eq!(map[37], "c");
    /// ```
    #[rune::function]
    fn insert(&mut self, key: Key, value: Value) -> Option<Value> {
        self.map.insert(key, value)
    }

    /// Returns the value corresponding to the [`Key`].
    ///
    /// # Examples
    ///
    /// ```rune
    /// use std::collections::HashMap;
    ///
    /// let map = HashMap::new();
    /// map.insert(1, "a");
    /// assert_eq!(map.get(1), Some("a"));
    /// assert_eq!(map.get(2), None);
    /// ```
    #[rune::function]
    fn get(&self, key: Key) -> Option<Value> {
        self.map.get(&key).cloned()
    }

    /// Returns `true` if the map contains a value for the specified [`Key`].
    ///
    /// # Examples
    ///
    /// ```rune
    /// use std::collections::HashMap;
    ///
    /// let map = HashMap::new();
    /// map.insert(1, "a");
    /// assert_eq!(map.contains_key(1), true);
    /// assert_eq!(map.contains_key(2), false);
    /// ```
    #[rune::function]
    fn contains_key(&self, key: Key) -> bool {
        self.map.contains_key(&key)
    }

    /// Removes a key from the map, returning the value at the [`Key`] if the
    /// key was previously in the map.
    ///
    /// # Examples
    ///
    /// ```rune
    /// use std::collections::HashMap;
    ///
    /// let map = HashMap::new();
    /// map.insert(1, "a");
    /// assert_eq!(map.remove(1), Some("a"));
    /// assert_eq!(map.remove(1), None);
    /// ```
    #[rune::function]
    fn remove(&mut self, key: Key) -> Option<Value> {
        self.map.remove(&key)
    }

    /// Clears the map, removing all key-value pairs. Keeps the allocated memory
    /// for reuse.
    ///
    /// # Examples
    ///
    /// ```rune
    /// use std::collections::HashMap;
    ///
    /// let a = HashMap::new();
    /// a.insert(1, "a");
    /// a.clear();
    /// assert!(a.is_empty());
    /// ```
    #[rune::function]
    fn clear(&mut self) {
        self.map.clear()
    }

    /// An iterator visiting all keys in arbitrary order.
    ///
    /// # Examples
    ///
    /// ```rune
    /// use std::collections::HashMap;
    ///
    /// let map = HashMap::from([
    ///     ("a", 1),
    ///     ("b", 2),
    ///     ("c", 3),
    /// ]);
    ///
    /// let keys = map.keys().collect::<Vec>();
    /// keys.sort::<String>();
    /// assert_eq!(keys, ["a", "b", "c"]);
    /// ```
    ///
    /// # Performance
    ///
    /// In the current implementation, iterating over keys takes O(capacity)
    /// time instead of O(len) because it internally visits empty buckets too.
    #[rune::function]
    fn keys(&self) -> Iterator {
        let iter = self.map.keys().cloned().collect::<Vec<_>>().into_iter();
        Iterator::from("std::collections::map::Keys", iter)
    }

    /// An iterator visiting all values in arbitrary order.
    ///
    /// # Examples
    ///
    /// ```rune
    /// use std::collections::HashMap;
    ///
    /// let map = HashMap::from([
    ///     ("a", 1),
    ///     ("b", 2),
    ///     ("c", 3),
    /// ]);
    ///
    /// let values = map.values().collect::<Vec>();
    /// values.sort::<i64>();
    /// assert_eq!(values, [1, 2, 3]);
    /// ```
    ///
    /// # Performance
    ///
    /// In the current implementation, iterating over values takes O(capacity)
    /// time instead of O(len) because it internally visits empty buckets too.
    #[rune::function]
    fn values(&self) -> Iterator {
        let iter = self.map.values().cloned().collect::<Vec<_>>().into_iter();
        Iterator::from("std::collections::map::Values", iter)
    }

    /// Extend this map from an iterator.
    ///
    /// # Examples
    ///
    /// ```rune
    /// use std::collections::HashMap;
    ///
    /// let map = HashMap::new();
    ///
    /// map.extend([
    ///     ("a", 1),
    ///     ("b", 2),
    ///     ("c", 3),
    /// ]);
    /// ```
    #[rune::function]
    fn extend(&mut self, value: Value) -> VmResult<()> {
        let mut it = vm_try!(value.into_iter());

        while let Some(value) = vm_try!(it.next()) {
            let (key, value) = vm_try!(<(Key, Value)>::from_value(value));
            self.map.insert(key, value);
        }

        VmResult::Ok(())
    }

    pub(crate) fn from_iter(mut it: Iterator) -> VmResult<Self> {
        let mut map = collections::HashMap::new();

        while let Some(value) = vm_try!(it.next()) {
            let (key, value) = vm_try!(<(Key, Value)>::from_value(value));
            map.insert(key, value);
        }

        VmResult::Ok(Self { map })
    }

    fn index_set(&mut self, key: Key, value: Value) {
        let _ = self.map.insert(key, value);
    }

    fn index_get(&self, key: Key) -> VmResult<Value> {
        use crate::runtime::TypeOf;

        let value = vm_try!(self.map.get(&key).ok_or_else(|| {
            VmErrorKind::MissingIndexKey {
                target: Self::type_info(),
                index: key,
            }
        }));

        VmResult::Ok(value.clone())
    }

    fn string_debug(&self, s: &mut String) -> fmt::Result {
        write!(s, "{:?}", self.map)
    }

    fn partial_eq(&self, other: &Self) -> VmResult<bool> {
        if self.map.len() != other.map.len() {
            return VmResult::Ok(false);
        }

        for (k, v) in self.map.iter() {
            let Some(v2) = other.map.get(k) else {
                return VmResult::Ok(false);
            };

            if !vm_try!(Value::partial_eq(v, v2)) {
                return VmResult::Ok(false);
            }
        }

        VmResult::Ok(true)
    }

    fn eq(&self, other: &Self) -> VmResult<bool> {
        if self.map.len() != other.map.len() {
            return VmResult::Ok(false);
        }

        for (k, v) in self.map.iter() {
            let Some(v2) = other.map.get(k) else {
                return VmResult::Ok(false);
            };

            if !vm_try!(Value::eq(v, v2)) {
                return VmResult::Ok(false);
            }
        }

        VmResult::Ok(true)
    }
}

/// Convert a hashmap from a `value`.
///
/// The hashmap can be converted from anything that implements the [`INTO_ITER`]
/// protocol, and each item produces should be a tuple pair.
#[rune::function(path = HashMap::from)]
fn from(value: Value) -> VmResult<HashMap> {
    HashMap::from_iter(vm_try!(value.into_iter()))
}

/// Clone the map.
///
/// # Examples
///
/// ```rune
/// use std::collections::HashMap;
///
/// let a = HashMap::from([("a", 1), ("b", 2)]);
/// let b = a.clone();
///
/// b.insert("c", 3);
///
/// assert_eq!(a.len(), 2);
/// assert_eq!(b.len(), 3);
/// ```
#[rune::function(instance)]
fn clone(this: &HashMap) -> HashMap {
    this.clone()
}
