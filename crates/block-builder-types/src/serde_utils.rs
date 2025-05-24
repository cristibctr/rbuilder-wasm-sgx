pub mod serde_bytes_array {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use serde::de::{Error, Visitor};
    use std::fmt;
    use std::marker::PhantomData;

    pub fn serialize<S, const N: usize>(bytes: &[u8; N], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if serializer.is_human_readable() {
            let hex = hex::encode(bytes);
            serializer.serialize_str(&hex)
        } else {
            bytes.serialize(serializer)
        }
    }

    pub fn deserialize<'de, D, const N: usize>(deserializer: D) -> Result<[u8; N], D::Error>
    where
        D: Deserializer<'de>,
    {
        struct BytesVisitor<const N: usize>(PhantomData<[u8; N]>);

        impl<'de, const N: usize> Visitor<'de> for BytesVisitor<N> {
            type Value = [u8; N];

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                write!(formatter, "a byte array of length {}", N)
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: Error,
            {
                let bytes = hex::decode(v)
                    .map_err(|_| Error::custom("invalid hex string"))?;
                if bytes.len() != N {
                    return Err(Error::custom(format!(
                        "expected {} bytes, got {}",
                        N,
                        bytes.len()
                    )));
                }
                let mut result = [0u8; N];
                result.copy_from_slice(&bytes);
                Ok(result)
            }

            fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
            where
                E: Error,
            {
                if v.len() != N {
                    return Err(Error::custom(format!(
                        "expected {} bytes, got {}",
                        N,
                        v.len()
                    )));
                }
                let mut result = [0u8; N];
                result.copy_from_slice(v);
                Ok(result)
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                let mut result = [0u8; N];
                for i in 0..N {
                    match seq.next_element()? {
                        Some(v) => result[i] = v,
                        None => return Err(Error::custom(format!(
                            "expected {} bytes, got {}",
                            N, i
                        ))),
                    }
                }
                Ok(result)
            }
        }

        if deserializer.is_human_readable() {
            deserializer.deserialize_str(BytesVisitor(PhantomData))
        } else {
            deserializer.deserialize_bytes(BytesVisitor(PhantomData))
        }
    }
}