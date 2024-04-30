// Licensed to the Apache Software Foundation (ASF) under one
// or more contributor license agreements.  See the NOTICE file
// distributed with this work for additional information
// regarding copyright ownership.  The ASF licenses this file
// to you under the Apache License, Version 2.0 (the
// "License"); you may not use this file except in compliance
// with the License.  You may obtain a copy of the License at
//
//   http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing,
// software distributed under the License is distributed on an
// "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied.  See the License for the
// specific language governing permissions and limitations
// under the License.

//! This includes utilities for hashing and murmur3 hashing.

use arrow::datatypes::{ArrowNativeTypeOp, UInt16Type, UInt32Type, UInt64Type, UInt8Type};
use std::sync::Arc;

use datafusion::{
    arrow::{
        array::*,
        datatypes::{
            ArrowDictionaryKeyType, ArrowNativeType, DataType, Int16Type, Int32Type, Int64Type,
            Int8Type, TimeUnit,
        },
    },
    error::{DataFusionError, Result},
};

#[inline]
pub(crate) fn spark_compatible_murmur3_hash<T: AsRef<[u8]>>(data: T, seed: u32) -> u32 {
    #[inline]
    fn mix_k1(mut k1: i32) -> i32 {
        k1 = k1.mul_wrapping(0xcc9e2d51u32 as i32);
        k1 = k1.rotate_left(15);
        k1 = k1.mul_wrapping(0x1b873593u32 as i32);
        k1
    }

    #[inline]
    fn mix_h1(mut h1: i32, k1: i32) -> i32 {
        h1 ^= k1;
        h1 = h1.rotate_left(13);
        h1 = h1.mul_wrapping(5).add_wrapping(0xe6546b64u32 as i32);
        h1
    }

    #[inline]
    fn fmix(mut h1: i32, len: i32) -> i32 {
        h1 ^= len;
        h1 ^= (h1 as u32 >> 16) as i32;
        h1 = h1.mul_wrapping(0x85ebca6bu32 as i32);
        h1 ^= (h1 as u32 >> 13) as i32;
        h1 = h1.mul_wrapping(0xc2b2ae35u32 as i32);
        h1 ^= (h1 as u32 >> 16) as i32;
        h1
    }

    #[inline]
    unsafe fn hash_bytes_by_int(data: &[u8], seed: u32) -> i32 {
        // safety: data length must be aligned to 4 bytes
        let mut h1 = seed as i32;
        for i in (0..data.len()).step_by(4) {
            let ints = data.as_ptr().add(i) as *const i32;
            let mut half_word = ints.read_unaligned();
            if cfg!(target_endian = "big") {
                half_word = half_word.reverse_bits();
            }
            h1 = mix_h1(h1, mix_k1(half_word));
        }
        h1
    }
    let data = data.as_ref();
    let len = data.len();
    let len_aligned = len - len % 4;

    // safety:
    // avoid boundary checking in performance critical codes.
    // all operations are garenteed to be safe
    unsafe {
        let mut h1 = hash_bytes_by_int(
            std::slice::from_raw_parts(data.get_unchecked(0), len_aligned),
            seed,
        );

        for i in len_aligned..len {
            let half_word = *data.get_unchecked(i) as i8 as i32;
            h1 = mix_h1(h1, mix_k1(half_word));
        }
        fmix(h1, len as i32) as u32
    }
}

#[test]
fn test_murmur3() {
    let _hashes = ["", "a", "ab", "abc", "abcd", "abcde"]
        .into_iter()
        .map(|s| spark_compatible_murmur3_hash(s.as_bytes(), 42) as i32)
        .collect::<Vec<_>>();
    let _expected = vec![
        142593372, 1485273170, -97053317, 1322437556, -396302900, 814637928,
    ];
}

macro_rules! hash_array {
    ($array_type:ident, $column: ident, $hashes: ident) => {
        let array = $column.as_any().downcast_ref::<$array_type>().unwrap();
        if array.null_count() == 0 {
            for (i, hash) in $hashes.iter_mut().enumerate() {
                *hash = spark_compatible_murmur3_hash(&array.value(i), *hash);
            }
        } else {
            for (i, hash) in $hashes.iter_mut().enumerate() {
                if !array.is_null(i) {
                    *hash = spark_compatible_murmur3_hash(&array.value(i), *hash);
                }
            }
        }
    };
}

macro_rules! hash_array_primitive {
    ($array_type:ident, $column: ident, $ty: ident, $hashes: ident) => {
        let array = $column.as_any().downcast_ref::<$array_type>().unwrap();
        let values = array.values();

        if array.null_count() == 0 {
            for (hash, value) in $hashes.iter_mut().zip(values.iter()) {
                *hash = spark_compatible_murmur3_hash((*value as $ty).to_le_bytes(), *hash);
            }
        } else {
            for (i, (hash, value)) in $hashes.iter_mut().zip(values.iter()).enumerate() {
                if !array.is_null(i) {
                    *hash = spark_compatible_murmur3_hash((*value as $ty).to_le_bytes(), *hash);
                }
            }
        }
    };
}

macro_rules! hash_array_primitive_float {
    ($array_type:ident, $column: ident, $ty: ident, $ty2: ident, $hashes: ident) => {
        let array = $column.as_any().downcast_ref::<$array_type>().unwrap();
        let values = array.values();

        if array.null_count() == 0 {
            for (hash, value) in $hashes.iter_mut().zip(values.iter()) {
                // Spark uses 0 as hash for -0.0, see `Murmur3Hash` expression.
                if *value == 0.0 && value.is_sign_negative() {
                    *hash = spark_compatible_murmur3_hash((0 as $ty2).to_le_bytes(), *hash);
                } else {
                    *hash = spark_compatible_murmur3_hash((*value as $ty).to_le_bytes(), *hash);
                }
            }
        } else {
            for (i, (hash, value)) in $hashes.iter_mut().zip(values.iter()).enumerate() {
                if !array.is_null(i) {
                    // Spark uses 0 as hash for -0.0, see `Murmur3Hash` expression.
                    if *value == 0.0 && value.is_sign_negative() {
                        *hash = spark_compatible_murmur3_hash((0 as $ty2).to_le_bytes(), *hash);
                    } else {
                        *hash = spark_compatible_murmur3_hash((*value as $ty).to_le_bytes(), *hash);
                    }
                }
            }
        }
    };
}

macro_rules! hash_array_decimal {
    ($array_type:ident, $column: ident, $hashes: ident) => {
        let array = $column.as_any().downcast_ref::<$array_type>().unwrap();

        if array.null_count() == 0 {
            for (i, hash) in $hashes.iter_mut().enumerate() {
                *hash = spark_compatible_murmur3_hash(array.value(i).to_le_bytes(), *hash);
            }
        } else {
            for (i, hash) in $hashes.iter_mut().enumerate() {
                if !array.is_null(i) {
                    *hash = spark_compatible_murmur3_hash(array.value(i).to_le_bytes(), *hash);
                }
            }
        }
    };
}

/// Hash the values in a dictionary array
fn create_hashes_dictionary<K: ArrowDictionaryKeyType>(
    array: &ArrayRef,
    hashes_buffer: &mut [u32],
) -> Result<()> {
    let dict_array = array.as_any().downcast_ref::<DictionaryArray<K>>().unwrap();

    // Hash each dictionary value once, and then use that computed
    // hash for each key value to avoid a potentially expensive
    // redundant hashing for large dictionary elements (e.g. strings)
    let dict_values = Arc::clone(dict_array.values());
    let mut dict_hashes = vec![0; dict_values.len()];
    create_hashes(&[dict_values], &mut dict_hashes)?;

    for (hash, key) in hashes_buffer.iter_mut().zip(dict_array.keys().iter()) {
        if let Some(key) = key {
            let idx = key.to_usize().ok_or_else(|| {
                DataFusionError::Internal(format!(
                    "Can not convert key value {:?} to usize in dictionary of type {:?}",
                    key,
                    dict_array.data_type()
                ))
            })?;
            *hash = dict_hashes[idx]
        } // no update for Null, consistent with other hashes
    }
    Ok(())
}

/// Creates hash values for every row, based on the values in the
/// columns.
///
/// The number of rows to hash is determined by `hashes_buffer.len()`.
/// `hashes_buffer` should be pre-sized appropriately
pub fn create_hashes<'a>(
    arrays: &[ArrayRef],
    hashes_buffer: &'a mut [u32],
) -> Result<&'a mut [u32]> {
    for col in arrays {
        match col.data_type() {
            DataType::Boolean => {
                let array = col.as_any().downcast_ref::<BooleanArray>().unwrap();
                if array.null_count() == 0 {
                    for (i, hash) in hashes_buffer.iter_mut().enumerate() {
                        *hash = spark_compatible_murmur3_hash(
                            i32::from(array.value(i)).to_le_bytes(),
                            *hash,
                        );
                    }
                } else {
                    for (i, hash) in hashes_buffer.iter_mut().enumerate() {
                        if !array.is_null(i) {
                            *hash = spark_compatible_murmur3_hash(
                                i32::from(array.value(i)).to_le_bytes(),
                                *hash,
                            );
                        }
                    }
                }
            }
            DataType::Int8 => {
                hash_array_primitive!(Int8Array, col, i32, hashes_buffer);
            }
            DataType::Int16 => {
                hash_array_primitive!(Int16Array, col, i32, hashes_buffer);
            }
            DataType::Int32 => {
                hash_array_primitive!(Int32Array, col, i32, hashes_buffer);
            }
            DataType::Int64 => {
                hash_array_primitive!(Int64Array, col, i64, hashes_buffer);
            }
            DataType::Float32 => {
                hash_array_primitive_float!(Float32Array, col, f32, i32, hashes_buffer);
            }
            DataType::Float64 => {
                hash_array_primitive_float!(Float64Array, col, f64, i64, hashes_buffer);
            }
            DataType::Timestamp(TimeUnit::Second, _) => {
                hash_array_primitive!(TimestampSecondArray, col, i64, hashes_buffer);
            }
            DataType::Timestamp(TimeUnit::Millisecond, _) => {
                hash_array_primitive!(TimestampMillisecondArray, col, i64, hashes_buffer);
            }
            DataType::Timestamp(TimeUnit::Microsecond, _) => {
                hash_array_primitive!(TimestampMicrosecondArray, col, i64, hashes_buffer);
            }
            DataType::Timestamp(TimeUnit::Nanosecond, _) => {
                hash_array_primitive!(TimestampNanosecondArray, col, i64, hashes_buffer);
            }
            DataType::Date32 => {
                hash_array_primitive!(Date32Array, col, i32, hashes_buffer);
            }
            DataType::Date64 => {
                hash_array_primitive!(Date64Array, col, i64, hashes_buffer);
            }
            DataType::Utf8 => {
                hash_array!(StringArray, col, hashes_buffer);
            }
            DataType::LargeUtf8 => {
                hash_array!(LargeStringArray, col, hashes_buffer);
            }
            DataType::Binary => {
                hash_array!(BinaryArray, col, hashes_buffer);
            }
            DataType::LargeBinary => {
                hash_array!(LargeBinaryArray, col, hashes_buffer);
            }
            DataType::FixedSizeBinary(_) => {
                hash_array!(FixedSizeBinaryArray, col, hashes_buffer);
            }
            DataType::Decimal128(_, _) => {
                hash_array_decimal!(Decimal128Array, col, hashes_buffer);
            }
            DataType::Dictionary(index_type, _) => match **index_type {
                DataType::Int8 => {
                    create_hashes_dictionary::<Int8Type>(col, hashes_buffer)?;
                }
                DataType::Int16 => {
                    create_hashes_dictionary::<Int16Type>(col, hashes_buffer)?;
                }
                DataType::Int32 => {
                    create_hashes_dictionary::<Int32Type>(col, hashes_buffer)?;
                }
                DataType::Int64 => {
                    create_hashes_dictionary::<Int64Type>(col, hashes_buffer)?;
                }
                DataType::UInt8 => {
                    create_hashes_dictionary::<UInt8Type>(col, hashes_buffer)?;
                }
                DataType::UInt16 => {
                    create_hashes_dictionary::<UInt16Type>(col, hashes_buffer)?;
                }
                DataType::UInt32 => {
                    create_hashes_dictionary::<UInt32Type>(col, hashes_buffer)?;
                }
                DataType::UInt64 => {
                    create_hashes_dictionary::<UInt64Type>(col, hashes_buffer)?;
                }
                _ => {
                    return Err(DataFusionError::Internal(format!(
                        "Unsupported dictionary type in hasher hashing: {}",
                        col.data_type(),
                    )))
                }
            },
            _ => {
                // This is internal because we should have caught this before.
                return Err(DataFusionError::Internal(format!(
                    "Unsupported data type in hasher: {}",
                    col.data_type()
                )));
            }
        }
    }
    Ok(hashes_buffer)
}

pub(crate) fn pmod(hash: u32, n: usize) -> usize {
    let hash = hash as i32;
    let n = n as i32;
    let r = hash % n;
    let result = if r < 0 { (r + n) % n } else { r };
    result as usize
}

#[cfg(test)]
mod tests {
    use arrow::array::{Float32Array, Float64Array};
    use std::sync::Arc;

    use crate::execution::datafusion::spark_hash::{create_hashes, pmod};
    use datafusion::arrow::array::{ArrayRef, Int32Array, Int64Array, Int8Array, StringArray};

    macro_rules! test_hashes {
        ($ty:ty, $values:expr, $expected:expr) => {
            let i = Arc::new(<$ty>::from($values)) as ArrayRef;
            let mut hashes = vec![42; $values.len()];
            create_hashes(&[i], &mut hashes).unwrap();
            assert_eq!(hashes, $expected);
        };
    }

    #[test]
    fn test_i8() {
        test_hashes!(
            Int8Array,
            vec![Some(1), Some(0), Some(-1), Some(i8::MAX), Some(i8::MIN)],
            vec![0xdea578e3, 0x379fae8f, 0xa0590e3d, 0x43b4d8ed, 0x422a1365]
        );
        // with null input
        test_hashes!(
            Int8Array,
            vec![Some(1), None, Some(-1), Some(i8::MAX), Some(i8::MIN)],
            vec![0xdea578e3, 42, 0xa0590e3d, 0x43b4d8ed, 0x422a1365]
        );
    }

    #[test]
    fn test_i32() {
        test_hashes!(
            Int32Array,
            vec![Some(1), Some(0), Some(-1), Some(i32::MAX), Some(i32::MIN)],
            vec![0xdea578e3, 0x379fae8f, 0xa0590e3d, 0x07fb67e7, 0x2b1f0fc6]
        );
        // with null input
        test_hashes!(
            Int32Array,
            vec![
                Some(1),
                Some(0),
                Some(-1),
                None,
                Some(i32::MAX),
                Some(i32::MIN)
            ],
            vec![0xdea578e3, 0x379fae8f, 0xa0590e3d, 42, 0x07fb67e7, 0x2b1f0fc6]
        );
    }

    #[test]
    fn test_i64() {
        test_hashes!(
            Int64Array,
            vec![Some(1), Some(0), Some(-1), Some(i64::MAX), Some(i64::MIN)],
            vec![0x99f0149d, 0x9c67b85d, 0xc8008529, 0xa05b5d7b, 0xcd1e64fb]
        );
        // with null input
        test_hashes!(
            Int64Array,
            vec![
                Some(1),
                Some(0),
                Some(-1),
                None,
                Some(i64::MAX),
                Some(i64::MIN)
            ],
            vec![0x99f0149d, 0x9c67b85d, 0xc8008529, 42, 0xa05b5d7b, 0xcd1e64fb]
        );
    }

    #[test]
    fn test_f32() {
        test_hashes!(
            Float32Array,
            vec![
                Some(1.0),
                Some(0.0),
                Some(-0.0),
                Some(-1.0),
                Some(99999999999.99999999999),
                Some(-99999999999.99999999999),
            ],
            vec![0xe434cc39, 0x379fae8f, 0x379fae8f, 0xdc0da8eb, 0xcbdc340f, 0xc0361c86]
        );
        // with null input
        test_hashes!(
            Float32Array,
            vec![
                Some(1.0),
                Some(0.0),
                Some(-0.0),
                Some(-1.0),
                None,
                Some(99999999999.99999999999),
                Some(-99999999999.99999999999)
            ],
            vec![0xe434cc39, 0x379fae8f, 0x379fae8f, 0xdc0da8eb, 42, 0xcbdc340f, 0xc0361c86]
        );
    }

    #[test]
    fn test_f64() {
        test_hashes!(
            Float64Array,
            vec![
                Some(1.0),
                Some(0.0),
                Some(-0.0),
                Some(-1.0),
                Some(99999999999.99999999999),
                Some(-99999999999.99999999999),
            ],
            vec![0xe4876492, 0x9c67b85d, 0x9c67b85d, 0x13d81357, 0xb87e1595, 0xa0eef9f9]
        );
        // with null input
        test_hashes!(
            Float64Array,
            vec![
                Some(1.0),
                Some(0.0),
                Some(-0.0),
                Some(-1.0),
                None,
                Some(99999999999.99999999999),
                Some(-99999999999.99999999999)
            ],
            vec![0xe4876492, 0x9c67b85d, 0x9c67b85d, 0x13d81357, 42, 0xb87e1595, 0xa0eef9f9]
        );
    }

    #[test]
    fn test_str() {
        test_hashes!(
            StringArray,
            vec!["hello", "bar", "", "😁", "天地"],
            vec![3286402344, 2486176763, 142593372, 885025535, 2395000894]
        );
        // test with null input
        test_hashes!(
            StringArray,
            vec![
                Some("hello"),
                Some("bar"),
                None,
                Some(""),
                Some("😁"),
                Some("天地")
            ],
            vec![3286402344, 2486176763, 42, 142593372, 885025535, 2395000894]
        );
    }

    #[test]
    fn test_pmod() {
        let i: Vec<u32> = vec![0x99f0149d, 0x9c67b85d, 0xc8008529, 0xa05b5d7b, 0xcd1e64fb];
        let result = i.into_iter().map(|i| pmod(i, 200)).collect::<Vec<usize>>();

        // expected partition from Spark with n=200
        let expected = vec![69, 5, 193, 171, 115];
        assert_eq!(result, expected);
    }
}
