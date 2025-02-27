use std::sync::Arc;

use crate::{
    array::{Array, ListArray},
    bitmap::{Bitmap, MutableBitmap},
    buffer::{Buffer, MutableBuffer},
    datatypes::{DataType, Field},
    error::{ArrowError, Result},
};

/// trait describing deserialized repetition and definition levels
pub trait Nested: std::fmt::Debug {
    fn inner(&mut self) -> (Buffer<i64>, Option<Bitmap>);

    fn last_offset(&self) -> i64;

    fn push(&mut self, length: i64, is_valid: bool);

    fn offsets(&mut self) -> &[i64];

    fn close(&mut self, length: i64);

    fn is_nullable(&self) -> bool;
}

#[derive(Debug, Default)]
pub struct NestedPrimitive {
    is_nullable: bool,
}

impl NestedPrimitive {
    pub fn new(is_nullable: bool) -> Self {
        Self { is_nullable }
    }
}

impl Nested for NestedPrimitive {
    fn inner(&mut self) -> (Buffer<i64>, Option<Bitmap>) {
        (Default::default(), Default::default())
    }

    #[inline]
    fn last_offset(&self) -> i64 {
        0
    }

    fn is_nullable(&self) -> bool {
        self.is_nullable
    }

    fn push(&mut self, _value: i64, _is_valid: bool) {}

    fn offsets(&mut self) -> &[i64] {
        &[]
    }

    fn close(&mut self, _length: i64) {}
}

#[derive(Debug, Default)]
pub struct NestedOptional {
    pub validity: MutableBitmap,
    pub offsets: MutableBuffer<i64>,
}

impl Nested for NestedOptional {
    fn inner(&mut self) -> (Buffer<i64>, Option<Bitmap>) {
        let offsets = std::mem::take(&mut self.offsets);
        let validity = std::mem::take(&mut self.validity);
        (offsets.into(), validity.into())
    }

    #[inline]
    fn last_offset(&self) -> i64 {
        *self.offsets.last().unwrap()
    }

    fn is_nullable(&self) -> bool {
        true
    }

    fn push(&mut self, value: i64, is_valid: bool) {
        self.offsets.push(value);
        self.validity.push(is_valid);
    }

    fn offsets(&mut self) -> &[i64] {
        &self.offsets
    }

    fn close(&mut self, length: i64) {
        self.offsets.push(length)
    }
}

impl NestedOptional {
    pub fn with_capacity(capacity: usize) -> Self {
        let offsets = MutableBuffer::<i64>::with_capacity(capacity + 1);
        let validity = MutableBitmap::with_capacity(capacity);
        Self { validity, offsets }
    }
}

#[derive(Debug, Default)]
pub struct NestedValid {
    pub offsets: MutableBuffer<i64>,
}

impl Nested for NestedValid {
    fn inner(&mut self) -> (Buffer<i64>, Option<Bitmap>) {
        let offsets = std::mem::take(&mut self.offsets);
        (offsets.into(), None)
    }

    fn is_nullable(&self) -> bool {
        false
    }

    #[inline]
    fn last_offset(&self) -> i64 {
        *self.offsets.last().unwrap()
    }

    fn push(&mut self, value: i64, _is_valid: bool) {
        self.offsets.push(value);
    }

    fn offsets(&mut self) -> &[i64] {
        &self.offsets
    }

    fn close(&mut self, length: i64) {
        self.offsets.push(length)
    }
}

impl NestedValid {
    pub fn with_capacity(capacity: usize) -> Self {
        let offsets = MutableBuffer::<i64>::with_capacity(capacity + 1);
        Self { offsets }
    }
}

pub fn extend_offsets<R, D>(
    rep_levels: R,
    def_levels: D,
    is_nullable: bool,
    max_rep: u32,
    max_def: u32,
    nested: &mut Vec<Box<dyn Nested>>,
) where
    R: Iterator<Item = u32>,
    D: Iterator<Item = u32>,
{
    let mut values_count = vec![0; nested.len()];
    let mut prev_def: u32 = 0;
    let mut is_first = true;

    rep_levels.zip(def_levels).for_each(|(rep, def)| {
        let mut closures = max_rep - rep;
        if prev_def <= 1 {
            closures = 1;
        };
        if is_first {
            // close on first run to ensure offsets start with 0.
            closures = max_rep;
            is_first = false;
        }

        nested
            .iter_mut()
            .zip(values_count.iter())
            .enumerate()
            .skip(rep as usize)
            .take((rep + closures) as usize)
            .for_each(|(depth, (nested, length))| {
                let is_null = (def - rep) as usize == depth && depth == rep as usize;
                nested.push(*length, !is_null);
            });

        values_count
            .iter_mut()
            .enumerate()
            .for_each(|(depth, values)| {
                if depth == 1 {
                    if def == max_def || (is_nullable && def == max_def - 1) {
                        *values += 1
                    }
                } else if depth == 0 {
                    let a = nested
                        .get(depth + 1)
                        .map(|x| x.is_nullable())
                        .unwrap_or_default(); // todo: cumsum this
                    let condition = rep == 1
                        || rep == 0
                            && def >= max_def.saturating_sub((a as u32) + (is_nullable as u32));

                    if condition {
                        *values += 1;
                    }
                }
            });
        prev_def = def;
    });

    // close validities
    nested
        .iter_mut()
        .zip(values_count.iter())
        .for_each(|(nested, length)| {
            nested.close(*length);
        });
}

pub fn init_nested(field: &Field, capacity: usize, container: &mut Vec<Box<dyn Nested>>) {
    let is_nullable = field.is_nullable();

    use crate::datatypes::PhysicalType::*;
    match field.data_type().to_physical_type() {
        Null | Boolean | Primitive(_) | FixedSizeBinary | Binary | LargeBinary | Utf8
        | LargeUtf8 | Dictionary(_) => {
            container.push(Box::new(NestedPrimitive::new(is_nullable)) as Box<dyn Nested>)
        }
        List | LargeList | FixedSizeList => {
            if is_nullable {
                container.push(Box::new(NestedOptional::with_capacity(capacity)) as Box<dyn Nested>)
            } else {
                container.push(Box::new(NestedValid::with_capacity(capacity)) as Box<dyn Nested>)
            }
            match field.data_type().to_logical_type() {
                DataType::List(ref inner)
                | DataType::LargeList(ref inner)
                | DataType::FixedSizeList(ref inner, _) => {
                    init_nested(inner.as_ref(), capacity, container)
                }
                _ => unreachable!(),
            };
        }
        Struct => {
            container.push(Box::new(NestedPrimitive::new(is_nullable)) as Box<dyn Nested>);
            if let DataType::Struct(fields) = field.data_type().to_logical_type() {
                fields
                    .iter()
                    .for_each(|field| init_nested(field, capacity, container));
            } else {
                unreachable!()
            }
        }
        _ => todo!(),
    }
}

pub fn create_list(
    data_type: DataType,
    nested: &mut Vec<Box<dyn Nested>>,
    values: Arc<dyn Array>,
) -> Result<Box<dyn Array>> {
    Ok(match data_type {
        DataType::List(_) => {
            let (offsets, validity) = nested.pop().unwrap().inner();

            let offsets = Buffer::<i32>::from_trusted_len_iter(offsets.iter().map(|x| *x as i32));
            Box::new(ListArray::<i32>::from_data(
                data_type, offsets, values, validity,
            ))
        }
        DataType::LargeList(_) => {
            let (offsets, validity) = nested.pop().unwrap().inner();

            Box::new(ListArray::<i64>::from_data(
                data_type, offsets, values, validity,
            ))
        }
        _ => {
            return Err(ArrowError::NotYetImplemented(format!(
                "Read nested datatype {:?}",
                data_type
            )))
        }
    })
}
