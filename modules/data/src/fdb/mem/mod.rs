//! Low-Level API that is suitable for non-little-endian machines
//!
//! This is the default in-memory API the the FDB file format. It is useful
//! for batch processing because it is fast and only loads the values that
//! are accessed.
//!
//! The reference structures in this module all implement [`Copy`].
//!
//! The only limitation is, that all references are bounded by the lifetime
//! of the original database buffer.
use assembly_core::buffer::{self, Repr, LEI64};
use buffer::CastError;
use memchr::memchr;

mod c;
use super::{
    core::ValueType,
    ro::{slice::Latin1Str, Handle, RefHandle},
};
use c::{
    FDBBucketHeaderC, FDBColumnHeaderC, FDBFieldDataC, FDBHeaderC, FDBRowHeaderC,
    FDBRowHeaderListEntryC, FDBTableDataHeaderC, FDBTableDefHeaderC, FDBTableHeaderC,
};
use std::{borrow::Cow, cmp::Ordering};

fn get_latin1_str(buf: &[u8], offset: u32) -> &Latin1Str {
    let (_, haystack) = buf.split_at(offset as usize);
    if let Some(end) = memchr(0, haystack) {
        let (content, _) = haystack.split_at(end);
        unsafe { Latin1Str::from_bytes_unchecked(content) }
    } else {
        panic!(
            "Offset {} is supposed to be a string but does not have a null-terminator",
            offset
        );
    }
}

/// A complete in-memory read-only database
///
/// This struct contains a reference to the complete byte buffer of an FDB file.
#[derive(Copy, Clone)]
pub struct Database<'a> {
    inner: Handle<'a, ()>,
}

impl<'a> Database<'a> {
    /// Create a new database reference
    pub fn new(buf: &'a [u8]) -> Self {
        let inner = Handle::new(buf);
        Self { inner }
    }

    /// Get a reference to the header
    pub fn header(self) -> Result<Header<'a>, CastError> {
        let inner = self.inner.try_map_cast(0)?;
        Ok(Header { inner })
    }

    /// Returns a reference to the tables array
    pub fn tables(self) -> Result<Tables<'a>, CastError> {
        let header = self.header()?;
        let tables = header.tables()?;
        Ok(tables)
    }
}

#[derive(Copy, Clone)]
/// Reference to the tables array
pub struct Header<'a> {
    inner: RefHandle<'a, FDBHeaderC>,
}

impl<'a> Header<'a> {
    fn tables(self) -> Result<Tables<'a>, CastError> {
        let header = self.inner.map_extract();
        let inner = self.inner.try_map_cast_array(header.into_raw().tables)?;
        Ok(Tables { inner })
    }
}

fn map_table_header<'a>(handle: RefHandle<'a, FDBTableHeaderC>) -> Result<Table<'a>, CastError> {
    let table_header = handle.into_raw().extract();

    let def_header: &'a FDBTableDefHeaderC =
        handle.buf().try_cast(table_header.table_def_header_addr)?;
    let def_header = def_header.extract();

    let data_header: &'a FDBTableDataHeaderC =
        handle.buf().try_cast(table_header.table_data_header_addr)?;
    let data_header = data_header.extract();

    let name = get_latin1_str(handle.buf().as_bytes(), def_header.table_name_addr);

    let columns: RefHandle<'a, [FDBColumnHeaderC]> =
        handle.try_map_cast_slice(def_header.column_header_list_addr, def_header.column_count)?;

    let buckets: RefHandle<'a, [FDBBucketHeaderC]> =
        handle.try_map_cast_array(data_header.buckets)?;

    Ok(Table::new(handle.wrap(InnerTable {
        name,
        columns: columns.raw(),
        buckets: buckets.raw(),
    })))
}

/// Compares two name strings
///
/// ## Safety
///
/// This panics if name_bytes does not contains a null terminator
fn compare_bytes(bytes: &[u8], name_bytes: &[u8]) -> Ordering {
    for i in 0..bytes.len() {
        match name_bytes[i].cmp(&bytes[i]) {
            Ordering::Equal => {}
            Ordering::Less => {
                // the null terminator is a special case of this one
                return Ordering::Less;
            }
            Ordering::Greater => {
                return Ordering::Greater;
            }
        }
    }
    if name_bytes[bytes.len()] == 0 {
        Ordering::Equal
    } else {
        Ordering::Greater
    }
}

#[derive(Copy, Clone)]
/// Reference to the tables array
pub struct Tables<'a> {
    inner: RefHandle<'a, [FDBTableHeaderC]>,
}

impl<'a> Tables<'a> {
    /// Returns the length of the tables array
    pub fn len(self) -> usize {
        self.inner.into_raw().len()
    }

    /// Checks whether the tables array is empty
    pub fn is_empty(self) -> bool {
        self.inner.into_raw().len() == 0
    }

    /// Get the table reference at the specified index
    pub fn get(self, index: usize) -> Option<Result<Table<'a>, CastError>> {
        self.inner.get(index).map(map_table_header)
    }

    /// Get an interator over all tables
    pub fn iter(&self) -> impl Iterator<Item = Result<Table<'a>, CastError>> {
        TableIter {
            inner: self.inner.map_val(<[FDBTableHeaderC]>::iter),
        }
    }

    /// Get a table by its name
    pub fn by_name(&self, name: &str) -> Option<Result<Table<'a>, CastError>> {
        let bytes = name.as_bytes();
        self.inner
            .into_raw()
            .binary_search_by(|table_header| {
                let def_header_addr = table_header.table_def_header_addr.extract();
                let def_header = buffer::cast::<FDBTableDefHeaderC>(
                    self.inner.buf().as_bytes(),
                    def_header_addr,
                );

                let name_addr = def_header.table_name_addr.extract() as usize;
                let name_bytes = &self.inner.buf().as_bytes()[name_addr..];

                compare_bytes(bytes, name_bytes)
            })
            .ok()
            .and_then(|index| self.get(index))
    }
}

#[allow(clippy::needless_lifetimes)] // <- clippy gets this wrong, presumably because of impl trait?
fn map_column_header<'a>(buf: &'a [u8]) -> impl Fn(&'a FDBColumnHeaderC) -> Column<'a> {
    move |header: &FDBColumnHeaderC| {
        let column_header = header.extract();
        let name = get_latin1_str(buf, column_header.column_name_addr);
        let domain = ValueType::from(column_header.column_data_type);

        Column { name, domain }
    }
}

fn get_row_header_list_entry(buf: &[u8], addr: u32) -> Option<&FDBRowHeaderListEntryC> {
    if addr == u32::MAX {
        None
    } else {
        Some(buffer::cast::<FDBRowHeaderListEntryC>(buf, addr))
    }
}

#[allow(clippy::needless_lifetimes)] // <- clippy gets this wrong
fn map_bucket_header<'a>(buf: &'a [u8]) -> impl Fn(&'a FDBBucketHeaderC) -> Bucket<'a> {
    move |header: &FDBBucketHeaderC| {
        let bucket_header = header.extract();
        let addr = bucket_header.row_header_list_head_addr;
        let first = get_row_header_list_entry(buf, addr);
        Bucket { buf, first }
    }
}

#[derive(Clone)]
/// An iterator over tables
pub struct TableIter<'a> {
    inner: Handle<'a, std::slice::Iter<'a, FDBTableHeaderC>>,
}

impl<'a> Iterator for TableIter<'a> {
    type Item = Result<Table<'a>, CastError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner
            .raw_mut()
            .next()
            .map(|raw| self.inner.wrap(raw))
            .map(map_table_header)
    }
}

#[derive(Copy, Clone)]
struct InnerTable<'a> {
    name: &'a Latin1Str,
    columns: &'a [FDBColumnHeaderC],
    buckets: &'a [FDBBucketHeaderC],
}

#[derive(Copy, Clone)]
/// Reference to a single table
pub struct Table<'a> {
    inner: Handle<'a, InnerTable<'a>>,
}

impl<'a> Table<'a> {
    fn new(inner: Handle<'a, InnerTable<'a>>) -> Self {
        Self { inner }
    }

    /// Get the undecoded name of the table
    pub fn name_raw(&self) -> &Latin1Str {
        self.inner.raw.name
    }

    /// Get the name of the table
    pub fn name(&self) -> Cow<str> {
        self.inner.raw.name.decode()
    }

    /// Get a list of rows by index
    pub fn index_iter(&self, id: u32) -> impl Iterator<Item = Row<'a>> {
        let bucket: usize = id as usize % self.bucket_count();
        self.bucket_at(bucket).into_iter().flat_map(move |b| {
            b.row_iter()
                .filter(move |r| r.field_at(0) == Some(Field::Integer(id as i32)))
        })
    }

    /// Get the column at the index
    ///
    /// **Note**: This does some computation, call only once per colum if possible
    pub fn column_at(&self, index: usize) -> Option<Column<'a>> {
        self.inner
            .raw
            .columns
            .get(index)
            .map(map_column_header(self.inner.buffer.as_bytes()))
    }

    /// Get the column iterator
    ///
    /// **Note**: This does some computation, call only once if possible
    pub fn column_iter(&self) -> impl Iterator<Item = Column<'a>> {
        self.inner
            .raw
            .columns
            .iter()
            .map(map_column_header(self.inner.buffer.as_bytes()))
    }

    /// The amount of columns in this table
    pub fn column_count(&self) -> usize {
        self.inner.raw.columns.len()
    }

    /// Get the bucket at the index
    ///
    /// **Note**: This does some computation, call only once per bucket if possible
    pub fn bucket_at(&self, index: usize) -> Option<Bucket<'a>> {
        self.inner
            .raw
            .buckets
            .get(index)
            .map(map_bucket_header(self.inner.buffer.as_bytes()))
    }

    /// Get the bucket iterator
    ///
    /// **Note**: This does some computation, call only once if possible
    pub fn bucket_iter(&self) -> impl Iterator<Item = Bucket<'a>> {
        self.inner
            .raw
            .buckets
            .iter()
            .map(map_bucket_header(self.inner.buffer.as_bytes()))
    }

    /// Get the amount of buckets
    pub fn bucket_count(&self) -> usize {
        self.inner.raw.buckets.len()
    }

    /// Get an iterator over all rows
    pub fn row_iter(&self) -> impl Iterator<Item = Row<'a>> {
        self.bucket_iter().map(|b| b.row_iter()).flatten()
    }
}

/// Reference to a column definition
pub struct Column<'a> {
    name: &'a Latin1Str,
    domain: ValueType,
}

impl<'a> Column<'a> {
    /// Returns the name of a column
    pub fn name(&self) -> Cow<'a, str> {
        self.name.decode()
    }

    /// Returns the default value type of the column
    pub fn value_type(&self) -> ValueType {
        self.domain
    }
}

/// Reference to a single bucket
#[derive(Debug)]
pub struct Bucket<'a> {
    buf: &'a [u8],
    first: Option<&'a FDBRowHeaderListEntryC>,
}

impl<'a> Bucket<'a> {
    /// Returns an iterator over all rows in this bucket
    pub fn row_iter(&self) -> RowHeaderIter<'a> {
        RowHeaderIter {
            buf: self.buf,
            next: self.first,
        }
    }

    /// Check whether the bucket is empty
    pub fn is_empty(&self) -> bool {
        self.first.is_none()
    }
}

/// Struct that implements [`Bucket::row_iter`].
pub struct RowHeaderIter<'a> {
    buf: &'a [u8],
    next: Option<&'a FDBRowHeaderListEntryC>,
}

impl<'a> Iterator for RowHeaderIter<'a> {
    type Item = Row<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(next) = self.next {
            let entry = next.extract();
            self.next = get_row_header_list_entry(self.buf, entry.row_header_list_next_addr);
            let row_header =
                buffer::cast::<FDBRowHeaderC>(self.buf, entry.row_header_addr).extract();

            let fields = buffer::cast_slice::<FDBFieldDataC>(
                self.buf,
                row_header.fields.base_offset,
                row_header.fields.count,
            );

            Some(Row {
                buf: self.buf,
                fields,
            })
        } else {
            None
        }
    }
}

#[derive(Copy, Clone)]
/// Reference to a single row
pub struct Row<'a> {
    buf: &'a [u8],
    fields: &'a [FDBFieldDataC],
}

#[allow(clippy::needless_lifetimes)] // <- clippy gets this wrong
fn map_field<'a>(buf: &'a [u8]) -> impl Fn(&'a FDBFieldDataC) -> Field<'a> {
    move |data: &FDBFieldDataC| {
        let data_type = ValueType::from(data.data_type.extract());
        let bytes = data.value.0;
        match data_type {
            ValueType::Nothing => Field::Nothing,
            ValueType::Integer => Field::Integer(i32::from_le_bytes(bytes)),
            ValueType::Float => Field::Float(f32::from_le_bytes(bytes)),
            ValueType::Text => {
                let addr = u32::from_le_bytes(bytes);
                let text = get_latin1_str(buf, addr);
                Field::Text(text)
            }
            ValueType::Boolean => Field::Boolean(bytes != [0, 0, 0, 0]),
            ValueType::BigInt => {
                let addr = u32::from_le_bytes(bytes);
                let val = buffer::cast::<LEI64>(buf, addr).extract();
                Field::BigInt(val)
            }
            ValueType::VarChar => {
                let addr = u32::from_le_bytes(bytes);
                let text = get_latin1_str(buf, addr);
                Field::VarChar(text)
            }
            ValueType::Unknown(i) => unimplemented!("Cannot read unknown value type {}", i),
        }
    }
}

impl<'a> Row<'a> {
    /// Get the field at the index
    pub fn field_at(&self, index: usize) -> Option<Field<'a>> {
        self.fields.get(index).map(map_field(self.buf))
    }

    /// Get the iterator over all fields
    pub fn field_iter(&self) -> impl Iterator<Item = Field<'a>> {
        self.fields.iter().map(map_field(self.buf))
    }

    /// Get the count of fields
    pub fn field_count(&self) -> usize {
        self.fields.len()
    }
}

#[derive(Debug, PartialEq)]
/// Value of or reference to a field value
pub enum Field<'a> {
    /// The `NULL` value
    Nothing,
    /// A 32 bit signed integer
    Integer(i32),
    /// A 32 bit IEEE floating point number
    Float(f32),
    /// A latin-1 encoded string
    Text(&'a Latin1Str),
    /// A boolean
    Boolean(bool),
    /// A 64 bit integer.
    BigInt(i64),
    /// Reference to a (base64 encoded?) byte array.
    VarChar(&'a Latin1Str),
}

impl<'a> Field<'a> {
    /// Returns `Some` with the value if the field contains an [`Field::Integer`].
    pub fn into_opt_integer(self) -> Option<i32> {
        if let Self::Integer(value) = self {
            Some(value)
        } else {
            None
        }
    }

    /// Returns `Some` with the value if the field contains a [`Field::Float`].
    pub fn into_opt_float(self) -> Option<f32> {
        if let Self::Float(value) = self {
            Some(value)
        } else {
            None
        }
    }

    /// Returns `Some` with the value if the field contains a [`Field::Text`].
    pub fn into_opt_text(self) -> Option<&'a Latin1Str> {
        if let Self::Text(value) = self {
            Some(value)
        } else {
            None
        }
    }

    /// Returns `Some` with the value if the field contains a [`Field::Boolean`].
    pub fn into_opt_boolean(self) -> Option<bool> {
        if let Self::Boolean(value) = self {
            Some(value)
        } else {
            None
        }
    }

    /// Returns `Some` with the value if the field contains a [`Field::BigInt`].
    pub fn into_opt_big_int(self) -> Option<i64> {
        if let Self::BigInt(value) = self {
            Some(value)
        } else {
            None
        }
    }

    /// Returns `Some` with the value if the field contains a [`Field::VarChar`].
    pub fn into_opt_varchar(self) -> Option<&'a Latin1Str> {
        if let Self::VarChar(value) = self {
            Some(value)
        } else {
            None
        }
    }
}