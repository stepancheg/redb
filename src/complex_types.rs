use crate::types::{TypeName, Value};
use crate::{Result, StorageError};
use alloc::format;
use alloc::vec::Vec;

// Encode len as a varint and store it at the end of output
pub(super) fn encode_varint_len(len: usize, output: &mut Vec<u8>) {
    if len < 254 {
        output.push(len.try_into().unwrap());
    } else if len <= u16::MAX.into() {
        let u16_len: u16 = len.try_into().unwrap();
        output.push(254);
        output.extend_from_slice(&u16_len.to_le_bytes());
    } else {
        let u32_len: u32 = len.try_into().unwrap();
        output.push(255);
        output.extend_from_slice(&u32_len.to_le_bytes());
    }
}

// Decode a variable length int starting at the beginning of data
// Returns (decoded length, length consumed of `data`)
pub(super) fn decode_varint_len(data: &[u8]) -> (usize, usize) {
    match data[0] {
        0..=253 => (data[0] as usize, 1),
        254 => (
            u16::from_le_bytes(data[1..3].try_into().unwrap()) as usize,
            3,
        ),
        255 => (
            u32::from_le_bytes(data[1..5].try_into().unwrap()) as usize,
            5,
        ),
    }
}

impl<T: Value> Value for Vec<T> {
    type SelfType<'a>
        = Vec<T::SelfType<'a>>
    where
        Self: 'a;
    type AsBytes<'a>
        = Vec<u8>
    where
        Self: 'a;

    fn fixed_width() -> Option<usize> {
        None
    }

    fn from_bytes<'a>(data: &'a [u8]) -> Vec<T::SelfType<'a>>
    where
        Self: 'a,
    {
        // Value::from_bytes cannot return Result. try_vec_from_bytes reports the same input as
        // StorageError::Corrupted; every read still goes through this method.
        try_vec_from_bytes::<T>(data).unwrap_or_else(|error| panic!("{error}"))
    }

    fn as_bytes<'a, 'b: 'a>(value: &'a Vec<T::SelfType<'b>>) -> Vec<u8>
    where
        Self: 'b,
    {
        let mut result = if let Some(width) = T::fixed_width() {
            Vec::with_capacity(value.len() * width + 5)
        } else {
            Vec::with_capacity(value.len() * 2 + 5)
        };
        encode_varint_len(value.len(), &mut result);

        for element in value {
            let serialized = T::as_bytes(element);
            if T::fixed_width().is_none() {
                encode_varint_len(serialized.as_ref().len(), &mut result);
            }
            result.extend_from_slice(serialized.as_ref());
        }
        result
    }

    fn type_name() -> TypeName {
        let inner = T::type_name();
        TypeName::internal(&format!("Vec<{}>", inner.name()))
            .into_composite(inner.is_user_defined())
    }
}

// `Value::from_bytes` is infallible, so this is the fallible decode. Nothing in the read path
// can call it: table reads decode through `Value::from_bytes`.
pub(crate) fn try_vec_from_bytes<T: Value>(data: &[u8]) -> Result<Vec<T::SelfType<'_>>> {
    let (elements, mut offset) = read_varint_len(data, 0)?;
    let mut result = Vec::with_capacity(elements.min(data.len()));
    for _ in 0..elements {
        let element_len = if let Some(len) = T::fixed_width() {
            len
        } else {
            let (len, consumed) = read_varint_len(data, offset)?;
            offset += consumed;
            len
        };
        let Some(end) = offset
            .checked_add(element_len)
            .filter(|end| *end <= data.len())
        else {
            return Err(StorageError::Corrupted(format!(
                "Invalid Vec element length {element_len} at offset {offset} in a {} byte value",
                data.len()
            )));
        };
        result.push(T::from_bytes(&data[offset..end]));
        offset = end;
    }
    if offset != data.len() {
        return Err(StorageError::Corrupted(format!(
            "Invalid Vec length: decoded {offset} bytes from a {} byte value",
            data.len()
        )));
    }
    Ok(result)
}

fn read_varint_len(data: &[u8], offset: usize) -> Result<(usize, usize)> {
    let rest = data.get(offset..).unwrap_or(&[]);
    let Some(&tag) = rest.first() else {
        return Err(StorageError::Corrupted(format!(
            "Invalid Vec length prefix at offset {offset} in a {} byte value",
            data.len()
        )));
    };
    match tag {
        0..=253 => Ok((usize::from(tag), 1)),
        254 => {
            let Some(bytes) = rest.get(1..3) else {
                return Err(StorageError::Corrupted(format!(
                    "Invalid Vec length prefix {tag} at offset {offset}: need 3 bytes, {} remain in a {} byte value",
                    rest.len(),
                    data.len()
                )));
            };
            Ok((
                usize::from(u16::from_le_bytes(bytes.try_into().unwrap())),
                3,
            ))
        }
        255 => {
            let Some(bytes) = rest.get(1..5) else {
                return Err(StorageError::Corrupted(format!(
                    "Invalid Vec length prefix {tag} at offset {offset}: need 5 bytes, {} remain in a {} byte value",
                    rest.len(),
                    data.len()
                )));
            };
            Ok((u32::from_le_bytes(bytes.try_into().unwrap()) as usize, 5))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::try_vec_from_bytes;
    use crate::{StorageError, Value};

    #[test]
    fn malformed_vec_length_is_corrupted() {
        let bytes = <Vec<u8> as Value>::as_bytes(&vec![1, 2, 3]);
        assert_eq!(
            try_vec_from_bytes::<u8>(bytes.as_ref()).unwrap(),
            vec![1, 2, 3]
        );

        let err = try_vec_from_bytes::<u8>(&[1]).unwrap_err();
        let StorageError::Corrupted(message) = err else {
            panic!("unexpected error");
        };
        assert!(message.contains("length 1"), "{message}");

        let err = try_vec_from_bytes::<u8>(&[0, 9]).unwrap_err();
        let StorageError::Corrupted(message) = err else {
            panic!("unexpected error");
        };
        assert!(
            message.contains("decoded 1") && message.contains("2 byte"),
            "{message}"
        );

        let err = try_vec_from_bytes::<u8>(&[254]).unwrap_err();
        let StorageError::Corrupted(message) = err else {
            panic!("unexpected error");
        };
        assert!(message.contains("254"), "{message}");
    }
}
