use assembly_core::buffer::{Unaligned, LEI64};
use derive_new::new;
use memchr::memchr;

mod c;
use super::{core::ValueType, de::slice::Latin1Str};
use c::{
    FDBBucketHeaderC, FDBColumnHeaderC, FDBFieldDataC, FDBHeaderC, FDBRowHeaderC,
    FDBRowHeaderListEntryC, FDBTableDataHeaderC, FDBTableDefHeaderC, FDBTableHeaderC,
};
use std::borrow::Cow;

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

#[derive(Copy, Clone, new)]
pub struct Database<'a> {
    buf: &'a [u8],
}

impl<'a> Database<'a> {
    pub fn tables(self) -> Tables<'a> {
        let header = FDBHeaderC::cast(self.buf, 0);
        let len = header.table_count.extract();
        let base = header.table_header_list_addr.extract();
        let slice = FDBTableHeaderC::cast_slice(self.buf, base, len);
        Tables {
            buf: self.buf,
            slice,
        }
    }
}

#[derive(Copy, Clone)]
pub struct Tables<'a> {
    buf: &'a [u8],
    slice: &'a [FDBTableHeaderC],
}

fn map_table_header<'a>(buf: &'a [u8]) -> impl Fn(&'a FDBTableHeaderC) -> Table<'a> {
    move |header: &'a FDBTableHeaderC| {
        let table_header = header.extract();

        let def_header =
            FDBTableDefHeaderC::cast(buf, table_header.table_def_header_addr).extract();
        let data_header =
            FDBTableDataHeaderC::cast(buf, table_header.table_data_header_addr).extract();

        let name = get_latin1_str(buf, def_header.table_name_addr);
        let columns = FDBColumnHeaderC::cast_slice(
            buf,
            def_header.column_header_list_addr,
            def_header.column_count,
        );
        let buckets = FDBBucketHeaderC::cast_slice(
            buf,
            data_header.bucket_header_list_addr,
            data_header.bucket_count,
        );

        Table {
            buf,
            name,
            columns,
            buckets,
        }
    }
}

impl<'a> Tables<'a> {
    pub fn len(&self) -> usize {
        self.slice.len()
    }

    pub fn get(self, index: usize) -> Option<Table<'a>> {
        self.slice.get(index).map(map_table_header(self.buf))
    }

    pub fn iter(&self) -> impl Iterator<Item = Table<'a>> {
        self.slice.iter().map(map_table_header(self.buf))
    }
}

#[derive(Copy, Clone)]
pub struct Table<'a> {
    buf: &'a [u8],
    name: &'a Latin1Str,
    columns: &'a [FDBColumnHeaderC],
    buckets: &'a [FDBBucketHeaderC],
}

fn map_column_header<'a>(buf: &'a [u8]) -> impl Fn(&'a FDBColumnHeaderC) -> Column<'a> {
    move |header: &FDBColumnHeaderC| {
        let column_header = header.extract();
        let name = get_latin1_str(buf, column_header.column_name_addr);
        let domain = ValueType::from(column_header.column_data_type);

        Column { name, domain }
    }
}

fn get_row_header_list_entry<'a>(buf: &'a [u8], addr: u32) -> Option<&'a FDBRowHeaderListEntryC> {
    if addr == u32::MAX {
        None
    } else {
        Some(FDBRowHeaderListEntryC::cast(buf, addr))
    }
}

fn map_bucket_header<'a>(buf: &'a [u8]) -> impl Fn(&'a FDBBucketHeaderC) -> Bucket<'a> {
    move |header: &FDBBucketHeaderC| {
        let bucket_header = header.extract();
        let addr = bucket_header.row_header_list_head_addr;
        let first = get_row_header_list_entry(buf, addr);
        Bucket { buf, first }
    }
}

impl<'a> Table<'a> {
    /// Get the undecoded name of the table
    pub fn name_raw(&self) -> &Latin1Str {
        self.name
    }

    /// Get the name of the table
    pub fn name(&self) -> Cow<str> {
        self.name.decode()
    }

    /// Get the column at the index
    ///
    /// **Note**: This does some computation, call only once per colum if possible
    pub fn column_at(&self, index: usize) -> Option<Column<'a>> {
        self.columns.get(index).map(map_column_header(self.buf))
    }

    /// Get the column iterator
    ///
    /// **Note**: This does some computation, call only once if possible
    pub fn column_iter(&self) -> impl Iterator<Item = Column<'a>> {
        self.columns.iter().map(map_column_header(self.buf))
    }

    /// The amount of columns in this table
    pub fn column_count(&self) -> usize {
        self.columns.len()
    }

    /// Get the bucket at the index
    ///
    /// **Note**: This does some computation, call only once per bucket if possible
    pub fn bucket_at(&self, index: usize) -> Option<Bucket<'a>> {
        self.buckets.get(index).map(map_bucket_header(self.buf))
    }

    /// Get the bucket iterator
    ///
    /// **Note**: This does some computation, call only once if possible
    pub fn bucket_iter(&self) -> impl Iterator<Item = Bucket<'a>> {
        self.buckets.iter().map(map_bucket_header(self.buf))
    }

    /// Get the amount of buckets
    pub fn bucket_count(&self) -> usize {
        self.buckets.len()
    }
}

pub struct Column<'a> {
    name: &'a Latin1Str,
    domain: ValueType,
}

impl<'a> Column<'a> {
    pub fn name(&self) -> Cow<str> {
        self.name.decode()
    }

    pub fn value_type(&self) -> ValueType {
        self.domain
    }
}

pub struct Bucket<'a> {
    buf: &'a [u8],
    first: Option<&'a FDBRowHeaderListEntryC>,
}

impl<'a> Bucket<'a> {
    pub fn row_iter(&self) -> RowHeaderIter<'a> {
        RowHeaderIter {
            buf: self.buf,
            next: self.first,
        }
    }
}

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
            let row_header = FDBRowHeaderC::cast(self.buf, entry.row_header_addr).extract();

            let fields = FDBFieldDataC::cast_slice(
                self.buf,
                row_header.field_data_list_addr,
                row_header.field_count,
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

pub struct Row<'a> {
    buf: &'a [u8],
    fields: &'a [FDBFieldDataC],
}

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
                let val = LEI64::cast(buf, addr).extract();
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

pub enum Field<'a> {
    Nothing,
    Integer(i32),
    Float(f32),
    Text(&'a Latin1Str),
    Boolean(bool),
    BigInt(i64),
    VarChar(&'a Latin1Str),
}